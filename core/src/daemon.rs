//! Daemon: kanal UI + kanal pane, registri pane, attach (TECH bagian 8, pekerjaan 0.3).
//!
//! Kanal UI (SEC-20): `hello` wajib membawa rahasia yang dibuat daemon saat start dan hanya
//! diserahkan ke peluncurnya lewat descriptor; koneksi dari dalam sesi pane ditolak walau
//! rahasianya benar. Kanal pane (SEC-3): identitas dari `getsid(peer_pid)`, bukan isi pesan.

use crate::account;
use crate::addon;
use crate::bus;
use crate::config;
use crate::msg;
use crate::git;
use crate::profile;
use crate::grid::Grid;
use crate::history;
use crate::snap;
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
/// FR-79: nama lama per pane yang tetap jadi alamat
const ALIAS_MAX: usize = 8;

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
    /// FR-79: nama lama pane ini (yang tertua di depan, maks `ALIAS_MAX`), tetap jadi alamat `send`/`wait`/`read`
    aliases: Mutex<Vec<String>>,
    agent: String,
    /// FR-4: idle | working | blocked | done; kosong = tidak dilacak (shell, profil tanpa hook)
    status: Mutex<&'static str>,
    /// FR-53, FR-68: id sesi agent di pane ini (tervalidasi per agent): dari Gembala (Claude profil), argumen
    /// `--session-id` agent manual, atau hook (Codex `thread-id`, OpenCode `sessionID`). Untuk lanjut sesi persis.
    session: Mutex<Option<String>>,
    /// FR-56: output sesi agent yang sedang jalan (sejak penanda mulai) dan sesi terakhir yang sudah selesai; RAM saja (SEC-13)
    agent_log: Mutex<Option<(std::time::Instant, Vec<u8>)>>,
    last_output: Mutex<Vec<u8>>,
    /// FR-56: posisi baris perintah terakhir (penanda OSC 133 C) dan isi pane sebelum agent jalan; blok ringkas
    /// ditulis di bawahnya saat agent keluar, riwayat perintah sebelumnya tetap (ala Warp). Urutan kunci: `view` dulu.
    cmd_at: Mutex<(usize, u16)>,
    /// FR-56: isi pane sebelum perintah terakhir, disalin saat perintah mulai (OSC 133 C). Agent manual baru
    /// terdeteksi ±1 detik kemudian, saat itu Claude sudah membersihkan layar dan scrollback
    pre_cmd: Mutex<Vec<u8>>,
    pre_agent: Mutex<Vec<u8>>,
    /// FR-62: file riwayat blok pane ini; blok yang sedang jalan (sejak OSC 133 C) dan blok agent manual yang
    /// sudah selesai tapi menunggu perintah lanjutnya dari `agent_done`.
    hist: std::path::PathBuf,
    /// FR-69: judul terminal terakhir (OSC 0/2) yang dipasang program di pane; RAM saja (SEC-13)
    title: Mutex<String>,
    /// FR-70: potretan per giliran agent (antrean `snap::queue`)
    turns: Arc<Mutex<snap::Turns>>,
    /// FR-61: id akun agent pane ini; kosong = Utama. Agent yang diketik: dari `CLAUDE_CONFIG_DIR` prosesnya
    account: Mutex<String>,
    /// FR-61: akun pemilik sesi Claude yang sedang jalan bila beda dari akun pane ("" = Utama); dari hook SessionStart
    owner: Mutex<Option<String>>,
    block: Mutex<OpenBlocks>,
    /// FR-56: agent yang diketik manual di pane shell (nama proses, perintah lengkap, folder kerja agent).
    /// Urutan kunci: `view` lalu `manual`.
    manual: Mutex<Option<(String, String, Option<std::path::PathBuf>)>>,
    /// FR-4 cadangan: kapan output terakhir datang; profil `custom` dilacak dari layar
    last_out: Mutex<std::time::Instant>,
    /// FR-59: shell sedang di prompt (penanda OSC 133 B terakhir, belum C); ikut di attach karena snapshot tanpa OSC
    at_prompt: std::sync::atomic::AtomicBool,
    /// FR-4: agent manual ini mengirim hook lengkap (Claude, omp, OpenCode; bukan Codex yang hanya `notify`):
    /// status dari hook saja. Tanpa ini redraw status line Claude terbaca bekerja lalu selesai berulang (notif menumpuk)
    hooked: std::sync::atomic::AtomicBool,
    screen: bool,
    started: std::time::Instant,
    /// Fase 2: workspace (aturan akses SEC-4/21), kebijakan penerima (SEC-5), kapan status terakhir berubah
    /// (stabil 500 ms, SEC-28), boleh dibaca agent (pane shell, SEC-21), ketikan pengguna terakhir (FR-11)
    workspace: Mutex<String>,
    policy: Mutex<msg::Policy>,
    status_at: Mutex<std::time::Instant>,
    readable: std::sync::atomic::AtomicBool,
    typed_at: Mutex<Option<std::time::Instant>>,
    /// SEC-3 cadangan: token 128-bit untuk proses yang lepas dari sesi pane (mis. server tmux, profil `token = true`)
    /// atau yang tidak bisa memakai socket (FR-66: Codex bersandbox, dibuat lewat `gembala token`)
    token: Mutex<Option<String>>,
    /// Grid dan pelanggan dikunci bersama: attach tidak kehilangan atau menggandakan output.
    view: Mutex<(Grid, Vec<Sub>)>,
}

impl Pane {
    /// Agent dari profil, atau agent yang diketik manual di pane shell (FR-56): terlihat di `status`, boleh dikirimi
    /// dan dibaca seperti agent lain. Pane shell biasa tetap tunduk SEC-21.
    fn agent_name(&self) -> Option<String> {
        if self.agent != "shell" {
            return Some(self.agent.clone());
        }
        self.manual.lock().unwrap().as_ref().map(|m| m.0.clone())
    }
}

struct Sub {
    tx: SyncSender<String>,
    sock: UnixStream,
    /// id koneksi: satu langganan per koneksi per pane (attach ulang tidak menggandakan event)
    conn: u64,
}

static NEXT_CONN: AtomicU64 = AtomicU64::new(1);

struct State {
    panes: Mutex<HashMap<u64, Arc<Pane>>>,
    next: AtomicU64,
    secret: Option<String>,
    /// SEC-20: requirement tanda tangan UI (dari sertifikat daemon sendiri); `None` = hanya rahasia
    ui_req: Option<String>,
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
    /// FR-61: `[[account]]` dari config
    accounts: Mutex<Vec<account::Account>>,
    /// FR-78: add-ons from config
    addons: Mutex<addon::Addons>,
    /// Hasil deteksi agent terakhir (FR-28); Null = belum pernah.
    agents: Mutex<Value>,
    /// Id acak per run daemon: id pane dari run lain tidak boleh dianggap pane yang sama (FR-45).
    boot: String,
    hooks: profile::Hooks,
    engine: Mutex<bus::Engine>,
    /// FR-8: langganan event status dari kanal pane (pelanggan, antreannya), disaring SEC-21 saat kirim
    pane_subs: Mutex<Vec<(u64, SyncSender<String>)>>,
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
/// FR-62: (blok yang sedang jalan + waktu mulainya, blok agent manual yang menunggu perintah lanjut)
type OpenBlocks = (Option<(history::Block, std::time::Instant)>, Option<history::Block>);

pub fn run(dir: &Path, secret: Option<String>, on_ready: impl FnOnce()) -> io::Result<()> {
    let ui = bind(&crate::rundir::socket_path(dir, "ui.sock")?)?;
    let pane_path = crate::rundir::socket_path(dir, "pane.sock")?;
    let pn = bind(&pane_path)?;
    let st = Arc::new(State {
        panes: Mutex::default(),
        next: AtomicU64::new(1),
        secret,
        ui_req: crate::sign::ui_requirement(),
        pane_sock: pane_path.to_string_lossy().into_owned(),
        ui_clients: AtomicUsize::new(0),
        ui_txs: Mutex::default(),
        ui_next: AtomicU64::new(1),
        config: Mutex::new(Value::Null),
        profiles: Mutex::new(profile::builtin()),
        accounts: Mutex::default(),
        addons: Mutex::default(),
        agents: Mutex::new(Value::Null),
        boot: new_secret()?[..16].to_string(),
        hooks: profile::Hooks {
            exe: std::env::current_exe()?.to_string_lossy().into_owned(),
            dir: config::path().with_file_name("hooks"),
        },
        engine: Mutex::default(),
        pane_subs: Mutex::default(),
        audit: bus::Audit::open(config::path().with_file_name("audit")).ok(),
    });
    let st7 = st.clone();
    thread::spawn(move || deliver_loop(st7));
    write_hook_files(&st.hooks);
    reload_config(&st);
    on_ready();
    let st3 = st.clone();
    thread::spawn(move || idle_exit(st3));
    let st9 = st.clone();
    thread::spawn(move || save_logs_loop(st9)); // FR-62: log layar ke disk, riwayat lebih dari 14 hari dibuang
    let (st8, run_dir) = (st.clone(), dir.to_path_buf());
    thread::spawn(move || spool(st8, run_dir));
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
    // SEC-20: UI bertanda tangan sama dengan daemon dikenali dari audit token saat connect, tanpa rahasia
    let signed = chan == Chan::Ui && st.ui_req.as_deref().is_some_and(|r| crate::sign::peer_ok(&s, r));
    let (tx, rx) = sync_channel::<String>(QUEUE);
    let conn = NEXT_CONN.fetch_add(1, Ordering::Relaxed);
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
    // kanal pane: identitas ditetapkan sekali saat hello (getsid atau token), bukan dari isi pesan
    let mut ident: Option<u64> = None;
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
            let r = if method == "hello" { hello(&st, chan, peer, signed, p) } else { Err("hello_required") };
            ready = r.is_ok();
            ident = r.as_ref().ok().and_then(|v| v["pane"].as_u64());
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
            // identitas dari hello (getsid atau token), bukan dari isi pesan (SEC-3); pane yang sudah ditutup = tidak berlaku
            let me = ident.filter(|id| st.panes.lock().unwrap().contains_key(id));
            let r = match (method, me) {
                ("subscribe", Some(me)) => {
                    st.pane_subs.lock().unwrap().push((me, tx.clone()));
                    Ok(json!({}))
                }
                ("send" | "reply" | "wait" | "read" | "status" | "token" | "addons", Some(me)) => pane_method(&st, me, method, p),
                // "start" = SessionStart Claude: id sesi, pemiliknya, dan status diam (kecuali compact di tengah giliran)
                ("hook", Some(me)) => match p["status"].as_str().and_then(|x| ["idle", "working", "blocked", "done", "start"].into_iter().find(|s| *s == x)) {
                    Some(status) => {
                        // FR-66: pesan terakhir giliran agent bersandbox = balasan untuk pesan yang menunggu
                        if let Some(text) = p["reply"].as_str() {
                            auto_reply(&st, me, text);
                        }
                        let pane = st.panes.lock().unwrap().get(&me).cloned();
                        if let Some(pane) = pane {
                            // FR-68: id sesi dari hook agent (Codex thread-id, OpenCode sessionID, Claude session_id)
                            if let (Some(agent), Some(id)) = (p["agent"].as_str(), p["session"].as_str())
                                && profile::valid_session(agent, id)
                            {
                                // FR-61: sesi baru atau ganti sesi (pemilih `claude -r`) = cek akun pemiliknya
                                if agent == "claude" && (status == "start" || pane.session.lock().unwrap().as_deref() != Some(id)) {
                                    let cur = acct_of_dir(&st, p["dir"].as_str().map(String::from));
                                    let new = status == "start" && matches!(p["source"].as_str(), Some("startup" | "clear"));
                                    let owner = account::session_owner(&config::path().with_file_name("sessions.json"), id, &cur, new);
                                    let known = owner.is_empty() || st.accounts.lock().unwrap().iter().any(|a| a.id == owner && a.agent == "claude");
                                    *pane.owner.lock().unwrap() = (known && owner != cur).then_some(owner);
                                }
                                *pane.session.lock().unwrap() = Some(id.to_string());
                            }
                            if p["agent"].as_str().is_some_and(|a| a != "codex") {
                                pane.hooked.store(true, Ordering::Relaxed);
                            }
                            if status != "start" {
                                set_status(&st, me, &pane, status, None);
                            } else if p["source"].as_str() != Some("compact") {
                                // SessionStart = agent menunggu prompt; tebakan layar sebelum hook pertama (spinner
                                // startup `claude` diketik = "bekerja") tidak lagi diperbaiki layar setelah pane ber-hook
                                set_status(&st, me, &pane, "idle", None);
                            }
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
                    let prompt = pane.at_prompt.load(Ordering::Relaxed);
                    // FR-2: raw history per size (UI writes each at its size, then resizes), snapshot = tail at rows/cols
                    let (segs, tail) = v.0.replay();
                    let segs: Vec<_> = segs.iter().map(|s| json!({"rows": s.rows, "cols": s.cols, "data": B64.encode(&s.data)})).collect();
                    reply(&tx, id, Ok(json!({"segments": segs, "snapshot": B64.encode(tail), "rows": rows, "cols": cols, "prompt": prompt, "sync": true})));
                    // penanda urutan di antrean yang sama: event pane sebelum ini sudah ada di snapshot, sesudahnya belum.
                    // UI (balasan dan event lewat jalur berbeda di Tauri) membuang event sebelum penanda; tanpa ini event
                    // yang ditahan selama attach ditulis dua kali dan gambar ulang TUI (kursor relatif) jadi berantakan
                    let _ = tx.try_send(line(json!({"event": "pane.attached", "params": {"pane": p["pane"]}})));
                    // attach ulang dari koneksi yang sama (tab pindah workspace) mengganti langganan lama, bukan menambah
                    v.1.retain(|s| s.conn != conn);
                    if let Ok(sock) = sock.try_clone() {
                        v.1.push(Sub { tx: tx.clone(), sock, conn });
                    }
                }
                Err(e) => reply(&tx, id, Err(e)),
            }
            continue;
        }
        reply(&tx, id, ui_method(&st, method, p));
    }
}

fn hello(st: &State, chan: Chan, peer: i32, signed: bool, p: &Value) -> Res {
    if p["proto"].as_u64() != Some(PROTO) {
        return Err("version_mismatch");
    }
    let mut pane = st.pane_of_pid(peer);
    if chan == Chan::Pane && pane.is_none() {
        // SEC-3 cadangan: proses di luar sesi pane membuktikan diri dengan token pane (dicabut saat pane ditutup)
        pane = p["token"].as_str().and_then(|t| {
            st.pane_of_token(t)
        });
    }
    match chan {
        Chan::Ui => {
            let ok = signed
                || match (&st.secret, p["secret"].as_str()) {
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
    Ok(json!({"proto": PROTO, "daemon": env!("CARGO_PKG_VERSION"), "pane": pane, "boot": st.boot, "signed": signed}))
}

/// FR-3: huruf kecil, angka, '-', maks 32
fn valid_name(n: &str) -> bool {
    (1..=32).contains(&n.len()) && n.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
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
        // `msg` dipakai Pengaturan > Keamanan supaya angka rem tidak ditulis dua kali di UI
        "daemon.info" => Ok(json!({"boot": st.boot, "daemon": env!("CARGO_PKG_VERSION"), "proto": PROTO,
                                   "msg": {"rate_per_min": crate::msg::RATE_PER_MIN, "hop_max": crate::msg::HOP_MAX}})),
        "git.status" | "git.diff" => {
            // folder kerja pane itu sendiri (proses foreground atau shell), bukan dari parameter
            let pane = st.pane(p)?;
            let pid = pty::busy(&pane.master, pane.sid).map_or(pane.sid, |(pid, _)| pid);
            let cwd = pty::cwd_of(pid).or_else(|| pty::cwd_of(pane.sid)).ok_or("io")?;
            Ok(if m == "git.status" { git::status(&cwd) } else { git::diff(&cwd) })
        }
        // FR-15: Rich Input text with `@file:` / `@pane:` attachments appended as labeled blocks. Files must sit inside
        // the pane's cwd after symlinks (SEC-18); pane output is the agents' own `read` as if this pane asked, so the
        // same workspace and shell-pane rules apply (SEC-17, SEC-19, SEC-21). All of it redacted, sanitized, 32 KB.
        "ctx.compose" => {
            let me = p["pane"].as_u64().ok_or("bad_params")?;
            let pane = st.pane(p)?;
            let text = p["text"].as_str().ok_or("bad_params")?;
            let pid = pty::busy(&pane.master, pane.sid).map_or(pane.sid, |(pid, _)| pid);
            let mut body = text.trim_end().to_string();
            for (kind, arg) in msg::ctx_tokens(text) {
                let block = if kind == "file" {
                    let cwd = pty::cwd_of(pid).or_else(|| pty::cwd_of(pane.sid)).ok_or("io")?;
                    msg::file_block(&cwd, arg).map(|mut b| {
                        for t in st.panes.lock().unwrap().values().filter_map(|x| x.token.lock().unwrap().clone()) {
                            b = b.replace(&t, "[disamarkan]");
                        }
                        b
                    })
                } else {
                    pane_method(st, me, "read", &json!({"target": arg, "lines": 100})).map(|r| r["text"].as_str().unwrap_or_default().to_string())
                };
                match block {
                    Ok(b) => body = format!("{body}\n\n{b}"),
                    Err(code) => return Ok(crate::diag::Diag::new(code).at(format!("@{kind}:{arg}")).reply()),
                }
            }
            msg::sanitize(&body).map(|body| json!({"body": body}))
        }
        // FR-23: restore/commit files from the changes panel, UI channel only (SEC-20, SEC-29); repo = the pane's cwd.
        // The preview names other panes working in the same repo, for the confirmation card's warning.
        "git.file_preview" | "git.file_restore" | "git.file_commit" => {
            let pane = st.pane(p)?;
            let pid = pty::busy(&pane.master, pane.sid).map_or(pane.sid, |(pid, _)| pid);
            let cwd = pty::cwd_of(pid).or_else(|| pty::cwd_of(pane.sid)).ok_or("io")?;
            let paths: Vec<String> = p["paths"].as_array().map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect()).unwrap_or_default();
            match m {
                "git.file_restore" => git::restore_file(&cwd, p["path"].as_str().unwrap_or_default()),
                "git.file_commit" => Ok(git::commit_files(&cwd, &paths, p["message"].as_str().unwrap_or_default()).unwrap_or_else(|e| e.reply())),
                _ => {
                    let files = git::file_kinds(&cwd, &paths)?;
                    let root = git::git(&cwd, &["rev-parse", "--show-toplevel"]).and_then(|s| std::path::PathBuf::from(s.trim()).canonicalize().ok()).ok_or("not_repo")?;
                    let me = p["pane"].as_u64().unwrap_or_default();
                    let others: Vec<String> = st.panes.lock().unwrap().iter().filter(|(k, _)| **k != me).filter_map(|(_, q)| {
                        let pid = pty::busy(&q.master, q.sid).map_or(q.sid, |(pid, _)| pid);
                        let hit = [pty::cwd_of(pid), pty::cwd_of(q.sid)].into_iter().flatten().any(|c| c.canonicalize().is_ok_and(|c| c.starts_with(&root)));
                        hit.then(|| q.name.lock().unwrap().clone())
                    }).collect();
                    Ok(json!({"files": files, "others": others}))
                }
            }
        }
        // FR-71: worktree hanya lewat kanal UI (SEC-20); folder repo dari pane, bukan parameter
        "git.worktree_add" => {
            let pane = st.pane(p)?;
            let cwd = pty::cwd_of(pane.sid).ok_or("io")?;
            git::worktree_add(&cwd, p["name"].as_str().unwrap_or_default(), p["dir"].as_str())
        }
        "git.worktree_remove" => {
            let dir = std::path::PathBuf::from(p["dir"].as_str().filter(|d| d.starts_with('/')).ok_or("bad_dir")?);
            let me = dir.canonicalize().map_err(|_| "io")?;
            let used = st.panes.lock().unwrap().values().any(|q| {
                let pid = pty::busy(&q.master, q.sid).map_or(q.sid, |(pid, _)| pid);
                [pty::cwd_of(pid), pty::cwd_of(q.sid)].into_iter().flatten().any(|c| c.canonicalize().is_ok_and(|c| c.starts_with(&me)))
            });
            if used {
                return Err("pane_in_worktree");
            }
            git::worktree_remove(&dir, p["force"].as_bool() == Some(true))
        }
        // FR-70: daftar giliran pane, diff satu giliran, kembalikan ke sebelum giliran (kanal UI saja, SEC-20)
        "git.turns" => {
            let pane = st.pane(p)?;
            let done = pane.turns.lock().unwrap().done.clone();
            Ok(json!(done.iter().enumerate().rev().map(|(i, t)| {
                let (files, add, del) = snap::stat(t);
                json!({"i": i, "start": t.start, "end": t.end, "title": t.title, "files": files, "add": add, "del": del, "skipped": t.skipped})
            }).collect::<Vec<_>>()))
        }
        "git.turn_diff" | "git.turn_preview" | "git.turn_restore" => {
            let pane = st.pane(p)?;
            let i = p["i"].as_u64().ok_or("bad_params")? as usize;
            let t = pane.turns.lock().unwrap().done.get(i).cloned().ok_or("no_such_turn")?;
            if m == "git.turn_diff" {
                return Ok(snap::diff(&t));
            }
            let files = snap::preview(&t.root, &t.before).ok_or("io")?;
            if m == "git.turn_restore" {
                return Ok(json!({"restored": snap::restore(&t.root, &t.before, &files).ok_or("io")?}));
            }
            // peringatan: pane lain mengubah repo yang sama sejak giliran ini mulai, atau HEAD berubah
            let me = p["pane"].as_u64().unwrap_or_default();
            let others: Vec<String> = st.panes.lock().unwrap().iter().filter(|(k, _)| **k != me).filter_map(|(_, q)| {
                let tq = q.turns.lock().unwrap();
                let hit = tq.done.iter().any(|x| x.root == t.root && x.end >= t.start) || tq.open.as_ref().is_some_and(|o| o.root == t.root);
                hit.then(|| q.name.lock().unwrap().clone())
            }).collect();
            let head_moved = snap::head(&t.root) != t.head;
            Ok(json!({"files": files.iter().map(|(k, f)| json!({"kind": k, "path": f})).collect::<Vec<_>>(), "others": others, "head_moved": head_moved, "skipped": t.skipped}))
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
                // SEC-12 pengecualian: kartu konfirmasi menampilkan isi persis yang akan dikirim (yang disetujui = yang sampai)
                "pending": eng.pending.iter().map(|m| { let mut v = m.summary(); v["body"] = json!(m.body); v }).collect::<Vec<_>>(),
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
            (&to.master).write_all(&msg::wrap("pengguna", "", &body)).map_err(|_| "io")?;
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
                return Ok(why.reply());
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
                return Ok(why.reply());
            }
            reload_config(st);
            Ok(json!({}))
        }
        // FR-61: daftar akun + status (sudah dipakai, email); Utama selalu ada
        "account.list" => {
            let main = json!({"id": "utama", "agent": "claude", "name": "Utama", "dir": "~/.claude", "share": [], "main": true});
            let mut out = vec![json!({"account": main, "status": account::status(&account::config_json(None))})];
            for a in st.accounts.lock().unwrap().iter() {
                out.push(json!({"account": account::to_json(a), "status": account::status(&account::config_json(Some(a)))}));
            }
            Ok(json!({"accounts": out, "share": account::SHARE, "models": account::MODELS, "efforts": account::EFFORTS}))
        }
        "account.create" => {
            let name = p["name"].as_str().map(str::trim).filter(|n| !n.is_empty()).ok_or("bad_params")?.to_string();
            let id = account::slug(&name);
            let dir = p["dir"].as_str().map(str::trim).filter(|d| !d.is_empty()).map(String::from).unwrap_or_else(|| format!("~/.claude-{id}"));
            let share = match p["share"].as_array() {
                Some(a) => a.iter().filter_map(|v| v.as_str()).filter(|x| account::SHARE.contains(x)).map(String::from).collect(),
                None => account::SHARE_DEFAULT.iter().map(|x| x.to_string()).collect(),
            };
            let a = account::Account { id, agent: "claude".into(), name, dir, share, model: String::new(), effort: String::new(), trust: true };
            if let Err(why) = account::valid_dir(&a.dir) {
                return Ok(why.reply());
            }
            // config dulu (validasi penuh, id/folder ganda ditolak), baru folder dan tautan
            if let Err(why) = config::save_account(&config::path(), &a) {
                return Ok(why.reply());
            }
            let res = account::create(&a);
            reload_config(st);
            match res {
                Ok(skipped) => Ok(json!({"id": a.id, "skipped": skipped})),
                Err(why) => Ok(json!({"id": a.id, "error": why.code, "args": why.json()["args"]})),
            }
        }
        "account.remove" => {
            let id = p["id"].as_str().ok_or("bad_params")?;
            let dir = st.accounts.lock().unwrap().iter().find(|a| a.id == id).map(|a| a.dir.clone());
            if let Err(why) = config::remove_account(&config::path(), id) {
                return Ok(why.reply());
            }
            reload_config(st);
            // FR-61 (7): folder akun ikut dihapus setelah config tidak lagi merujuknya
            match dir.map(|d| account::delete_dir(&d)) {
                Some(Err(why)) => Ok(why.reply()),
                _ => Ok(json!({})),
            }
        }
        // FR-61 (8): model/effort/trust, atau satu butir jadi Ikut Utama/Sendiri (`item` + `follow`)
        "account.update" => {
            let id = p["id"].as_str().ok_or("bad_params")?;
            let Some(mut a) = st.accounts.lock().unwrap().iter().find(|a| a.id == id).cloned() else { return Ok(json!({"error": "no_such_account"})) };
            if let Some(m) = p["model"].as_str() {
                a.model = m.into();
            }
            if let Some(e) = p["effort"].as_str() {
                a.effort = e.into();
            }
            if let Some(t) = p["trust"].as_bool() {
                a.trust = t;
            }
            if let (Some(item), Some(follow)) = (p["item"].as_str(), p["follow"].as_bool()) {
                if let Err(why) = account::set_share(&a, item, follow) {
                    return Ok(why.reply());
                }
                a.share.retain(|x| x != item);
                if follow {
                    a.share.push(item.into());
                }
            }
            if let Err(why) = config::update_account(&config::path(), &a) {
                return Ok(why.reply());
            }
            reload_config(st);
            Ok(json!({}))
        }
        "config.set" => {
            let (section, key) = (p["section"].as_str().ok_or("bad_params")?, p["key"].as_str().ok_or("bad_params")?);
            if let Err(why) = config::set(&config::path(), section, key, &p["value"]) {
                return Ok(why.reply());
            }
            reload_config(st);
            Ok(json!({}))
        }
        // FR-78: Settings › Agent › Add-on. A plugin folder is only ever one the user picked, never the repo or cwd.
        "addon.plugin" => {
            let dir = p["path"].as_str();
            let id = match (p["id"].as_str(), dir) {
                (Some(id), _) => id.to_string(),
                (None, Some(d)) => {
                    let base = account::slug(&std::path::Path::new(d).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
                    let base = if base.is_empty() { "plugin".to_string() } else { base };
                    let taken: Vec<String> = st.addons.lock().unwrap().plugins.iter().map(|x| x.id.clone()).collect();
                    std::iter::once(base.clone()).chain((2..).map(|n| format!("{base}-{n}"))).find(|i| !taken.contains(i)).unwrap_or(base)
                }
                (None, None) => return Err("bad_params"),
            };
            if !addon::valid_key(&id) {
                return Err("bad_params");
            }
            let ws: Option<Vec<String>> = p["workspaces"].as_array().map(|a| a.iter().filter_map(|v| v.as_str()).map(String::from).collect());
            if let Err(why) = config::set_plugin(&config::path(), &id, dir, p["scope"].as_str(), ws.as_deref(), p["remove"].as_bool() == Some(true)) {
                return Ok(why.reply());
            }
            reload_config(st);
            Ok(json!({"id": id}))
        }
        "addon.mcp.get" => {
            let ws = p["workspace"].as_str().filter(|w| addon::valid_key(w)).ok_or("bad_params")?;
            let a = st.addons.lock().unwrap();
            let m = a.mcp.iter().find(|m| m.workspace == ws);
            Ok(json!({"text": m.and_then(|m| std::fs::read_to_string(&m.path).ok()).unwrap_or_default(), "strict": m.is_some_and(|m| m.strict)}))
        }
        // text = Claude's `--mcp-config` JSON, saved as a 0600 file inside the Gembala config folder; empty = off
        "addon.mcp.set" => {
            let ws = p["workspace"].as_str().filter(|w| addon::valid_key(w)).ok_or("bad_params")?;
            let text = p["text"].as_str().unwrap_or_default().trim();
            let cfg = config::path();
            let file = st.addons.lock().unwrap().mcp.iter().find(|m| m.workspace == ws).map_or(format!("mcp/{ws}.json"), |m| m.file.clone());
            let full = cfg.parent().unwrap_or(std::path::Path::new("/")).join(&file);
            let r = if text.is_empty() {
                config::set_mcp(&cfg, ws, None).map(|_| drop(std::fs::remove_file(&full)))
            } else if text.len() > 64 * 1024 || !serde_json::from_str::<Value>(text).is_ok_and(|v| v["mcpServers"].is_object()) {
                Err(crate::diag::Diag::new("mcp_json"))
            } else {
                config::write_atomic(&full, format!("{text}\n").as_bytes())
                    .map_err(|e| config::io_err("io_write", e))
                    .and_then(|_| config::set_mcp(&cfg, ws, Some((&file, p["strict"].as_bool() == Some(true)))))
            };
            if let Err(why) = r {
                return Ok(why.reply());
            }
            reload_config(st);
            Ok(json!({}))
        }
        "addon.status" => Ok(json!({"rtk": profile::rtk_version()})),
        "pane.create" => {
            // batas bagian 11 BRD, bisa diubah lewat [limits] max_panes
            let max = st.config.lock().unwrap()["values"]["limits"]["max_panes"].as_u64().unwrap_or(32) as usize;
            if st.panes.lock().unwrap().len() >= max {
                return Err("max_panes");
            }
            let (rows, cols) = (dim(&p["rows"], 24), dim(&p["cols"], 80));
            // FR-30: profil (bawaan "shell"); folder = folder profil, atau folder pane yang fokus
            let mut prof = match p["profile"].as_str() {
                Some(id) => Some(st.profiles.lock().unwrap().iter().find(|x| x.id == id).cloned().ok_or("no_such_profile")?),
                None => None,
            };
            // FR-13: peran (template) dan brief workspace ikut petunjuk launch; teks dari UI dibersihkan dan dibatasi
            if let Some(x) = prof.as_mut() {
                let role = profile::ROLES.iter().find(|r| p["role"].as_str() == Some(r.0));
                let brief = p["brief"].as_str().map(|b| b.chars().take(4000).collect::<String>()).and_then(|b| msg::sanitize(&b).ok()).filter(|b| !b.trim().is_empty());
                x.readonly = role.is_some_and(|r| r.1);
                // FR-78: add-ons of the workspace the pane opens in
                x.addons = addon::args(&st.addons.lock().unwrap(), &x.agent, p["workspace"].as_str().unwrap_or_default());
                x.brief = [role.map(|r| r.2.to_string()), brief.map(|b| format!("Workspace brief from the user (read only):\n{b}"))].into_iter().flatten().collect::<Vec<_>>().join("\n\n");
            }
            // FR-61, C-9: akun = pilihan manusia saat membuka pane (UI, termasuk bawaan workspace) > akun profil > Utama
            // pane shell: akun bawaan workspace dipasang ke env shell, jadi `claude` yang diketik ikut akun itu
            let mut shell_acct = None;
            let acct_name = match prof.as_mut() {
                Some(pr) => account::apply(pr, p["account"].as_str(), &st.accounts.lock().unwrap())?,
                None => match p["account"].as_str().filter(|a| !a.is_empty() && *a != "utama") {
                    Some(id) => {
                        let a = st.accounts.lock().unwrap().iter().find(|a| a.id == id && a.agent == "claude").cloned().ok_or("no_such_account")?;
                        shell_acct = Some(account::env(&a));
                        a.name
                    }
                    None => String::new(),
                },
            };
            // FR-61: folder yang dipercaya di Utama ikut dipercaya di akun ini sebelum Claude-nya jalan
            if let Some(a) = st.accounts.lock().unwrap().iter().find(|a| !acct_name.is_empty() && a.name == acct_name && a.trust) {
                account::sync_trust(&a.dir);
            }
            // FR-72: pemilih sesi bawaan agent (tanpa id); Gembala tidak membaca daftar sesi (SEC-15)
            let pick = p["pick"].as_bool() == Some(true);
            let picked = prof.as_ref().filter(|_| pick).map(|x| {
                let mut x = x.clone();
                x.args.extend(profile::picker_flags(&x.agent).iter().map(|s| s.to_string()));
                x
            });
            let session = if pick { profile::Session { id: None, resume: false } } else { profile::Session { id: p["session"].as_str().map(String::from), resume: p["resume"].as_bool() == Some(true) } };
            if pick && prof.as_ref().is_none_or(|x| profile::picker_flags(&x.agent).is_empty()) {
                return Err("no_picker");
            }
            let launch = picked.as_ref().or(prof.as_ref()).and_then(|x| profile::launch(x, Some(&st.hooks), Some(&session)));
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
            let token = if prof.as_ref().is_some_and(|x| x.token) { Some(new_secret().map_err(|_| "io")?[..32].to_string()) } else { None };
            // FR-9: perintah `gembala` bisa dipanggil langsung dari pane (folder binary daemon di PATH;
            // login shell mempertahankannya lewat path_helper)
            let bin = std::path::Path::new(&st.hooks.exe).parent().map(|d| d.to_string_lossy().into_owned()).unwrap_or_default();
            let path = format!("{bin}:{}", std::env::var("PATH").unwrap_or_default());
            let mut env = vec![("GEMBALA_SOCKET", st.pane_sock.as_str()), ("PATH", path.as_str())];
            // FR-58: pane zsh membaca file startup lewat folder hook (penanda blok OSC 133), lalu ZDOTDIR pengguna kembali
            let zdot = st.hooks.dir.to_string_lossy().into_owned();
            let user_zdot = std::env::var("ZDOTDIR").or_else(|_| std::env::var("HOME")).unwrap_or_default();
            if pty::login_shell().ends_with("/zsh") {
                env.extend([("ZDOTDIR", zdot.as_str()), ("GEMBALA_USER_ZDOTDIR", user_zdot.as_str())]);
                // FR-59: editor UI menggantikan prompt shell
                if st.config.lock().unwrap()["values"]["ui"]["input_editor"].as_bool() != Some(false) {
                    env.push(("GEMBALA_EDITOR", "1"));
                }
            }
            if let Some(t) = &token {
                env.push(("GEMBALA_TOKEN", t.as_str()));
            }
            let acct_dir = shell_acct.as_ref().map(|(_, v)| v.strip_prefix("~/").map_or_else(|| v.clone(), |rest| format!("{home}/{rest}")));
            if let (Some((k, _)), Some(d)) = (&shell_acct, &acct_dir) {
                env.push((k.as_str(), d.as_str()));
            }
            let pty = Pty::spawn(cmd, rows, cols, &env, cwd.as_deref()).map_err(|_| "spawn_failed")?;
            let id = st.next.fetch_add(1, Ordering::Relaxed);
            let pane = Arc::new(Pane {
                master: pty.master.try_clone().map_err(|_| "spawn_failed")?,
                sid: pty.child.id() as i32,
                name: {
                    let label = prof.as_ref().map_or("shell", |x| if x.agent == "custom" { x.id.as_str() } else { x.agent.as_str() });
                    let names: Vec<String> = st.panes.lock().unwrap().values().map(|q| q.name.lock().unwrap().clone()).collect();
                    // FR-79: pane pengganti memakai nama lamanya selama tidak dipakai pane lain (nama sekarang menang)
                    match p["name"].as_str().filter(|n| valid_name(n) && !names.iter().any(|x| x == n)) {
                        Some(n) => Mutex::new(n.to_string()),
                        None => {
                            // nama lama pane lain juga dilewati supaya alamatnya tidak diam-diam pindah
                            let aliases: Vec<String> = st.panes.lock().unwrap().values().flat_map(|q| q.aliases.lock().unwrap().clone()).collect();
                            let n = (1..).map(|n| format!("{label}-{n}")).find(|n| !names.contains(n) && !aliases.contains(n)).unwrap_or_default();
                            Mutex::new(n)
                        }
                    }
                },
                aliases: Mutex::default(),
                session: Mutex::new(session.id.clone().filter(|s| profile::valid_session(&agent, s))),
                agent_log: Mutex::new(None),
                last_output: Mutex::new(Vec::new()),
                cmd_at: Mutex::new((0, 0)),
                pre_cmd: Mutex::new(Vec::new()),
                pre_agent: Mutex::new(Vec::new()),
                hist: history::path(&st.boot, id),
                title: Mutex::new(String::new()),
                turns: Arc::default(),
                account: Mutex::new(st.accounts.lock().unwrap().iter().find(|a| !acct_name.is_empty() && a.name == acct_name).map(|a| a.id.clone()).unwrap_or_default()),
                owner: Mutex::new(None),
                block: Mutex::new((None, None)),
                manual: Mutex::new(None),
                last_out: Mutex::new(std::time::Instant::now()),
                at_prompt: std::sync::atomic::AtomicBool::new(false),
                hooked: std::sync::atomic::AtomicBool::new(false),
                // FR-68: Codex hanya punya hook `notify` (selesai); bekerja dan butuh izin dari layar
                screen: agent == "custom" || agent == "codex",
                started: std::time::Instant::now(),
                workspace: Mutex::new(p["workspace"].as_str().unwrap_or_default().chars().take(64).collect()),
                policy: Mutex::new(prof.as_ref().map_or(msg::Policy::Auto, |x| {
                    msg::effective_policy(msg::Policy::parse(&x.policy).unwrap_or(msg::Policy::Auto), &[x.args.clone(), profile::permission_flags(&x.agent, &x.permission)].concat())
                })),
                status_at: Mutex::new(std::time::Instant::now()),
                readable: std::sync::atomic::AtomicBool::new(false),
                typed_at: Mutex::new(None),
                token: Mutex::new(token),
                status: Mutex::new(if ["claude", "codex", "opencode", "omp", "custom"].contains(&agent.as_str()) { "idle" } else { "" }),
                agent,
                view: Mutex::new((Grid::new(rows, cols, SCROLLBACK), Vec::new())),
            });
            // FR-62: blok pane lama (setelah daemon berhenti) tampil di atas shell baru dan bersambung di file baru
            let prev = history::load(&p["history"]);
            for b in &prev {
                history::append(&pane.hist, b);
            }
            // log layar lama sudah memuat blok-blok itu; tanpa log cukup blok ringkasnya
            match history::load_log(&p["history"]) {
                Some((log, t)) => {
                    let mut v = pane.view.lock().unwrap();
                    v.0.feed(&log);
                    v.0.feed(&history::separator(t));
                }
                None => pane.view.lock().unwrap().0.feed(&history::render(&prev)),
            }
            st.panes.lock().unwrap().insert(id, pane.clone());
            // FR-79: pane pengganti (jalankan ulang, daemon restart) membawa nama dan alias dari tata letak (tidak tepercaya);
            // nama yang sedang dipakai pane lain jadi alias, yang terbaru
            let name = pane.name.lock().unwrap().clone();
            st.release(&name);
            let old = p["aliases"].as_array().into_iter().flatten().filter_map(Value::as_str).take(ALIAS_MAX);
            for a in old.chain(p["name"].as_str()).filter(|a| valid_name(a)) {
                st.add_alias(&pane, a);
            }
            let aliases = pane.aliases.lock().unwrap().clone();
            let st = st.clone();
            thread::spawn(move || pump(st, id, pane, pty));
            Ok(json!({"pane": id, "name": name, "aliases": aliases}))
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
            let name = p["name"].as_str().filter(|n| valid_name(n)).ok_or("bad_name")?;
            if st.panes.lock().unwrap().values().any(|q| !Arc::ptr_eq(q, &pane) && *q.name.lock().unwrap() == name) {
                return Err("name_taken");
            }
            let old = std::mem::replace(&mut *pane.name.lock().unwrap(), name.to_string());
            // FR-79: nama lama jadi alias; nama baru dilepas dari alias pane mana pun (nama sekarang menang)
            st.release(name);
            st.add_alias(&pane, &old);
            Ok(json!({"name": name, "aliases": *pane.aliases.lock().unwrap()}))
        }
        "pane.info" => {
            let pane = st.pane(p)?;
            let busy = pty::busy(&pane.master, pane.sid);
            Ok(json!({
                "busy": busy.is_some(), "name": busy.and_then(|(pid, _)| pty::proc_name(pid)), "pid": pane.sid, // FR-60
                "shell": pty::proc_name(pane.sid), // Gembala.pen 02: subjudul tab shell diam (`zsh`)
                "password": busy.is_some() && pty::wants_password(&pane.master),
                "pane_name": *pane.name.lock().unwrap(), "agent": pane.agent, "status": *pane.status.lock().unwrap(),
                "policy": pane.policy.lock().unwrap().name(), "readable": pane.readable.load(Ordering::Relaxed),
                "session": *pane.session.lock().unwrap(), // FR-68: kartu lanjut walau daemon berhenti
                "title": *pane.title.lock().unwrap(), // FR-69: label pane
                "account": acct_label(st, &pane.account.lock().unwrap()), // FR-61: chip akun
                "account_id": *pane.account.lock().unwrap(), // FR-68: kartu lanjut `claude-<akun>` walau daemon berhenti
                // FR-61: sesi milik akun lain: nama untuk label, id untuk Lanjutkan ("utama" = Utama)
                "owner": pane.owner.lock().unwrap().as_ref().map(|o| if o.is_empty() { "Utama".into() } else { acct_label(st, o) }),
                "owner_id": pane.owner.lock().unwrap().as_ref().map(|o| if o.is_empty() { "utama" } else { o.as_str() }.to_string()),
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
            for name in profile::hook_files(&st.hooks).into_iter().map(|f| f.0).chain(["accounts.zsh"]) {
                let _ = std::fs::remove_file(hooks.join(name));
            }
            let _ = std::fs::remove_dir(&hooks);
            let _ = std::fs::remove_dir(dir);
            eprintln!("daemon keluar: daemon.reset");
            thread::spawn(|| {
                thread::sleep(Duration::from_millis(200));
                std::process::exit(0);
            });
            Ok(json!({}))
        }
        "daemon.stop" => {
            // FR-46 "Keluar dan hentikan semua agent": balasan sempat terkirim, lalu keluar;
            // master PTY tertutup bersama proses = kernel mengirim SIGHUP ke semua sesi pane
            eprintln!("daemon keluar: daemon.stop");
            save_logs(st, &mut HashMap::new());
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
        // SEC-5: flag lewati-izin dari profil sudah masuk saat pane dibuat; yang diketik sendiri di pane dibaca dari argumen proses
        policy: msg::effective_policy(*to.policy.lock().unwrap(), &pty::busy(&to.master, to.sid).map(|(pid, _)| pty::args_of(pid)).unwrap_or_default()),
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

/// FR-66: pesan terakhir giliran agent bersandbox menjawab pesan masuk yang masih menunggu (jalur `reply` biasa:
/// hop, batas laju, jeda tetap berlaku). Tanpa pesan yang menunggu, teks dibuang dan tidak disimpan.
fn auto_reply(st: &Arc<State>, me: u64, text: &str) {
    let waiting = st.engine.lock().unwrap().waiting_for(me, Duration::from_secs(1800)).map(|m| m.id.clone());
    if let Some(id) = waiting {
        let body: String = text.chars().take(4000).collect();
        let _ = pane_method(st, me, "reply", &json!({"msg_id": id, "body": body}));
    }
}

/// FR-66: permintaan CLI dari proses bersandbox lewat file `<run>/spool/<id>.req`; jawaban `<id>.res` (0600).
/// Identitas dari token pane (constant-time), bukan dari isi lain permintaan.
fn spool(st: Arc<State>, dir: std::path::PathBuf) {
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    let dir = dir.join("spool");
    let _ = fs::DirBuilder::new().mode(0o700).create(&dir);
    loop {
        thread::sleep(Duration::from_millis(200));
        let Ok(entries) = fs::read_dir(&dir) else { continue };
        for e in entries.flatten() {
            let path = e.path();
            let Some(name) = path.file_name().and_then(|n| n.to_str()).map(String::from) else { continue };
            let Some((id, ext)) = name.split_once('.') else { continue };
            if id.is_empty() || !id.bytes().all(|c| c.is_ascii_alphanumeric()) {
                continue;
            }
            if ext != "req" {
                // jawaban yang tidak diambil 5 menit
                if e.metadata().and_then(|m| m.modified()).is_ok_and(|t| t.elapsed().is_ok_and(|a| a > Duration::from_secs(300))) {
                    let _ = fs::remove_file(&path);
                }
                continue;
            }
            let text = fs::read_to_string(&path).unwrap_or_default();
            let _ = fs::remove_file(&path);
            let (st, out, tmp) = (st.clone(), dir.join(format!("{id}.res")), dir.join(format!("{id}.out")));
            thread::spawn(move || {
                let v: Value = serde_json::from_str(&text).unwrap_or_default();
                let method = v["method"].as_str().unwrap_or_default();
                let r = match v["token"].as_str().and_then(|t| st.pane_of_token(t)) {
                    None => Err("unauthorized"),
                    Some(me) if ["send", "reply", "wait", "read", "status", "token"].contains(&method) => pane_method(&st, me, method, &v["params"]),
                    Some(_) => Err("not_allowed"),
                };
                let body = match r {
                    Ok(x) => json!({"result": x}),
                    Err(e) => json!({"error": {"code": e}}),
                };
                // file baru (bukan symlink yang sudah ada), lalu rename: CLI tidak pernah membaca jawaban setengah jadi
                let wrote = fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp).and_then(|mut f| f.write_all(body.to_string().as_bytes()));
                if wrote.is_ok() {
                    let _ = fs::rename(&tmp, &out);
                }
            });
        }
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
        if *pane.workspace.lock().unwrap() != my_ws || (pane.agent_name().is_none() && !pane.readable.load(Ordering::Relaxed)) {
            return Err("acl_denied");
        }
        Ok((id, pane))
    };
    match method {
        // FR-78: typed `claude`/`codex` (zsh function) gets the add-ons of this pane's workspace now
        "addons" => Ok(json!({"args": addon::args(&st.addons.lock().unwrap(), p["agent"].as_str().unwrap_or_default(), &my_ws)})),
        "token" => {
            // FR-66: token pane untuk proses bersandbox (jalur spool); dibuat sekali, dicabut saat pane ditutup
            let pane = st.panes.lock().unwrap().get(&me).cloned().ok_or("not_a_pane")?;
            let mut t = pane.token.lock().unwrap();
            if t.is_none() {
                *t = Some(new_secret().map_err(|_| "io")?[..32].to_string());
            }
            Ok(json!({"token": *t}))
        }
        "status" => {
            let list: Vec<Value> = st
                .panes
                .lock()
                .unwrap()
                .iter()
                .filter(|(id, x)| **id != me && *x.workspace.lock().unwrap() == my_ws)
                .filter_map(|(_, x)| Some((x, x.agent_name()?)))
                .map(|(x, agent)| json!({"name": *x.name.lock().unwrap(), "aliases": *x.aliases.lock().unwrap(), "agent": agent, "status": *x.status.lock().unwrap(), "policy": x.policy.lock().unwrap().name()}))
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
            let to_name = to.name.lock().unwrap().clone();
            let m = bus::Msg { id, from: me, from_name: my_name, to: to_id, to_name: to_name.clone(), body, reply_to, hop, at: std::time::SystemTime::now() };
            // FR-79: nama sekarang ikut jawaban, supaya pengirim yang memakai nama lama beralih
            let mut r = try_send(st, m, p["queue"].as_bool() == Some(true), false);
            r["to"] = json!(to_name);
            Ok(r)
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
            let mut text = git::redact(&pane.view.lock().unwrap().0.tail(n));
            // SEC-12: nilai GEMBALA_TOKEN yang aktif ikut disamarkan
            for t in st.panes.lock().unwrap().values().filter_map(|x| x.token.lock().unwrap().clone()) {
                text = text.replace(&t, "[disamarkan]");
            }
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
            // FR-79: nama sekarang target ikut jawaban, supaya pengirim yang memakai nama lama beralih
            let done = |mut v: Value| {
                if let Some((_, pane)) = &target {
                    v["target"] = json!(*pane.name.lock().unwrap());
                }
                Ok(v)
            };
            let t0 = std::time::Instant::now();
            loop {
                if let Some(r) = &reply {
                    if let Some(m) = st.engine.lock().unwrap().replies.remove(r) {
                        return Ok(json!({"reply": {"id": m.id, "from": m.from_name, "body": m.body}}));
                    }
                } else if let Some((_, pane)) = &target {
                    if let Some(re) = &pattern {
                        if re.is_match(&pane.view.lock().unwrap().0.text()) {
                            return done(json!({"matched": true}));
                        }
                    } else {
                        let s = *pane.status.lock().unwrap();
                        if matches!(s, "idle" | "done" | "blocked") && pane.status_at.lock().unwrap().elapsed() >= msg::STABLE {
                            return done(json!({"status": s}));
                        }
                    }
                }
                if t0.elapsed() > timeout {
                    return done(json!({"timeout": true}));
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
            let fg = (pane.agent == "shell").then(|| pty::busy(&pane.master, pane.sid)).flatten();
            let found = fg.and_then(|(pid, _)| Some((pid, pty::proc_name(pid)?))).filter(|(_, n)| AGENTS.contains(&n.as_str()));
            // FR-56: agent manual mulai = catat log sesi. Keluar biasanya ditangani `pump` begitu shell menulis prompt;
            // di sini cadangan bila shell diam, prompt yang sudah tampil ditulis lagi di bawah blok.
            let active = pane.manual.lock().unwrap().is_some();
            // proses lain di depan selagi agent aktif = anak agent (mis. `brew` dari omp): agent belum keluar
            let manual = found.is_some() || (active && fg.is_some());
            match (active, found, fg) {
                (false, Some((pid, name)), _) => {
                    let mut v = pane.view.lock().unwrap();
                    let at = *pane.cmd_at.lock().unwrap();
                    let pre = std::mem::take(&mut *pane.pre_cmd.lock().unwrap());
                    *pane.pre_agent.lock().unwrap() = if pre.is_empty() { v.0.before(at) } else { pre };
                    drop(v);
                    let args = pty::args_of(pid);
                    // FR-68: Claude diketik membawa `--session-id` dari fungsi zsh Gembala; selain itu id lama tidak berlaku
                    let sid = args.windows(2).find(|w| w[0] == "--session-id").map(|w| w[1].clone()).filter(|s| profile::valid_session(&name, s));
                    *pane.session.lock().unwrap() = sid;
                    // FR-61: `claude-kantor` diketik = akun dari folder config proses agent
                    if name == "claude" {
                        *pane.account.lock().unwrap() = acct_of_dir(st.as_ref(), pty::env_of(pid, "CLAUDE_CONFIG_DIR"));
                    }
                    *pane.manual.lock().unwrap() = Some((name, profile::shown_args(&args), pty::cwd_of(pid)));
                    *pane.agent_log.lock().unwrap() = Some((std::time::Instant::now(), Vec::new()));
                }
                // keluar = shell kembali di depan (sama dengan cek di `pump`)
                (true, None, None) => {
                    let mut v = pane.view.lock().unwrap();
                    let prompt = v.0.cursor_line();
                    manual_done(&mut v, &pane, id, &prompt, None);
                }
                _ => {}
            }
            let cur = *pane.status.lock().unwrap();
            let next = if manual && pane.hooked.load(Ordering::Relaxed) {
                // hook (mis. SessionStart) bisa datang sebelum agent dikenali: status awal = menunggu tugas
                if !cur.is_empty() {
                    continue;
                }
                "idle"
            } else if pane.screen || manual {
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
                status_changed(&st, id, &pane, next);
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
    let prev = std::mem::replace(&mut *cur, status);
    drop(cur);
    *pane.status_at.lock().unwrap() = std::time::Instant::now();
    turn_edge(pane, prev, status);
    status_changed(st, id, pane, status);
}

/// FR-70: giliran mulai (bekerja) dan selesai (diam/selesai) = potret folder kerja agent, di antrean
fn turn_edge(pane: &Pane, prev: &str, status: &str) {
    let busy = |s: &str| matches!(s, "working" | "blocked");
    let turns = pane.turns.clone();
    if status == "working" && !busy(prev) {
        let pid = pty::busy(&pane.master, pane.sid).map_or(pane.sid, |(pid, _)| pid);
        let Some(cwd) = pty::cwd_of(pid).or_else(|| pty::cwd_of(pane.sid)) else { return };
        snap::queue(move || {
            let Some(root) = snap::root_of(&cwd) else { return };
            if let Some((before, _)) = snap::take(&root) {
                let head = snap::head(&root);
                turns.lock().unwrap().open = Some(snap::Open { start: history::now(), root, before, head });
            }
        });
    } else if busy(prev) && matches!(status, "idle" | "done") {
        let title = pane.title.lock().unwrap().clone();
        snap::queue(move || {
            let Some(open) = turns.lock().unwrap().open.take() else { return };
            if let Some(t) = snap::finish(open, history::now(), title) {
                let mut cur = turns.lock().unwrap();
                cur.done.push(t);
                if cur.done.len() > snap::MAX_TURNS {
                    cur.done.remove(0);
                }
            }
        });
    }
}

/// Kabari UI dan pelanggan kanal pane (FR-8). Pelanggan hanya menerima pane yang boleh ia baca (SEC-21).
fn status_changed(st: &State, id: u64, pane: &Pane, status: &str) {
    let ev = line(json!({"event": "pane.status", "params": {"pane": id, "status": status}}));
    st.ui_txs.lock().unwrap().retain(|(_, tx)| tx.try_send(ev.clone()).is_ok());
    let ws = pane.workspace.lock().unwrap().clone();
    let visible = pane.agent_name().is_some() || pane.readable.load(Ordering::Relaxed);
    let name = pane.name.lock().unwrap().clone();
    let ev = line(json!({"event": "pane.status", "params": {"name": name, "status": status}}));
    let subs: Vec<(u64, SyncSender<String>)> = st.pane_subs.lock().unwrap().clone();
    let mut dead = vec![];
    for (who, tx) in &subs {
        if *who == id || !visible || st.workspace_of(*who) != ws {
            continue;
        }
        if tx.try_send(ev.clone()).is_err() {
            dead.push(*who);
        }
    }
    if !dead.is_empty() {
        st.pane_subs.lock().unwrap().retain(|(who, _)| !dead.contains(who));
    }
}

/// FR-61: nama akun untuk chip; Utama atau tidak dikenal = kosong
fn acct_label(st: &State, id: &str) -> String {
    st.accounts.lock().unwrap().iter().find(|a| a.id == id).map(|a| a.name.clone()).unwrap_or_default()
}

/// FR-61: folder config Claude proses → id akun; tanpa env atau folder Utama = kosong
fn acct_of_dir(st: &State, dir: Option<String>) -> String {
    let Some(dir) = dir.map(|d| account::expand(&d)) else { return String::new() };
    st.accounts.lock().unwrap().iter().find(|a| a.agent == "claude" && account::expand(&a.dir) == dir).map(|a| a.id.clone()).unwrap_or_default()
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
        if let Err(e) = config::write_atomic(&st.hooks.dir.join("accounts.zsh"), profile::zsh_accounts(&c.accounts).as_bytes()) {
            eprintln!("gembala daemon: tulis accounts.zsh: {e}");
        }
        *st.profiles.lock().unwrap() = c.profiles;
        *st.accounts.lock().unwrap() = c.accounts;
        *st.addons.lock().unwrap() = c.addons;
    }
    let errors: Vec<Value> = c.errors.iter().map(crate::diag::Diag::json).collect();
    *cur = json!({"values": values, "errors": errors, "path": path.to_string_lossy()});
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
/// FR-62: log layar utama tiap pane yang dapat output baru; layar alternatif (TUI) tidak menimpa log terakhir
fn save_logs(st: &State, saved: &mut HashMap<u64, std::time::Instant>) {
    let panes: Vec<(u64, Arc<Pane>)> = st.panes.lock().unwrap().iter().map(|(k, v)| (*k, v.clone())).collect();
    saved.retain(|id, _| panes.iter().any(|(k, _)| k == id));
    for (id, pane) in panes {
        let at = *pane.last_out.lock().unwrap();
        if saved.get(&id) == Some(&at) {
            continue;
        }
        let log = {
            let mut v = pane.view.lock().unwrap();
            if pane.agent_log.lock().unwrap().is_some() {
                // agent masih jalan: tampilan TUI-nya tidak disimpan, cukup isi pane sebelum agent mulai
                let name = pane.agent_name().unwrap_or_else(|| "agent".into());
                let mut log = pane.pre_agent.lock().unwrap().clone();
                log.extend(format!("   \x1b[2m{name} (masih jalan saat daemon berhenti)\x1b[0m\r\n").as_bytes());
                log
            } else {
                let pos = v.0.position();
                v.0.before(pos)
            }
        };
        if !log.is_empty() {
            history::save_log(&st.boot, id, &log);
        }
        saved.insert(id, at);
    }
}

fn save_logs_loop(st: Arc<State>) {
    let mut saved = HashMap::new();
    for n in (0..5760u32).cycle() {
        if n == 0 {
            history::sweep(); // saat mulai lalu tiap hari (5760 x 15 dtk)
            snap::sweep();
        }
        thread::sleep(Duration::from_secs(15));
        save_logs(&st, &mut saved);
    }
}

fn idle_exit(st: Arc<State>) {
    let every = std::env::var("GEMBALA_IDLE_SECS").ok().and_then(|s| s.parse().ok()).unwrap_or(5);
    let mut strikes = 0;
    loop {
        thread::sleep(Duration::from_secs(every));
        let busy = st.panes.lock().unwrap().values().any(|p| pty::busy(&p.master, p.sid).is_some());
        strikes = if st.ui_clients.load(Ordering::Relaxed) == 0 && !busy { strikes + 1 } else { 0 };
        if strikes >= 2 {
            eprintln!("daemon keluar: tanpa UI dan semua pane diam");
            save_logs(&st, &mut HashMap::new());
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
    // FR-56: perintah mulai (OSC 133 C) = salin isi pane dulu, sebelum output perintah (mis. Claude membersihkan
    // layar di potongan yang sama) masuk ke grid
    const C: &[u8] = b"\x1b]133;C";
    match b_find(bytes, C).and_then(|p| bytes[p..].iter().position(|&c| c == 0x07).map(|e| p + e + 1)) {
        Some(end) => {
            v.0.feed(&bytes[..end]);
            let (sb, row) = v.0.position();
            *pane.pre_cmd.lock().unwrap() = v.0.before(((sb + row as usize).saturating_sub(1), 0));
            v.0.feed(&bytes[end..]);
        }
        None => v.0.feed(bytes),
    }
    *pane.last_out.lock().unwrap() = std::time::Instant::now();
    for m in history::marks(bytes) {
        on_block(pane, m);
    }
    if let Some(t) = osc_title(bytes) {
        *pane.title.lock().unwrap() = t;
    }
    if let Some(on) = prompt_mark(bytes) {
        pane.at_prompt.store(on, Ordering::Relaxed);
        if !on {
            // perintah mulai: kursor di baris output pertama, baris perintahnya satu di atas
            let (sb, row) = v.0.position();
            *pane.cmd_at.lock().unwrap() = ((sb + row as usize).saturating_sub(1), 0);
        }
    }
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
        let at = v.0.position();
        *pane.pre_agent.lock().unwrap() = v.0.before(at);
        *pane.agent_log.lock().unwrap() = Some((std::time::Instant::now(), Vec::new()));
        // agent dari profil jalan (lagi, mis. "Lanjutkan") = siap menerima pesan
        *pane.status.lock().unwrap() = "idle";
        *pane.status_at.lock().unwrap() = std::time::Instant::now();
        return;
    }
    let Some(code) = mark.strip_prefix("agent-exit;") else { return };
    let name = pane.name.lock().unwrap().clone();
    agent_done(v, pane, id, &pane.agent, &name, None, code, &[]);
}

/// FR-69: judul terminal terakhir di potongan output (`ESC ] 0|2 ; teks BEL|ST`), tanpa karakter kontrol, maks 200.
// ponytail: judul yang terpotong batas baca PTY terlewat; judul berikutnya membetulkan
fn osc_title(b: &[u8]) -> Option<String> {
    let start = [b"\x1b]0;", b"\x1b]2;"].iter().filter_map(|m| b.windows(4).rposition(|w| w == *m)).max()? + 4;
    let end = b[start..].iter().position(|&c| c == 0x07 || c == 0x1b)?;
    let t: String = String::from_utf8_lossy(&b[start..start + end]).chars().filter(|c| !c.is_control()).take(200).collect();
    Some(t.trim().to_string())
}

/// FR-59: penanda OSC 133 terakhir di potongan output: B = shell menunggu di prompt, C = perintah mulai jalan.
// ponytail: penanda yang terpotong batas baca PTY tidak terlihat; prompt berikutnya membetulkan
fn b_find(b: &[u8], m: &[u8]) -> Option<usize> {
    b.windows(m.len()).rposition(|w| w == m)
}

fn prompt_mark(b: &[u8]) -> Option<bool> {
    let last = |m: &[u8]| b.windows(m.len()).rposition(|w| w == m);
    match (last(b"\x1b]133;B"), last(b"\x1b]133;C")) {
        (Some(x), Some(y)) => Some(x > y),
        (x, y) => x.map(|_| true).or(y.map(|_| false)),
    }
}

/// FR-62: blok perintah shell ke file riwayat. Selesai saat agent manual masih tercatat = tunggu `agent_done`
/// supaya perintah lanjutnya ikut; blok tertunda ditulis apa adanya bila perintah berikutnya mulai lebih dulu.
fn on_block(pane: &Pane, m: history::Mark) {
    let mut b = pane.block.lock().unwrap();
    match m {
        history::Mark::Start(cmd) => {
            if let Some(done) = b.1.take() {
                history::append(&pane.hist, &done);
            }
            let home = std::env::var("HOME").unwrap_or_default();
            let cwd = pty::cwd_of(pane.sid).map(|c| c.to_string_lossy().replacen(&home, "~", 1)).unwrap_or_default();
            let blk = history::Block { t: history::now(), cwd, cmd, code: 0, secs: 0, resume: None };
            b.0 = Some((blk, std::time::Instant::now()));
        }
        history::Mark::End(code) => {
            let Some((mut blk, at)) = b.0.take() else { return };
            (blk.code, blk.secs) = (code, at.elapsed().as_secs());
            if pane.manual.lock().unwrap().is_some() {
                b.1 = Some(blk);
            } else {
                history::append(&pane.hist, &blk);
            }
        }
    }
}

/// FR-56: agent manual sudah keluar = blok ringkas (sekali; `pump` dan `screen_watch` bisa sama-sama memanggil).
/// `code` = kode keluar dari penanda zsh bila sudah terbaca bersama prompt.
fn manual_done(v: &mut View, pane: &Pane, id: u64, prompt: &[u8], code: Option<i64>) {
    let Some((agent, cmd, dir)) = pane.manual.lock().unwrap().take() else { return };
    agent_done(v, pane, id, &agent, &cmd, dir, &code.unwrap_or_default().to_string(), prompt);
}

/// FR-56: blok ringkas ala Warp: `<folder> (<durasi>)`, perintah, lalu perintah untuk melanjutkan sesi.
/// `agent_dir` = folder kerja agent (bisa beda dari shell; dipakai mencari sesi omp).
/// `prompt` = baris prompt shell yang sudah tampil (agent manual), ditulis lagi di bawah blok.
#[allow(clippy::too_many_arguments)]
fn agent_done(v: &mut View, pane: &Pane, id: u64, agent: &str, cmd: &str, agent_dir: Option<std::path::PathBuf>, code: &str, prompt: &[u8]) {
    // agent berikutnya yang diketik di pane ini (mis. Codex tanpa hook lengkap) dinilai ulang
    pane.hooked.store(false, Ordering::Relaxed);
    let Some((started, log)) = pane.agent_log.lock().unwrap().take() else { return };
    // agent keluar = tidak ada lagi yang membaca pesan: status kosong, `gembala send` ke pane ini ditolak
    // (target_unknown) alih-alih menempel pesan ke zsh dan pengirim mengira terkirim
    *pane.status.lock().unwrap() = "";
    // 2 detik cadangan: agent manual baru terdeteksi sampai 1 detik setelah mulai
    let since = std::time::SystemTime::now() - started.elapsed() - Duration::from_secs(2);
    let secs = started.elapsed().as_secs();
    // diagnosa agent keluar sendiri (GESEKAN 3 Nov): kapan dan kode saja, isi layar tidak (SEC-13)
    eprintln!("{} pane {id}: {agent} keluar, kode {code}, {secs} dtk", history::now());
    let home = std::env::var("HOME").unwrap_or_default();
    let dir = pty::cwd_of(pane.sid);
    let cwd = dir.as_ref().map(|c| c.to_string_lossy().replacen(&home, "~", 1)).unwrap_or_default();
    let omp = |d: std::path::PathBuf| omp_session(std::path::Path::new(&home), &d, since);
    let resume = match agent {
        "claude" => {
            // FR-61: sesi tersimpan per akun, jadi lanjut lewat perintah akun pemiliknya (`claude-kantor`); sesi Utama
            // yang dilanjutkan di akun lain = `claude-utama`, karena `claude` di shell berakun memakai akun shell itu
            let owner = pane.owner.lock().unwrap().clone();
            let cmd = match owner {
                Some(o) if o.is_empty() => "claude-utama".to_string(),
                Some(o) => format!("claude-{o}"),
                None => match pane.account.lock().unwrap().as_str() {
                    "" => "claude".to_string(),
                    a => format!("claude-{a}"),
                },
            };
            let saved = |s: &String| claude_saved(std::path::Path::new(&home), s);
            pane.session.lock().unwrap().clone().filter(saved).or_else(|| printed_resume(&log)).map(|s| format!("{cmd} --resume {s}"))
        }
        "codex" => Some(printed_resume(&log).or_else(|| pane.session.lock().unwrap().clone()).map_or("codex resume --last".into(), |s| format!("codex resume {s}"))),
        "opencode" => Some(pane.session.lock().unwrap().clone().map_or("opencode --continue".into(), |s| format!("opencode --session {s}"))),
        "omp" => Some(agent_dir.or(dir).and_then(omp).map_or("omp --continue".into(), |s| format!("omp --resume {s}"))),
        _ => None,
    };
    *pane.last_output.lock().unwrap() = log;
    let cmd = msg::sanitize(cmd).unwrap_or_default();
    let text = history::block_text(&cwd, secs, code, &cmd, resume.as_deref());
    // FR-62: blok agent manual (dari penanda zsh) mendapat perintah lanjutnya; agent dari profil jadi blok sendiri
    // blok shell yang belum selesai (agent keluar terdeteksi sebelum penanda D) ikut dipakai, supaya penanda D yang
    // datang sesudahnya tidak mencatat blok kedua tanpa perintah lanjut
    let open = {
        let mut b = pane.block.lock().unwrap();
        b.1.take().or_else(|| b.0.take().map(|(blk, at)| history::Block { secs: at.elapsed().as_secs(), ..blk }))
    };
    let blk = match open {
        Some(b) => history::Block { resume: resume.clone(), ..b },
        None => history::Block { t: history::now() - secs, cwd, cmd, code: code.parse().unwrap_or_default(), secs, resume: resume.clone() },
    };
    history::append(&pane.hist, &blk);
    let mut text = [std::mem::take(&mut *pane.pre_agent.lock().unwrap()), text.into_bytes()].concat();
    text.extend_from_slice(prompt);
    let (rows, cols) = v.0.size();
    v.0 = Grid::new(rows, cols, SCROLLBACK);
    v.0.feed(&text);
    send_all(v, line(json!({"event": "pane.replace", "params": {"pane": id, "data": B64.encode(&text), "resume": resume}})));
}

/// FR-56: id sesi dari baris yang dicetak agent sendiri saat keluar: `claude --resume <uuid>` (Claude Code)
/// atau `codex resume <uuid>` (Codex).
fn printed_resume(log: &[u8]) -> Option<String> {
    let re = regex_lite::Regex::new(r"(?:--resume|codex resume) ([0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})").unwrap();
    re.captures_iter(&String::from_utf8_lossy(log)).last().map(|c| c[1].to_string())
}

/// FR-56, pengecualian SEC-15: id sesi Oh My Pi = nama file sesi terbaru di folder sesinya; isi file tidak dibuka.
// ponytail: aturan nama folder ditiru dari omp 18.13 (home = "----", di dalam home "-a-b", di luar "--a-b--");
// bila omp mengubahnya, blok kembali ke `omp --continue`
fn omp_session(home: &std::path::Path, cwd: &std::path::Path, since: std::time::SystemTime) -> Option<String> {
    let slug = |p: &std::path::Path| p.to_string_lossy().trim_matches('/').replace('/', "-");
    let dir = match cwd.strip_prefix(home) {
        Ok(r) if r.as_os_str().is_empty() => "----".to_string(),
        Ok(r) => format!("-{}", slug(r)),
        Err(_) => format!("--{}--", slug(cwd)),
    };
    let newest = std::fs::read_dir(home.join(".omp/agent/sessions").join(dir))
        .ok()?
        .flatten()
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.file_name().into_string().ok()?)))
        .filter(|(t, n)| *t >= since && n.ends_with(".jsonl"))
        .max()?;
    let id = newest.1.strip_suffix(".jsonl")?.split_once('_')?.1;
    // nama file ditulis ke terminal: hanya karakter id yang lolos
    id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-').then(|| id.to_string())
}

/// FR-56: Claude writes a transcript only after the first prompt, so the `--session-id` Gembala hands out is not
/// resumable ("No conversation found") when Claude was closed without one.
// ponytail: looks in ~/.claude* only; an account dir elsewhere falls back to the id Claude prints on exit
fn claude_saved(home: &std::path::Path, id: &str) -> bool {
    let Ok(dirs) = std::fs::read_dir(home) else { return false };
    dirs.flatten()
        .filter(|d| d.file_name().to_string_lossy().starts_with(".claude"))
        .filter_map(|d| std::fs::read_dir(d.path().join("projects")).ok())
        .flatten()
        .flatten()
        .any(|p| p.path().join(format!("{id}.jsonl")).is_file())
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
        // FR-56: agent manual sudah keluar = blok ringkas dulu, lalu output shell (prompt) tampil di bawahnya
        // ponytail: output terakhir agent yang terbaca bersama prompt dalam satu baca tidak ikut ke log sesi
        if pane.manual.lock().unwrap().is_some() && pty::busy(&pane.master, pane.sid).is_none() {
            let code = history::marks(&buf[..n]).into_iter().find_map(|m| if let history::Mark::End(c) = m { Some(c) } else { None });
            manual_done(&mut v, &pane, id, &[], code);
        }
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

    /// FR-79: nama sekarang dulu, baru nama lama (alias) pane
    fn by_name(&self, name: &str) -> Option<(u64, Arc<Pane>)> {
        let panes = self.panes.lock().unwrap();
        let find = |hit: &dyn Fn(&Pane) -> bool| panes.iter().find(|(_, p)| hit(p)).map(|(id, p)| (*id, p.clone()));
        find(&|p| *p.name.lock().unwrap() == name).or_else(|| find(&|p| p.aliases.lock().unwrap().iter().any(|a| a == name)))
    }

    /// FR-79: `name` bukan lagi alias pane mana pun
    fn release(&self, name: &str) {
        for q in self.panes.lock().unwrap().values() {
            q.aliases.lock().unwrap().retain(|a| a != name);
        }
    }

    /// FR-79: alias pindah ke `pane` (pemilik terbaru menang), yang tertua dilepas lewat `ALIAS_MAX`
    fn add_alias(&self, pane: &Arc<Pane>, alias: &str) {
        if *pane.name.lock().unwrap() == alias {
            return;
        }
        self.release(alias);
        let mut a = pane.aliases.lock().unwrap();
        a.push(alias.to_string());
        if a.len() > ALIAS_MAX {
            a.remove(0);
        }
    }

    fn workspace_of(&self, id: u64) -> String {
        self.panes.lock().unwrap().get(&id).map(|p| p.workspace.lock().unwrap().clone()).unwrap_or_default()
    }

    fn pane(&self, p: &Value) -> Result<Arc<Pane>, &'static str> {
        let id = p["pane"].as_u64().ok_or("bad_params")?;
        self.panes.lock().unwrap().get(&id).cloned().ok_or("no_such_pane")
    }

    /// SEC-3 cadangan, FR-66: pane pemilik token (constant-time); token dicabut bersama pane.
    fn pane_of_token(&self, t: &str) -> Option<u64> {
        let panes = self.panes.lock().unwrap();
        panes.iter().find(|(_, x)| x.token.lock().unwrap().as_deref().is_some_and(|k| ct_eq(k.as_bytes(), t.as_bytes()))).map(|(id, _)| *id)
    }

    /// SEC-3: peer PID ditelusuri lewat proses induk sampai sesi pane. Tool agent sering membuat sesi
    /// baru (mis. Bash Claude Code), jadi `getsid` peer saja tidak cukup.
    fn pane_of_pid(&self, mut pid: i32) -> Option<u64> {
        let panes = self.panes.lock().unwrap();
        for _ in 0..64 {
            if pid <= 1 {
                return None;
            }
            let sid = unsafe { libc::getsid(pid) };
            if let Some((id, _)) = panes.iter().find(|(_, p)| sid > 0 && p.sid == sid) {
                return Some(*id);
            }
            pid = pty::ppid(pid)?;
        }
        None
    }
}

fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(target_os = "macos")]
pub fn peer_pid(s: &UnixStream) -> io::Result<i32> {
    let mut pid: libc::pid_t = 0;
    let mut len = std::mem::size_of::<libc::pid_t>() as libc::socklen_t;
    let rc = unsafe {
        libc::getsockopt(s.as_raw_fd(), libc::SOL_LOCAL, libc::LOCAL_PEERPID, (&raw mut pid).cast(), &mut len)
    };
    if rc == -1 { Err(io::Error::last_os_error()) } else { Ok(pid) }
}

#[cfg(target_os = "linux")]
pub fn peer_pid(s: &UnixStream) -> io::Result<i32> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn judul_terminal() {
        assert_eq!(osc_title(b"x\x1b]0;\xe2\x9c\xb3 Claude Code\x07\x1b]2;\xe2\x9c\xb3 Fix login\x07y").as_deref(), Some("\u{2733} Fix login"));
        assert_eq!(osc_title(b"\x1b]2;omp\x1b\\").as_deref(), Some("omp"));
        assert_eq!(osc_title(b"\x1b]2;terpotong").as_deref(), None);
        assert_eq!(osc_title(b"tanpa judul"), None);
    }

    #[test]
    fn id_resume_dari_output_claude() {
        let log = b"\x1b[2mResume this session with:\x1b[0m\r\nclaude --resume 87cc338d-d5dc-45c7-8adc-99937a7d18a5\r\n";
        assert_eq!(printed_resume(log).as_deref(), Some("87cc338d-d5dc-45c7-8adc-99937a7d18a5"));
        assert_eq!(printed_resume(b"claude --resume x; rm -rf /"), None);
        let codex = b"To continue this session, run:\r\n\x1b[1mcodex resume 0199a213-81c0-7800-8aa1-bbab2a035a53\x1b[0m";
        assert_eq!(printed_resume(codex).as_deref(), Some("0199a213-81c0-7800-8aa1-bbab2a035a53"));
    }

    #[test]
    fn id_sesi_omp_dari_nama_file() {
        let home = std::env::temp_dir().join(format!("gomp-{}", std::process::id()));
        let since = std::time::SystemTime::now() - Duration::from_secs(5);
        for (cwd, dir) in [(home.clone(), "----"), (home.join("a/b c"), "-a-b c"), ("/private/tmp".into(), "--private-tmp--")] {
            let d = home.join(".omp/agent/sessions").join(dir);
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join("2026-11-01T01-09-10-679Z_01a194a7-3d97-72f7-b2ce-1499a5f0dbee.jsonl"), "").unwrap();
            assert_eq!(omp_session(&home, &cwd, since).as_deref(), Some("01a194a7-3d97-72f7-b2ce-1499a5f0dbee"), "{dir}");
        }
        // file lama (sebelum agent mulai) dan nama berisi karakter kendali tidak dipakai
        let later = std::time::SystemTime::now() + Duration::from_secs(60);
        assert_eq!(omp_session(&home, &home, later), None);
        std::fs::write(home.join(".omp/agent/sessions/----/2026-11-02T00-00-00-000Z_\x1b[2J.jsonl"), "").unwrap();
        assert_eq!(omp_session(&home, &home, since), None);
        std::fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn claude_session_resumable_only_with_transcript() {
        let home = std::env::temp_dir().join(format!("gclaude-{}", std::process::id()));
        let d = home.join(".claude-kantor/projects/-Users-x-repo");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("450814a1-b889-4ef9-b8f6-b6e4d68cf2d3.jsonl"), "").unwrap();
        assert!(claude_saved(&home, "450814a1-b889-4ef9-b8f6-b6e4d68cf2d3"));
        // closed before the first prompt: no transcript anywhere
        assert!(!claude_saved(&home, "bda68e32-869e-46ca-ae5b-b09d7ffb02d4"));
        std::fs::remove_dir_all(&home).unwrap();
    }
}
