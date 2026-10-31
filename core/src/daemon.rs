//! Daemon: kanal UI + kanal pane, registri pane, attach (TECH bagian 8, pekerjaan 0.3).
//!
//! Kanal UI (SEC-20): `hello` wajib membawa rahasia yang dibuat daemon saat start dan hanya
//! diserahkan ke peluncurnya lewat descriptor; koneksi dari dalam sesi pane ditolak walau
//! rahasianya benar. Kanal pane (SEC-3): identitas dari `getsid(peer_pid)`, bukan isi pesan.

use crate::config;
use crate::profile;
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
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

pub const PROTO: u64 = 1;
const MAX_LINE: u64 = 64 * 1024;
/// Snapshot hanya layar terlihat dan scrollback UI dipegang xterm.js, jadi daemon cukup
/// menyimpan baris terakhir untuk `read` (FR-7). 10.000 baris penuh = sekitar 40 MB per pane di vt100.
const SCROLLBACK: usize = 1_000;
/// Antrean per koneksi. Penuh = klien terlalu lambat: koneksinya diputus (attach ulang dapat snapshot)
/// supaya RAM daemon tidak membengkak dan agent tidak tertahan.
const QUEUE: usize = 256;

#[derive(Clone, Copy, PartialEq)]
enum Chan {
    Ui,
    Pane,
}

struct Pane {
    master: File,
    /// PID shell pane = id sesi semua prosesnya (shell dijalankan dengan `setsid`).
    sid: i32,
    /// Nama otomatis `<agent>-<n>` (FR-30) dan agent profilnya ("shell" bila tanpa profil).
    name: String,
    agent: String,
    /// FR-4: idle | working | blocked | done; kosong = tidak dilacak (shell, profil tanpa hook)
    status: Mutex<&'static str>,
    /// Grid dan pelanggan dikunci bersama: attach tidak kehilangan atau menggandakan output.
    view: Mutex<(Grid, Vec<Sub>)>,
}

struct Sub {
    tx: SyncSender<String>,
    sock: UnixStream,
}

struct State {
    panes: Mutex<HashMap<u64, Arc<Pane>>>,
    next: AtomicU64,
    secret: Option<String>,
    pane_sock: String,
    /// Klien kanal UI yang sedang tersambung (FR-20: daemon berhenti bila 0 dan semua pane diam).
    ui_clients: AtomicUsize,
    /// Antrean event semua klien UI, untuk event yang bukan milik satu pane (`config.changed`).
    /// Dilepas saat klien putus (UiClient), supaya thread penulisnya ikut selesai.
    ui_txs: Mutex<Vec<(u64, SyncSender<String>)>>,
    ui_next: AtomicU64,
    /// Config yang berlaku + galatnya (FR-42). File rusak = nilai valid terakhir tetap dipakai.
    config: Mutex<Value>,
    profiles: Mutex<Vec<profile::Profile>>,
    /// Hasil deteksi agent terakhir (FR-28); Null = belum pernah.
    agents: Mutex<Value>,
    /// Id acak per run daemon: id pane dari run lain tidak boleh dianggap pane yang sama (FR-45).
    boot: String,
    hooks: profile::Hooks,
}

struct UiClient(Arc<State>, u64);

impl Drop for UiClient {
    fn drop(&mut self) {
        self.0.ui_clients.fetch_sub(1, Ordering::Relaxed);
        self.0.ui_txs.lock().unwrap().retain(|(id, _)| *id != self.1);
    }
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
        ui_clients: AtomicUsize::new(0),
        ui_txs: Mutex::default(),
        ui_next: AtomicU64::new(1),
        config: Mutex::new(Value::Null),
        profiles: Mutex::new(profile::builtin()),
        agents: Mutex::new(Value::Null),
        boot: new_secret()?[..16].to_string(),
        hooks: profile::Hooks {
            exe: std::env::current_exe()?.to_string_lossy().into_owned(),
            dir: config::path().with_file_name("hooks"),
        },
    });
    write_hook_files(&st.hooks);
    reload_config(&st);
    on_ready();
    let st3 = st.clone();
    thread::spawn(move || idle_exit(st3));
    let st4 = st.clone();
    thread::spawn(move || watch_config(st4));
    let st5 = st.clone();
    thread::spawn(move || *st5.agents.lock().unwrap() = profile::detect()); // FR-28: saat pertama dibuka
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

fn reply(tx: &SyncSender<String>, id: &Value, r: Res) {
    let v = match r {
        Ok(x) => json!({"id": id, "result": x}),
        Err(code) => json!({"id": id, "error": {"code": code}}),
    };
    let _ = tx.send(line(v));
}

/// Sesi selesai (normal atau ditolak): penulis diberi tanda kosong, menghabiskan antrean, lalu menutup
/// socket dua arah. Klien tidak menunggu selamanya dan balasan terakhir tetap sampai.
struct CloseAfterFlush(SyncSender<String>, UnixStream);

impl Drop for CloseAfterFlush {
    fn drop(&mut self) {
        if self.0.try_send(String::new()).is_err() {
            let _ = self.1.shutdown(std::net::Shutdown::Both);
        }
    }
}

fn client(s: UnixStream, st: Arc<State>, chan: Chan) -> io::Result<()> {
    let peer = peer_pid(&s)?;
    let (tx, rx) = sync_channel::<String>(QUEUE);
    let mut w = s.try_clone()?;
    let sock = s.try_clone()?;
    // satu penulis per koneksi: balasan dan event tidak saling potong
    thread::spawn(move || {
        for l in rx {
            if l.is_empty() || w.write_all(l.as_bytes()).is_err() {
                break;
            }
        }
        let _ = w.shutdown(std::net::Shutdown::Both);
    });
    let _close = CloseAfterFlush(tx.clone(), s.try_clone()?);
    let mut r = BufReader::new(s);
    let mut ready = false;
    let mut _ui: Option<UiClient> = None;
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
            if ready && chan == Chan::Ui {
                st.ui_clients.fetch_add(1, Ordering::Relaxed);
                let id = st.ui_next.fetch_add(1, Ordering::Relaxed);
                st.ui_txs.lock().unwrap().push((id, tx.clone()));
                _ui = Some(UiClient(st.clone(), id));
            }
            reply(&tx, id, r);
            if !ready {
                return Ok(());
            }
            continue;
        }
        if chan == Chan::Pane {
            // identitas dari hello (getsid), bukan dari isi pesan (SEC-3)
            let me = st.pane_of_pid(peer);
            let r = match (method, me) {
                ("hook", Some(me)) => match p["status"].as_str().and_then(|x| ["idle", "working", "blocked", "done"].into_iter().find(|s| *s == x)) {
                    Some(status) => {
                        let pane = st.panes.lock().unwrap().get(&me).cloned();
                        if let Some(pane) = pane {
                            set_status(&st, me, &pane, status, None);
                        }
                        Ok(json!({}))
                    }
                    None => Err("bad_params"),
                },
                // ponytail: method pane (send/wait/read/reply/status) baru di Fase 2
                _ => Err("not_allowed"),
            };
            reply(&tx, id, r);
            continue;
        }
        if method == "pane.attach" {
            match st.pane(p) {
                Ok(pane) => {
                    let mut v = pane.view.lock().unwrap();
                    let (rows, cols) = v.0.size();
                    reply(&tx, id, Ok(json!({"snapshot": B64.encode(v.0.snapshot()), "rows": rows, "cols": cols})));
                    if let Ok(sock) = sock.try_clone() {
                        v.1.push(Sub { tx: tx.clone(), sock });
                    }
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
    Ok(json!({"proto": PROTO, "daemon": env!("CARGO_PKG_VERSION"), "pane": pane, "boot": st.boot}))
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
        "config.get" => Ok(st.config.lock().unwrap().clone()),
        "daemon.info" => Ok(json!({"boot": st.boot, "daemon": env!("CARGO_PKG_VERSION"), "proto": PROTO})),
        "layout.load" => {
            let text = std::fs::read_to_string(config::layout_path()).unwrap_or_default();
            Ok(serde_json::from_str::<Value>(&text).ok().filter(Value::is_object).unwrap_or(Value::Null))
        }
        "layout.save" => {
            // hanya struktur dari UI; ukurannya sudah dibatasi 64 KB per baris protokol
            let body = p["layout"].to_string();
            if !p["layout"].is_object() {
                return Err("bad_params");
            }
            config::write_atomic(&config::layout_path(), body.as_bytes()).map_err(|_| "io")?;
            Ok(json!({}))
        }
        "agents.detect" => {
            // FR-28: hasil tersimpan; "Periksa ulang" = refresh
            if p["refresh"].as_bool() == Some(true) || st.agents.lock().unwrap().is_null() {
                let found = profile::detect();
                *st.agents.lock().unwrap() = found;
            }
            Ok(st.agents.lock().unwrap().clone())
        }
        "config.set" => {
            let (section, key) = (p["section"].as_str().ok_or("bad_params")?, p["key"].as_str().ok_or("bad_params")?);
            if let Err(why) = config::set(&config::path(), section, key, &p["value"]) {
                return Ok(json!({"error": why}));
            }
            reload_config(st);
            Ok(json!({}))
        }
        "pane.create" => {
            // batas bagian 11 BRD, bisa diubah lewat [limits] max_panes
            let max = st.config.lock().unwrap()["values"]["limits"]["max_panes"].as_u64().unwrap_or(32) as usize;
            if st.panes.lock().unwrap().len() >= max {
                return Err("max_panes");
            }
            let (rows, cols) = (dim(&p["rows"], 24), dim(&p["cols"], 80));
            // FR-30: profil (bawaan "shell"); folder = folder profil, atau folder pane yang fokus
            let prof = match p["profile"].as_str() {
                Some(id) => Some(st.profiles.lock().unwrap().iter().find(|x| x.id == id).cloned().ok_or("no_such_profile")?),
                None => None,
            };
            let launch = prof.as_ref().and_then(|x| profile::launch(x, Some(&st.hooks)));
            let cmd = launch.as_deref().or(p["cmd"].as_str());
            let home = std::env::var("HOME").unwrap_or_default();
            // urutan folder: `cwd` eksplisit (jalankan ulang pane mati, FR-45) > folder profil > pane yang fokus
            let explicit = p["cwd"].as_str().filter(|c| c.starts_with('/') || c.starts_with('~')).map(std::path::PathBuf::from);
            let cwd = match explicit.or_else(|| prof.as_ref().map(|x| x.cwd.clone()).filter(|c| !c.is_empty()).map(Into::into)) {
                Some(c) => {
                    let c = c.to_string_lossy();
                    Some(std::path::PathBuf::from(c.replacen('~', &home, usize::from(c.starts_with('~')))))
                }
                None => st.pane(&json!({"pane": p["from_pane"]})).ok().and_then(|f| {
                    let pid = pty::busy(&f.master, f.sid).map(|(pid, _)| pid).unwrap_or(f.sid);
                    pty::cwd_of(pid).or_else(|| pty::cwd_of(f.sid))
                }),
            };
            let agent = prof.as_ref().map_or("shell".to_string(), |x| x.agent.clone());
            let pty = Pty::spawn(cmd, rows, cols, &[("GEMBALA_SOCKET", &st.pane_sock)], cwd.as_deref())
                .map_err(|_| "spawn_failed")?;
            let id = st.next.fetch_add(1, Ordering::Relaxed);
            let pane = Arc::new(Pane {
                master: pty.master.try_clone().map_err(|_| "spawn_failed")?,
                sid: pty.child.id() as i32,
                name: {
                    let label = prof.as_ref().map_or("shell", |x| if x.agent == "custom" { x.id.as_str() } else { x.agent.as_str() });
                    let n = st.panes.lock().unwrap().values().filter(|q| q.name.starts_with(&format!("{label}-"))).count() + 1;
                    format!("{label}-{n}")
                },
                status: Mutex::new(if ["claude", "codex", "opencode", "omp"].contains(&agent.as_str()) { "idle" } else { "" }),
                agent,
                view: Mutex::new((Grid::new(rows, cols, SCROLLBACK), Vec::new())),
            });
            st.panes.lock().unwrap().insert(id, pane.clone());
            let name = pane.name.clone();
            let st = st.clone();
            thread::spawn(move || pump(st, id, pane, pty));
            Ok(json!({"pane": id, "name": name}))
        }
        "pane.write" => {
            let pane = st.pane(p)?;
            let data = p["data"].as_str().and_then(|d| B64.decode(d).ok()).ok_or("bad_params")?;
            (&pane.master).write_all(&data).map_err(|_| "io")?;
            // hasil "done" dianggap sudah dibaca begitu pengguna mengetik di pane itu (bagian 8 BRD)
            set_status(st, p["pane"].as_u64().unwrap_or_default(), &pane, "idle", Some("done"));
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
        "pane.info" => {
            let pane = st.pane(p)?;
            let busy = pty::busy(&pane.master, pane.sid);
            Ok(json!({
                "busy": busy.is_some(), "name": busy.and_then(|(pid, _)| pty::proc_name(pid)),
                "pane_name": pane.name, "agent": pane.agent, "status": *pane.status.lock().unwrap(),
                "cwd": pty::cwd_of(busy.map_or(pane.sid, |(pid, _)| pid)).or_else(|| pty::cwd_of(pane.sid)),
            }))
        }
        "pane.kill" => {
            // FR-50: SIGTERM ke proses yang jalan, SIGKILL bila masih hidup setelah 3 detik; shell pane tetap
            let pane = st.pane(p)?;
            let Some(target) = pty::busy(&pane.master, pane.sid) else {
                return Ok(json!({"stopped": false}));
            };
            pty::signal(target, libc::SIGTERM);
            thread::spawn(move || {
                for _ in 0..30 {
                    thread::sleep(Duration::from_millis(100));
                    if !pty::signal(target, 0) {
                        return;
                    }
                }
                pty::signal(target, libc::SIGKILL);
            });
            Ok(json!({"stopped": true}))
        }
        "daemon.reset" => {
            // FR-48: hanya file milik Gembala dengan nama yang dikenal, lalu folder bila kosong; data agent tidak disentuh
            let path = config::path();
            let dir = path.parent().ok_or("io")?;
            for f in [path.clone(), config::layout_path()] {
                let _ = std::fs::remove_file(f);
            }
            let hooks = st.hooks.dir.clone();
            for (name, _) in profile::hook_files(&st.hooks) {
                let _ = std::fs::remove_file(hooks.join(name));
            }
            let _ = std::fs::remove_dir(&hooks);
            let _ = std::fs::remove_dir(dir);
            thread::spawn(|| {
                thread::sleep(Duration::from_millis(200));
                std::process::exit(0);
            });
            Ok(json!({}))
        }
        "daemon.stop" => {
            // FR-46 "Keluar dan hentikan semua agent": balasan sempat terkirim, lalu keluar;
            // master PTY tertutup bersama proses = kernel mengirim SIGHUP ke semua sesi pane
            thread::spawn(|| {
                thread::sleep(Duration::from_millis(200));
                std::process::exit(0);
            });
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

/// Ubah status pane dan beri tahu semua UI (`pane.status`). `only_from` = ubah hanya bila status sekarang itu.
fn set_status(st: &State, id: u64, pane: &Pane, status: &'static str, only_from: Option<&str>) {
    let mut cur = pane.status.lock().unwrap();
    if cur.is_empty() || *cur == status || only_from.is_some_and(|f| *cur != f) {
        return;
    }
    *cur = status;
    drop(cur);
    let ev = line(json!({"event": "pane.status", "params": {"pane": id, "status": status}}));
    st.ui_txs.lock().unwrap().retain(|(_, tx)| tx.try_send(ev.clone()).is_ok());
}

/// File hook OpenCode/omp di folder config Gembala (SEC-26). Gagal tulis = agent itu tanpa hook, pane tetap jalan.
fn write_hook_files(h: &profile::Hooks) {
    for (name, body) in profile::hook_files(h) {
        if let Err(e) = config::write_atomic(&h.dir.join(name), body.as_bytes()) {
            eprintln!("gembala daemon: tulis hook {name}: {e}");
        }
    }
}

/// Baca ulang config; file rusak = nilai valid terakhir tetap, galat diperbarui. Semua UI diberi tahu.
fn reload_config(st: &State) {
    let path = config::path();
    let text = std::fs::read_to_string(&path).ok();
    let c = config::load(text.as_deref());
    let mut cur = st.config.lock().unwrap();
    let values = if c.broken && !cur.is_null() { cur["values"].clone() } else { c.values };
    if !c.broken {
        *st.profiles.lock().unwrap() = c.profiles;
    }
    *cur = json!({"values": values, "errors": c.errors, "path": path.to_string_lossy()});
    let ev = line(json!({"event": "config.changed", "params": *cur}));
    st.ui_txs.lock().unwrap().retain(|(_, tx)| tx.try_send(ev.clone()).is_ok());
}

/// Pantau `mtime` + ukuran tiap 1 detik (TECH bagian 5), tanpa dependency.
fn watch_config(st: Arc<State>) {
    let stamp = || std::fs::metadata(config::path()).ok().map(|m| (m.modified().ok(), m.len()));
    // None = belum pernah dibandingkan: pemeriksaan pertama selalu memuat ulang, supaya file yang ditulis
    // di antara pembacaan awal dan start watcher tidak terlewat
    let mut last = None;
    loop {
        thread::sleep(Duration::from_secs(1));
        let now = Some(stamp());
        if now != last {
            last = now;
            reload_config(&st);
        }
    }
}

/// FR-20: tanpa UI dan semua pane shell diam selama dua pemeriksaan berturut-turut = keluar.
fn idle_exit(st: Arc<State>) {
    let every = std::env::var("GEMBALA_IDLE_SECS").ok().and_then(|s| s.parse().ok()).unwrap_or(5);
    let mut strikes = 0;
    loop {
        thread::sleep(Duration::from_secs(every));
        let busy = st.panes.lock().unwrap().values().any(|p| pty::busy(&p.master, p.sid).is_some());
        strikes = if st.ui_clients.load(Ordering::Relaxed) == 0 && !busy { strikes + 1 } else { 0 };
        if strikes >= 2 {
            std::process::exit(0);
        }
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
        v.1.retain(|sub| match sub.tx.try_send(ev.clone()) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) => {
                let _ = sub.sock.shutdown(std::net::Shutdown::Both);
                false
            }
            Err(TrySendError::Disconnected(_)) => false,
        });
    }
    let code = pty.child.wait().ok().and_then(|s| s.code());
    st.panes.lock().unwrap().remove(&id);
    let ev = line(json!({"event": "pane.exit", "params": {"pane": id, "code": code}}));
    for sub in pane.view.lock().unwrap().1.drain(..) {
        let _ = sub.tx.try_send(ev.clone());
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
