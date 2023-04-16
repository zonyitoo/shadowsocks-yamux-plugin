use std::{
    env,
    io::{self, ErrorKind},
    sync::Arc,
    time::Duration,
};

use env_logger::Builder;
use futures::StreamExt;
use log::{debug, error, info, trace};
use shadowsocks::{
    config::ServerType,
    context::{Context, SharedContext},
    dns_resolver::DnsResolver,
    lookup_then,
    lookup_then_connect,
    net::{TcpListener, TcpStream, UdpSocket},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream as TokioTcpStream,
    time::{self, Instant},
};
use tokio_yamux::{Config, Session, StreamHandle};

use yamux_plugin::PluginOpts;

enum ConnectionType {
    Tcp,
    Udp,
}

async fn handle_tcp_connection(
    context: &Context,
    local_host: &str,
    local_port: u16,
    plugin_opts: &PluginOpts,
    mut yamux_stream: StreamHandle,
    magic_buffer: &[u8],
) -> io::Result<()> {
    let connect_opts = plugin_opts.as_connect_opts();

    let local_stream_result = lookup_then_connect!(context, local_host, local_port, |addr| {
        TcpStream::connect_with_opts(&addr, &connect_opts).await
    });

    let mut local_stream = match local_stream_result {
        Ok((_, s)) => {
            trace!(
                "connected tcp host {}:{}, opts: {:?}",
                local_host,
                local_port,
                plugin_opts
            );
            s
        }
        Err(err) => {
            error!(
                "failed to connect to local {}:{}, error: {}",
                local_host, local_port, err
            );
            return Err(err);
        }
    };

    if !magic_buffer.is_empty() {
        if let Err(err) = local_stream.write_all(magic_buffer).await {
            error!("failed to write first data chunk, error: {}", err);
            return Err(err);
        }
    }

    let _ = tokio::io::copy_bidirectional(&mut yamux_stream, &mut local_stream).await;
    Ok(())
}

async fn handle_udp_connection(
    context: &Context,
    local_host: &str,
    local_port: u16,
    plugin_opts: &PluginOpts,
    mut yamux_stream: StreamHandle,
) -> io::Result<()> {
    let connect_opts = plugin_opts.as_connect_opts();

    let (_, socket) = lookup_then!(context, local_host, local_port, |addr| {
        UdpSocket::connect_with_opts(&addr, &connect_opts).await
    })?;

    let mut udp_recv_buffer = [0u8; 65535];
    let mut yamux_recv_buffer = Vec::new();
    let timeout = Duration::from_secs(plugin_opts.udp_timeout.unwrap_or(yamux_plugin::UDP_DEFAULT_TIMEOUT_SEC));
    let timer = time::sleep(timeout);
    tokio::pin!(timer);

    loop {
        let mut timer = timer.as_mut();

        tokio::select! {
            _ = timer.as_mut() => {
                debug!("UDP tunnel timed out");
                break;
            }

            udp_result = socket.recv(&mut udp_recv_buffer) => {
                let n = match udp_result {
                    Ok(n) => n,
                    Err(err) => {
                        error!("UDP tunnel recv failed, error: {}", err);
                        return Err(err);
                    }
                };
                timer.reset(Instant::now() + timeout);

                // [LENGTH 8-bytes][PACKET .. LENGTH bytes]
                yamux_stream.write_u64(n as u64).await?;
                yamux_stream.write_all(&udp_recv_buffer[..n]).await?;
            }

            yamux_result = yamux_stream.read_u64() => {
                let length = match yamux_result {
                    Ok(n) => n,
                    Err(ref err) if err.kind() == ErrorKind::UnexpectedEof => {
                        break;
                    }
                    Err(err) => {
                        error!("UDP tunnel ended with error: {}", err);
                        return Err(err);
                    }
                };

                if length > usize::MAX as u64 {
                    error!(
                        "UDP tunnel received packet length {} > usize::MAX {}",
                        length,
                        usize::MAX
                    );
                    return Err(io::Error::new(ErrorKind::Other, "UDP tunnel received packet too large"));
                }

                timer.reset(Instant::now() + timeout);

                let length = length as usize;

                if yamux_recv_buffer.len() < length {
                    yamux_recv_buffer.resize(length, 0);
                }

                yamux_stream.read_exact(&mut yamux_recv_buffer[0..length]).await?;

                match socket.send(&yamux_recv_buffer[0..length]).await {
                    Ok(n) => {
                        trace!("UDP tunnel sent back {} bytes (expected {} bytes)", n, length,);
                    }
                    Err(err) => {
                        error!("UDP tunnel send back {} bytes failed with error: {}", length, err);
                    }
                }
            }
        }
    }

    Ok(())
}

async fn handle_tcp_stream(
    context: SharedContext,
    local_host: &str,
    local_port: u16,
    plugin_opts: &PluginOpts,
    mut yamux_stream: StreamHandle,
) -> io::Result<()> {
    let mut connection_type = ConnectionType::Tcp;

    let mut magic_buffer = [0u8; 4];
    let magic_length = match yamux_stream.read(&mut magic_buffer[..]).await {
        Ok(0) => {
            // EOF.
            return Ok(());
        }
        Ok(4) => {
            trace!("yamux stream magic: {:?}", magic_buffer);

            if magic_buffer == yamux_plugin::TCP_TUNNEL_MAGIC {
                connection_type = ConnectionType::Tcp;
                0
            } else if magic_buffer == yamux_plugin::UDP_TUNNEL_MAGIC {
                connection_type = ConnectionType::Udp;
                0
            } else {
                4
            }
        }
        Ok(n) => n,
        Err(err) => {
            error!("receive magic failed with error: {}", err);
            return Err(err);
        }
    };

    match connection_type {
        ConnectionType::Tcp => {
            handle_tcp_connection(
                &context,
                local_host,
                local_port,
                plugin_opts,
                yamux_stream,
                &magic_buffer[..magic_length],
            )
            .await
        }
        ConnectionType::Udp => handle_udp_connection(&context, local_host, local_port, plugin_opts, yamux_stream).await,
    }
}

async fn handle_tcp_session(
    context: SharedContext,
    local_host: Arc<String>,
    local_port: u16,
    plugin_opts: Arc<PluginOpts>,
    mut yamux_session: Session<TokioTcpStream>,
) -> io::Result<()> {
    loop {
        let yamux_stream = match yamux_session.next().await {
            Some(Ok(s)) => s,
            Some(Err(err)) => {
                error!("yamux session accept failed, error: {}", err);
                break;
            }
            None => break,
        };

        trace!("yamux session accepted new stream. {:?}", yamux_stream);

        let local_host = local_host.clone();
        let plugin_opts = plugin_opts.clone();
        let context = context.clone();
        tokio::spawn(async move {
            if let Err(err) = handle_tcp_stream(context, &local_host, local_port, &plugin_opts, yamux_stream).await {
                error!("failed to handle yamux stream, error: {}", err);
            }
        });
    }

    Ok(())
}

#[tokio::main]
async fn main() -> io::Result<()> {
    let mut builder = Builder::from_default_env();
    builder.format_timestamp_millis().init();

    #[cfg(all(unix, not(target_os = "android")))]
    yamux_plugin::adjust_nofile();

    let remote_host = env::var("SS_REMOTE_HOST").expect("require SS_REMOTE_HOST");
    let remote_port = env::var("SS_REMOTE_PORT").expect("require SS_REMOTE_PORT");
    let local_host = env::var("SS_LOCAL_HOST").expect("require SS_LOCAL_HOST");
    let local_port = env::var("SS_LOCAL_PORT").expect("require SS_LOCAL_PORT");

    let remote_port = remote_port.parse::<u16>().expect("SS_REMOTE_PORT must be a valid port");
    let local_port = local_port.parse::<u16>().expect("SS_LOCAL_PORT must be a valid port");

    let mut plugin_opts = PluginOpts::default();
    if let Ok(opts) = env::var("SS_PLUGIN_OPTIONS") {
        plugin_opts = PluginOpts::from_str(&opts).expect("unrecognized SS_PLUGIN_OPTIONS");
    }

    let connect_opts = plugin_opts.as_connect_opts();
    let accept_opts = plugin_opts.as_accept_opts();
    let dns_resolver = Arc::new(DnsResolver::trust_dns_system_resolver(connect_opts).await?);

    let mut context = Context::new(ServerType::Local);
    context.set_dns_resolver(dns_resolver);
    context.set_ipv6_first(plugin_opts.ipv6_first.unwrap_or(true));

    let (_, listener) = lookup_then!(context, &remote_host, remote_port, |addr| {
        TcpListener::bind_with_opts(&addr, accept_opts.clone()).await
    })?;

    info!(
        "yamux-plugin listening on {}:{}, local {}:{}",
        remote_host, remote_port, local_host, local_port
    );

    let local_host = Arc::new(local_host);
    let plugin_opts = Arc::new(plugin_opts);
    let context = Arc::new(context);
    loop {
        let (stream, peer_addr) = match listener.accept().await {
            Ok(s) => s,
            Err(err) => {
                error!("TcpListener::accept failed, error: {}", err);
                time::sleep(Duration::from_secs(1)).await;
                continue;
            }
        };

        trace!("accepted TCP (shadowsocks) client {}", peer_addr);

        let local_host = local_host.clone();
        let plugin_opts = plugin_opts.clone();
        let context = context.clone();
        let yamux_session = Session::new_server(stream, Config::default());
        tokio::spawn(handle_tcp_session(
            context,
            local_host,
            local_port,
            plugin_opts,
            yamux_session,
        ));
    }
}
