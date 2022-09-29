use std::{cell::RefCell, collections::LinkedList, env, io, time::Duration};

use env_logger::Builder;
use futures::StreamExt;
use log::{error, info, trace};
use tokio::{
    net::{TcpListener, TcpStream},
    time,
};
use tokio_yamux::{Config, Control, Error, Session};

#[tokio::main]
async fn main() -> io::Result<()> {
    let mut builder = Builder::from_default_env();
    builder.format_timestamp_millis().init();

    let remote_host = env::var("SS_REMOTE_HOST").expect("require SS_REMOTE_HOST");
    let remote_port = env::var("SS_REMOTE_PORT").expect("require SS_REMOTE_PORT");
    let local_host = env::var("SS_LOCAL_HOST").expect("require SS_LOCAL_HOST");
    let local_port = env::var("SS_LOCAL_PORT").expect("require SS_LOCAL_PORT");

    let remote_port = remote_port.parse::<u16>().expect("SS_REMOTE_PORT must be a valid port");
    let local_port = local_port.parse::<u16>().expect("SS_LOCAL_PORT must be a valid port");

    let listener = TcpListener::bind((local_host.as_str(), local_port)).await?;
    info!(
        "yamux-plugin listening on {}:{}, remote {}:{}",
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

        thread_local! {
            static YAMUX_SESSION_LIST: RefCell<LinkedList<Control>> = RefCell::new(LinkedList::new());
        }

        let mut yamux_stream = loop {
            let control_opt = YAMUX_SESSION_LIST.with(|list| list.borrow_mut().pop_front());

            if let Some(mut control) = control_opt {
                match control.open_stream().await {
                    Ok(s) => {
                        trace!("yamux opened stream {:?}", s);
                        YAMUX_SESSION_LIST.with(|list| list.borrow_mut().push_back(control));
                        break s;
                    }
                    Err(Error::StreamsExhausted) => {
                        trace!("yamux connection stream id exhaused");
                        YAMUX_SESSION_LIST.with(|list| list.borrow_mut().push_back(control));
                    }
                    Err(err) => {
                        error!("yamux connection open stream failed, error: {}", err);
                    }
                }
            }

            let remote_stream = match TcpStream::connect((remote_host.as_str(), remote_port)).await {
                Ok(s) => s,
                Err(err) => {
                    error!(
                        "failed to connect to remote {}:{}, error: {}",
                        remote_host, remote_port, err
                    );
                    continue;
                }
            };

            let mut yamux_session = Session::new_client(remote_stream, Config::default());
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

            YAMUX_SESSION_LIST.with(|list| list.borrow_mut().push_front(yamux_control));
        };

        tokio::spawn(async move {
            let _ = tokio::io::copy_bidirectional(&mut stream, &mut yamux_stream).await;
        });
    }
}
