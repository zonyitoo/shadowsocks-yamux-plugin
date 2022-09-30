use std::{ffi::CString, io, net::SocketAddr, os::windows::io::AsRawSocket};

use log::error;
use windows_sys::{
    core::PCSTR,
    Win32::{
        NetworkManagement::IpHelper::if_nametoindex,
        Networking::WinSock::{
            setsockopt,
            WSAGetLastError,
            IPPROTO_IP,
            IPPROTO_IPV6,
            IPV6_UNICAST_IF,
            IP_UNICAST_IF,
            SOCKET,
            SOCKET_ERROR,
        },
    },
};

pub fn set_ip_unicast_if<S: AsRawSocket>(socket: &S, addr: SocketAddr, iface: &str) -> io::Result<()> {
    let handle = socket.as_raw_socket() as SOCKET;

    unsafe {
        // Windows if_nametoindex requires a C-string for interface name
        let ifname = CString::new(iface).expect("iface");

        // https://docs.microsoft.com/en-us/previous-versions/windows/hardware/drivers/ff553788(v=vs.85)
        let if_index = if_nametoindex(ifname.as_ptr() as PCSTR);
        if if_index == 0 {
            // If the if_nametoindex function fails and returns zero, it is not possible to determine an error code.
            error!("if_nametoindex {} fails", iface);
            return Err(io::Error::new(ErrorKind::InvalidInput, "invalid interface name"));
        }

        // https://docs.microsoft.com/en-us/windows/win32/winsock/ipproto-ip-socket-options
        let ret = match addr {
            SocketAddr::V4(..) => setsockopt(
                handle,
                IPPROTO_IP as i32,
                IP_UNICAST_IF as i32,
                &if_index as *const _ as PCSTR,
                mem::size_of_val(&if_index) as i32,
            ),
            SocketAddr::V6(..) => setsockopt(
                handle,
                IPPROTO_IPV6 as i32,
                IPV6_UNICAST_IF as i32,
                &if_index as *const _ as PCSTR,
                mem::size_of_val(&if_index) as i32,
            ),
        };

        if ret == SOCKET_ERROR {
            let err = io::Error::from_raw_os_error(WSAGetLastError());
            error!("set IP_UNICAST_IF / IPV6_UNICAST_IF error: {}", err);
            return Err(err);
        }
    }

    Ok(())
}
