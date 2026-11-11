//! FR-98 / SEC-20: `gembala jail -- <cmd>` (TECH 21.8 lapis 1). Linux only; when the kernel has no Landlock scope
//! (ABI 6, kernel 6.12+) or netcat is missing, the test says so and passes: lapis 2 is what protects those kernels.

#![cfg(target_os = "linux")]

use std::os::fd::FromRawFd;
use std::os::unix::net::UnixListener;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn gembala() -> &'static str {
    env!("CARGO_BIN_EXE_gembala")
}

fn have(cmd: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(cmd).is_file()))
}

/// Landlock ABI of this kernel; 0 = no syscall (ENOSYS) or disabled.
fn abi() -> libc::c_long {
    let r = unsafe { libc::syscall(libc::SYS_landlock_create_ruleset, std::ptr::null::<u8>(), 0usize, 1 as libc::c_uint) };
    if r < 0 { 0 } else { r }
}

fn bind_abstract(name: &str) -> UnixListener {
    let bytes = name.as_bytes();
    let mut sa: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    sa.sun_family = libc::AF_UNIX as libc::sa_family_t;
    for (i, b) in bytes.iter().enumerate() {
        sa.sun_path[i + 1] = *b as libc::c_char;
    }
    let len = (std::mem::size_of::<libc::sa_family_t>() + 1 + bytes.len()) as libc::socklen_t;
    let fd = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM | libc::SOCK_CLOEXEC, 0) };
    assert!(fd >= 0, "socket");
    if unsafe { libc::bind(fd, (&raw const sa).cast(), len) } == -1 {
        panic!("bind abstrak: {}", std::io::Error::last_os_error());
    }
    assert_eq!(unsafe { libc::listen(fd, 8) }, 0, "listen");
    assert_eq!(unsafe { libc::fcntl(fd, libc::F_SETFL, libc::O_NONBLOCK) }, 0, "nonblock");
    unsafe { UnixListener::from_raw_fd(fd) }
}

/// A connection within `within`, or `None`; the listener stays non-blocking.
fn accept_within(l: &UnixListener, within: Duration) -> Option<()> {
    let end = Instant::now() + within;
    loop {
        match l.accept() {
            Ok(_) => return Some(()),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < end => std::thread::sleep(Duration::from_millis(20)),
            Err(_) => return None,
        }
    }
}

/// A free process reaches the abstract socket; a jailed one gets it refused by the kernel (the pane's whole point).
#[test]
fn jail_menutup_socket_abstrak() {
    let abi = abi();
    if abi < 6 {
        eprintln!("lewati: Landlock ABI {abi} (< 6), kernel ini memakai lapis 2");
        return;
    }
    if !have("nc") {
        eprintln!("lewati: nc (netcat) tidak terpasang");
        return;
    }
    let name = format!("gembala-jail-{}", std::process::id());
    let l = bind_abstract(&name);
    let target = format!("@{name}");
    // free: connects; keep nc waiting until the accept closes, then it exits on its own
    let mut free = Command::new("nc").args(["-U", &target]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap();
    assert!(accept_within(&l, Duration::from_secs(2)).is_some(), "koneksi bebas tidak sampai");
    let st = free.wait().unwrap();
    assert!(st.success(), "nc bebas keluar normal");
    // jailed: the kernel refuses the connect
    let st = Command::new(gembala()).args(["jail", "--", "nc", "-U", &target]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status().unwrap();
    assert!(!st.success(), "nc di dalam jail tidak boleh bisa connect");
    assert!(accept_within(&l, Duration::from_millis(500)).is_none(), "koneksi dari jail nyasar");
}

/// `no_new_privs` is what makes the domain stick; the child sees it and the flag is inherited by its own children.
#[test]
fn jail_menyalakan_no_new_privs() {
    let abi = abi();
    if abi < 6 {
        eprintln!("lewati: Landlock ABI {abi} (< 6)");
        return;
    }
    let out = Command::new(gembala()).args(["jail", "--", "sh", "-c", "grep NoNewPrivs /proc/self/status"]).output().unwrap();
    assert!(out.status.success(), "{out:?}");
    assert!(String::from_utf8_lossy(&out.stdout).contains("NoNewPrivs:\t1"), "{out:?}");
}
