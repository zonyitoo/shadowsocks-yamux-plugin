//! shadowsocks yamux SIP003 plugin

pub mod opt;
mod sys;

pub use self::opt::PluginOpts;
#[cfg(all(unix, not(target_os = "android")))]
pub use sys::adjust_nofile;

/// TCP MAGIC
pub const TCP_TUNNEL_MAGIC: &[u8] = &[0xab, 0x10, 0x24, 0xef];
/// UDP MAGIC
pub const UDP_TUNNEL_MAGIC: &[u8] = &[0xab, 0x10, 0x24, 0xee];
/// Default UDP timeout duration
pub const UDP_DEFAULT_TIMEOUT_SEC: u64 = 5 * 60;
