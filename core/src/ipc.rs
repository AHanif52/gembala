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

/// Linux abstract socket name for the UI channel (SEC-20): `\0` + `name`, no file. Only a process outside a pane's
/// Landlock domain can connect to it; see `crate::jail`.
#[cfg(target_os = "linux")]
fn addr(name: &str) -> io::Result<(libc::sockaddr_un, libc::socklen_t)> {
    let bytes = name.as_bytes();
    let mut sa: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    if name.is_empty() || bytes.len() + 1 >= sa.sun_path.len() || name.contains('/') {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "abstract name"));
    }
    sa.sun_family = libc::AF_UNIX as libc::sa_family_t;
    sa.sun_path[0] = 0; // abstract namespace: the name starts after this NUL
    for (i, b) in bytes.iter().enumerate() {
        sa.sun_path[i + 1] = *b as libc::c_char;
    }
    // the kernel reads offsetof(sun_path) + leading NUL + name
    Ok((sa, (std::mem::size_of::<libc::sa_family_t>() + 1 + bytes.len()) as libc::socklen_t))
}

#[cfg(target_os = "linux")]
pub fn bind_abstract(name: &str) -> io::Result<std::os::unix::net::UnixListener> {
    use std::os::fd::FromRawFd;
    let (sa, len) = addr(name)?;
    let fd = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM | libc::SOCK_CLOEXEC, 0) };
    if fd == -1 {
        return Err(io::Error::last_os_error());
    }
    let bad = |e: io::Error| {
        unsafe { libc::close(fd) };
        e
    };
    if unsafe { libc::bind(fd, (&raw const sa).cast(), len) } == -1 {
        return Err(bad(io::Error::last_os_error()));
    }
    if unsafe { libc::listen(fd, 64) } == -1 {
        return Err(bad(io::Error::last_os_error()));
    }
    Ok(unsafe { std::os::unix::net::UnixListener::from_raw_fd(fd) })
}

/// `SO_PEERPIDFD` (Linux 6.5+): a pidfd pins the pid, so `/proc/<pid>` can't be reused by another process between
/// `SO_PEERCRED` and the reads below.
#[cfg(target_os = "linux")]
fn peer_pidfd(s: &UnixStream) -> Option<std::os::fd::OwnedFd> {
    use std::os::fd::{AsRawFd, FromRawFd};
    const SO_PEERPIDFD: libc::c_int = 77;
    let mut fd: libc::c_int = -1;
    let mut len = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
    let rc = unsafe { libc::getsockopt(s.as_raw_fd(), libc::SOL_SOCKET, SO_PEERPIDFD, (&raw mut fd).cast(), &mut len) };
    (rc == 0 && fd >= 0).then(|| unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) })
}

/// Same value as the daemon's own environment = inherited from the same launcher (a dev shell or `cargo test`, which
/// sets `LD_LIBRARY_PATH` for the test binary), not an injection into the peer. Anything else counts as injected.
#[cfg(target_os = "linux")]
fn inherited(name: &str, value: &[u8]) -> bool {
    std::env::var_os(name).is_some_and(|v| v.as_encoded_bytes() == value)
}

/// SEC-20 lapis 2 (kernel without a Landlock scope): a UI peer carrying an injection library or being traced is refused.
/// A list of paths is a speed bump, never a wall (TECH 21.8); unreadable `/proc` = refused (closed).
#[cfg(target_os = "linux")]
pub fn peer_tainted(s: &UnixStream) -> bool {
    const INJECT: &[&[u8]] = &[b"LD_PRELOAD=", b"LD_AUDIT=", b"LD_LIBRARY_PATH=", b"GTK_MODULES=", b"GIO_EXTRA_MODULES="];
    let Ok(pid) = peer_pid(s) else { return true };
    let _pin = peer_pidfd(s);
    match std::fs::read(format!("/proc/{pid}/environ")) {
        Ok(env) => {
            let injected = env.split(|b| *b == 0).any(|e| {
                INJECT.iter().any(|k| {
                    let name = std::str::from_utf8(&k[..k.len() - 1]).unwrap_or_default();
                    e.starts_with(k) && e.len() > k.len() && !inherited(name, &e[k.len()..])
                })
            });
            let traced = std::fs::read_to_string(format!("/proc/{pid}/status"))
                .ok()
                .and_then(|t| t.lines().find_map(|l| l.strip_prefix("TracerPid:")).and_then(|v| v.trim().parse::<u32>().ok()))
                .is_none_or(|t| t > 0);
            injected || traced
        }
        Err(_) => true,
    }
}

#[cfg(not(target_os = "linux"))]
pub fn peer_tainted(_s: &UnixStream) -> bool {
    false
}
