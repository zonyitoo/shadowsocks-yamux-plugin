use std::{io, mem, os::unix::io::AsRawFd};

use log::error;

pub fn set_user_cookie<S: AsRawFd>(socket: &S, user_cookie: u32) -> io::Result<()> {
    let ret = unsafe {
        libc::setsockopt(
            socket.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_USER_COOKIE,
            &user_cookie as *const _ as *const _,
            mem::size_of_val(&user_cookie) as libc::socklen_t,
        )
    };
    if ret != 0 {
        let err = io::Error::last_os_error();
        error!("set SO_USER_COOKIE error: {}", err);
        return Err(err);
    }

    Ok(())
}
