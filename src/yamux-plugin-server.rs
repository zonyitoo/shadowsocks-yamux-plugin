use std::{env, io, sync::Arc, time::Duration};

use env_logger::Builder;
use futures::StreamExt;
use log::{error, info, trace};
use tokio::{net::TcpListener, time};
use tokio_yamux::{Config, Session};

use yamux_plugin::{create_outbound_socket, PluginOpts};

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

    let listener = TcpListener::bind((remote_host.as_str(), remote_port)).await?;
    info!(
        "yamux-plugin listening on {}:{}, local {}:{}",
        remote_host, remote_port, local_host, local_port
    );

    let local_host = Arc::new(local_host);
    let plugin_opts = Arc::new(plugin_opts);
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
        let mut yamux_session = Session::new_server(stream, Config::default());
        tokio::spawn(async move {
            loop {
                let mut yamux_stream = match yamux_session.next().await {
                    Some(Ok(s)) => s,
                    Some(Err(err)) => {
                        error!("yamux session accept failed, error: {}", err);
                        break;
                    }
                    None => break,
                };

                trace!("yamux session accepted new stream. {:?}", yamux_stream);

                let mut local_stream =
                    match create_outbound_socket((local_host.as_str(), local_port), &plugin_opts).await {
                        Ok(s) => {
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
                            continue;
                        }
                    };

                tokio::spawn(async move {
                    let _ = tokio::io::copy_bidirectional(&mut yamux_stream, &mut local_stream).await;
                });
            }
        });
    }
}
