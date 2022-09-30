//! shadowsocks yamux SIP003 plugin

pub mod opt;
mod sys;

pub use self::opt::{create_outbound_socket, PluginOpts};
#[cfg(all(unix, not(target_os = "android")))]
pub use sys::adjust_nofile;
