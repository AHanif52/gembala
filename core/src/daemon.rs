//! Daemon: kanal UI + kanal pane, registri pane, attach (TECH bagian 8, pekerjaan 0.3).
//!
//! Kanal UI (SEC-20): `hello` wajib membawa rahasia yang dibuat daemon saat start dan hanya
//! diserahkan ke peluncurnya lewat descriptor; koneksi dari dalam sesi pane ditolak walau
//! rahasianya benar. Kanal pane (SEC-3): identitas dari `getsid(peer_pid)`, bukan isi pesan.

use crate::grid::Grid;
use crate::pty::{self, Pty};
use base64::{Engine, engine::general_purpose::STANDARD as B64};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex};
use std::thread;

pub const PROTO: u64 = 1;
const MAX_LINE: u64 = 64 * 1024;
const SCROLLBACK: usize = 10_000;

#[derive(Clone, Copy, PartialEq)]
enum Chan {
    Ui,
    Pane,
}

struct Pane {
    master: File,
    /// PID shell pane = id sesi semua prosesnya (shell dijalankan dengan `setsid`).
    sid: i32,
    /// Grid dan pelanggan dikunci bersama: attach tidak kehilangan atau menggandakan output.
    view: Mutex<(Grid, Vec<Sender<String>>)>,
}

struct State {
    panes: Mutex<HashMap<u64, Arc<Pane>>>,
    next: AtomicU64,
    secret: Option<String>,
    pane_sock: String,
}

type Res = Result<Value, &'static str>;

pub fn run(dir: &Path, secret: Option<String>, on_ready: impl FnOnce()) -> io::Result<()> {
    let ui = bind(&crate::rundir::socket_path(dir, "ui.sock")?)?;
    let pane_path = crate::rundir::socket_path(dir, "pane.sock")?;
    let pn = bind(&pane_path)?;
    let st = Arc::new(State {
        panes: Mutex::default(),
        next: AtomicU64::new(1),
        secret,
        pane_sock: pane_path.to_string_lossy().into_owned(),
    });
    on_ready();
    let st2 = st.clone();
    thread::spawn(move || accept(pn, st2, Chan::Pane));
    accept(ui, st, Chan::Ui);
    Ok(())
}

fn bind(p: &Path) -> io::Result<UnixListener> {
    if UnixStream::connect(p).is_ok() {
        return Err(io::Error::new(io::ErrorKind::AddrInUse, format!("daemon sudah jalan: {}", p.display())));
    }
    let _ = fs::remove_file(p); // socket basi dari daemon yang mati
    let l = UnixListener::bind(p)?;
    fs::set_permissions(p, fs::Permissions::from_mode(0o600))?;
    Ok(l)
}

fn accept(l: UnixListener, st: Arc<State>, chan: Chan) {
    for s in l.incoming().flatten() {
        let st = st.clone();
        thread::spawn(move || client(s, st, chan));
    }
}

fn line(v: Value) -> String {
    let mut s = v.to_string();
    s.push('\n');
    s
}

fn reply(tx: &Sender<String>, id: &Value, r: Res) {
    let v = match r {
        Ok(x) => json!({"id": id, "result": x}),
        Err(code) => json!({"id": id, "error": {"code": code}}),
    };
    let _ = tx.send(line(v));
}

fn client(s: UnixStream, st: Arc<State>, chan: Chan) -> io::Result<()> {
    let peer = peer_pid(&s)?;
    let (tx, rx) = channel::<String>();
    let mut w = s.try_clone()?;
    // satu penulis per koneksi: balasan dan event tidak saling potong.
    // ponytail: setelah klien putus, thread + fd ini baru lepas saat pane yang di-attach mengeluarkan output berikutnya
    thread::spawn(move || {
        for l in rx {
            if w.write_all(l.as_bytes()).is_err() {
                break;
            }
        }
    });
    let mut r = BufReader::new(s);
    let mut ready = false;
    loop {
        let mut buf = String::new();
        let n = (&mut r).take(MAX_LINE + 1).read_line(&mut buf)?;
        if n == 0 {
            return Ok(());
        }
        if n as u64 > MAX_LINE {
            reply(&tx, &Value::Null, Err("line_too_long"));
            return Ok(());
        }
        let Ok(req) = serde_json::from_str::<Value>(&buf) else {
            reply(&tx, &Value::Null, Err("bad_json"));
            return Ok(());
        };
        let (id, method, p) = (&req["id"], req["method"].as_str().unwrap_or(""), &req["params"]);
        if !ready {
            let r = if method == "hello" { hello(&st, chan, peer, p) } else { Err("hello_required") };
            ready = r.is_ok();
            reply(&tx, id, r);
            if !ready {
                return Ok(());
            }
            continue;
        }
        if chan == Chan::Pane {
            // ponytail: method pane (send/wait/read/reply/status) baru di Fase 2
            reply(&tx, id, Err("not_allowed"));
            continue;
        }
        if method == "pane.attach" {
            match st.pane(p) {
                Ok(pane) => {
                    let mut v = pane.view.lock().unwrap();
                    let (rows, cols) = v.0.size();
                    reply(&tx, id, Ok(json!({"snapshot": B64.encode(v.0.snapshot()), "rows": rows, "cols": cols})));
                    v.1.push(tx.clone());
                }
                Err(e) => reply(&tx, id, Err(e)),
            }
            continue;
        }
        reply(&tx, id, ui_method(&st, method, p));
    }
}

fn hello(st: &State, chan: Chan, peer: i32, p: &Value) -> Res {
    if p["proto"].as_u64() != Some(PROTO) {
        return Err("version_mismatch");
    }
    let pane = st.pane_of_pid(peer);
    match chan {
        Chan::Ui => {
            let ok = match (&st.secret, p["secret"].as_str()) {
                (Some(a), Some(b)) => ct_eq(a.as_bytes(), b.as_bytes()),
                _ => false,
            };
            if pane.is_some() || !ok {
                return Err("unauthorized");
            }
        }
        Chan::Pane if pane.is_none() => return Err("not_a_pane"),
        Chan::Pane => {}
    }
    Ok(json!({"proto": PROTO, "daemon": env!("CARGO_PKG_VERSION"), "pane": pane}))
}

fn dim(v: &Value, default: u16) -> u16 {
    v.as_u64().and_then(|n| u16::try_from(n).ok()).filter(|&n| n > 0).unwrap_or(default)
}

fn ui_method(st: &Arc<State>, m: &str, p: &Value) -> Res {
    match m {
        "pane.list" => {
            let panes = st.panes.lock().unwrap();
            let mut ids: Vec<_> = panes.keys().copied().collect();
            ids.sort();
            Ok(json!(ids))
        }
        "pane.create" => {
            let (rows, cols) = (dim(&p["rows"], 24), dim(&p["cols"], 80));
            let pty = Pty::spawn(p["cmd"].as_str(), rows, cols, &[("GEMBALA_SOCKET", &st.pane_sock)])
                .map_err(|_| "spawn_failed")?;
            let id = st.next.fetch_add(1, Ordering::Relaxed);
            let pane = Arc::new(Pane {
                master: pty.master.try_clone().map_err(|_| "spawn_failed")?,
                sid: pty.child.id() as i32,
                view: Mutex::new((Grid::new(rows, cols, SCROLLBACK), Vec::new())),
            });
            st.panes.lock().unwrap().insert(id, pane.clone());
            let st = st.clone();
            thread::spawn(move || pump(st, id, pane, pty));
            Ok(json!({"pane": id}))
        }
        "pane.write" => {
            let pane = st.pane(p)?;
            let data = p["data"].as_str().and_then(|d| B64.decode(d).ok()).ok_or("bad_params")?;
            (&pane.master).write_all(&data).map_err(|_| "io")?;
            Ok(json!({}))
        }
        "pane.resize" => {
            let pane = st.pane(p)?;
            let (rows, cols) = (dim(&p["rows"], 0), dim(&p["cols"], 0));
            if rows == 0 || cols == 0 {
                return Err("bad_params");
            }
            let mut v = pane.view.lock().unwrap();
            pty::resize(&pane.master, rows, cols).map_err(|_| "io")?;
            v.0.resize(rows, cols);
            Ok(json!({}))
        }
        "pane.close" => {
            let pane = st.pane(p)?;
            // seperti menutup jendela terminal: SIGHUP ke job foreground dan ke sesi pane
            unsafe {
                let fg = libc::tcgetpgrp(pane.master.as_raw_fd());
                if fg > 0 {
                    libc::killpg(fg, libc::SIGHUP);
                }
                libc::killpg(pane.sid, libc::SIGHUP);
            }
            Ok(json!({}))
        }
        _ => Err("unknown_method"),
    }
}

/// Output PTY → grid + semua pelanggan. Selesai saat shell pane keluar.
fn pump(st: Arc<State>, id: u64, pane: Arc<Pane>, mut pty: Pty) {
    let mut buf = [0u8; 16 * 1024];
    loop {
        let n = match (&pty.master).read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => break, // EIO: slave tertutup
        };
        let mut v = pane.view.lock().unwrap();
        v.0.feed(&buf[..n]);
        let ev = line(json!({"event": "pane.output", "params": {"pane": id, "data": B64.encode(&buf[..n])}}));
        v.1.retain(|tx| tx.send(ev.clone()).is_ok());
    }
    let code = pty.child.wait().ok().and_then(|s| s.code());
    st.panes.lock().unwrap().remove(&id);
    let ev = line(json!({"event": "pane.exit", "params": {"pane": id, "code": code}}));
    for tx in pane.view.lock().unwrap().1.drain(..) {
        let _ = tx.send(ev.clone());
    }
}

impl State {
    fn pane(&self, p: &Value) -> Result<Arc<Pane>, &'static str> {
        let id = p["pane"].as_u64().ok_or("bad_params")?;
        self.panes.lock().unwrap().get(&id).cloned().ok_or("no_such_pane")
    }

    fn pane_of_pid(&self, pid: i32) -> Option<u64> {
        let sid = unsafe { libc::getsid(pid) };
        if sid <= 0 {
            return None;
        }
        self.panes.lock().unwrap().iter().find(|(_, p)| p.sid == sid).map(|(id, _)| *id)
    }
}

fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(target_os = "macos")]
fn peer_pid(s: &UnixStream) -> io::Result<i32> {
    let mut pid: libc::pid_t = 0;
    let mut len = std::mem::size_of::<libc::pid_t>() as libc::socklen_t;
    let rc = unsafe {
        libc::getsockopt(s.as_raw_fd(), libc::SOL_LOCAL, libc::LOCAL_PEERPID, (&raw mut pid).cast(), &mut len)
    };
    if rc == -1 { Err(io::Error::last_os_error()) } else { Ok(pid) }
}

#[cfg(target_os = "linux")]
fn peer_pid(s: &UnixStream) -> io::Result<i32> {
    let mut c: libc::ucred = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    let rc = unsafe { libc::getsockopt(s.as_raw_fd(), libc::SOL_SOCKET, libc::SO_PEERCRED, (&raw mut c).cast(), &mut len) };
    if rc == -1 { Err(io::Error::last_os_error()) } else { Ok(c.pid) }
}

/// 32 byte acak dari kernel, hex.
pub fn new_secret() -> io::Result<String> {
    let mut b = [0u8; 32];
    File::open("/dev/urandom")?.read_exact(&mut b)?;
    Ok(b.iter().map(|x| format!("{x:02x}")).collect())
}

pub fn default_dir() -> PathBuf {
    std::env::var_os("GEMBALA_RUNDIR").map(PathBuf::from).unwrap_or_else(crate::rundir::default_dir)
}
