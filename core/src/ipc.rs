//! Peer identity of a Unix socket (SEC-2, SEC-3): uid and pid of the process at the other end.
//! One module per direction of change: macOS `getpeereid`/`LOCAL_PEERPID`, Linux `SO_PEERCRED` (TECH 21.1).

use std::io;
use std::os::unix::net::UnixStream;

#[cfg(target_os = "macos")]
pub fn peer_pid(s: &UnixStream) -> io::Result<i32> {
    use std::os::fd::AsRawFd;
    let mut pid: libc::pid_t = 0;
    let mut len = std::mem::size_of::<libc::pid_t>() as libc::socklen_t;
    let rc = unsafe { libc::getsockopt(s.as_raw_fd(), libc::SOL_LOCAL, libc::LOCAL_PEERPID, (&raw mut pid).cast(), &mut len) };
    if rc == -1 { Err(io::Error::last_os_error()) } else { Ok(pid) }
}

#[cfg(target_os = "macos")]
pub fn peer_uid(s: &UnixStream) -> io::Result<libc::uid_t> {
    use std::os::fd::AsRawFd;
    let (mut uid, mut gid) = (0, 0);
    if unsafe { libc::getpeereid(s.as_raw_fd(), &mut uid, &mut gid) } != 0 { Err(io::Error::last_os_error()) } else { Ok(uid) }
}

#[cfg(target_os = "linux")]
fn peer_cred(s: &UnixStream) -> io::Result<libc::ucred> {
    use std::os::fd::AsRawFd;
    let mut c: libc::ucred = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    let rc = unsafe { libc::getsockopt(s.as_raw_fd(), libc::SOL_SOCKET, libc::SO_PEERCRED, (&raw mut c).cast(), &mut len) };
    if rc == -1 { Err(io::Error::last_os_error()) } else { Ok(c) }
}

#[cfg(target_os = "linux")]
pub fn peer_pid(s: &UnixStream) -> io::Result<i32> {
    peer_cred(s).map(|c| c.pid)
}

#[cfg(target_os = "linux")]
pub fn peer_uid(s: &UnixStream) -> io::Result<libc::uid_t> {
    peer_cred(s).map(|c| c.uid)
}
