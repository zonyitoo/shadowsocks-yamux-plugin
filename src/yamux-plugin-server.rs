use std::{
    env,
    io::{self, Cursor, ErrorKind, IsTerminal},
    sync::Arc,
    time::Duration,
};

use futures::StreamExt;
use log::{debug, error, info, trace, warn};
#[cfg(feature = "mimalloc")]
use mimalloc::MiMalloc;
use shadowsocks::{
    config::ServerType,
    context::{Context, SharedContext},
    dns_resolver::DnsResolver,
    lookup_then,
    lookup_then_connect,
    net::{TcpListener, TcpStream, UdpSocket},
    relay::tcprelay::utils::copy_bidirectional,
};
use time::UtcOffset;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream as TokioTcpStream,
    time::Instant,
};
use tokio_yamux::{Config, Session, StreamHandle};
use tracing_subscriber::{filter::EnvFilter, fmt::time::OffsetTime, FmtSubscriber};

use shadowsocks_yamux_plugin::PluginOpts;

#[cfg(feature = "mimalloc")]
#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
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

    let _ = copy_bidirectional(&mut yamux_stream, &mut local_stream).await;
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
    let timeout = Duration::from_secs(
        plugin_opts
            .udp_timeout
            .unwrap_or(shadowsocks_yamux_plugin::UDP_DEFAULT_TIMEOUT_SEC),
    );
    let timer = tokio::time::sleep(timeout);
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
                let mut buffer = vec![0u8; 8 + n];
                let mut buffer_cursor = Cursor::new(&mut buffer);

                buffer_cursor.write_u64(n as u64).await?;
                buffer_cursor.write_all(&udp_recv_buffer[..n]).await?;

                yamux_stream.write_all(&mut buffer).await?;
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

    // shutdown() will call StreamHandle::close(), which will send Fin
    if let Err(err) = yamux_stream.shutdown().await {
        warn!("UDP tunnel shutdown() with FIN gracefully failed, error: {}", err);
    }

    Ok(())
}

async fn handle_tcp_session(
    context: SharedContext,
    local_host: Arc<String>,
    local_port: u16,
    plugin_opts: Arc<PluginOpts>,
    mut yamux_session: Session<TokioTcpStream>,
    connection_type: ConnectionType,
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
            match connection_type {
                ConnectionType::Tcp => {
                    handle_tcp_connection(&context, &local_host, local_port, &plugin_opts, yamux_stream).await
                }
                ConnectionType::Udp => {
                    handle_udp_connection(&context, &local_host, local_port, &plugin_opts, yamux_stream).await
                }
            }
        });
    }

    Ok(())
}

#[tokio::main]
async fn main() -> io::Result<()> {
    let mut builder = FmtSubscriber::builder()
        .with_timer(match OffsetTime::local_rfc_3339() {
            Ok(t) => t,
            Err(..) => {
                // Reinit with UTC time
                OffsetTime::new(UtcOffset::UTC, time::format_description::well_known::Rfc3339)
            }
        })
        .compact()
        .with_env_filter(EnvFilter::from_default_env());

    // NOTE: ansi is enabled by default.
    // Could be disabled by `NO_COLOR` environment variable.
    // https://no-color.org/
    if !std::io::stdout().is_terminal() {
        builder = builder.with_ansi(false);
    }

    builder.init();

    #[cfg(all(unix, not(target_os = "android")))]
    shadowsocks_yamux_plugin::adjust_nofile();

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
    let dns_resolver = Arc::new(DnsResolver::hickory_dns_system_resolver(None, connect_opts).await?);

    let mut context = Context::new(ServerType::Server);
    context.set_dns_resolver(dns_resolver);
    context.set_ipv6_first(plugin_opts.ipv6_first.unwrap_or(true));

    let (_, tcp_listener) = lookup_then!(context, &remote_host, remote_port, |addr| {
        TcpListener::bind_with_opts(&addr, accept_opts.clone()).await
    })?;

    let udp_remote_port = if remote_port == u16::MAX {
        remote_port - 1
    } else {
        remote_port + 1
    };

    let (_, udp_listener) = lookup_then!(context, &remote_host, udp_remote_port, |addr| {
        TcpListener::bind_with_opts(&addr, accept_opts.clone()).await
    })?;

    info!(
        "yamux-plugin listening on {}:{} (udp: {}), local {}:{}",
        remote_host, remote_port, udp_remote_port, local_host, local_port
    );

    let local_host = Arc::new(local_host);
    let plugin_opts = Arc::new(plugin_opts);
    let context = Arc::new(context);

    let custom_config = Config {
        max_stream_window_size: (100 * 1024 * 1024),
        ..Config::default()
    };

    let tcp_fut = {
        let local_host = local_host.clone();
        let plugin_opts = plugin_opts.clone();
        let context = context.clone();

        async move {
            loop {
                let (stream, peer_addr) = match tcp_listener.accept().await {
                    Ok(s) => s,
                    Err(err) => {
                        error!("TcpListener::accept failed, error: {}", err);
                        tokio::time::sleep(Duration::from_secs(1)).await;
                        continue;
                    }
                };

                trace!("accepted TCP (shadowsocks) client {}", peer_addr);

                let local_host = local_host.clone();
                let plugin_opts = plugin_opts.clone();
                let context = context.clone();
                let yamux_session = Session::new_server(stream, custom_config);
                tokio::spawn(handle_tcp_session(
                    context,
                    local_host,
                    local_port,
                    plugin_opts,
                    yamux_session,
                    ConnectionType::Tcp,
                ));
            }
        }
    };

    let udp_fut = async move {
        loop {
            let (stream, peer_addr) = match udp_listener.accept().await {
                Ok(s) => s,
                Err(err) => {
                    error!("TcpListener::accept failed, error: {}", err);
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    continue;
                }
            };

            trace!("accepted UDP (shadowsocks) client {}", peer_addr);

            let local_host = local_host.clone();
            let plugin_opts = plugin_opts.clone();
            let context = context.clone();
            let yamux_session = Session::new_server(stream, custom_config);
            tokio::spawn(handle_tcp_session(
                context,
                local_host,
                local_port,
                plugin_opts,
                yamux_session,
                ConnectionType::Udp,
            ));
        }
    };

    tokio::pin!(tcp_fut);
    tokio::pin!(udp_fut);

    loop {
        tokio::select! {
            _ = &mut tcp_fut => {},
            _ = &mut udp_fut => {},
        }
    }
}
