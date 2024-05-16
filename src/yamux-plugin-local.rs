use std::{
    cell::RefCell,
    collections::LinkedList,
    env,
    io::{self, Cursor, ErrorKind},
    net::SocketAddr,
    sync::Arc,
    time::Duration,
};

use env_logger::Builder;
use futures::StreamExt;
use log::{error, info, trace, warn};
use lru_time_cache::LruCache;
#[cfg(feature = "mimalloc")]
use mimalloc::MiMalloc;
use once_cell::sync::OnceCell;
use shadowsocks::{
    config::ServerType,
    context::Context,
    dns_resolver::DnsResolver,
    lookup_then,
    lookup_then_connect,
    net::{TcpListener, TcpStream, UdpSocket},
    relay::tcprelay::utils::copy_bidirectional,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt, WriteHalf},
    sync::Mutex,
    time,
};
use tokio_yamux::{Config, Control, Error, Session, StreamHandle};

use yamux_plugin::PluginOpts;

#[cfg(feature = "mimalloc")]
#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum ConnectionType {
    Tcp,
    Udp,
}

async fn get_yamux_stream(
    connection_type: ConnectionType,
    context: &Context,
    remote_host: &str,
    remote_port: u16,
    plugin_opts: &PluginOpts,
) -> io::Result<StreamHandle> {
    thread_local! {
        static TCP_YAMUX_SESSION_LIST: RefCell<LinkedList<Control>> = RefCell::new(LinkedList::new());
        static UDP_YAMUX_SESSION_LIST: RefCell<LinkedList<Control>> = RefCell::new(LinkedList::new());
    }

    let session_list = match connection_type {
        ConnectionType::Tcp => &TCP_YAMUX_SESSION_LIST,
        ConnectionType::Udp => &UDP_YAMUX_SESSION_LIST,
    };

    const YAMUX_CONNECT_RETRY_COUNT: usize = 3;

    let mut connect_tried_count = 0;

    let custom_config = Config {
        max_stream_window_size: (100 * 1024 * 1024),
        ..Config::default()
    };

    let yamux_stream = loop {
        connect_tried_count += 1;

        if connect_tried_count > YAMUX_CONNECT_RETRY_COUNT {
            return Err(io::Error::new(ErrorKind::Other, "failed to connect remote"));
        }

        let control_opt = session_list.with(|list| list.borrow_mut().pop_front());

        if let Some(mut control) = control_opt {
            match control.open_stream().await {
                Ok(s) => {
                    trace!("yamux opened stream {:?}", s);
                    session_list.with(|list| list.borrow_mut().push_back(control));
                    break s;
                }
                Err(Error::StreamsExhausted) => {
                    trace!("yamux connection stream id exhaused");
                    session_list.with(|list| list.borrow_mut().push_back(control));
                }
                Err(err) => {
                    error!("yamux connection open stream failed, error: {}", err);
                }
            }
        }

        let connect_opts = plugin_opts.as_connect_opts();
        let remote_stream_result = lookup_then_connect!(context, remote_host, remote_port, |addr| {
            TcpStream::connect_with_opts(&addr, &connect_opts).await
        });

        let remote_stream = match remote_stream_result {
            Ok((_, s)) => {
                trace!(
                    "connected tcp host {}:{}, opts: {:?}",
                    remote_host,
                    remote_port,
                    plugin_opts
                );
                s
            }
            Err(err) => {
                error!(
                    "failed to connect to remote {}:{}, error: {}",
                    remote_host, remote_port, err
                );
                continue;
            }
        };

        let mut yamux_session = Session::new_client(remote_stream, custom_config);
        let yamux_control = yamux_session.control();

        tokio::spawn(async move {
            loop {
                match yamux_session.next().await {
                    Some(Ok(..)) => {}
                    Some(Err(e)) => {
                        error!("yamux connection aborted with connection error: {}", e);
                        break;
                    }
                    None => {
                        trace!("yamux client session closed");
                        break;
                    }
                }
            }
        });

        session_list.with(|list| list.borrow_mut().push_front(yamux_control));
    };

    Ok(yamux_stream)
}

#[inline]
async fn get_tcp_yamux_stream(
    context: &Context,
    remote_host: &str,
    remote_port: u16,
    plugin_opts: &PluginOpts,
) -> io::Result<StreamHandle> {
    get_yamux_stream(ConnectionType::Tcp, context, remote_host, remote_port, plugin_opts).await
}

#[inline]
async fn get_udp_yamux_stream(
    context: &Context,
    remote_host: &str,
    remote_port: u16,
    plugin_opts: &PluginOpts,
) -> io::Result<StreamHandle> {
    get_yamux_stream(ConnectionType::Udp, context, remote_host, remote_port, plugin_opts).await
}

async fn start_tcp(
    context: &Context,
    local_host: &str,
    local_port: u16,
    remote_host: &str,
    remote_port: u16,
    plugin_opts: &PluginOpts,
) -> io::Result<()> {
    let accept_opts = plugin_opts.as_accept_opts();

    let (_, listener) = lookup_then!(context, local_host, local_port, |addr| {
        TcpListener::bind_with_opts(&addr, accept_opts.clone()).await
    })?;
    info!(
        "yamux-plugin TCP listening on {}:{}, remote {}:{}",
        local_host, local_port, remote_host, remote_port
    );

    loop {
        let (mut stream, peer_addr) = match listener.accept().await {
            Ok(s) => s,
            Err(err) => {
                error!("TcpListener::accept failed, error: {}", err);
                time::sleep(Duration::from_secs(1)).await;
                continue;
            }
        };

        trace!("accepted TCP (shadowsocks) client {}", peer_addr);

        let mut yamux_stream = get_tcp_yamux_stream(context, remote_host, remote_port, plugin_opts).await?;

        tokio::spawn(async move {
            let _ = copy_bidirectional(&mut stream, &mut yamux_stream).await;
        });
    }
}

async fn start_udp(
    context: &Context,
    local_host: &str,
    local_port: u16,
    remote_host: &str,
    remote_port: u16,
    plugin_opts: &PluginOpts,
) -> io::Result<()> {
    let accept_opts = plugin_opts.as_accept_opts();

    let (_, listener) = lookup_then!(context, local_host, local_port, |addr| {
        UdpSocket::listen_with_opts(&addr, accept_opts.clone()).await
    })?;

    info!(
        "yamux-plugin UDP listening on {}:{}, remote {}:{}",
        local_host, local_port, remote_host, remote_port
    );

    let listener = Arc::new(listener);
    let mut buffer = [0u8; 65535];

    loop {
        let (n, peer_addr) = match listener.recv_from(&mut buffer).await {
            Ok(s) => s,
            Err(err) => {
                error!("UdpSocket::recv_from failed, error: {}", err);
                time::sleep(Duration::from_secs(1)).await;
                continue;
            }
        };

        trace!("received UDP packet {} bytes from {}", n, peer_addr);

        struct UnderlyingStream {
            handle: WriteHalf<StreamHandle>,
        }

        static UDP_TUNNEL_MAP: OnceCell<Mutex<LruCache<SocketAddr, UnderlyingStream>>> = OnceCell::new();

        let mut tunnel_map = UDP_TUNNEL_MAP
            .get_or_init(|| {
                let timeout =
                    Duration::from_secs(plugin_opts.udp_timeout.unwrap_or(yamux_plugin::UDP_DEFAULT_TIMEOUT_SEC));
                Mutex::new(LruCache::with_expiry_duration(timeout))
            })
            .lock()
            .await;

        let (opt_yamux_stream, expired_streams) = tunnel_map.notify_get_mut(&peer_addr);

        // Close expired streams gracefully
        // Create a new task, don't block the main loop
        tokio::spawn(async move {
            for (_, mut stream) in expired_streams {
                if let Err(err) = stream.handle.shutdown().await {
                    warn!("UDP tunnel expired. closing with FIN failed with error: {}", err);
                }
            }
        });

        let yamux_stream = match opt_yamux_stream {
            Some(s) => s,
            // Create a new YAMUX stream. slow-path
            None => {
                let new_stream = get_udp_yamux_stream(context, remote_host, remote_port, plugin_opts).await?;

                let (mut rx, tx) = tokio::io::split(new_stream);

                let listener = listener.clone();
                tokio::spawn(async move {
                    let mut buffer = Vec::new();

                    loop {
                        // [LENGTH 8-bytes][PACKET .. LENGTH bytes]
                        let length = match rx.read_u64().await {
                            Ok(n) => n,
                            Err(ref err) if err.kind() == ErrorKind::UnexpectedEof => {
                                break;
                            }
                            Err(err) => {
                                error!("UDP tunnel for {} ended with error: {}", peer_addr, err);
                                break;
                            }
                        };

                        if length > usize::MAX as u64 {
                            error!(
                                "UDP tunnel received packet length {} > usize::MAX {}",
                                length,
                                usize::MAX
                            );
                            break;
                        }

                        let length = length as usize;

                        if buffer.len() < length {
                            buffer.resize(length, 0);
                        }

                        if let Err(err) = rx.read_exact(&mut buffer[0..length]).await {
                            error!("UDP tunnel for {} read with error: {}", peer_addr, err);
                            break;
                        }

                        match listener.send_to(&buffer[0..length], peer_addr).await {
                            Ok(n) => {
                                trace!(
                                    "UDP tunnel sent back {} bytes (expected {} bytes) to {}",
                                    n,
                                    length,
                                    peer_addr
                                );
                            }
                            Err(err) => {
                                error!(
                                    "UDP tunnel send back {} bytes to {} failed with error: {}",
                                    length, peer_addr, err
                                );
                            }
                        }
                    }
                });

                let new_stream = UnderlyingStream { handle: tx };

                tunnel_map.insert(peer_addr, new_stream);
                tunnel_map.get_mut(&peer_addr).unwrap()
            }
        };

        // [LENGTH 8-bytes][PACKET .. LENGTH bytes]
        let result: io::Result<()> = async {
            let buffer_len = 8 + n;

            let mut packet_buffer = vec![0u8; buffer_len];
            let mut packet_buffer_cursor = Cursor::new(&mut packet_buffer);
            packet_buffer_cursor.write_u64(n as u64).await?;
            packet_buffer_cursor.write_all(&buffer[..n]).await?;

            yamux_stream.handle.write_all(&packet_buffer).await?;

            Ok(())
        }
        .await;

        if let Err(err) = result {
            error!("UDP tunnel send packet from {} failed with error: {}", peer_addr, err);
            tunnel_map.remove(&peer_addr);
        }
    }
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
    let dns_resolver = Arc::new(DnsResolver::hickory_dns_system_resolver(None, connect_opts).await?);

    let mut context = Context::new(ServerType::Local);
    context.set_dns_resolver(dns_resolver);
    context.set_ipv6_first(plugin_opts.ipv6_first.unwrap_or(true));

    let tcp_fut = start_tcp(
        &context,
        &local_host,
        local_port,
        &remote_host,
        remote_port,
        &plugin_opts,
    );

    let udp_remote_port = if remote_port == u16::MAX {
        remote_port - 1
    } else {
        remote_port + 1
    };

    let udp_fut = start_udp(
        &context,
        &local_host,
        local_port,
        &remote_host,
        udp_remote_port,
        &plugin_opts,
    );

    info!(
        "yamux-plugin listening on {}:{}, remote {}:{} (udp: {})",
        local_host, local_port, remote_host, remote_port, udp_remote_port
    );

    tokio::pin!(tcp_fut);
    tokio::pin!(udp_fut);

    loop {
        let tcp_fut = tcp_fut.as_mut();
        let udp_fut = udp_fut.as_mut();

        tokio::select! {
            result = tcp_fut => {
                error!("TCP service ended with result {:?}", result);
                return Err(io::Error::new(io::ErrorKind::Other, "TCP service exited unexpectly"));
            }
            result = udp_fut => {
                error!("UDP service ended with result {:?}", result);
                return Err(io::Error::new(io::ErrorKind::Other, "UDP service exited unexpectly"));
            }
        }
    }
}
