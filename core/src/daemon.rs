//! Daemon: kanal UI + kanal pane, registri pane, attach (TECH bagian 8, pekerjaan 0.3).
//!
//! Kanal UI (SEC-20): `hello` wajib membawa rahasia yang dibuat daemon saat start dan hanya
//! diserahkan ke peluncurnya lewat descriptor; koneksi dari dalam sesi pane ditolak walau
//! rahasianya benar. Kanal pane (SEC-3): identitas dari `getsid(peer_pid)`, bukan isi pesan.

use crate::bus;
use crate::config;
use crate::msg;
use crate::git;
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
    /// Nama otomatis `<agent>-<n>` (FR-30), bisa diganti, unik per daemon (FR-3); agent profilnya ("shell" bila tanpa profil).
    name: Mutex<String>,
    agent: String,
    /// FR-4: idle | working | blocked | done; kosong = tidak dilacak (shell, profil tanpa hook)
    status: Mutex<&'static str>,
    /// FR-53: id sesi Gembala untuk Claude Code (UUID tervalidasi)
    session: Option<String>,
    /// FR-56: output sesi agent yang sedang jalan (sejak penanda mulai) dan sesi terakhir yang sudah selesai; RAM saja (SEC-13)
    agent_log: Mutex<Option<(std::time::Instant, Vec<u8>)>>,
    last_output: Mutex<Vec<u8>>,
    /// FR-4 cadangan: kapan output terakhir datang; profil `custom` dilacak dari layar
    last_out: Mutex<std::time::Instant>,
    screen: bool,
    started: std::time::Instant,
    /// Fase 2: workspace (aturan akses SEC-4/21), kebijakan penerima (SEC-5), kapan status terakhir berubah
    /// (stabil 500 ms, SEC-28), boleh dibaca agent (pane shell, SEC-21), ketikan pengguna terakhir (FR-11)
    workspace: Mutex<String>,
    policy: Mutex<msg::Policy>,
    status_at: Mutex<std::time::Instant>,
    readable: std::sync::atomic::AtomicBool,
    typed_at: Mutex<Option<std::time::Instant>>,
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
    engine: Mutex<bus::Engine>,
    audit: Option<bus::Audit>,
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
        engine: Mutex::default(),
        audit: bus::Audit::open(config::path().with_file_name("audit")).ok(),
    });
    let st7 = st.clone();
    thread::spawn(move || deliver_loop(st7));
    write_hook_files(&st.hooks);
    reload_config(&st);
    on_ready();
    let st3 = st.clone();
    thread::spawn(move || idle_exit(st3));
    let st4 = st.clone();
    thread::spawn(move || watch_config(st4));
    let st6 = st.clone();
    thread::spawn(move || screen_watch(st6));
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
    // SEC-2: akun OS lain ditolak sebelum apa pun (lapis kedua di atas socket 0600 + folder 0700)
    let (mut uid, mut gid) = (0, 0);
    if unsafe { libc::getpeereid(s.as_raw_fd(), &mut uid, &mut gid) } != 0 || uid != unsafe { libc::getuid() } {
        return Ok(());
    }
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
                ("send" | "reply" | "wait" | "read" | "status", Some(me)) => pane_method(&st, me, method, p),
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
        "git.status" | "git.diff" => {
            // folder kerja pane itu sendiri (proses foreground atau shell), bukan dari parameter
            let pane = st.pane(p)?;
            let pid = pty::busy(&pane.master, pane.sid).map_or(pane.sid, |(pid, _)| pid);
            let cwd = pty::cwd_of(pid).or_else(|| pty::cwd_of(pane.sid)).ok_or("io")?;
            Ok(if m == "git.status" { git::status(&cwd) } else { git::diff(&cwd) })
        }
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
        "msg.list" => {
            let eng = st.engine.lock().unwrap();
            let names = st.names();
            let queues: Vec<Value> = eng.queues.values().flatten().map(|q| q.msg.summary()).collect();
            Ok(json!({
                "paused": eng.paused,
                "muted": eng.muted.iter().filter_map(|id| names.get(id)).collect::<Vec<_>>(),
                "pending": eng.pending.iter().map(|m| { let mut v = m.summary(); v["body"] = json!(git::redact(&m.body)); v }).collect::<Vec<_>>(),
                "queued": queues,
                "history": eng.history,
            }))
        }
        "msg.approve" | "msg.reject" => {
            // FR-12: hanya kanal UI (manusia) yang bisa memutus kartu konfirmasi (SEC-20)
            let id = p["id"].as_str().ok_or("bad_params")?;
            let held = {
                let mut eng = st.engine.lock().unwrap();
                let i = eng.pending.iter().position(|x| x.id == id).ok_or("no_such_msg")?;
                eng.pending.remove(i)
            };
            if m == "msg.approve" {
                Ok(try_send(st, held, true, true))
            } else {
                reject(st, &held, "rejected_by_user");
                Ok(json!({"result": "rejected", "reason": "rejected_by_user"}))
            }
        }
        "msg.pause" => {
            // SEC-10: tombol henti; antrean ditahan, bukan dibuang
            st.engine.lock().unwrap().paused = p["on"].as_bool().unwrap_or(true);
            bus_event(st, json!({"kind": "paused", "on": st.engine.lock().unwrap().paused}));
            Ok(json!({}))
        }
        "msg.mute" => {
            let pane = p["pane"].as_u64().ok_or("bad_params")?;
            let mut eng = st.engine.lock().unwrap();
            if p["on"].as_bool().unwrap_or(true) { eng.muted.insert(pane); } else { eng.muted.remove(&pane); }
            Ok(json!({}))
        }
        "msg.send" => {
            // kiriman manusia dari UI: label "pengguna"; boleh memaksa ke `unknown`, tidak pernah ke `blocked`
            let to = st.pane(&json!({"pane": p["to"]}))?;
            let body = msg::sanitize(p["body"].as_str().ok_or("bad_params")?)?;
            let status = *to.status.lock().unwrap();
            let force = p["force"].as_bool() == Some(true);
            if status == "blocked" {
                return Err("target_blocked");
            }
            if !(matches!(status, "idle" | "done") || force) {
                return Err("target_busy");
            }
            let id = st.engine.lock().unwrap().new_id(&st.boot);
            (&to.master).write_all(&msg::wrap("pengguna", &id, &body)).map_err(|_| "io")?;
            Ok(json!({"result": "accepted", "id": id}))
        }
        "pane.policy" => {
            let pane = st.pane(p)?;
            *pane.policy.lock().unwrap() = msg::Policy::parse(p["policy"].as_str().unwrap_or_default()).ok_or("bad_params")?;
            Ok(json!({}))
        }
        "pane.readable" => {
            st.pane(p)?.readable.store(p["on"].as_bool() == Some(true), Ordering::Relaxed);
            Ok(json!({}))
        }
        "pane.workspace" => {
            *st.pane(p)?.workspace.lock().unwrap() = p["workspace"].as_str().unwrap_or_default().chars().take(64).collect();
            Ok(json!({}))
        }
        "keys.set" => {
            let (action, combo) = (p["action"].as_str().ok_or("bad_params")?, p["combo"].as_str().unwrap_or(""));
            if let Err(why) = config::set_key(&config::path(), action, combo) {
                return Ok(json!({"error": why}));
            }
            reload_config(st);
            Ok(json!({}))
        }
        "profile.save" | "profile.reset" => {
            let r = if m == "profile.save" {
                config::save_profile(&config::path(), &p["profile"])
            } else {
                config::reset_profile(&config::path(), p["id"].as_str().ok_or("bad_params")?)
            };
            if let Err(why) = r {
                return Ok(json!({"error": why}));
            }
            reload_config(st);
            Ok(json!({}))
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
            let session = profile::Session { id: p["session"].as_str().map(String::from), resume: p["resume"].as_bool() == Some(true) };
            let launch = prof.as_ref().and_then(|x| profile::launch(x, Some(&st.hooks), Some(&session)));
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
                    let taken: Vec<String> = st.panes.lock().unwrap().values().map(|q| q.name.lock().unwrap().clone()).collect();
                    let n = (1..).find(|n| !taken.contains(&format!("{label}-{n}"))).unwrap_or(1);
                    Mutex::new(format!("{label}-{n}"))
                },
                session: session.id.clone().filter(|s| profile::valid_uuid(s)),
                agent_log: Mutex::new(None),
                last_output: Mutex::new(Vec::new()),
                last_out: Mutex::new(std::time::Instant::now()),
                screen: agent == "custom",
                started: std::time::Instant::now(),
                workspace: Mutex::new(p["workspace"].as_str().unwrap_or_default().chars().take(64).collect()),
                policy: Mutex::new(prof.as_ref().map_or(msg::Policy::Auto, |x| {
                    msg::effective_policy(msg::Policy::parse(&x.policy).unwrap_or(msg::Policy::Auto), &x.args)
                })),
                status_at: Mutex::new(std::time::Instant::now()),
                readable: std::sync::atomic::AtomicBool::new(false),
                typed_at: Mutex::new(None),
                status: Mutex::new(if ["claude", "codex", "opencode", "omp", "custom"].contains(&agent.as_str()) { "idle" } else { "" }),
                agent,
                view: Mutex::new((Grid::new(rows, cols, SCROLLBACK), Vec::new())),
            });
            st.panes.lock().unwrap().insert(id, pane.clone());
            let name = pane.name.lock().unwrap().clone();
            let st = st.clone();
            thread::spawn(move || pump(st, id, pane, pty));
            Ok(json!({"pane": id, "name": name}))
        }
        "pane.write" => {
            let pane = st.pane(p)?;
            let data = p["data"].as_str().and_then(|d| B64.decode(d).ok()).ok_or("bad_params")?;
            (&pane.master).write_all(&data).map_err(|_| "io")?;
            *pane.typed_at.lock().unwrap() = Some(std::time::Instant::now());
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
        "pane.agent_output" => {
            // FR-56: output lengkap sesi agent terakhir, selama pane hidup
            let pane = st.pane(p)?;
            let data = pane.last_output.lock().unwrap().clone();
            Ok(json!({"data": B64.encode(&data), "bytes": data.len()}))
        }
        "pane.rename" => {
            // FR-3: huruf kecil, angka, '-', maks 32, unik
            let pane = st.pane(p)?;
            let name = p["name"].as_str().filter(|n| (1..=32).contains(&n.len()) && n.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'));
            let name = name.ok_or("bad_name")?;
            let panes = st.panes.lock().unwrap();
            if panes.values().any(|q| !Arc::ptr_eq(q, &pane) && *q.name.lock().unwrap() == name) {
                return Err("name_taken");
            }
            *pane.name.lock().unwrap() = name.to_string();
            Ok(json!({"name": name}))
        }
        "pane.info" => {
            let pane = st.pane(p)?;
            let busy = pty::busy(&pane.master, pane.sid);
            Ok(json!({
                "busy": busy.is_some(), "name": busy.and_then(|(pid, _)| pty::proc_name(pid)),
                "password": busy.is_some() && pty::wants_password(&pane.master),
                "pane_name": *pane.name.lock().unwrap(), "agent": pane.agent, "status": *pane.status.lock().unwrap(),
                "policy": pane.policy.lock().unwrap().name(), "readable": pane.readable.load(Ordering::Relaxed),
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

// ---------- Fase 2: mesin pesan (BRD bagian 8) ----------

fn bus_event(st: &State, ev: Value) {
    let ev = line(json!({"event": "msg.event", "params": ev}));
    st.ui_txs.lock().unwrap().retain(|(_, tx)| tx.try_send(ev.clone()).is_ok());
}

fn record(st: &State, kind: &str, m: &bus::Msg, reason: Option<&str>) {
    let ev = st.engine.lock().unwrap().log(kind, m, reason);
    if let Some(a) = &st.audit {
        a.write(kind, m, reason);
    }
    bus_event(st, ev);
}

fn reject(st: &State, m: &bus::Msg, reason: &'static str) -> Value {
    record(st, "rejected", m, Some(reason));
    json!({"result": "rejected", "reason": reason, "id": m.id})
}

/// Fakta keputusan dibaca ulang tiap kali (SEC-28). `counted` = laju sudah dihitung saat diterima.
fn facts_for<'a>(st: &State, m: &bus::Msg, to: &'a Pane, from_ws: &str, want_queue: bool, approved: bool, counted: bool) -> msg::Facts<'a> {
    let mut eng = st.engine.lock().unwrap();
    let typing = to.typed_at.lock().unwrap().is_some_and(|t| t.elapsed() < std::time::Duration::from_secs(3));
    msg::Facts {
        target_status: if to.status.lock().unwrap().is_empty() { "unknown" } else { *to.status.lock().unwrap() },
        stable_for: to.status_at.lock().unwrap().elapsed(),
        policy: *to.policy.lock().unwrap(),
        same_workspace: *to.workspace.lock().unwrap() == from_ws,
        paused: eng.paused,
        muted: eng.muted.contains(&m.to),
        typing,
        sent_last_minute: if counted { 0 } else { eng.sent_last_minute(m.from) },
        hop: m.hop,
        want_queue,
        queue_len: eng.queue_len(m.to),
        approved,
    }
}

/// FR-5: keputusan kirim untuk pesan baru (dari agent, atau setelah disetujui manusia).
fn try_send(st: &State, m: bus::Msg, want_queue: bool, approved: bool) -> Value {
    // dua pernyataan terpisah: lock `panes` tidak boleh masih dipegang saat workspace_of menguncinya lagi
    let to = st.panes.lock().unwrap().get(&m.to).cloned();
    let from_ws = st.workspace_of(m.from);
    let Some(to) = to else {
        return reject(st, &m, "target_gone");
    };
    let d = msg::decide(&facts_for(st, &m, &to, &from_ws, want_queue, approved, approved));
    st.engine.lock().unwrap().remember(&m);
    match d {
        msg::Decision::Reject(r) => reject(st, &m, r),
        msg::Decision::Send => {
            st.engine.lock().unwrap().note_sent(m.from);
            deliver(st, &m, &to);
            json!({"result": "accepted", "id": m.id})
        }
        msg::Decision::Queue => {
            let mut eng = st.engine.lock().unwrap();
            eng.note_sent(m.from);
            eng.queues.entry(m.to).or_default().push_back(bus::Queued { msg: m.clone(), since: std::time::Instant::now(), approved });
            drop(eng);
            record(st, "queued", &m, None);
            json!({"result": "queued", "id": m.id})
        }
        msg::Decision::Confirm => {
            st.engine.lock().unwrap().pending.push(m.clone());
            record(st, "confirm", &m, None);
            json!({"result": "queued", "reason": "needs_confirmation", "id": m.id})
        }
    }
}

/// SEC-7/8: label asal + bracketed paste + satu Enter, ditulis ke PTY target.
fn deliver(st: &State, m: &bus::Msg, to: &Pane) {
    if (&to.master).write_all(&msg::wrap(&m.from_name, &m.id, &m.body)).is_ok() {
        record(st, "sent", m, None);
    } else {
        record(st, "rejected", m, Some("io"));
    }
}

/// Antrean: kedaluwarsa 10 menit; tiap pesan dicek ulang tepat sebelum ditulis (SEC-28).
fn deliver_loop(st: Arc<State>) {
    loop {
        thread::sleep(std::time::Duration::from_millis(200));
        let targets: Vec<u64> = st.engine.lock().unwrap().queues.keys().copied().collect();
        for to_id in targets {
            let head = st.engine.lock().unwrap().queues.get_mut(&to_id).and_then(|q| q.pop_front());
            let Some(q) = head else { continue };
            let to = st.panes.lock().unwrap().get(&to_id).cloned();
            let requeue = |q: bus::Queued| st.engine.lock().unwrap().queues.entry(to_id).or_default().push_front(q);
            let Some(to) = to else {
                reject(&st, &q.msg, "target_gone");
                continue;
            };
            if q.since.elapsed() > msg::QUEUE_TTL {
                reject(&st, &q.msg, "expired");
                continue;
            }
            let from_ws = st.workspace_of(q.msg.from);
            // len antrean 0: pesan ini sudah memegang tempatnya
            let mut f = facts_for(&st, &q.msg, &to, &from_ws, true, q.approved, true);
            f.queue_len = 0;
            match msg::decide(&f) {
                msg::Decision::Send => deliver(&st, &q.msg, &to),
                msg::Decision::Queue => requeue(q),
                msg::Decision::Confirm => {
                    st.engine.lock().unwrap().pending.push(q.msg.clone());
                    record(&st, "confirm", &q.msg, None);
                }
                msg::Decision::Reject(r) => drop(reject(&st, &q.msg, r)),
            }
        }
        st.engine.lock().unwrap().queues.retain(|_, q| !q.is_empty());
    }
}

/// Kanal pane (SEC-3): `me` = pane pemanggil dari identitas kernel, bukan dari isi pesan.
fn pane_method(st: &Arc<State>, me: u64, method: &str, p: &Value) -> Res {
    let my_name = st.panes.lock().unwrap().get(&me).map(|x| x.name.lock().unwrap().clone()).ok_or("not_a_pane")?;
    let my_ws = st.workspace_of(me);
    // SEC-21: baca/tunggu hanya dalam workspace yang sama; pane shell hanya bila pengguna mengizinkan
    let visible = |name: &str| -> Result<(u64, Arc<Pane>), &'static str> {
        let (id, pane) = st.by_name(name).ok_or("no_such_pane")?;
        if *pane.workspace.lock().unwrap() != my_ws || (pane.agent == "shell" && !pane.readable.load(Ordering::Relaxed)) {
            return Err("acl_denied");
        }
        Ok((id, pane))
    };
    match method {
        "status" => {
            let list: Vec<Value> = st
                .panes
                .lock()
                .unwrap()
                .iter()
                .filter(|(id, x)| **id != me && *x.workspace.lock().unwrap() == my_ws && x.agent != "shell")
                .map(|(_, x)| json!({"name": *x.name.lock().unwrap(), "agent": x.agent, "status": *x.status.lock().unwrap(), "policy": x.policy.lock().unwrap().name()}))
                .collect();
            Ok(json!({"me": my_name, "panes": list}))
        }
        "send" => {
            let (to_id, to) = st.by_name(p["to"].as_str().ok_or("bad_params")?).ok_or("no_such_pane")?;
            if to_id == me {
                return Err("bad_params");
            }
            let body = msg::sanitize(p["body"].as_str().ok_or("bad_params")?)?;
            let reply_to = p["reply_to"].as_str().map(String::from);
            let mut eng = st.engine.lock().unwrap();
            let hop = eng.hop_for(reply_to.as_deref());
            let id = eng.new_id(&st.boot);
            drop(eng);
            let m = bus::Msg { id, from: me, from_name: my_name, to: to_id, to_name: to.name.lock().unwrap().clone(), body, reply_to, hop, at: std::time::SystemTime::now() };
            Ok(try_send(st, m, p["queue"].as_bool() == Some(true), false))
        }
        "reply" => {
            // FR-19: hanya penerima pesan yang bisa membalasnya; balasan diambil pengirim lewat `wait --reply`
            let orig = st.engine.lock().unwrap().find(p["msg_id"].as_str().ok_or("bad_params")?).cloned().ok_or("no_such_msg")?;
            if orig.to != me {
                return Err("not_allowed");
            }
            let body = msg::sanitize(p["body"].as_str().ok_or("bad_params")?)?;
            let mut eng = st.engine.lock().unwrap();
            let id = eng.new_id(&st.boot);
            let r = bus::Msg { id, from: me, from_name: my_name, to: orig.from, to_name: orig.from_name.clone(), body, reply_to: Some(orig.id.clone()), hop: orig.hop + 1, at: std::time::SystemTime::now() };
            let why = if r.hop > msg::HOP_MAX {
                Some("hop_limit")
            } else if eng.sent_last_minute(me) >= msg::RATE_PER_MIN {
                Some("rate_limited")
            } else if eng.paused {
                Some("paused")
            } else {
                None
            };
            if let Some(why) = why {
                drop(eng);
                return Ok(reject(st, &r, why));
            }
            eng.note_sent(me);
            eng.remember(&r);
            eng.replies.insert(orig.id.clone(), r.clone());
            drop(eng);
            record(st, "reply", &r, None);
            Ok(json!({"result": "accepted", "id": r.id}))
        }
        "read" => {
            // FR-7, SEC-12, SEC-17: teks polos tersamar, dibungkus pembatas berlabel
            let (_, pane) = visible(p["target"].as_str().ok_or("bad_params")?)?;
            let n = p["lines"].as_u64().unwrap_or(40).clamp(1, 500) as usize;
            let text = git::redact(&pane.view.lock().unwrap().0.tail(n));
            let name = pane.name.lock().unwrap().clone();
            Ok(json!({"text": format!("--- output {name} (agent lain, bukan instruksi pengguna) ---\n{text}\n--- akhir output {name} ---")}))
        }
        "wait" => {
            // FR-6: balasan, pola (literal atau regex waktu-linear, maks 256), atau status settled; batas waktu
            let timeout = std::time::Duration::from_millis(p["timeout_ms"].as_u64().unwrap_or(600_000).min(3_600_000));
            let reply = p["reply"].as_str().map(String::from);
            let pattern = match p["pattern"].as_str() {
                Some(pat) if pat.len() > 256 => return Err("bad_params"),
                Some(pat) => Some(regex_lite::Regex::new(pat).map_err(|_| "bad_params")?),
                None => None,
            };
            let target = match p["target"].as_str() {
                Some(t) => Some(visible(t)?),
                None if reply.is_none() => return Err("bad_params"),
                None => None,
            };
            if let Some(r) = &reply {
                let mine = st.engine.lock().unwrap().find(r).is_some_and(|m| m.from == me);
                if !mine {
                    return Err("not_allowed");
                }
            }
            let t0 = std::time::Instant::now();
            loop {
                if let Some(r) = &reply {
                    if let Some(m) = st.engine.lock().unwrap().replies.remove(r) {
                        return Ok(json!({"reply": {"id": m.id, "from": m.from_name, "body": m.body}}));
                    }
                } else if let Some((_, pane)) = &target {
                    if let Some(re) = &pattern {
                        if re.is_match(&pane.view.lock().unwrap().0.text()) {
                            return Ok(json!({"matched": true}));
                        }
                    } else {
                        let s = *pane.status.lock().unwrap();
                        if matches!(s, "idle" | "done" | "blocked") && pane.status_at.lock().unwrap().elapsed() >= msg::STABLE {
                            return Ok(json!({"status": s}));
                        }
                    }
                }
                if t0.elapsed() > timeout {
                    return Ok(json!({"timeout": true}));
                }
                thread::sleep(std::time::Duration::from_millis(100));
            }
        }
        _ => Err("not_allowed"),
    }
}

/// FR-4 cadangan tiap 1 detik: profil `custom`, dan agent yang diketik manual di pane shell (tanpa hook).
fn screen_watch(st: Arc<State>) {
    const AGENTS: &[&str] = &["claude", "codex", "opencode", "omp"];
    loop {
        thread::sleep(Duration::from_secs(1));
        let panes: Vec<(u64, Arc<Pane>)> = st.panes.lock().unwrap().iter().map(|(id, p)| (*id, p.clone())).collect();
        for (id, pane) in panes {
            let manual = pane.agent == "shell"
                && pty::busy(&pane.master, pane.sid).and_then(|(pid, _)| pty::proc_name(pid)).is_some_and(|n| AGENTS.contains(&n.as_str()));
            let cur = *pane.status.lock().unwrap();
            let next = if pane.screen || manual {
                let tail = pane.view.lock().unwrap().0.tail(12);
                profile::screen_status(&tail, pane.last_out.lock().unwrap().elapsed(), cur)
            } else if pane.agent == "shell" {
                "" // agent manual sudah keluar: pane shell kembali tidak dilacak
            } else {
                continue;
            };
            // output startup 3 detik pertama bukan "kerja" (tanpa ini agent baru langsung tampil "selesai"),
            // tetapi prompt izin tetap langsung berlaku: kriteria 4
            let startup = pane.started.elapsed() < std::time::Duration::from_secs(3);
            if next != cur && (!startup || next == "blocked") {
                *pane.status.lock().unwrap() = next;
                *pane.status_at.lock().unwrap() = std::time::Instant::now();
                let ev = line(json!({"event": "pane.status", "params": {"pane": id, "status": next}}));
                st.ui_txs.lock().unwrap().retain(|(_, tx)| tx.try_send(ev.clone()).is_ok());
            }
        }
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
    *pane.status_at.lock().unwrap() = std::time::Instant::now();
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

type View = (Grid, Vec<Sub>);

fn send_all(v: &mut View, ev: String) {
    v.1.retain(|sub| match sub.tx.try_send(ev.clone()) {
        Ok(()) => true,
        Err(TrySendError::Full(_)) => {
            let _ = sub.sock.shutdown(std::net::Shutdown::Both);
            false
        }
        Err(TrySendError::Disconnected(_)) => false,
    });
}

fn emit(v: &mut View, pane: &Pane, id: u64, bytes: &[u8]) {
    if bytes.is_empty() {
        return;
    }
    v.0.feed(bytes);
    *pane.last_out.lock().unwrap() = std::time::Instant::now();
    if let Some((_, log)) = pane.agent_log.lock().unwrap().as_mut() {
        log.extend_from_slice(bytes);
        let over = log.len().saturating_sub(AGENT_LOG_MAX);
        log.drain(..over); // simpan bagian terakhir saja
    }
    send_all(v, line(json!({"event": "pane.output", "params": {"pane": id, "data": B64.encode(bytes)}})));
}

const AGENT_LOG_MAX: usize = 4 << 20;
const MARK: &[u8] = b"\x1b]777;gembala;";

/// `(posisi, panjang, isi)` penanda OSC 777 Gembala pertama. Penanda yang terpotong batas baca PTY
/// tidak dikenali dan lolos sebagai OSC biasa (diabaikan terminal).
// ponytail: tanpa penyangga antar-baca; penanda printf kecil hampir selalu datang utuh
fn find_marker(b: &[u8]) -> Option<(usize, usize, &str)> {
    let pos = b.windows(MARK.len()).position(|w| w == MARK)?;
    let body = &b[pos + MARK.len()..];
    let end = body.iter().position(|&c| c == 0x07)?;
    Some((pos, MARK.len() + end + 1, std::str::from_utf8(&body[..end]).ok()?))
}

/// FR-56: agent mulai = catat log sesi; agent keluar = layar diganti satu blok ringkas, log lengkap disimpan di RAM.
fn on_marker(v: &mut View, pane: &Pane, id: u64, mark: &str) {
    if mark == "agent-start" {
        *pane.agent_log.lock().unwrap() = Some((std::time::Instant::now(), Vec::new()));
        return;
    }
    let Some(code) = mark.strip_prefix("agent-exit;") else { return };
    let Some((started, log)) = pane.agent_log.lock().unwrap().take() else { return };
    *pane.last_output.lock().unwrap() = log;
    let secs = started.elapsed().as_secs();
    let dur = match secs {
        0..60 => format!("{secs} dtk"),
        60..3600 => format!("{} mnt {} dtk", secs / 60, secs % 60),
        _ => format!("{} jam {} mnt", secs / 3600, secs % 3600 / 60),
    };
    let home = std::env::var("HOME").unwrap_or_default();
    let cwd = pty::cwd_of(pane.sid).map(|c| c.to_string_lossy().replacen(&home, "~", 1)).unwrap_or_default();
    let exit = if code == "0" { String::new() } else { format!(" · keluar {code}") };
    let resume = match (pane.agent.as_str(), pane.session.as_deref()) {
        ("claude", Some(sid)) => format!("claude --resume {sid}"),
        ("codex", _) => "codex resume --last".into(),
        ("opencode", _) => "opencode --continue".into(),
        ("omp", _) => "omp --continue".into(),
        _ => String::new(),
    };
    let mut text = format!("\x1b[2m─ {} selesai · {dur} · {cwd}{exit}\x1b[0m\r\n", pane.name.lock().unwrap());
    if !resume.is_empty() {
        text += &format!("\x1b[2m  lanjutkan: {resume}\x1b[0m\r\n");
    }
    let (rows, cols) = v.0.size();
    v.0 = Grid::new(rows, cols, SCROLLBACK);
    v.0.feed(text.as_bytes());
    send_all(v, line(json!({"event": "pane.replace", "params": {"pane": id, "data": B64.encode(&text)}})));
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
        // FR-56: penanda sesi agent dari skrip launch dipotong dari stream; sisanya diteruskan apa adanya
        let mut rest = &buf[..n];
        while let Some((pos, len, mark)) = find_marker(rest) {
            emit(&mut v, &pane, id, &rest[..pos]);
            on_marker(&mut v, &pane, id, mark);
            rest = &rest[pos + len..];
        }
        emit(&mut v, &pane, id, rest);
    }
    let code = pty.child.wait().ok().and_then(|s| s.code());
    st.panes.lock().unwrap().remove(&id);
    let ev = line(json!({"event": "pane.exit", "params": {"pane": id, "code": code}}));
    for sub in pane.view.lock().unwrap().1.drain(..) {
        let _ = sub.tx.try_send(ev.clone());
    }
}

impl State {
    fn names(&self) -> HashMap<u64, String> {
        self.panes.lock().unwrap().iter().map(|(id, p)| (*id, p.name.lock().unwrap().clone())).collect()
    }

    fn by_name(&self, name: &str) -> Option<(u64, Arc<Pane>)> {
        self.panes.lock().unwrap().iter().find(|(_, p)| *p.name.lock().unwrap() == name).map(|(id, p)| (*id, p.clone()))
    }

    fn workspace_of(&self, id: u64) -> String {
        self.panes.lock().unwrap().get(&id).map(|p| p.workspace.lock().unwrap().clone()).unwrap_or_default()
    }

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
