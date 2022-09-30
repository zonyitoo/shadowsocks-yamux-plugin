use std::{io, mem, os::unix::io::AsRawFd};

use log::error;

pub fn set_fwmark<S: AsRawFd>(socket: &S, fwmark: u32) -> io::Result<()> {
    // Set SO_MARK for mark-based routing on Linux (since 2.6.25)
    // NOTE: This will require CAP_NET_ADMIN capability (root in most cases)
    let ret = unsafe {
        libc::setsockopt(
            socket.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_MARK,
            &mark as *const _ as *const _,
            mem::size_of_val(&mark) as libc::socklen_t,
        )
    };
    if ret != 0 {
        let err = io::Error::last_os_error();
        error!(
            "setsockopt SOL_SOCKET SO_MARK failed, fwmark: {}, error: {}",
            fwmark, err
        );
        return Err(err);
    }

    Ok(())
}

fn set_bindtodevice<S: AsRawFd>(socket: &S, iface: &str) -> io::Result<()> {
    let iface_bytes = iface.as_bytes();

    unsafe {
        let ret = libc::setsockopt(
            socket.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_BINDTODEVICE,
            iface_bytes.as_ptr() as *const _ as *const libc::c_void,
            iface_bytes.len() as libc::socklen_t,
        );

        if ret != 0 {
            let err = io::Error::last_os_error();
            error!(
                "setsockopt SOL_SOCKET SO_BINDTODEVICE failed, iface: {}, error: {}",
                iface, err
            );
            return Err(err);
        }
    }

    Ok(())
}
