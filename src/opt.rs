//! shadowsocks yamux plugin options

use std::{net::IpAddr, time::Duration};

use serde::{Deserialize, Serialize};
use serde_urlencoded::{self, de::Error as DeError, ser::Error as SerError};
use shadowsocks::net::{AcceptOpts, ConnectOpts};

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
    /// UDP tunnel timeout (in seconds)
    pub udp_timeout: Option<u64>,
    /// TCP Keep Alive
    pub tcp_keep_alive: Option<u64>,
    /// TCP Fast Open
    pub tcp_fast_open: Option<bool>,
    /// MPTCP
    pub mptcp: Option<bool>,
    /// IPv6 First
    pub ipv6_first: Option<bool>,
}

impl PluginOpts {
    pub fn from_str(opt: &str) -> Result<PluginOpts, DeError> {
        serde_urlencoded::from_str(opt)
    }

    pub fn to_string(&self) -> Result<String, SerError> {
        serde_urlencoded::to_string(self)
    }

    pub fn as_connect_opts(&self) -> ConnectOpts {
        let mut connect_opts = ConnectOpts::default();

        #[cfg(any(target_os = "linux", target_os = "android"))]
        if let Some(outbound_fwmark) = self.outbound_fwmark {
            connect_opts.fwmark = Some(outbound_fwmark);
        }

        #[cfg(target_os = "freebsd")]
        if let Some(outbound_user_cookie) = self.outbound_user_cookie {
            connect_opts.user_cookie = Some(outbound_user_cookie);
        }

        connect_opts.bind_interface = self.outbound_bind_interface.clone();
        connect_opts.bind_local_addr = self.outbound_bind_addr;

        connect_opts.tcp.keepalive = self.tcp_keep_alive.map(|sec| Duration::from_secs(sec));
        connect_opts.tcp.fastopen = self.tcp_fast_open.unwrap_or(false);
        connect_opts.tcp.mptcp = self.mptcp.unwrap_or(false);

        connect_opts
    }

    pub fn as_accept_opts(&self) -> AcceptOpts {
        let mut accept_opts = AcceptOpts::default();

        accept_opts.tcp.keepalive = self.tcp_keep_alive.map(|sec| Duration::from_secs(sec));
        accept_opts.tcp.fastopen = self.tcp_fast_open.unwrap_or(false);
        accept_opts.tcp.mptcp = self.mptcp.unwrap_or(false);

        accept_opts
    }
}
