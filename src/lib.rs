//! shadowsocks yamux SIP003 plugin

pub mod opt;
mod sys;

pub use self::opt::PluginOpts;
#[cfg(all(unix, not(target_os = "android")))]
pub use sys::adjust_nofile;

/// Default UDP timeout duration
pub const UDP_DEFAULT_TIMEOUT_SEC: u64 = 5 * 60;
