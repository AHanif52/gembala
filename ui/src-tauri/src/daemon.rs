//! UI channel client (TECH section 8, T1): secret from the login store (macOS Keychain, Linux Secret Service); when the
//! daemon isn't running, the UI launches it and receives the secret through `hand.sock` (a pipe in the fallback).

use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
#[cfg(target_os = "macos")]
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex};
use std::thread;

pub const KC_SERVICE: &str = "dev.gembala.app";

/// SEC-20: the UI channel secret in the login store, same service/account words on every system.
/// macOS uses the Keychain through `security-framework`; Linux the Secret Service through `keyring`.
#[cfg(target_os = "macos")]
fn secret_get(account: &str) -> Option<String> {
    security_framework::passwords::get_generic_password(KC_SERVICE, account).ok().and_then(|s| String::from_utf8(s).ok())
}

#[cfg(not(target_os = "macos"))]
fn secret_get(account: &str) -> Option<String> {
    keyring::Entry::new(KC_SERVICE, account).ok()?.get_password().ok()
}

#[cfg(target_os = "macos")]
fn secret_set(account: &str, secret: &str) -> Result<(), String> {
    security_framework::passwords::set_generic_password(KC_SERVICE, account, secret.as_bytes()).map_err(|e| e.to_string())
}

#[cfg(not(target_os = "macos"))]
fn secret_set(account: &str, secret: &str) -> Result<(), String> {
    keyring::Entry::new(KC_SERVICE, account).map_err(|e| e.to_string())?.set_password(secret).map_err(|e| e.to_string())
}

/// FR-48: forget this app's item (Settings > Security > "Forget secret").
pub fn secret_forget(account: &str) {
    #[cfg(target_os = "macos")]
    let _ = security_framework::passwords::delete_generic_password(KC_SERVICE, account);
    #[cfg(not(target_os = "macos"))]
    if let Ok(e) = keyring::Entry::new(KC_SERVICE, account) {
        let _ = e.delete_credential();
    }
}

pub struct Client {
    /// Daemon disconnected = false; the caller makes a new client (criterion 17: the daemon is launched again).
    pub alive: Arc<std::sync::atomic::AtomicBool>,
    /// SEC-20: the daemon recognizes this UI from its code signature (the secret isn't used)
    pub signed: bool,
    w: Mutex<UnixStream>,
    pending: Arc<Mutex<HashMap<u64, Sender<Value>>>>,
    next: Mutex<u64>,
}

fn err(msg: impl Into<String>) -> io::Error {
    io::Error::other(msg.into())
}

/// Linux SEC-20 lapis 1: connect to the abstract UI socket named in `<rundir>/ui.addr` (0600). A kernel without a
/// Landlock scope has no such file, and the daemon then listens on the path socket.
#[cfg(target_os = "linux")]
fn abstract_connect(name: &str) -> io::Result<UnixStream> {
    use std::os::fd::FromRawFd;
    let bytes = name.as_bytes();
    let mut sa: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    if bytes.is_empty() || bytes.len() + 1 >= sa.sun_path.len() {
        return Err(err("abstract name"));
    }
    sa.sun_family = libc::AF_UNIX as libc::sa_family_t;
    for (i, b) in bytes.iter().enumerate() {
        sa.sun_path[i + 1] = *b as libc::c_char;
    }
    let len = (std::mem::size_of::<libc::sa_family_t>() + 1 + bytes.len()) as libc::socklen_t;
    let fd = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM | libc::SOCK_CLOEXEC, 0) };
    if fd == -1 {
        return Err(io::Error::last_os_error());
    }
    if unsafe { libc::connect(fd, (&raw const sa).cast(), len) } == -1 {
        let e = io::Error::last_os_error();
        unsafe { libc::close(fd) };
        return Err(e);
    }
    Ok(unsafe { UnixStream::from_raw_fd(fd) })
}

/// UI channel of the daemon in `dir`: the abstract socket the run folder names (Linux lapis 1), else the path socket.
fn ui_stream(dir: &std::path::Path) -> io::Result<UnixStream> {
    #[cfg(target_os = "linux")]
    if let Ok(s) = std::fs::read_to_string(dir.join("ui.addr")) {
        let name = s.trim();
        if !name.is_empty() && !name.contains('/') && let Ok(s) = abstract_connect(name) {
            return Ok(s);
        }
    }
    UnixStream::connect(dir.join("ui.sock"))
}

/// Same folder as the daemon's `core::os::runtime_dir`: `$TMPDIR/gembala` on macOS, `$XDG_RUNTIME_DIR/gembala` on Linux.
pub fn rundir() -> PathBuf {
    if let Some(p) = std::env::var_os("GEMBALA_RUNDIR") {
        return p.into();
    }
    // macOS TMPDIR is already per user; the /tmp fallback is still checked by the daemon's `ensure_private_dir`
    let base = |k: &str| std::env::var_os(k).filter(|x| !x.is_empty()).map(PathBuf::from);
    #[cfg(target_os = "linux")]
    if let Some(d) = base("XDG_RUNTIME_DIR") {
        return d.join("gembala");
    }
    base("TMPDIR").unwrap_or_else(|| "/tmp".into()).join("gembala")
}

pub type OnEvent = Arc<dyn Fn(Value) + Send + Sync>;

/// Connect to the daemon; launch it if it isn't there. `on_event` receives daemon events.
pub fn connect(on_event: OnEvent) -> io::Result<Client> {
    connect_try(on_event, true)
}

fn connect_try(on_event: OnEvent, retry: bool) -> io::Result<Client> {
    let dir = rundir();
    // one secret per run folder: dev, tests, and the installed app don't overwrite each other
    let account = format!("ui-secret:{}", dir.display());
    // Debug builds only (measuring tools, tests): secret from env, no Keychain. Release builds don't have
    // this path, because SEC-20 forbids the UI channel secret in the environment.
    #[cfg(debug_assertions)]
    let dev = std::env::var("GEMBALA_DEV_SECRET").ok();
    #[cfg(not(debug_assertions))]
    let dev: Option<String> = None;
    if let (Some(secret), Ok(s)) = (&dev, ui_stream(&dir)) {
        return Client::open(s, secret, on_event);
    }
    // socket first: the Keychain is only touched when needed. Keychain items of a self-signed app are tied to the build's cdhash,
    // so reading/writing an old build's item brings up a macOS permission dialog on every update.
    let Ok(s) = ui_stream(&dir) else {
        let secret = spawn_daemon()?;
        let c = Client::open(ui_stream(&dir)?, &secret, on_event)?;
        // SEC-20: the daemon recognizes the UI from its signature = no Keychain needed. Build without a certificate:
        // the secret is stored for reconnecting; a failed save doesn't fail this session.
        // Measuring/test tools (debug build + GEMBALA_DEV_SECRET) don't write to the user's Keychain.
        if !c.signed
            && dev.is_none()
            && let Err(e) = secret_set(&account, &secret)
        {
            eprintln!("simpan rahasia: {e}");
        }
        return Ok(c);
    };
    // a daemon stopping on its own (FR-20: no UI, panes idle) closes the socket in the middle of hello:
    // wait for it to exit, then connect or launch a new daemon
    let closing = |e: &io::Error| matches!(e.kind(), io::ErrorKind::BrokenPipe | io::ErrorKind::ConnectionReset | io::ErrorKind::UnexpectedEof);
    // SEC-20: no secret first (recognized by signature); the Keychain is only a fallback for old/uncertified daemons
    match Client::open(s, "", on_event.clone()) {
        Ok(c) => return Ok(c),
        Err(e) if closing(&e) && retry => {
            thread::sleep(std::time::Duration::from_millis(500));
            return connect_try(on_event, false);
        }
        Err(e) if closing(&e) => return Err(e),
        Err(_) => {} // rejected: try the secret from the Keychain
    }
    let s = ui_stream(&dir)?;
    match secret_get(&account) {
        // ponytail: daemon alive but the Keychain secret is stale/missing; automatic reset in FR-49 (Phase 1)
        Some(secret) => match Client::open(s, &secret, on_event.clone()) {
            Err(e) if retry && closing(&e) => {
                thread::sleep(std::time::Duration::from_millis(500));
                connect_try(on_event, false)
            }
            r => r.map_err(|e| err(format!("{e}; stop the old daemon with: pkill -f 'gembala daemon'"))),
        },
        None => Err(err("the daemon is running but its secret isn't in the login store; stop it with: pkill -f 'gembala daemon'")),
    }
}

/// Daemon binary: helper app inside the bundle (release), or `gembala` next to the app binary (dev: `target/debug`).
pub fn daemon_exe() -> Option<PathBuf> {
    let me = std::env::current_exe().ok()?;
    let helper = me.parent()?.parent()?.join("Helpers/Gembala Daemon.app/Contents/MacOS/gembala");
    Some(if helper.exists() { helper } else { me.with_file_name("gembala") })
}

/// FR-20: macOS launches the daemon helper through LaunchServices (`open`), because processes started by the app
/// (setsid, double-fork, disclaim, even a `launchctl bootstrap` job) land in the app's coalition and loginwindow stops
/// them when the app closes, pane shells and agents included (tested 2 Nov 2026). Linux has no coalition: `setsid` plus a
/// direct spawn is enough (the app's systemd scope stays alive while the daemon lives). Both hand the secret over
/// `hand.sock` in the run folder (`0700`), and the UI only accepts it from this install's daemon binary.
fn spawn_daemon() -> io::Result<String> {
    use std::os::unix::fs::DirBuilderExt;
    let exe = daemon_exe().ok_or_else(|| err("path app tidak terbaca"))?;
    let dir = rundir();
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&dir)?;
    let hand = dir.join("hand.sock");
    let _ = std::fs::remove_file(&hand);
    let l = std::os::unix::net::UnixListener::bind(&hand)?;
    // the previous daemon log is kept one generation for diagnosis
    let log = dir.join("daemon.log");
    let _ = std::fs::rename(&log, dir.join("daemon.log.1"));
    #[cfg(target_os = "macos")]
    {
        let helper = exe.ancestors().nth(3).filter(|p| p.extension().is_some_and(|e| e == "app")).map(|p| p.to_path_buf());
        match helper {
            Some(app) => spawn_open(&app, &exe, &hand, &log, l).or_else(|e| {
                eprintln!("helper daemon: {e}; daemon dijalankan langsung");
                spawn_child(&exe)
            }),
            None => spawn_child(&exe),
        }
    }
    #[cfg(not(target_os = "macos"))]
    accept_hand(&exe, &hand, l, detach(&exe, &hand, &log))
}

/// macOS: `open -g -n` puts the daemon in its own app instance, never in the UI's coalition.
#[cfg(target_os = "macos")]
fn spawn_open(app: &std::path::Path, exe: &std::path::Path, hand: &std::path::Path, log: &std::path::Path, l: std::os::unix::net::UnixListener) -> io::Result<String> {
    let mut c = Command::new("/usr/bin/open");
    c.args(["-g", "-n", "--stderr"]).arg(log);
    // the app environment is passed on as when the daemon was the app's child (panes inherit it)
    for (k, v) in std::env::vars_os() {
        let (Some(k), Some(v)) = (k.to_str(), v.to_str()) else { continue };
        if !k.starts_with("XPC_") && !k.starts_with("__CF") {
            c.arg("--env").arg(format!("{k}={v}"));
        }
    }
    c.arg(app).args(["--args", "daemon", "--secret-sock"]).arg(hand);
    let out = c.output()?;
    if !out.status.success() {
        return Err(err(format!("open: {}", String::from_utf8_lossy(&out.stderr).trim())));
    }
    accept_hand(exe, hand, l, Ok(()))
}

/// Linux: direct spawn in its own session, stderr into the daemon log; the app doesn't wait for it.
#[cfg(not(target_os = "macos"))]
fn detach(exe: &std::path::Path, hand: &std::path::Path, log: &std::path::Path) -> io::Result<()> {
    use std::os::unix::process::CommandExt;
    let mut c = Command::new(exe);
    c.args(["daemon", "--secret-sock"]).arg(hand).stdin(Stdio::null()).stdout(Stdio::null());
    c.stderr(Stdio::from(std::fs::OpenOptions::new().append(true).create(true).open(log)?));
    unsafe {
        c.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let mut child = c.spawn().map_err(|e| err(format!("jalankan {}: {e}", exe.display())))?;
    // the daemon lives until it's stopped on purpose; reap it so the app doesn't keep a zombie
    thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

/// Secret through `hand.sock`: the daemon only writes when the socket owner is this install's `gembala-ui`, and the UI
/// only accepts it from this install's daemon binary (`peer_path`).
fn accept_hand(exe: &std::path::Path, hand: &std::path::Path, l: std::os::unix::net::UnixListener, launched: io::Result<()>) -> io::Result<String> {
    use std::time::{Duration, Instant};
    launched?;
    l.set_nonblocking(true)?;
    let deadline = Instant::now() + Duration::from_secs(10);
    let s = loop {
        match l.accept() {
            Ok((s, _)) => break s,
            Err(e) if e.kind() == io::ErrorKind::WouldBlock && Instant::now() < deadline => thread::sleep(Duration::from_millis(50)),
            Err(e) => return Err(err(format!("the daemon didn't hand over its secret: {e}"))),
        }
    };
    let _ = std::fs::remove_file(hand);
    s.set_nonblocking(false)?;
    s.set_read_timeout(Some(Duration::from_secs(5)))?;
    if peer_path(&s) != exe.canonicalize().ok() {
        return Err(err("serah-terima rahasia dari proses selain daemon Gembala"));
    }
    let mut secret = String::new();
    BufReader::new(s).read_line(&mut secret)?;
    let secret = secret.trim().to_string();
    if secret.is_empty() {
        return Err(err("daemon gagal start (daemon lain masih jalan?)"));
    }
    Ok(secret)
}

/// Binary path of the process at the other end of the socket.
#[cfg(target_os = "macos")]
fn peer_path(s: &UnixStream) -> Option<PathBuf> {
    use std::os::fd::AsRawFd;
    use std::os::unix::ffi::OsStrExt;
    let mut pid: libc::pid_t = 0;
    let mut len = std::mem::size_of::<libc::pid_t>() as libc::socklen_t;
    if unsafe { libc::getsockopt(s.as_raw_fd(), libc::SOL_LOCAL, libc::LOCAL_PEERPID, (&raw mut pid).cast(), &mut len) } != 0 {
        return None;
    }
    let mut buf = [0u8; 4 * libc::MAXPATHLEN as usize];
    let n = unsafe { libc::proc_pidpath(pid, buf.as_mut_ptr().cast(), buf.len() as u32) };
    let p = PathBuf::from(std::ffi::OsStr::from_bytes(&buf[..usize::try_from(n).ok().filter(|&n| n > 0)?]));
    p.canonicalize().ok()
}

/// Linux: `SO_PEERCRED` carries the pid; `/proc/<pid>/exe` its binary.
#[cfg(target_os = "linux")]
fn peer_path(s: &UnixStream) -> Option<PathBuf> {
    use std::os::fd::AsRawFd;
    let mut c: libc::ucred = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    if unsafe { libc::getsockopt(s.as_raw_fd(), libc::SOL_SOCKET, libc::SO_PEERCRED, (&raw mut c).cast(), &mut len) } != 0 {
        return None;
    }
    std::fs::read_link(format!("/proc/{}/exe", c.pid)).ok().and_then(|p| p.canonicalize().ok())
}

/// Old way (macOS without a helper): a direct child of the app, secret through descriptor 3.
#[cfg(target_os = "macos")]
fn spawn_child(exe: &std::path::Path) -> io::Result<String> {
    use std::os::fd::{FromRawFd, OwnedFd};
    let mut fds = [0; 2];
    if unsafe { libc::pipe(fds.as_mut_ptr()) } == -1 {
        return Err(io::Error::last_os_error());
    }
    let (r, w) = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
    for fd in fds {
        unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
    }
    let wfd = fds[1];
    let mut c = Command::new(exe);
    c.args(["daemon", "--secret-fd", "3"]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    unsafe {
        c.pre_exec(move || {
            libc::setsid();
            if libc::dup2(wfd, 3) == -1 { Err(io::Error::last_os_error()) } else { Ok(()) }
        });
    }
    c.spawn().map_err(|e| err(format!("jalankan {}: {e}", exe.display())))?;
    drop(c);
    drop(w);
    let mut secret = String::new();
    BufReader::new(std::fs::File::from(r)).read_line(&mut secret)?;
    let secret = secret.trim().to_string();
    if secret.is_empty() {
        return Err(err("daemon gagal start (daemon lain masih jalan?)"));
    }
    Ok(secret)
}

impl Client {
    fn open(s: UnixStream, secret: &str, on_event: OnEvent) -> io::Result<Client> {
        let mut r = BufReader::new(s.try_clone()?);
        let mut w = s;
        let hello = json!({"id": 0, "method": "hello", "params": {"proto": 1, "client": "ui", "secret": secret}});
        writeln!(w, "{hello}")?;
        let mut l = String::new();
        if r.read_line(&mut l)? == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into()); // the daemon closes the socket before answering
        }
        let v: Value = serde_json::from_str(&l).unwrap_or(Value::Null);
        if v.get("result").is_none() {
            return Err(err(format!("hello rejected: {}", v["error"]["code"])));
        }
        let pending: Arc<Mutex<HashMap<u64, Sender<Value>>>> = Arc::default();
        let p2 = pending.clone();
        let alive = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let alive2 = alive.clone();
        thread::spawn(move || {
            for l in r.lines() {
                let Ok(v) = serde_json::from_str::<Value>(&l.unwrap_or_default()) else { break };
                match v["id"].as_u64() {
                    Some(id) => {
                        if let Some(tx) = p2.lock().unwrap().remove(&id) {
                            let _ = tx.send(v);
                        }
                    }
                    None => on_event(v),
                }
            }
            // calls still waiting are cancelled, not left hanging forever
            alive2.store(false, std::sync::atomic::Ordering::Relaxed);
            p2.lock().unwrap().clear();
            on_event(json!({"event": "daemon.lost"}));
        });
        let signed = v["result"]["signed"].as_bool() == Some(true);
        Ok(Client { alive, signed, w: Mutex::new(w), pending, next: Mutex::new(1) })
    }

    pub fn call(&self, method: &str, params: Value) -> Result<Value, String> {
        let id = {
            let mut n = self.next.lock().unwrap();
            *n += 1;
            *n
        };
        let (tx, rx) = channel();
        self.pending.lock().unwrap().insert(id, tx);
        let req = json!({"id": id, "method": method, "params": params});
        writeln!(self.w.lock().unwrap(), "{req}").map_err(|e| e.to_string())?;
        let v = rx.recv().map_err(|_| "daemon putus".to_string())?;
        match v.get("result") {
            Some(r) => Ok(r.clone()),
            None => Err(v["error"]["code"].as_str().unwrap_or("error").to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stopping daemon closes the socket before answering hello: the error must be the kind that triggers a reconnect.
    #[test]
    fn hello_putus_bisa_diulang() {
        let (a, b) = UnixStream::pair().unwrap();
        let t = thread::spawn(move || {
            let mut l = String::new();
            let _ = BufReader::new(&b).read_line(&mut l); // accept hello, then close without answering
        });
        let e = Client::open(a, "x", Arc::new(|_| {})).err().unwrap();
        t.join().unwrap();
        assert!(matches!(e.kind(), io::ErrorKind::BrokenPipe | io::ErrorKind::ConnectionReset | io::ErrorKind::UnexpectedEof), "{e:?}");
    }
}
