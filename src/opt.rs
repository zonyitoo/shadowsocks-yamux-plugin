//! shadowsocks yamux plugin options

use std::{
    io,
    net::{IpAddr, SocketAddr},
};

use serde::{Deserialize, Serialize};
use serde_urlencoded::{self, de::Error as DeError, ser::Error as SerError};
use tokio::net::{self, TcpSocket, TcpStream, ToSocketAddrs};

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct PluginOpts {
    /// Set `SO_MARK` socket option for outbound sockets
    #[cfg(any(target_os = "linux", target_os = "android"))]
    pub outbound_fwmark: Option<u32>,
    /// Set `SO_USER_COOKIE` socket option for outbound sockets
    #[cfg(target_os = "freebsd")]
    pub outbound_user_cookie: Option<u32>,
    /// Set `SO_BINDTODEVICE` (Linux), `IP_BOUND_IF` (BSD), `IP_UNICAST_IF` (Windows) socket option for outbound sockets
    pub outbound_bind_interface: Option<String>,
    /// Outbound sockets will `bind` to this address
    pub outbound_bind_addr: Option<IpAddr>,
}

impl PluginOpts {
    pub fn from_str(opt: &str) -> Result<PluginOpts, DeError> {
        serde_urlencoded::from_str(opt)
    }

    pub fn to_string(&self) -> Result<String, SerError> {
        serde_urlencoded::to_string(self)
    }
}

async fn create_outbound_socket_one(addr: SocketAddr, opts: &PluginOpts) -> io::Result<TcpStream> {
    let socket = match addr {
        SocketAddr::V4(..) => TcpSocket::new_v4()?,
        SocketAddr::V6(..) => TcpSocket::new_v6()?,
    };

    #[cfg(any(target_os = "linux", target_os = "android"))]
    if let Some(fwmark) = opts.outbound_fwmark {
        crate::sys::set_fwmark(&socket, fwmark)?;
    }

    #[cfg(target_os = "freebsd")]
    if let Some(user_cookie) = opts.outbound_user_cookie {
        crate::sys::set_user_cookie(&socket, user_cookie)?;
    }

    #[cfg(any(target_os = "macos", target_os = "watchos", target_os = "tvos", target_os = "ios"))]
    if let Some(ref iface) = opts.outbound_bind_interface {
        crate::sys::set_ip_bound_if(&socket, addr, iface)?;
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    if let Some(ref iface) = opts.outbound_bind_interface {
        crate::sys::set_bindtodevice(&socket, iface)?;
    }

    #[cfg(windows)]
    if let Some(ref iface) = opts.outbound_bind_interface {
        crate::sys::set_ip_unicast_if(&socket, addr, iface)?;
    }

    if let Some(addr) = opts.outbound_bind_addr {
        socket.bind(SocketAddr::new(addr, 0))?;
    }

    socket.connect(addr).await
}

/// Create a TcpStream for connecting to outbound address `addr`
pub async fn create_outbound_socket<A: ToSocketAddrs>(addr: A, opts: &PluginOpts) -> io::Result<TcpStream> {
    let mut last_err = None;
    for saddr in net::lookup_host(addr).await? {
        match create_outbound_socket_one(saddr, opts).await {
            Ok(s) => return Ok(s),
            Err(err) => last_err = Some(err),
        }
    }

    Err(last_err.unwrap_or_else(|| io::Error::new(io::ErrorKind::Other, "dns resolve to none")))
}
