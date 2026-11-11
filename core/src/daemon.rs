//! Daemon: kanal UI + kanal pane, registri pane, attach (TECH bagian 8, pekerjaan 0.3).
//!
//! UI channel (SEC-20): `hello` must carry the secret the daemon creates at start and only
//! hands to its launcher through a descriptor; connections from inside a pane session are rejected even
//! with the right secret. Pane channel (SEC-3): identity from `getsid(peer_pid)`, not the message content.

use crate::account;
use crate::addon;
use crate::bus;
use crate::config;
use crate::msg;
use crate::git;
use crate::profile;
use crate::recipe;
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
/// vt100 scrollback of the daemon grid: attach history, FR-56 and the FR-62 log come from the raw byte ring (`Grid::mark`,
/// `rewind`, `history`), `read`/`wait` only see the visible screen. One vt100 row is ~5 KB, hence no scrollback.
const SCROLLBACK: usize = 0;
/// FR-62 / SEC-13: main screen log on disk = last 1,000 rows (user decision 3 Nov 2026)
const LOG_LINES: usize = 1_000;
/// Queue per connection. Full = client too slow: its connection is dropped (re-attach gets a snapshot)
/// so daemon RAM doesn't balloon and agents aren't held up.
const QUEUE: usize = 256;
/// FR-79: old names per pane that still work as addresses
const ALIAS_MAX: usize = 8;
/// FR-75: helper panes (alive + waiting cards) per requesting agent; a helper's first task waits this long after launch
const HELPERS_MAX: usize = 4;
const HELPER_GRACE: Duration = Duration::from_secs(4);

#[derive(Clone, Copy, PartialEq)]
enum Chan {
    Ui,
    Pane,
}

struct Pane {
    master: File,
    /// Pane shell PID = session id of all its processes (the shell runs under `setsid`).
    sid: i32,
    /// Automatic name `<agent>-<n>` (FR-30), renamable, unique per daemon (FR-3); the profile's agent ("shell" without a profile).
    name: Mutex<String>,
    /// FR-79: old names of this pane (oldest first, max `ALIAS_MAX`), still addresses for `send`/`wait`/`read`
    aliases: Mutex<Vec<String>>,
    agent: String,
    /// FR-4: idle | working | blocked | done; empty = not tracked (shell, profile without hooks)
    status: Mutex<&'static str>,
    claude: Mutex<claude::Cache>,
    /// FR-53, FR-68: agent session id in this pane (validated per agent): from Gembala (Claude profile), the `--session-id`
    /// argument of a manual agent, or a hook (Codex `thread-id`, OpenCode `sessionID`). For exact session resume.
    session: Mutex<Option<String>>,
    /// FR-56: output of the running agent session (since the start marker) and of the last finished session; RAM only (SEC-13)
    agent_log: Mutex<Option<(std::time::Instant, Vec<u8>)>>,
    last_output: Mutex<Vec<u8>>,
    /// FR-56: ring position (`Grid::mark`) at the start of the last prompt (OSC 133 A marker) and the history before the agent ran;
    /// the summary block is written below it when the agent exits, earlier command history stays (like Warp).
    /// Key order: `view` first.
    prompt_at: Mutex<Option<u64>>,
    /// FR-56: history before the last command, recorded when the command starts (OSC 133 C). A manual agent is only
    /// detected ±1 second later, by which time Claude has cleared the screen and scrollback
    pre_cmd: Mutex<Option<u64>>,
    pre_agent: Mutex<Option<u64>>,
    /// FR-62: history key of the layout pane (`<key>.jsonl`, `<key>.log`), the same across reruns and daemon restarts
    key: String,
    /// FR-62: this pane's block history file; the running block (since OSC 133 C) and the manual agent block that
    /// has finished but is waiting for its resume command from `agent_done`.
    hist: std::path::PathBuf,
    /// FR-69: last terminal title (OSC 0/2) set by a program in the pane; RAM only (SEC-13)
    title: Mutex<String>,
    /// FR-70: potretan per giliran agent (antrean `snap::queue`)
    turns: Arc<Mutex<snap::Turns>>,
    /// FR-61: agent account id of this pane; empty = Utama. Typed agent: from its process's `CLAUDE_CONFIG_DIR`
    account: Mutex<String>,
    /// FR-61: account owning the running Claude session when it differs from the pane account ("" = Utama); from the SessionStart hook
    owner: Mutex<Option<String>>,
    block: Mutex<OpenBlocks>,
    /// FR-56: agent typed by hand in a shell pane (process name, full command, agent working folder).
    /// Key order: `view`, then `manual`.
    manual: Mutex<Option<(String, String, Option<std::path::PathBuf>)>>,
    /// FR-4 fallback: when the last output arrived; `custom` profiles are tracked from the screen
    last_out: Mutex<std::time::Instant>,
    /// FR-59: shell is at the prompt (last OSC 133 marker is B, no C yet); sent with attach since the snapshot has no OSC
    at_prompt: std::sync::atomic::AtomicBool,
    /// FR-4: this manual agent sends full hooks (Claude, omp, OpenCode; not Codex, which only has `notify`):
    /// status from hooks only. Without this, Claude's status line redraws read as working then done over and over (notifications pile up)
    hooked: std::sync::atomic::AtomicBool,
    /// FR-84: the agent's folder-trust dialog was on screen at the last check (`screen_watch`), and when it went away
    trust: std::sync::atomic::AtomicBool,
    trust_off: Mutex<Option<std::time::Instant>>,
    screen: bool,
    started: std::time::Instant,
    /// Phase 2: workspace (access rules SEC-4/21), recipient policy (SEC-5), when the status last changed
    /// (stable 500 ms, SEC-28), readable by agents (shell pane, SEC-21), last user keystroke (FR-11)
    workspace: Mutex<String>,
    policy: Mutex<msg::Policy>,
    status_at: Mutex<std::time::Instant>,
    /// FR-80: bumped on every status change; a Telegram permission button only works for the same dialog (SEC-28 pattern)
    status_seq: AtomicU64,
    /// FR-80: terminal was waiting for a password at the last check, a shell command that ran long has just finished (secs)
    asked_pw: std::sync::atomic::AtomicBool,
    long_done: Mutex<Option<u64>>,
    /// FR-80: Telegram messages typed into this pane, answered (👍 + result) when the pane is done
    tg_ask: Mutex<Vec<i64>>,
    /// FR-80: last text typed from the phone; the result snippet starts at its echo on screen
    tg_text: Mutex<Option<String>>,
    /// FR-80: the agent's last answer of this turn from its stop hook (RAM only, SEC-13); the "done" message sends it
    /// instead of the screen. Cleared when a new turn starts.
    last_msg: Mutex<Option<String>>,
    /// FR-16: when the agent's own hook last said its turn ended; the harness waits for it on Codex (see `runner::stage_status`)
    hook_done: Mutex<Option<std::time::Instant>>,
    readable: std::sync::atomic::AtomicBool,
    /// FR-75: pane opened through an agent's request card = that agent's helper (counts to its limit, can't ask for panes)
    helper_of: Mutex<Option<u64>>,
    /// FR-86: when this pane last replied or (after that) got a new message; a quiet helper is closed `[ui] helper_idle_close` later
    replied_at: Mutex<Option<std::time::Instant>>,
    typed_at: Mutex<Option<std::time::Instant>>,
    /// SEC-3 fallback: 128-bit token for processes detached from the pane session (e.g. a tmux server, profile `token = true`)
    /// or that can't use sockets (FR-66: sandboxed Codex, created through `gembala token`)
    token: Mutex<Option<String>>,
    /// Grid and subscribers are locked together: attach never loses or duplicates output.
    view: Mutex<(Grid, Vec<Sub>)>,
}

impl Pane {
    /// Agent from a profile, or an agent typed by hand in a shell pane (FR-56): shows in `status`, can be messaged
    /// and read like any other agent. A plain shell pane still falls under SEC-21.
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
    /// connection id: one subscription per connection per pane (re-attach doesn't duplicate events)
    conn: u64,
}

static NEXT_CONN: AtomicU64 = AtomicU64::new(1);

struct State {
    panes: Mutex<HashMap<u64, Arc<Pane>>>,
    next: AtomicU64,
    secret: Option<String>,
    /// SEC-20: UI signature requirement (from the daemon's own certificate); `None` = secret only
    ui_req: Option<String>,
    pane_sock: String,
    /// UI channel clients currently connected (FR-20: the daemon stops at 0 when every pane is idle).
    ui_clients: AtomicUsize,
    /// Event queues of all UI clients, for events that don't belong to one pane (`config.changed`).
    /// Removed when a client disconnects (UiClient), so its writer thread ends too.
    ui_txs: Mutex<Vec<(u64, SyncSender<String>)>>,
    ui_next: AtomicU64,
    /// Config in effect + its errors (FR-42). Broken file = the last valid values stay in use.
    config: Mutex<Value>,
    profiles: Mutex<Vec<profile::Profile>>,
    /// FR-61: `[[account]]` from config
    accounts: Mutex<Vec<account::Account>>,
    /// FR-78: add-ons from config
    addons: Mutex<addon::Addons>,
    /// Last agent detection result (FR-28); Null = never ran.
    agents: Mutex<Value>,
    /// Random id per daemon run: a pane id from another run must not be taken for the same pane (FR-45).
    boot: String,
    hooks: profile::Hooks,
    engine: Mutex<bus::Engine>,
    /// FR-8: status event subscriptions from the pane channel (subscriber, its queue), filtered by SEC-21 on send
    pane_subs: Mutex<Vec<(u64, SyncSender<String>)>>,
    audit: Option<bus::Audit>,
    /// FR-16/17: active harness runs, panes they hold, sender ids (runner.rs)
    harness: Mutex<runner::Registry>,
    /// FR-80: the user's Telegram bot, when on (telegram.rs)
    tg: telegram::Slot,
    /// FR-82: last answer of the GitHub release check (update.rs)
    update: Mutex<Value>,
    /// FR-102: allowance left per agent login (usage.rs)
    usage: Mutex<usage::Store>,
    /// FR-100: the last report draft asked for a window screenshot (the user takes it; the agent never sees it)
    report_shot: Mutex<bool>,
}

struct UiClient(Arc<State>, u64);

impl Drop for UiClient {
    fn drop(&mut self) {
        self.0.ui_clients.fetch_sub(1, Ordering::Relaxed);
        self.0.ui_txs.lock().unwrap().retain(|(id, _)| *id != self.1);
    }
}

mod runner;
mod telegram;
mod claude;
mod update;
mod usage;

type Res = Result<Value, &'static str>;
/// FR-62: (running block + its start time, manual agent block waiting for its resume command)
type OpenBlocks = (Option<(history::Block, std::time::Instant)>, Option<history::Block>);

pub fn run(dir: &Path, secret: Option<String>, on_ready: impl FnOnce()) -> io::Result<()> {
    // FR-62: SIGTERM (kill, logout, laptop restart) is blocked before any thread starts, so every thread inherits that and
    // only `on_sigterm` takes it. Pane processes start with a clean mask (std resets it when spawning).
    let term = unsafe {
        let mut s: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut s);
        libc::sigaddset(&mut s, libc::SIGTERM);
        libc::pthread_sigmask(libc::SIG_BLOCK, &s, std::ptr::null_mut());
        s
    };
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
        harness: Mutex::default(),
        tg: Mutex::default(),
        update: Mutex::default(),
        usage: Mutex::default(),
        report_shot: Mutex::default(),
    });
    let st7 = st.clone();
    thread::spawn(move || deliver_loop(st7));
    write_hook_files(&st.hooks);
    reload_config(&st);
    telegram::start_saved(&st);
    update::start(&st);
    usage::start(&st);
    on_ready();
    let st3 = st.clone();
    thread::spawn(move || idle_exit(st3));
    let st9 = st.clone();
    thread::spawn(move || save_logs_loop(st9)); // FR-62: screen log to disk, history older than 14 days dropped
    let st10 = st.clone();
    thread::spawn(move || on_sigterm(st10, term));
    let (st8, run_dir) = (st.clone(), dir.to_path_buf());
    thread::spawn(move || spool(st8, run_dir));
    let st4 = st.clone();
    thread::spawn(move || watch_config(st4));
    let st6 = st.clone();
    thread::spawn(move || screen_watch(st6));
    let st5 = st.clone();
    thread::spawn(move || *st5.agents.lock().unwrap() = profile::detect()); // FR-28: on first open
    let st2 = st.clone();
    thread::spawn(move || accept(pn, st2, Chan::Pane));
    accept(ui, st, Chan::Ui);
    Ok(())
}

fn bind(p: &Path) -> io::Result<UnixListener> {
    if UnixStream::connect(p).is_ok() {
        return Err(io::Error::new(io::ErrorKind::AddrInUse, format!("daemon already running: {}", p.display())));
    }
    let _ = fs::remove_file(p); // stale socket from a dead daemon
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

/// Session over (normal or rejected): the writer gets an empty marker, drains the queue, then shuts down
/// the socket both ways. The client doesn't wait forever and the last reply still arrives.
struct CloseAfterFlush(SyncSender<String>, UnixStream);

impl Drop for CloseAfterFlush {
    fn drop(&mut self) {
        if self.0.try_send(String::new()).is_err() {
            let _ = self.1.shutdown(std::net::Shutdown::Both);
        }
    }
}

fn client(s: UnixStream, st: Arc<State>, chan: Chan) -> io::Result<()> {
    // SEC-2: other OS accounts are rejected before anything else (second layer on top of socket 0600 + folder 0700)
    if peer_uid(&s).ok() != Some(unsafe { libc::getuid() }) {
        return Ok(());
    }
    let peer = peer_pid(&s)?;
    // SEC-20: a UI signed like the daemon is recognized from the audit token at connect, no secret
    let signed = chan == Chan::Ui && st.ui_req.as_deref().is_some_and(|r| crate::sign::peer_ok(&s, r));
    let (tx, rx) = sync_channel::<String>(QUEUE);
    let conn = NEXT_CONN.fetch_add(1, Ordering::Relaxed);
    let mut w = s.try_clone()?;
    let sock = s.try_clone()?;
    // one writer per connection: replies and events don't cut into each other
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
    // pane channel: identity is fixed once at hello (getsid or token), not from message content
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
            // identity from hello (getsid or token), not from message content (SEC-3); a closed pane = invalid
            let me = ident.filter(|id| st.panes.lock().unwrap().contains_key(id));
            let r = match (method, me) {
                ("subscribe", Some(me)) => {
                    st.pane_subs.lock().unwrap().push((me, tx.clone()));
                    Ok(json!({}))
                }
                ("send" | "reply" | "wait" | "read" | "status" | "token" | "addons" | "close" | "report" | "usage", Some(me)) => pane_method(&st, me, method, p),
                // "start" = Claude SessionStart: session id, its owner, and idle status (except a compact mid-turn)
                ("hook", Some(me)) => match p["status"].as_str().and_then(|x| ["idle", "working", "blocked", "done", "start"].into_iter().find(|s| *s == x)) {
                    Some(status) => {
                        // FR-66: last message of a sandboxed agent's turn = reply to the waiting message
                        if let Some(text) = p["reply"].as_str() {
                            auto_reply(&st, me, text);
                        }
                        let pane = st.panes.lock().unwrap().get(&me).cloned();
                        if let Some(pane) = pane {
                            // A short manually launched turn can stop before the process watcher finds its account.
                            if p["agent"].as_str() == Some("claude") {
                                *pane.account.lock().unwrap() = acct_of_dir(&st, p["dir"].as_str().map(String::from));
                            }
                            // FR-68: session id from the agent hook (Codex thread-id, OpenCode sessionID, Claude session_id)
                            if let (Some(agent), Some(id)) = (p["agent"].as_str(), p["session"].as_str())
                                && profile::valid_session(agent, id)
                            {
                                // FR-61: new session or session switch (`claude -r` picker) = check its owning account
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
                            // FR-80: set before the status, the "done" notice reads it
                            match (status, p["last"].as_str()) {
                                ("working" | "start", _) => *pane.last_msg.lock().unwrap() = None,
                                (_, Some(t)) => *pane.last_msg.lock().unwrap() = Some(t.chars().take(4000).collect()),
                                _ => {}
                            }
                            if matches!(status, "working" | "start") {
                                pane.claude.lock().unwrap().clear();
                            }
                            if status == "done" {
                                *pane.hook_done.lock().unwrap() = Some(std::time::Instant::now());
                            }
                            if matches!(status, "done" | "blocked" | "idle") && p["agent"].as_str() == Some("claude") {
                                claude::refresh(&st, me, &pane, Some(status));
                            } else if status != "start" {
                                set_status(&st, me, &pane, status, None);
                            } else if p["source"].as_str() != Some("compact") {
                                // SessionStart = agent waiting for a prompt; the screen guess before the first hook (startup spinner
                                // of a typed `claude` = "working") is no longer corrected by the screen once the pane has hooks
                                set_status(&st, me, &pane, "idle", None);
                            }
                        }
                        Ok(json!({}))
                    }
                    None => Err("bad_params"),
                },
                // ponytail: pane methods (send/wait/read/reply/status) arrive in Phase 2
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
                    // order marker in the same queue: pane events before it are already in the snapshot, the ones after aren't.
                    // The UI (replies and events take different paths in Tauri) drops events before the marker; without this, events
                    // held during attach get written twice and TUI redraws (relative cursor) get garbled
                    let _ = tx.try_send(line(json!({"event": "pane.attached", "params": {"pane": p["pane"]}})));
                    // re-attach from the same connection (tab moved to another workspace) replaces the old subscription, doesn't add one
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
        // SEC-3 fallback: a process outside the pane session proves itself with the pane token (revoked when the pane closes)
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

/// FR-3: lowercase, digits, '-', max 32
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
        // `msg` is used by Settings > Security so the brake numbers aren't written twice in the UI
        "daemon.info" => Ok(json!({"boot": st.boot, "daemon": env!("CARGO_PKG_VERSION"), "proto": PROTO,
                                   "msg": {"rate_per_min": crate::msg::RATE_PER_MIN, "hop_max": crate::msg::HOP_MAX}})),
        "git.status" | "git.diff" => {
            // the pane's own working folder (foreground process or shell), not from a parameter
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
        // FR-71: worktrees only through the UI channel (SEC-20); repo folder from the pane, not a parameter
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
        // FR-70: a pane's turn list, the diff of one turn, restore to before a turn (UI channel only, SEC-20)
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
            // warning: another pane changed the same repo since this turn started, or HEAD moved
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
            // structure only from the UI; its size is already capped at 64 KB per protocol line
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
                // SEC-12 exception: the confirm card shows exactly what will be sent (what's approved = what arrives)
                "pending": eng.pending.iter().map(|m| { let mut v = m.summary(); v["body"] = json!(m.body); v }).collect::<Vec<_>>(),
                // FR-75: same exception, the open card shows the task as it will be sent
                "opens": eng.opens.iter().map(|o| json!({"id": o.msg.id, "from": o.msg.from_name, "from_pane": o.msg.from, "target": o.msg.to_name, "profile": o.profile,
                    "account": o.account, "cwd": o.cwd, "workspace": o.workspace, "body": o.msg.body})).collect::<Vec<_>>(),
                "queued": queues,
                // FR-100: drafts an agent made for the user to review; title = first line
                "reports": eng.reports.iter().map(|r| {
                    let (title, text) = r.body.split_once('\n').unwrap_or((&r.body, ""));
                    json!({"id": r.id, "from": r.from_name, "from_pane": r.from, "title": title, "body": text})
                }).collect::<Vec<_>>(),
                "history": eng.history,
            }))
        }
        "msg.approve" | "msg.reject" => {
            // FR-12: only the UI channel (a human) can decide a confirm card (SEC-20)
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
        // FR-100: the user dropped an agent's draft, or opened it in the report dialog (it leaves the list either way)
        "report.discard" => {
            let id = p["id"].as_str().ok_or("bad_params")?;
            let mut eng = st.engine.lock().unwrap();
            let i = eng.reports.iter().position(|r| r.id == id).ok_or("no_such_msg")?;
            eng.reports.remove(i);
            Ok(json!({}))
        }
        // FR-100: what goes under "Environment" in a report, and the daemon log tail only when asked; secrets masked, home = ~
        "report.env" => {
            let macos = std::process::Command::new("/usr/bin/sw_vers").arg("-productVersion").output().ok().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
            let agents: Vec<Value> = st.agents.lock().unwrap().as_array().into_iter().flatten().filter(|a| a["installed"] == true)
                .map(|a| json!({"name": a["name"], "version": a["version"]})).collect();
            let log = p["log"].as_bool().unwrap_or(false).then(|| {
                let path = std::path::Path::new(&st.pane_sock).parent().unwrap_or(std::path::Path::new("/")).join("daemon.log");
                let text = fs::read_to_string(path).unwrap_or_default();
                let lines: Vec<&str> = text.lines().collect();
                report_clean(&lines[lines.len().saturating_sub(50)..].join("\n"))
            });
            let shot = std::mem::take(&mut *st.report_shot.lock().unwrap());
            Ok(json!({"daemon": env!("CARGO_PKG_VERSION"), "proto": PROTO, "macos": macos, "agents": agents, "log": log, "screenshot": shot}))
        }
        "open.approve" | "open.reject" => {
            // FR-75: only a human opens a pane an agent asked for (SEC-20). Approve = the UI already opened `pane`
            // in the requester's tab; the task waits in that pane's queue until its agent is up.
            let id = p["id"].as_str().ok_or("bad_params")?;
            let ws = st.engine.lock().unwrap().opens.iter().find(|o| o.msg.id == id).map(|o| o.workspace.clone()).ok_or("no_such_msg")?;
            let target = if m == "open.approve" {
                let pane = st.pane(p)?;
                if pane.agent_name().is_none() || *pane.workspace.lock().unwrap() != ws || pane.helper_of.lock().unwrap().is_some() {
                    return Err("bad_params");
                }
                Some(pane)
            } else {
                None
            };
            let o = {
                let mut eng = st.engine.lock().unwrap();
                let i = eng.opens.iter().position(|o| o.msg.id == id).ok_or("no_such_msg")?;
                eng.opens.remove(i)
            };
            let Some(pane) = target else {
                return Ok(open_refused(st, &o.msg, "rejected_by_user"));
            };
            *pane.helper_of.lock().unwrap() = Some(o.msg.from);
            let mut msg = o.msg;
            msg.to = p["pane"].as_u64().unwrap_or_default();
            msg.to_name = pane.name.lock().unwrap().clone();
            let mut eng = st.engine.lock().unwrap();
            eng.remember(&msg);
            eng.queues.entry(msg.to).or_default().push_back(bus::Queued { msg: msg.clone(), since: std::time::Instant::now(), approved: true });
            drop(eng);
            record(st, "queued", &msg, None);
            Ok(json!({"result": "queued", "id": msg.id}))
        }
        "msg.pause" => {
            // SEC-10: stop button; the queue is held, not dropped
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
            // human send from the UI: label "user"; may force to `unknown`, never to `blocked`
            let to = st.pane(&json!({"pane": p["to"]}))?;
            let body = msg::sanitize(p["body"].as_str().ok_or("bad_params")?)?;
            let status = *to.status.lock().unwrap();
            if p["to"].as_u64().is_some_and(|id| st.harness.lock().unwrap().is_locked(id)) {
                return Err("driver_locked");
            }
            let force = p["force"].as_bool() == Some(true);
            if status == "blocked" {
                return Err("target_blocked");
            }
            if !(matches!(status, "idle" | "done") || force) {
                return Err("target_busy");
            }
            let id = st.engine.lock().unwrap().new_id(&st.boot);
            (&to.master).write_all(&msg::wrap("user", "", &body)).map_err(|_| "io")?;
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
        // FR-61: account list + state (used, email); Utama is always there
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
            // config first (full validation, duplicate id/folder rejected), then folder and links
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
            // FR-61 (7): the account folder is removed too once config no longer refers to it
            match dir.map(|d| account::delete_dir(&d)) {
                Some(Err(why)) => Ok(why.reply()),
                _ => Ok(json!({})),
            }
        }
        // FR-61 (8): model/effort/trust, or one item becomes Follow Utama/Own (`item` + `follow`)
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
        // FR-16, FR-73: harness runs. UI channel only (SEC-20): the pane channel has no harness method.
        "harness.preflight" | "harness.start" | "harness.state" | "harness.stop" | "harness.gate" | "harness.takeover" | "harness.release" | "harness.close_panes" => runner::method(st, m, p),
        // FR-74, SEC-27: harness recipes. Approve and delete exist on the UI channel only; the pane channel has no recipe method.
        "recipe.list" | "recipe.get" | "recipe.approve" | "recipe.delete" | "recipe.validate" | "recipe.duplicate" | "recipe.draft_new" | "recipe.draft_get" | "recipe.draft_apply" | "recipe.library" | "recipe.render" | "recipe.save" => {
            let store = recipe::Store::new(config::path().parent().unwrap_or(std::path::Path::new("/")));
            // agent kinds found on this Mac; unknown until detection ran once, then nothing is flagged as missing
            let found = st.agents.lock().unwrap().clone();
            let installed: Option<Vec<String>> = found.as_array().map(|a| a.iter().filter(|r| r["installed"] == true).filter_map(|r| r["agent"].as_str().map(String::from)).collect());
            let inst = installed.as_deref();
            let id = || p["id"].as_str().ok_or("bad_params");
            let r = match m {
                "recipe.list" => {
                    if let Err(e) = store.ensure_defaults() {
                        eprintln!("gembala daemon: recipe defaults: {e}");
                    }
                    Ok(store.list(inst))
                }
                "recipe.get" => store.get(id()?, inst),
                "recipe.approve" => store.approve(id()?, p["hash"].as_str().ok_or("bad_params")?, inst).map(|_| json!({})),
                "recipe.delete" => store.delete(id()?).map(|_| json!({})),
                "recipe.duplicate" => store.duplicate(id()?, p["to"].as_str().ok_or("bad_params")?).map(|_| json!({})),
                // 3b builder (B1): cards to add, templates for "+ Resep baru", and which card fields each agent uses
                "recipe.library" => {
                    let agents: Vec<Value> = profile::agent_ids()
                        .map(|a| json!({"agent": a, "installed": inst.is_none_or(|i| i.iter().any(|x| x == a)), "flags": runner::supports(a)}))
                        .collect();
                    Ok(json!({
                        "steps": recipe::library().iter().map(recipe::step_json).collect::<Vec<_>>(),
                        "templates": recipe::templates(store.defaults()).iter().map(|(id, r)| json!({"id": id, "recipe": recipe::to_json(r)})).collect::<Vec<_>>(),
                        "agents": agents,
                    }))
                }
                // tinjau: text + rules for what the builder holds; nothing saved until recipe.save with the shown hash
                "recipe.render" => store.render(id()?, &recipe::from_json(&p["recipe"]), inst),
                "recipe.save" => store.save_rendered(id()?, p["hash"].as_str().ok_or("bad_params")?, inst).map(|_| json!({})),
                // FR-77 Atur pakai AI (H5): the brief goes to the agent pane the UI just opened, labeled as the user's own
                // (the user pressed Rumuskan); the agent writes only the draft file in the run folder
                "recipe.draft_new" => {
                    let pane_id = p["pane"].as_u64().ok_or("bad_params")?;
                    let pane = st.pane(p)?;
                    if pane.agent_name().is_none() {
                        return Err("bad_params");
                    }
                    let rid = id()?;
                    let ask = msg::sanitize(p["ask"].as_str().ok_or("bad_params")?)?.chars().take(4000).collect::<String>();
                    let current = store.get(rid, inst).ok().and_then(|v| v["text"].as_str().map(String::from));
                    let draft = new_secret().map_err(|_| "io")?[..16].to_string();
                    let path = drafts_dir(st).join(format!("{draft}.toml"));
                    fs::create_dir_all(drafts_dir(st)).map_err(|_| "io")?;
                    queue_user(st, pane_id, &pane, recipe::brief(&ask, rid, current.as_deref(), &path));
                    Ok(json!({"draft": draft, "path": path}))
                }
                // what the agent wrote so far + the rules applied to it + the recipe now, for the diff (layar 21)
                "recipe.draft_get" => {
                    let path = draft_path(st, p)?;
                    let text = match fs::symlink_metadata(&path) {
                        Ok(m) if m.is_file() && m.len() <= 64 * 1024 => fs::read_to_string(&path).ok(),
                        _ => None,
                    };
                    let current = store.get(id()?, inst).ok().and_then(|v| v["text"].as_str().map(String::from)).unwrap_or_default();
                    Ok(match text {
                        None => json!({"exists": false, "current": current}),
                        Some(t) => {
                            let r = store.parse(&t, id()?, inst);
                            json!({"exists": true, "text": t, "hash": recipe::hash(&t), "current": current, "recipe": r.recipe.as_ref().map(recipe::summary),
                                   "errors": r.errors.iter().map(crate::diag::Diag::json).collect::<Vec<_>>(), "warnings": r.warnings.iter().map(crate::diag::Diag::json).collect::<Vec<_>>()})
                        }
                    })
                }
                // "Setujui dan simpan": only the bytes the screen showed (hash), then the draft is gone
                "recipe.draft_apply" => {
                    let path = draft_path(st, p)?;
                    let text = fs::read_to_string(&path).map_err(|_| "no_such_draft")?;
                    if recipe::hash(&text) != p["hash"].as_str().ok_or("bad_params")? {
                        return Ok(crate::diag::Diag::new("recipe_changed").reply());
                    }
                    let r = store.save_approved(id()?, &text, inst).map(|_| json!({}));
                    if r.is_ok() {
                        let _ = fs::remove_file(&path);
                    }
                    r
                }
                // preview of text not saved yet (Atur pakai AI, H5): same rules, no file touched
                _ => {
                    let text = p["text"].as_str().ok_or("bad_params")?;
                    let r = store.parse(text, "preview", inst);
                    Ok(json!({"recipe": r.recipe.as_ref().map(recipe::summary), "errors": r.errors.iter().map(crate::diag::Diag::json).collect::<Vec<_>>(),
                              "warnings": r.warnings.iter().map(crate::diag::Diag::json).collect::<Vec<_>>()}))
                }
            };
            Ok(r.unwrap_or_else(|d| d.reply()))
        }
        // FR-77 target 2 (H5): Gembala's own config.toml through a draft; guarded parts can't change (config::guarded_changes)
        "config.draft_new" => {
            let pane_id = p["pane"].as_u64().ok_or("bad_params")?;
            let pane = st.pane(p)?;
            if pane.agent_name().is_none() {
                return Err("bad_params");
            }
            let ask = msg::sanitize(p["ask"].as_str().ok_or("bad_params")?)?.chars().take(4000).collect::<String>();
            let section: String = p["section"].as_str().unwrap_or_default().chars().filter(|c| c.is_ascii_alphanumeric() || *c == '_').take(32).collect();
            let current = fs::read_to_string(config::path()).unwrap_or_default();
            let draft = new_secret().map_err(|_| "io")?[..16].to_string();
            let path = drafts_dir(st).join(format!("{draft}.toml"));
            fs::create_dir_all(drafts_dir(st)).map_err(|_| "io")?;
            queue_user(st, pane_id, &pane, config::ai_brief(&ask, &section, &current, &path));
            Ok(json!({"draft": draft, "path": path}))
        }
        "config.draft_get" => {
            let path = draft_path(st, p)?;
            let current = fs::read_to_string(config::path()).unwrap_or_default();
            let text = match fs::symlink_metadata(&path) {
                Ok(m) if m.is_file() && m.len() <= 256 * 1024 => fs::read_to_string(&path).ok(),
                _ => None,
            };
            Ok(match text {
                None => json!({"exists": false, "current": current}),
                Some(t) => {
                    let c = config::load(Some(&t));
                    let mut errors: Vec<Value> = c.errors.iter().map(crate::diag::Diag::json).collect();
                    errors.extend(config::guarded_changes(&current, &t).into_iter().map(|k| crate::diag::Diag::new("ai_guarded").arg("key", k).json()));
                    json!({"exists": true, "text": t, "hash": recipe::hash(&t), "current": current, "broken": c.broken, "errors": errors, "warnings": []})
                }
            })
        }
        "config.draft_apply" => {
            let path = draft_path(st, p)?;
            let text = fs::read_to_string(&path).map_err(|_| "no_such_draft")?;
            let current = fs::read_to_string(config::path()).unwrap_or_default();
            if recipe::hash(&text) != p["hash"].as_str().ok_or("bad_params")? {
                return Ok(crate::diag::Diag::new("recipe_changed").reply());
            }
            let c = config::load(Some(&text));
            if c.broken || !c.errors.is_empty() || !config::guarded_changes(&current, &text).is_empty() {
                return Ok(crate::diag::Diag::new("ai_draft_refused").reply());
            }
            config::write_atomic(&config::path(), text.as_bytes()).map_err(|_| "io")?;
            let _ = fs::remove_file(&path);
            Ok(json!({}))
        }
        // FR-77 target 3: AGENTS.md / CLAUDE.md belong to the user; Gembala only sends the brief, the agent edits the file
        // through its own permission dialog and the diff shows in the changes panel (SEC-15: Gembala writes nothing)
        "agent_file.brief" => {
            let pane_id = p["pane"].as_u64().ok_or("bad_params")?;
            let pane = st.pane(p)?;
            let file = p["file"].as_str().filter(|f| ["AGENTS.md", "CLAUDE.md"].contains(f)).ok_or("bad_params")?;
            if pane.agent_name().is_none() {
                return Err("bad_params");
            }
            let ask = msg::sanitize(p["ask"].as_str().ok_or("bad_params")?)?.chars().take(4000).collect::<String>();
            queue_user(st, pane_id, &pane, format!(
                "Gembala \"Atur pakai AI\": update the agent instructions file `{file}` in this folder (create it if missing) as the user asks.\n\n\
Request from the user, as written:\n{ask}\n\n\
Read the file first, change only what the request asks, keep its structure and language. Edit only `{file}`. \
The user reviews the change in Gembala's changes panel. When done, summarize the change in 1-2 sentences."));
            Ok(json!({}))
        }
        "pane.create" => create_pane(st, p, &[]),
        "pane.write" => {
            let pane = st.pane(p)?;
            let data = p["data"].as_str().and_then(|d| B64.decode(d).ok()).ok_or("bad_params")?;
            type_into(st, p["pane"].as_u64().unwrap_or_default(), &pane, &data)?;
            Ok(json!({}))
        }
        "telegram.status" => Ok(telegram::status(st)),
        // FR-102: allowance left per agent login; new numbers arrive as `usage.changed`
        "usage.list" => Ok(usage::status(st)),
        // FR-82: version check; "check" answers at once, the result comes as `update.changed`
        "update.status" => Ok(update::status(st)),
        "update.check" => {
            update::check_now(st);
            Ok(update::status(st))
        }
        "telegram.pair" => telegram::pair(st, p["token"].as_str().unwrap_or_default()),
        "telegram.unpair" => Ok(telegram::unpair(st)),
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
            // FR-56: full output of the last agent session, while the pane lives
            let pane = st.pane(p)?;
            let data = pane.last_output.lock().unwrap().clone();
            Ok(json!({"data": B64.encode(&data), "bytes": data.len()}))
        }
        "pane.rename" => {
            // FR-3: lowercase, digits, '-', max 32, unique
            let pane = st.pane(p)?;
            let name = p["name"].as_str().filter(|n| valid_name(n)).ok_or("bad_name")?;
            if st.panes.lock().unwrap().values().any(|q| !Arc::ptr_eq(q, &pane) && *q.name.lock().unwrap() == name) {
                return Err("name_taken");
            }
            let old = std::mem::replace(&mut *pane.name.lock().unwrap(), name.to_string());
            // FR-79: the old name becomes an alias; the new name is released from any pane's aliases (current names win)
            st.release(name);
            st.add_alias(&pane, &old);
            Ok(json!({"name": name, "aliases": *pane.aliases.lock().unwrap()}))
        }
        "pane.info" => {
            let pane = st.pane(p)?;
            let busy = pty::busy(&pane.master, pane.sid);
            let (waiting_shell, reason) = { let c = pane.claude.lock().unwrap(); (c.shell, c.reason.clone()) };
            Ok(json!({
                "busy": busy.is_some(), "name": busy.and_then(|(pid, _)| pty::proc_name(pid)), "pid": pane.sid, // FR-60
                "shell": pty::proc_name(pane.sid), // Gembala.pen 02: subjudul tab shell diam (`zsh`)
                "password": busy.is_some() && pty::wants_password(&pane.master),
                "pane_name": *pane.name.lock().unwrap(), "agent": pane.agent, "status": *pane.status.lock().unwrap(),
                "waiting_shell": waiting_shell, "reason": reason,
                "policy": pane.policy.lock().unwrap().name(), "readable": pane.readable.load(Ordering::Relaxed),
                "session": *pane.session.lock().unwrap(), // FR-68: resume card even if the daemon stopped
                "title": *pane.title.lock().unwrap(), // FR-69: label pane
                "account": acct_label(st, &pane.account.lock().unwrap()), // FR-61: account chip
                "account_id": *pane.account.lock().unwrap(), // FR-68: `claude-<account>` resume card even if the daemon stopped
                // FR-61: session owned by another account: name for the label, id for Resume ("utama" = Utama)
                "owner": pane.owner.lock().unwrap().as_ref().map(|o| if o.is_empty() { "Utama".into() } else { acct_label(st, o) }),
                "owner_id": pane.owner.lock().unwrap().as_ref().map(|o| if o.is_empty() { "utama" } else { o.as_str() }.to_string()),
                "cwd": pty::cwd_of(busy.map_or(pane.sid, |(pid, _)| pid)).or_else(|| pty::cwd_of(pane.sid)),
            }))
        }
        "pane.kill" => {
            // FR-50: SIGTERM to the running process, SIGKILL if it's still alive after 3 seconds; the pane shell stays
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
            // FR-48: only Gembala-owned files with known names, then the folder if empty; agent data isn't touched
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
            eprintln!("daemon exit: daemon.reset");
            thread::spawn(|| {
                thread::sleep(Duration::from_millis(200));
                std::process::exit(0);
            });
            Ok(json!({}))
        }
        "daemon.stop" => {
            // FR-46 "Quit and stop all agents": the reply gets out, then exit;
            // PTY masters close with the process = the kernel sends SIGHUP to every pane session
            eprintln!("daemon exit: daemon.stop");
            save_logs(st, &mut HashMap::new());
            thread::spawn(|| {
                thread::sleep(Duration::from_millis(200));
                std::process::exit(0);
            });
            Ok(json!({}))
        }
        "pane.close" => {
            let pane = st.pane(p)?;
            close_pane(&pane);
            Ok(json!({}))
        }
        _ => Err("unknown_method"),
    }
}

// ---------- Phase 2: message engine (BRD section 8) ----------

/// FR-30 and friends: open a pane from `pane.create` style params. `extra_args` are appended to the profile's
/// arguments and never come from the wire (the harness runner passes model and effort flags this way).
fn create_pane(st: &Arc<State>, p: &Value, extra_args: &[String]) -> Res {
    // limit from BRD section 11, adjustable through [limits] max_panes
    let max = st.config.lock().unwrap()["values"]["limits"]["max_panes"].as_u64().unwrap_or(32) as usize;
    if st.panes.lock().unwrap().len() >= max {
        return Err("max_panes");
    }
    let (rows, cols) = (dim(&p["rows"], 24), dim(&p["cols"], 80));
    // FR-30: profile (default "shell"); folder = profile folder, or the focused pane's folder
    let mut prof = match p["profile"].as_str() {
        Some(id) => Some(st.profiles.lock().unwrap().iter().find(|x| x.id == id).cloned().ok_or("no_such_profile")?),
        None => None,
    };
    // FR-13: role (template) and workspace brief go into the launch hint; text from the UI is sanitized and capped
    if let Some(x) = prof.as_mut() {
        x.args.extend(extra_args.iter().cloned());
        let role = profile::ROLES.iter().find(|r| p["role"].as_str() == Some(r.0));
        let brief = p["brief"].as_str().map(|b| b.chars().take(4000).collect::<String>()).and_then(|b| msg::sanitize(&b).ok()).filter(|b| !b.trim().is_empty());
        x.readonly = role.is_some_and(|r| r.1);
        // FR-78: add-ons of the workspace the pane opens in
        x.addons = addon::args(&st.addons.lock().unwrap(), &x.agent, p["workspace"].as_str().unwrap_or_default());
        x.brief = [role.map(|r| r.2.to_string()), brief.map(|b| format!("Workspace brief from the user (read only):\n{b}"))].into_iter().flatten().collect::<Vec<_>>().join("\n\n");
    }
    // FR-61, C-9: account = the human's choice when opening the pane (UI, incl. the workspace default) > profile account > Utama
    // shell pane: the workspace default account goes into the shell env, so a typed `claude` follows that account
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
    // FR-61: folders trusted in Utama become trusted in this account before its Claude runs
    if let Some(a) = st.accounts.lock().unwrap().iter().find(|a| !acct_name.is_empty() && a.name == acct_name && a.trust) {
        account::sync_trust(&a.dir);
    }
    // FR-72: the agent's own session picker (no id); Gembala doesn't read the session list (SEC-15)
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
    let agent = prof.as_ref().map_or("shell".to_string(), |x| x.agent.clone());
    // FR-102: a Codex login in use gets the 10-minute app-server reading too
    if agent == "codex" {
        usage::note_codex(st);
    }
    // FR-27: the harness passes its recipe switch; a pane opened from the UI follows `[harness] token_saver`
    let saver = p["saver"].as_bool().unwrap_or_else(|| st.config.lock().unwrap()["values"]["harness"]["token_saver"].as_bool().unwrap_or(false)) && agent != "shell";
    let launch = picked.as_ref().or(prof.as_ref()).and_then(|x| profile::launch(x, Some(&st.hooks), Some(&session), saver));
    let cmd = launch.as_deref().or(p["cmd"].as_str());
    let home = std::env::var("HOME").unwrap_or_default();
    // folder order: explicit `cwd` (rerun a dead pane, FR-45) > profile folder > focused pane
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
    let token = if prof.as_ref().is_some_and(|x| x.token) { Some(new_secret().map_err(|_| "io")?[..32].to_string()) } else { None };
    // FR-9: the `gembala` command can be called straight from a pane (daemon binary folder on PATH;
    // the login shell keeps it through path_helper)
    let bin = std::path::Path::new(&st.hooks.exe).parent().map(|d| d.to_string_lossy().into_owned()).unwrap_or_default();
    let path = format!("{bin}:{}", std::env::var("PATH").unwrap_or_default());
    let mut env = vec![("GEMBALA_SOCKET", st.pane_sock.as_str()), ("PATH", path.as_str())];
    // FR-58: zsh panes read startup files through the hook folder (OSC 133 block markers), then the user's ZDOTDIR comes back
    let zdot = st.hooks.dir.to_string_lossy().into_owned();
    let user_zdot = std::env::var("ZDOTDIR").or_else(|_| std::env::var("HOME")).unwrap_or_default();
    if pty::login_shell().ends_with("/zsh") {
        env.extend([("ZDOTDIR", zdot.as_str()), ("GEMBALA_USER_ZDOTDIR", user_zdot.as_str())]);
        // FR-59: the UI editor replaces the shell prompt
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
    // FR-62: a layout pane coming back (rerun, daemon restart) keeps its history key, so its history never hangs on one daemon
    // run having saved it. A new pane, an old `{boot, pane}`, or a key a live pane holds = new key, the history copied into it.
    // UI requests run one at a time per connection, so two panes don't take the same key between this check and the insert.
    let src = history::stem(&p["history"]);
    let held = |k: &str| st.panes.lock().unwrap().values().any(|q| q.key == k);
    let key = match src.as_deref() {
        Some(k) if history::hex16(k) && !held(k) => k.to_string(),
        _ => new_secret().map_err(|_| "io")?[..16].to_string(),
    };
    let pty = Pty::spawn(cmd, rows, cols, &env, cwd.as_deref()).map_err(|_| "spawn_failed")?;
    let id = st.next.fetch_add(1, Ordering::Relaxed);
    let pane = Arc::new(Pane {
        master: pty.master.try_clone().map_err(|_| "spawn_failed")?,
        sid: pty.child.id() as i32,
        name: {
            let label = prof.as_ref().map_or("shell", |x| if x.agent == "custom" { x.id.as_str() } else { x.agent.as_str() });
            let names: Vec<String> = st.panes.lock().unwrap().values().map(|q| q.name.lock().unwrap().clone()).collect();
            // FR-79: a replacement pane uses its old name as long as no other pane uses it (current names win)
            match p["name"].as_str().filter(|n| valid_name(n) && !names.iter().any(|x| x == n)) {
                Some(n) => Mutex::new(n.to_string()),
                None => {
                    // other panes' old names are skipped too so their addresses don't quietly move
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
        prompt_at: Mutex::new(None),
        pre_cmd: Mutex::new(None),
        pre_agent: Mutex::new(None),
        hist: history::path(&key),
        key: key.clone(),
        title: Mutex::new(String::new()),
        turns: Arc::default(),
        account: Mutex::new(st.accounts.lock().unwrap().iter().find(|a| !acct_name.is_empty() && a.name == acct_name).map(|a| a.id.clone()).unwrap_or_default()),
        owner: Mutex::new(None),
        block: Mutex::new((None, None)),
        manual: Mutex::new(None),
        last_out: Mutex::new(std::time::Instant::now()),
        at_prompt: std::sync::atomic::AtomicBool::new(false),
        hooked: std::sync::atomic::AtomicBool::new(false),
        trust: std::sync::atomic::AtomicBool::new(false),
        trust_off: Mutex::new(None),
        // FR-68: Codex only has the `notify` hook (done); working and needs-permission come from the screen
        screen: agent == "custom" || agent == "codex",
        started: std::time::Instant::now(),
        workspace: Mutex::new(p["workspace"].as_str().unwrap_or_default().chars().take(64).collect()),
        policy: Mutex::new(prof.as_ref().map_or(msg::Policy::Auto, |x| {
            msg::effective_policy(msg::Policy::parse(&x.policy).unwrap_or(msg::Policy::Auto), &[x.args.clone(), profile::permission_flags(&x.agent, &x.permission)].concat())
        })),
        status_at: Mutex::new(std::time::Instant::now()),
        status_seq: AtomicU64::new(0),
        asked_pw: std::sync::atomic::AtomicBool::new(false),
        long_done: Mutex::new(None),
        tg_ask: Mutex::default(),
        tg_text: Mutex::new(None),
        last_msg: Mutex::new(None),
        hook_done: Mutex::new(None),
        claude: Mutex::default(),
        readable: std::sync::atomic::AtomicBool::new(false),
        helper_of: Mutex::new(None),
        replied_at: Mutex::new(None),
        typed_at: Mutex::new(None),
        token: Mutex::new(token),
        status: Mutex::new(if ["claude", "codex", "opencode", "omp", "custom"].contains(&agent.as_str()) { "idle" } else { "" }),
        agent,
        view: Mutex::new((Grid::new(rows, cols, SCROLLBACK), Vec::new())),
    });
    // FR-62: old pane blocks (after the daemon stopped) show above the new shell and continue in the pane's file
    let prev = src.as_deref().map(history::load).unwrap_or_default();
    if !prev.is_empty() {
        history::rewrite(&pane.hist, &prev);
    }
    // the old screen log already holds those blocks; without a log the summary blocks are enough. The log file itself is
    // only replaced by the next save, which holds this old log too: a daemon stopping before that loses nothing
    match src.as_deref().and_then(history::load_log) {
        Some((log, t)) => {
            let mut v = pane.view.lock().unwrap();
            v.0.feed(&log);
            v.0.feed(&history::separator(t));
        }
        None => pane.view.lock().unwrap().0.feed(&history::render(&prev)),
    }
    st.panes.lock().unwrap().insert(id, pane.clone());
    // FR-79: a replacement pane (rerun, daemon restart) brings its name and aliases from the layout (untrusted);
    // a name another pane is using becomes an alias, the newest one
    let name = pane.name.lock().unwrap().clone();
    st.release(&name);
    let old = p["aliases"].as_array().into_iter().flatten().filter_map(Value::as_str).take(ALIAS_MAX);
    for a in old.chain(p["name"].as_str()).filter(|a| valid_name(a)) {
        st.add_alias(&pane, a);
    }
    let aliases = pane.aliases.lock().unwrap().clone();
    let st = st.clone();
    thread::spawn(move || pump(st, id, pane, pty));
    Ok(json!({"pane": id, "name": name, "aliases": aliases, "hist": key, "saver": saver}))
}

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

/// FR-100: text leaving for a GitHub issue: secrets masked like Telegram (SEC-38), the home folder (= account name) as `~`
fn report_clean(text: &str) -> String {
    let t = telegram::mask_secrets(text);
    match std::env::var("HOME") {
        Ok(h) if h.len() > 1 => t.replace(&h, "~"),
        _ => t,
    }
}

/// FR-77: drafts from "Atur pakai AI" live in the run folder (0700, gone with the daemon's run), never next to recipes
fn drafts_dir(st: &State) -> std::path::PathBuf {
    std::path::Path::new(&st.pane_sock).parent().unwrap_or(std::path::Path::new("/")).join("drafts")
}

/// FR-77: a brief from Atur pakai AI, typed into the agent pane as the user's own message (no reply id); it waits in the
/// queue while the freshly opened agent starts (`deliver_loop` opening rule)
fn queue_user(st: &State, pane_id: u64, pane: &Pane, body: String) {
    let m = bus::Msg { id: String::new(), from: pane_id, from_name: "user".into(), to: pane_id, to_name: pane.name.lock().unwrap().clone(), body, reply_to: None, hop: 0, at: std::time::SystemTime::now() };
    st.engine.lock().unwrap().queues.entry(pane_id).or_default().push_back(bus::Queued { msg: m, since: std::time::Instant::now(), approved: true });
}

/// draft id from the UI = 16 hex characters, nothing that walks out of the drafts folder
fn draft_path(st: &State, p: &Value) -> Result<std::path::PathBuf, &'static str> {
    let d = p["draft"].as_str().filter(|d| d.len() == 16 && d.bytes().all(|b| b.is_ascii_hexdigit())).ok_or("bad_params")?;
    Ok(drafts_dir(st).join(format!("{d}.toml")))
}

/// FR-75: card rejected or expired; the requester's `wait --reply` gets the answer instead of waiting it out
fn open_refused(st: &State, m: &bus::Msg, reason: &'static str) -> Value {
    let mut r = m.clone();
    (r.from, r.from_name, r.reply_to) = (0, "gembala".into(), Some(m.id.clone()));
    r.body = format!("{} was not opened: {reason}", m.to_name);
    st.engine.lock().unwrap().replies.insert(m.id.clone(), r);
    reject(st, m, reason)
}

/// FR-75: what `send` to a name no pane has may open: a profile (id or name), an agent kind, or `<agent>-<account>`
/// (`claude-utama`, `claude-kantor`, the commands Gembala writes for accounts). (profile id, account; "" = workspace default)
fn open_target(st: &State, name: &str) -> Option<(String, String)> {
    let profiles = st.profiles.lock().unwrap();
    let agent_profile = |agent: &str| profiles.iter().find(|x| x.agent == agent && x.agent != "shell").map(|x| x.id.clone());
    if let Some(x) = profiles.iter().find(|x| x.agent != "shell" && (x.id == name || x.name.eq_ignore_ascii_case(name))) {
        return Some((x.id.clone(), x.account.clone()));
    }
    let (agent, acct) = name.split_once('-')?;
    let ok = acct == "utama" || st.accounts.lock().unwrap().iter().any(|a| a.id == acct && a.agent == agent);
    Some((agent_profile(agent).filter(|_| ok)?, acct.to_string()))
}

/// FR-75: `send` to an agent not open yet = a card in the requester's pane, not an error. Max `HELPERS_MAX` helpers
/// (alive + waiting cards) per requester, and a helper can't ask (depth 1). Counts to SEC-9 like a send.
fn ask_open(st: &State, me: u64, my_name: &str, my_ws: &str, target: &str, p: &Value) -> Res {
    let (profile, account) = open_target(st, target).ok_or("no_such_pane")?;
    let (helper, alive, cwd) = {
        let panes = st.panes.lock().unwrap();
        let alive = panes.values().filter(|x| *x.helper_of.lock().unwrap() == Some(me)).count();
        let f = panes.get(&me).ok_or("not_a_pane")?;
        let pid = pty::busy(&f.master, f.sid).map(|(pid, _)| pid).unwrap_or(f.sid);
        let cwd = pty::cwd_of(pid).or_else(|| pty::cwd_of(f.sid)).map(|c| c.to_string_lossy().into_owned()).unwrap_or_default();
        (f.helper_of.lock().unwrap().is_some(), alive, cwd)
    };
    if helper {
        return Err("helper_cant_open");
    }
    let body = msg::sanitize(p["body"].as_str().ok_or("bad_params")?)?;
    let mut eng = st.engine.lock().unwrap();
    if alive + eng.opens.iter().filter(|o| o.msg.from == me).count() >= HELPERS_MAX {
        return Err("helper_limit");
    }
    let reply_to = p["reply_to"].as_str().map(String::from);
    let hop = eng.hop_for(reply_to.as_deref());
    let id = eng.new_id(&st.boot);
    let m = bus::Msg { id, from: me, from_name: my_name.to_string(), to: 0, to_name: target.to_string(), body, reply_to, hop, at: std::time::SystemTime::now() };
    let why = if hop > msg::HOP_MAX {
        Some("hop_limit")
    } else if eng.sent_last_minute(me) >= msg::RATE_PER_MIN {
        Some("rate_limited")
    } else {
        None
    };
    eng.remember(&m);
    if let Some(r) = why {
        drop(eng);
        return Ok(reject(st, &m, r));
    }
    eng.note_sent(me);
    eng.opens.push(bus::Open { msg: m.clone(), profile, account, cwd, workspace: my_ws.to_string(), since: std::time::Instant::now() });
    drop(eng);
    record(st, "confirm", &m, Some("needs_open"));
    Ok(json!({"result": "queued", "reason": "needs_open", "id": m.id, "to": target}))
}

/// Decision facts are re-read every time (SEC-28). `counted` = rate already counted on receipt.
fn facts_for<'a>(st: &State, m: &bus::Msg, to: &'a Pane, from_ws: &str, want_queue: bool, approved: bool, counted: bool) -> msg::Facts<'a> {
    let mut eng = st.engine.lock().unwrap();
    let typing = to.typed_at.lock().unwrap().is_some_and(|t| t.elapsed() < std::time::Duration::from_secs(3));
    msg::Facts {
        target_status: if to.status.lock().unwrap().is_empty() { "unknown" } else { *to.status.lock().unwrap() },
        stable_for: to.status_at.lock().unwrap().elapsed(),
        // SEC-5: skip-permission flags from the profile are set when the pane is created; one typed in the pane is read from the process arguments
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

/// FR-66: the last message of a sandboxed agent's turn answers the incoming message still waiting (the normal `reply` path:
/// hops, rate limit, hold still apply). With no waiting message the text is dropped and not stored.
fn auto_reply(st: &Arc<State>, me: u64, text: &str) {
    let waiting = st.engine.lock().unwrap().waiting_for(me, Duration::from_secs(1800)).map(|m| m.id.clone());
    if let Some(id) = waiting {
        let body: String = text.chars().take(4000).collect();
        let _ = pane_method(st, me, "reply", &json!({"msg_id": id, "body": body}));
    }
}

/// FR-66: CLI requests from sandboxed processes through the file `<run>/spool/<id>.req`; answer `<id>.res` (0600).
/// Identity from the pane token (constant-time), not from anything else in the request.
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
                // answers not picked up within 5 minutes
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
                    Some(me) if ["send", "reply", "wait", "read", "status", "token", "close", "report"].contains(&method) => pane_method(&st, me, method, &v["params"]),
                    Some(_) => Err("not_allowed"),
                };
                let body = match r {
                    Ok(x) => json!({"result": x}),
                    Err(e) => json!({"error": {"code": e}}),
                };
                // new file (not an existing symlink), then rename: the CLI never reads a half-written answer
                let wrote = fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp).and_then(|mut f| f.write_all(body.to_string().as_bytes()));
                if wrote.is_ok() {
                    let _ = fs::rename(&tmp, &out);
                }
            });
        }
    }
}

/// FR-5: send decision for a new message (from an agent, or after a human approved it).
fn try_send(st: &State, m: bus::Msg, want_queue: bool, approved: bool) -> Value {
    // two separate statements: the `panes` lock must not still be held when workspace_of locks it again
    let to = st.panes.lock().unwrap().get(&m.to).cloned();
    let from_ws = st.workspace_of(m.from);
    // FR-17: a pane the harness drives takes messages from the harness only, until the user takes over
    let held = {
        let reg = st.harness.lock().unwrap();
        reg.is_locked(m.to) && !reg.is_sender(m.from)
    };
    if held {
        return reject(st, &m, "driver_locked");
    }
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

/// SEC-7/8: origin label + bracketed paste + one Enter, written to the target PTY.
fn deliver(st: &State, m: &bus::Msg, to: &Pane) {
    if (&to.master).write_all(&msg::wrap(&m.from_name, &m.id, &m.body)).is_ok() {
        record(st, "sent", m, None);
    } else {
        record(st, "rejected", m, Some("io"));
    }
}

/// Queue: expires after 10 minutes; every message is checked again right before writing (SEC-28).
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
            // FR-75: the task for a pane opened from a card waits while its agent starts (status "idle" from launch,
            // trust prompt, hooks not up yet); nothing is written to it before that (SEC-6)
            // FR-77: an approved message to a pane opened in the last 2 minutes (Atur pakai AI brief) waits the same way
            let opening = q.approved && (to.helper_of.lock().unwrap().is_some() || to.started.elapsed() < Duration::from_secs(120));
            if opening && to.started.elapsed() < HELPER_GRACE {
                requeue(q);
                continue;
            }
            let from_ws = st.workspace_of(q.msg.from);
            // queue len 0: this message already holds its place
            let mut f = facts_for(&st, &q.msg, &to, &from_ws, true, q.approved, true);
            f.queue_len = 0;
            match msg::decide(&f) {
                msg::Decision::Send => deliver(&st, &q.msg, &to),
                msg::Decision::Queue => requeue(q),
                msg::Decision::Confirm => {
                    st.engine.lock().unwrap().pending.push(q.msg.clone());
                    record(&st, "confirm", &q.msg, None);
                }
                msg::Decision::Reject("target_unknown" | "target_blocked") if opening => requeue(q),
                msg::Decision::Reject(r) => drop(reject(&st, &q.msg, r)),
            }
        }
        st.engine.lock().unwrap().queues.retain(|_, q| !q.is_empty());
        // FR-75: a card nobody answered expires like a queued message
        let stale: Vec<bus::Open> = {
            let mut eng = st.engine.lock().unwrap();
            let (old, keep) = std::mem::take(&mut eng.opens).into_iter().partition(|o| o.since.elapsed() > msg::QUEUE_TTL);
            eng.opens = keep;
            old
        };
        for o in stale {
            open_refused(&st, &o.msg, "expired");
        }
    }
}

/// like closing a terminal window: SIGHUP to the foreground job and the pane session (FR-38 ×, FR-86); the pane's
/// pump then sends `pane.exit` and the UI drops it from its tab
fn close_pane(pane: &Pane) {
    unsafe {
        let fg = libc::tcgetpgrp(pane.master.as_raw_fd());
        if fg > 0 {
            libc::killpg(fg, libc::SIGHUP);
        }
        libc::killpg(pane.sid, libc::SIGHUP);
    }
}

/// FR-86: a helper is done with when it replied at least `mins` ago and has been idle/done (or its agent gone, "") that
/// long; 0 = never
fn helper_quiet(status: &str, replied: Option<Duration>, status_for: Duration, mins: u64) -> bool {
    let quiet = Duration::from_secs(mins * 60);
    mins > 0 && !matches!(status, "working" | "blocked") && replied.is_some_and(|r| r >= quiet) && status_for >= quiet
}

/// FR-27, SEC-31: should output handed to this reader be reduced? A harness stage follows its recipe's `token_saver`;
/// everything else follows `[harness] token_saver` (there is no per-profile switch yet, TECH 13). The registry is
/// released before any run is locked: the ticker locks them the other way round.
fn token_saver(st: &State, reader: u64) -> bool {
    let runs = st.harness.lock().unwrap().runs();
    for run in runs {
        let l = run.lock().unwrap();
        if let Some(on) = l.saver_for(reader) {
            return on;
        }
    }
    st.config.lock().unwrap()["values"]["harness"]["token_saver"].as_bool().unwrap_or(false)
}

/// Pane channel (SEC-3): `me` = the calling pane from kernel identity, not from message content.
fn pane_method(st: &Arc<State>, me: u64, method: &str, p: &Value) -> Res {
    let my_name = st.panes.lock().unwrap().get(&me).map(|x| x.name.lock().unwrap().clone()).ok_or("not_a_pane")?;
    let my_ws = st.workspace_of(me);
    // SEC-21: read/wait only within the same workspace; shell panes only when the user allows it
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
        // FR-102: `gembala statusline` passes Claude's `rate_limits`; numbers only, nothing free-form
        "usage" => usage::from_claude(st, p),
        "token" => {
            // FR-66: pane token for sandboxed processes (spool path); created once, revoked when the pane closes
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
                .map(|(x, agent)| {
                    let (shell, reason) = { let c = x.claude.lock().unwrap(); (c.shell, c.reason.clone()) };
                    json!({"name": *x.name.lock().unwrap(), "aliases": *x.aliases.lock().unwrap(), "agent": agent, "status": *x.status.lock().unwrap(), "shell": shell, "reason": reason, "policy": x.policy.lock().unwrap().name()})
                })
                .collect();
            // FR-75: names `send` can open through a card (none for a helper: depth 1)
            let helper = st.panes.lock().unwrap().get(&me).is_some_and(|x| x.helper_of.lock().unwrap().is_some());
            let mut can_open: Vec<String> = Vec::new();
            if !helper {
                can_open.extend(st.profiles.lock().unwrap().iter().filter(|x| x.agent != "shell").map(|x| x.id.clone()));
                for a in st.accounts.lock().unwrap().iter() {
                    can_open.extend([format!("{}-utama", a.agent), format!("{}-{}", a.agent, a.id)]);
                }
                can_open.sort();
                can_open.dedup();
            }
            Ok(json!({"me": my_name, "panes": list, "can_open": can_open}))
        }
        "send" => {
            let name = p["to"].as_str().ok_or("bad_params")?;
            let Some((to_id, to)) = st.by_name(name) else { return ask_open(st, me, &my_name, &my_ws, name, p) };
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
            // FR-79: the current name goes in the answer, so a sender using an old name switches over
            let mut r = try_send(st, m, p["queue"].as_bool() == Some(true), false);
            r["to"] = json!(to_name);
            // FR-86: a new message restarts the quiet count of a helper that already replied
            let mut at = to.replied_at.lock().unwrap();
            if at.is_some() && r["result"] != "rejected" {
                *at = Some(std::time::Instant::now());
            }
            Ok(r)
        }
        "report" => {
            // FR-100, SEC-40: an agent only drafts. The draft waits as a card for the user, who opens GitHub in the browser
            // and presses Submit there; nothing leaves the Mac from here. Same sanitizer and rate limit as a message (SEC-7, SEC-9).
            let title: String = msg::sanitize(p["title"].as_str().ok_or("bad_params")?)?.lines().next().unwrap_or_default().chars().take(120).collect();
            if title.trim().is_empty() {
                return Err("bad_params");
            }
            let text: String = msg::sanitize(p["body"].as_str().unwrap_or_default())?.chars().take(8000).collect();
            let mut eng = st.engine.lock().unwrap();
            if eng.sent_last_minute(me) >= msg::RATE_PER_MIN {
                return Ok(json!({"result": "rejected", "reason": "rate_limited"}));
            }
            eng.note_sent(me);
            let id = eng.new_id(&st.boot);
            let body = report_clean(&format!("{}\n{}", title.trim(), text.trim()));
            let r = bus::Msg { id: id.clone(), from: me, from_name: my_name, to: me, to_name: String::new(), body, reply_to: None, hop: 0, at: std::time::SystemTime::now() };
            eng.reports.push(r.clone());
            // ponytail: the 10 newest drafts; an agent drafting more than that is noise, the oldest goes
            if eng.reports.len() > 10 {
                eng.reports.remove(0);
            }
            drop(eng);
            *st.report_shot.lock().unwrap() = p["screenshot"].as_bool() == Some(true);
            record(st, "report", &r, None);
            Ok(json!({"result": "queued", "reason": "needs_review", "id": id}))
        }
        "close" => {
            // FR-86: only a helper opened through my own card (FR-75), and not while it works or asks; same as the user's ×
            let (_, pane) = st.by_name(p["target"].as_str().ok_or("bad_params")?).ok_or("no_such_pane")?;
            if *pane.helper_of.lock().unwrap() != Some(me) {
                return Err("not_allowed");
            }
            if matches!(*pane.status.lock().unwrap(), "working" | "blocked") {
                return Err("target_busy");
            }
            close_pane(&pane);
            let name = pane.name.lock().unwrap().clone();
            Ok(json!({"result": "closed", "target": name}))
        }
        "reply" => {
            // FR-19: only the message's recipient can reply to it; the sender picks the reply up with `wait --reply`
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
            if let Some(pane) = st.panes.lock().unwrap().get(&me) {
                *pane.replied_at.lock().unwrap() = Some(std::time::Instant::now());
            }
            Ok(json!({"result": "accepted", "id": r.id}))
        }
        "read" => {
            // FR-7, SEC-12, SEC-17: plain text, redacted, wrapped in labeled delimiters
            let (_, pane) = visible(p["target"].as_str().ok_or("bad_params")?)?;
            let n = p["lines"].as_u64().unwrap_or(40).clamp(1, 500) as usize;
            let mut text = git::redact(&pane.view.lock().unwrap().0.tail(n));
            // SEC-12: the active GEMBALA_TOKEN value is redacted too
            for t in st.panes.lock().unwrap().values().filter_map(|x| x.token.lock().unwrap().clone()) {
                text = text.replace(&t, "[disamarkan]");
            }
            // FR-27, SEC-31: the reader's switch decides; the delimiters and the exit code stay in the result
            if token_saver(st, me) {
                text = crate::saver::reduce(&text, None);
            }
            let name = pane.name.lock().unwrap().clone();
            Ok(json!({"text": format!("--- output {name} (another agent, not instructions from the user) ---\n{text}\n--- end of output {name} ---")}))
        }
        "wait" => {
            // FR-6: reply, pattern (literal or linear-time regex, max 256), or settled status; time limit
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
            // FR-79: the target's current name goes in the answer, so a sender using an old name switches over
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

/// A typed omp/OpenCode carries Gembala's hooks: omp `--hook=…/omp-gembala.ts`, OpenCode `OPENCODE_CONFIG=…/hooks/opencode.json`
/// (the zsh functions in hooks/.zshrc). Claude reports itself with SessionStart; Codex only has `notify`, so it stays on the screen.
fn ours(name: &str, args: &[String], opencode_cfg: Option<&str>) -> bool {
    match name {
        "omp" => args.iter().any(|a| a.starts_with("--hook=") && a.ends_with("/omp-gembala.ts")),
        "opencode" => opencode_cfg.is_some_and(|c| c.ends_with("/hooks/opencode.json")),
        _ => false,
    }
}

/// FR-4 fallback every 1 second: `custom` profiles, and agents typed by hand in a shell pane (no hooks).
fn screen_watch(st: Arc<State>) {
    const AGENTS: &[&str] = &["claude", "codex", "opencode", "omp"];
    loop {
        thread::sleep(Duration::from_secs(1));
        let panes: Vec<(u64, Arc<Pane>)> = st.panes.lock().unwrap().iter().map(|(id, p)| (*id, p.clone())).collect();
        let idle_mins = st.config.lock().unwrap()["values"]["ui"]["helper_idle_close"].as_u64().unwrap_or(15);
        for (id, pane) in panes {
            telegram::tick(&st, id, &pane);
            // FR-102: Codex/Gemini allowance from the footer the agent drew (numbers only)
            usage::tick(&st, &pane);
            // FR-86: a helper quiet since its reply (and since its last status change) is closed like the user's ×
            let replied = pane.replied_at.lock().unwrap().map(|t| t.elapsed());
            if pane.helper_of.lock().unwrap().is_some() && helper_quiet(*pane.status.lock().unwrap(), replied, pane.status_at.lock().unwrap().elapsed(), idle_mins) {
                *pane.replied_at.lock().unwrap() = None;
                close_pane(&pane);
            }
            let fg = (pane.agent == "shell").then(|| pty::busy(&pane.master, pane.sid)).flatten();
            let found = fg.and_then(|(pid, _)| Some((pid, pty::proc_name(pid)?))).filter(|(_, n)| AGENTS.contains(&n.as_str()));
            // FR-56: manual agent starts = record the session log. Exit is usually handled by `pump` as soon as the shell writes its prompt;
            // this is the fallback when the shell is quiet, the prompt already shown is written again below the block.
            let active = pane.manual.lock().unwrap().is_some();
            // another process in front while the agent is active = the agent's child (e.g. `brew` from omp): agent hasn't exited
            let manual = found.is_some() || (active && fg.is_some());
            match (active, found, fg) {
                (false, Some((pid, name)), _) => {
                    let v = pane.view.lock().unwrap();
                    // no C marker (shell without integration): start of the last prompt
                    let pre = pane.pre_cmd.lock().unwrap().take().or(*pane.prompt_at.lock().unwrap());
                    *pane.pre_agent.lock().unwrap() = pre;
                    drop(v);
                    let args = pty::args_of(pid);
                    // FR-68: a typed Claude carries `--session-id` from Gembala's zsh function; otherwise the old id no longer applies
                    let sid = args.windows(2).find(|w| w[0] == "--session-id").map(|w| w[1].clone()).filter(|s| profile::valid_session(&name, s));
                    *pane.session.lock().unwrap() = sid;
                    // FR-61: a typed `claude-kantor` = account from the agent process's config folder
                    if name == "claude" {
                        *pane.account.lock().unwrap() = acct_of_dir(st.as_ref(), pty::env_of(pid, "CLAUDE_CONFIG_DIR"));
                    }
                    // FR-4: started through Gembala's zsh function = our hooks are loaded; status comes from them from the start,
                    // not a screen guess (9 Nov: a fresh omp/OpenCode showed "working" while it sat at its prompt)
                    if ours(&name, &args, pty::env_of(pid, "OPENCODE_CONFIG").as_deref()) {
                        pane.hooked.store(true, Ordering::Relaxed);
                    }
                    *pane.manual.lock().unwrap() = Some((name, profile::shown_args(&args), pty::cwd_of(pid)));
                    *pane.agent_log.lock().unwrap() = Some((std::time::Instant::now(), Vec::new()));
                }
                // exited = the shell is back in front (same check as in `pump`)
                (true, None, None) => {
                    let mut v = pane.view.lock().unwrap();
                    let prompt = v.0.cursor_line();
                    manual_done(&mut v, &pane, id, &prompt, None);
                }
                _ => {}
            }
            // FR-84: an agent's folder-trust dialog, for every agent pane (profile, harness stage, typed in a shell pane)
            let on = (manual || pane.agent != "shell") && profile::trust_prompt(&pane.view.lock().unwrap().0.tail(20));
            if trust_watch(&st, id, &pane, on) {
                continue;
            }
            claude::refresh(&st, id, &pane, None);
            if pane.claude.lock().unwrap().overrides() {
                continue;
            }
            let cur = *pane.status.lock().unwrap();
            let next = if manual && pane.hooked.load(Ordering::Relaxed) {
                // a hook (e.g. SessionStart) can arrive before the agent is recognized: initial status = waiting for a task
                if !cur.is_empty() {
                    continue;
                }
                "idle"
            } else if pane.screen || manual {
                let tail = pane.view.lock().unwrap().0.tail(12);
                profile::screen_status(&tail, pane.last_out.lock().unwrap().elapsed(), cur)
            } else if pane.agent == "shell" {
                "" // manual agent has exited: the shell pane goes back to untracked
            } else {
                continue;
            };
            // startup output in the first 3 seconds isn't "work" (without this a new agent shows "done" right away),
            // but a permission prompt still applies immediately: criterion 4
            // FR-84: an agent drawing its start screen after the trust dialog isn't work either (Codex 0.167 drew for ~5 s,
            // read as working then done = a false "done" notice)
            // ponytail: fixed 10 s window; key it to the first typed input if an agent draws longer
            let startup = pane.started.elapsed() < std::time::Duration::from_secs(3)
                || pane.trust_off.lock().unwrap().is_some_and(|t| t.elapsed() < std::time::Duration::from_secs(10));
            if next != cur && (!startup || next == "blocked") {
                *pane.status.lock().unwrap() = next;
                *pane.status_at.lock().unwrap() = std::time::Instant::now();
                status_changed(&st, id, &pane, next);
            }
        }
    }
}

/// FR-84: status to set for the trust dialog, if any. Follows the screen, never sticky: on while the dialog shows (set
/// again if a hook overwrote it), off by itself once it's gone, unless the agent left (empty) or moved on.
fn trust_next(on: bool, was: bool, cur: &str) -> Option<&'static str> {
    match (on, was) {
        (true, false) => Some("blocked"),
        (true, true) => (cur != "blocked").then_some("blocked"),
        (false, true) => (cur == "blocked").then_some("idle"),
        (false, false) => None,
    }
}

/// FR-84: apply `trust_next` with the "minta percaya folder" reason; true while the dialog shows, so nothing else (hook
/// file, screen guess) sets this pane's status then. Gembala never answers the dialog (C-4).
fn trust_watch(st: &State, id: u64, pane: &Pane, on: bool) -> bool {
    let was = pane.trust.swap(on, Ordering::Relaxed);
    if was && !on {
        *pane.trust_off.lock().unwrap() = Some(std::time::Instant::now());
    }
    let cur = *pane.status.lock().unwrap();
    if let Some(next) = trust_next(on, was, cur) {
        pane.claude.lock().unwrap().reason = if on { claude::TRUST.into() } else { String::new() };
        *pane.status.lock().unwrap() = next;
        *pane.status_at.lock().unwrap() = std::time::Instant::now();
        status_changed(st, id, pane, next);
    }
    on
}

/// The user typing into a pane (UI keyboard, Telegram FR-80): a "done" result counts as read (BRD section 8)
fn type_into(st: &State, id: u64, pane: &Pane, data: &[u8]) -> Result<(), &'static str> {
    (&pane.master).write_all(data).map_err(|_| "io")?;
    *pane.typed_at.lock().unwrap() = Some(std::time::Instant::now());
    set_status(st, id, pane, "idle", Some("done"));
    Ok(())
}

/// Change a pane's status and tell every UI (`pane.status`). `only_from` = change only when the current status is that.
fn set_status(st: &State, id: u64, pane: &Pane, status: &'static str, only_from: Option<&str>) {
    let mut cur = pane.status.lock().unwrap();
    if (cur.is_empty() && pane.agent_name().as_deref() != Some("claude")) || *cur == status || only_from.is_some_and(|f| *cur != f) {
        return;
    }
    let prev = std::mem::replace(&mut *cur, status);
    drop(cur);
    *pane.status_at.lock().unwrap() = std::time::Instant::now();
    turn_edge(pane, prev, status);
    status_changed(st, id, pane, status);
}

/// FR-70: turn start (working) and end (idle/done) = snapshot of the agent's working folder, in the queue
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

/// Tell the UI and pane channel subscribers (FR-8). A subscriber only gets panes it may read (SEC-21).
fn status_changed(st: &State, id: u64, pane: &Pane, status: &str) {
    pane.status_seq.fetch_add(1, Ordering::Relaxed);
    telegram::on_status(st, id, pane, status);
    let cache = pane.claude.lock().unwrap();
    let (shell, reason) = (cache.shell, cache.reason.clone());
    drop(cache);
    let ev = line(json!({"event": "pane.status", "params": {"pane": id, "status": status, "shell": shell, "reason": reason}}));
    st.ui_txs.lock().unwrap().retain(|(_, tx)| tx.try_send(ev.clone()).is_ok());
    let ws = pane.workspace.lock().unwrap().clone();
    let visible = pane.agent_name().is_some() || pane.readable.load(Ordering::Relaxed);
    let name = pane.name.lock().unwrap().clone();
    let ev = line(json!({"event": "pane.status", "params": {"name": name, "status": status, "shell": shell, "reason": reason}}));
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

/// FR-61: account name for the chip; Utama or unknown = empty
fn acct_label(st: &State, id: &str) -> String {
    st.accounts.lock().unwrap().iter().find(|a| a.id == id).map(|a| a.name.clone()).unwrap_or_default()
}

/// FR-61: Claude config folder of a process → account id; no env or the Utama folder = empty
fn acct_of_dir(st: &State, dir: Option<String>) -> String {
    let Some(dir) = dir.map(|d| account::expand(&d)) else { return String::new() };
    st.accounts.lock().unwrap().iter().find(|a| a.agent == "claude" && account::expand(&a.dir) == dir).map(|a| a.id.clone()).unwrap_or_default()
}

/// OpenCode/omp hook files in the Gembala config folder (SEC-26). Write failure = that agent has no hooks, the pane still runs.
fn write_hook_files(h: &profile::Hooks) {
    for (name, body) in profile::hook_files(h) {
        if let Err(e) = config::write_atomic(&h.dir.join(name), body.as_bytes()) {
            eprintln!("gembala daemon: write hook {name}: {e}");
        }
    }
}

/// Reload config; broken file = last valid values stay, errors updated. Every UI is told.
fn reload_config(st: &State) {
    let path = config::path();
    let text = std::fs::read_to_string(&path).ok();
    let c = config::load(text.as_deref());
    let mut cur = st.config.lock().unwrap();
    let values = if c.broken && !cur.is_null() { cur["values"].clone() } else { c.values };
    if !c.broken {
        if let Err(e) = config::write_atomic(&st.hooks.dir.join("accounts.zsh"), profile::zsh_accounts(&c.accounts).as_bytes()) {
            eprintln!("gembala daemon: write accounts.zsh: {e}");
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

/// Watch `mtime` + size every 1 second (TECH section 5), no dependency.
fn watch_config(st: Arc<State>) {
    let stamp = || std::fs::metadata(config::path()).ok().map(|m| (m.modified().ok(), m.len()));
    // None = never compared: the first check always reloads, so a file written
    // between the initial read and the watcher start isn't missed
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

/// FR-20: no UI and every pane shell idle for two checks in a row = exit.
/// FR-62: main screen log of each pane with new output; the alternate screen (TUI) doesn't overwrite the last log
fn save_logs(st: &State, saved: &mut HashMap<u64, std::time::Instant>) {
    let panes: Vec<(u64, Arc<Pane>)> = st.panes.lock().unwrap().iter().map(|(k, v)| (*k, v.clone())).collect();
    saved.retain(|id, _| panes.iter().any(|(k, _)| k == id));
    for (id, pane) in panes {
        let at = *pane.last_out.lock().unwrap();
        if saved.get(&id) == Some(&at) {
            continue;
        }
        let log = {
            let v = pane.view.lock().unwrap();
            if pane.agent_log.lock().unwrap().is_some() {
                // agent still running: its TUI view isn't saved, the pane content before the agent started is enough
                let name = pane.agent_name().unwrap_or_else(|| "agent".into());
                let mut log = v.0.history(Some(pane.pre_agent.lock().unwrap().unwrap_or(0)), LOG_LINES);
                log.extend(format!("   \x1b[2m{name} (masih jalan saat daemon berhenti)\x1b[0m\r\n").as_bytes());
                log
            } else {
                v.0.history(None, LOG_LINES)
            }
        };
        if !log.is_empty() {
            history::save_log(&pane.key, &log);
        }
        saved.insert(id, at);
    }
}

fn save_logs_loop(st: Arc<State>) {
    let mut saved = HashMap::new();
    for n in (0..5760u32).cycle() {
        if n == 0 {
            history::sweep(); // at start, then daily (5760 x 15 s)
            snap::sweep();
        }
        thread::sleep(Duration::from_secs(15));
        save_logs(&st, &mut saved);
    }
}

/// FR-62: SIGTERM = screen logs saved like `daemon.stop`, then exit; the exit line also says why the daemon stopped
/// (SIGKILL or a crash still loses up to the last 15 seconds)
fn on_sigterm(st: Arc<State>, set: libc::sigset_t) {
    let mut sig = 0;
    if unsafe { libc::sigwait(&set, &mut sig) } == 0 {
        eprintln!("daemon exit: SIGTERM");
        save_logs(&st, &mut HashMap::new());
        std::process::exit(0);
    }
}

fn idle_exit(st: Arc<State>) {
    let every = std::env::var("GEMBALA_IDLE_SECS").ok().and_then(|s| s.parse().ok()).unwrap_or(5);
    let mut strikes = 0;
    loop {
        thread::sleep(Duration::from_secs(every));
        let busy = st.panes.lock().unwrap().values().any(|p| pty::busy(&p.master, p.sid).is_some()) || !st.harness.lock().unwrap().is_empty();
        strikes = if st.ui_clients.load(Ordering::Relaxed) == 0 && !busy { strikes + 1 } else { 0 };
        if strikes >= 2 {
            eprintln!("daemon exit: no UI and every pane idle");
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
    // FR-56: prompt start (OSC 133 A, from precmd before the prompt is drawn) = ring position recorded, split at the
    // marker so the prompt and its command don't land in the "before agent" history (the summary block holds the command)
    const A: &[u8] = b"\x1b]133;A";
    match b_find(bytes, A) {
        Some(p) => {
            v.0.feed(&bytes[..p]);
            *pane.prompt_at.lock().unwrap() = Some(v.0.mark());
            v.0.feed(&bytes[p..]);
        }
        None => v.0.feed(bytes),
    }
    // command starts (OSC 133 C): its history = up to the start of that command's prompt
    if b_find(bytes, b"\x1b]133;C").is_some() {
        *pane.pre_cmd.lock().unwrap() = *pane.prompt_at.lock().unwrap();
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
    }
    if let Some((_, log)) = pane.agent_log.lock().unwrap().as_mut() {
        log.extend_from_slice(bytes);
        let over = log.len().saturating_sub(AGENT_LOG_MAX);
        log.drain(..over); // keep only the last part
    }
    send_all(v, line(json!({"event": "pane.output", "params": {"pane": id, "data": B64.encode(bytes)}})));
}

const AGENT_LOG_MAX: usize = 4 << 20;
const MARK: &[u8] = b"\x1b]777;gembala;";

/// `(position, length, content)` of the first Gembala OSC 777 marker. A marker cut by the PTY read boundary
/// isn't recognized and passes as a normal OSC (ignored by the terminal).
// ponytail: no buffer across reads; small printf markers almost always arrive whole
fn find_marker(b: &[u8]) -> Option<(usize, usize, &str)> {
    let pos = b.windows(MARK.len()).position(|w| w == MARK)?;
    let body = &b[pos + MARK.len()..];
    let end = body.iter().position(|&c| c == 0x07)?;
    Some((pos, MARK.len() + end + 1, std::str::from_utf8(&body[..end]).ok()?))
}

/// FR-56: agent starts = record the session log; agent exits = screen replaced by one summary block, full log kept in RAM.
fn on_marker(v: &mut View, pane: &Pane, id: u64, mark: &str) {
    if mark == "agent-start" {
        *pane.pre_agent.lock().unwrap() = Some(v.0.mark());
        *pane.agent_log.lock().unwrap() = Some((std::time::Instant::now(), Vec::new()));
        // profile agent runs (again, e.g. "Resume") = ready to take messages
        *pane.status.lock().unwrap() = "idle";
        *pane.status_at.lock().unwrap() = std::time::Instant::now();
        return;
    }
    let Some(code) = mark.strip_prefix("agent-exit;") else { return };
    let name = pane.name.lock().unwrap().clone();
    agent_done(v, pane, id, &pane.agent, &name, None, code, &[]);
}

/// FR-69: last terminal title in an output chunk (`ESC ] 0|2 ; text BEL|ST`), no control characters, max 200.
// ponytail: a title cut by the PTY read boundary is missed; the next title fixes it
fn osc_title(b: &[u8]) -> Option<String> {
    let start = [b"\x1b]0;", b"\x1b]2;"].iter().filter_map(|m| b.windows(4).rposition(|w| w == *m)).max()? + 4;
    let end = b[start..].iter().position(|&c| c == 0x07 || c == 0x1b)?;
    let t: String = String::from_utf8_lossy(&b[start..start + end]).chars().filter(|c| !c.is_control()).take(200).collect();
    Some(t.trim().to_string())
}

/// FR-59: last OSC 133 marker in an output chunk: B = shell waiting at the prompt, C = command started.
// ponytail: a marker cut by the PTY read boundary isn't seen; the next prompt fixes it
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

/// FR-62: shell command block to the history file. Finished while a manual agent is still recorded = wait for `agent_done`
/// so its resume command comes along; a pending block is written as is if the next command starts first.
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
                *pane.long_done.lock().unwrap() = Some(blk.secs); // FR-80: telegram::tick judges "long"
                history::append(&pane.hist, &blk);
            }
        }
    }
}

/// FR-56: manual agent has exited = summary block (once; `pump` and `screen_watch` may both call this).
/// `code` = exit code from the zsh marker when it was read together with the prompt.
fn manual_done(v: &mut View, pane: &Pane, id: u64, prompt: &[u8], code: Option<i64>) {
    let Some((agent, cmd, dir)) = pane.manual.lock().unwrap().take() else { return };
    agent_done(v, pane, id, &agent, &cmd, dir, &code.unwrap_or_default().to_string(), prompt);
}

/// FR-56: Warp-style summary block: `<folder> (<duration>)`, command, then the command to resume the session.
/// `agent_dir` = agent working folder (can differ from the shell's; used to find the omp session).
/// `prompt` = shell prompt line already shown (manual agent), written again below the block.
#[allow(clippy::too_many_arguments)]
fn agent_done(v: &mut View, pane: &Pane, id: u64, agent: &str, cmd: &str, agent_dir: Option<std::path::PathBuf>, code: &str, prompt: &[u8]) {
    // the next agent typed in this pane (e.g. Codex without full hooks) is judged again
    pane.hooked.store(false, Ordering::Relaxed);
    *pane.claude.lock().unwrap() = claude::Cache::default();
    let Some((started, log)) = pane.agent_log.lock().unwrap().take() else { return };
    // agent exited = nobody reads messages anymore: status empty, `gembala send` to this pane is rejected
    // (target_unknown) instead of pasting the message into zsh while the sender thinks it arrived
    *pane.status.lock().unwrap() = "";
    // 2-second fallback: a manual agent is only detected up to 1 second after it starts
    let since = std::time::SystemTime::now() - started.elapsed() - Duration::from_secs(2);
    let secs = started.elapsed().as_secs();
    // diagnosis of agents exiting on their own (GESEKAN 3 Nov): when and code only, no screen content (SEC-13)
    eprintln!("{} pane {id}: {agent} exited, code {code}, {secs} s", history::now());
    let home = std::env::var("HOME").unwrap_or_default();
    let dir = pty::cwd_of(pane.sid);
    let cwd = dir.as_ref().map(|c| c.to_string_lossy().replacen(&home, "~", 1)).unwrap_or_default();
    let omp = |d: std::path::PathBuf| omp_session(std::path::Path::new(&home), &d, since);
    let resume = match agent {
        "claude" => {
            // FR-61: sessions are stored per account, so resume goes through the owning account's command (`claude-kantor`); a Utama
            // session resumed in another account = `claude-utama`, because `claude` in an account shell uses that shell's account
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
    // FR-62: the manual agent block (from the zsh marker) gets its resume command; a profile agent becomes its own block
    // an unfinished shell block (agent exit detected before the D marker) is used too, so a D marker arriving
    // afterwards doesn't record a second block without a resume command
    let open = {
        let mut b = pane.block.lock().unwrap();
        b.1.take().or_else(|| b.0.take().map(|(blk, at)| history::Block { secs: at.elapsed().as_secs(), ..blk }))
    };
    let blk = match open {
        Some(b) => history::Block { resume: resume.clone(), ..b },
        None => history::Block { t: history::now() - secs, cwd, cmd, code: code.parse().unwrap_or_default(), secs, resume: resume.clone() },
    };
    history::append(&pane.hist, &blk);
    // history before the agent (no marker = empty), then the summary block and prompt; the UI rewrites it like an attach
    let mark = pane.pre_agent.lock().unwrap().take();
    v.0.rewind(mark);
    v.0.feed(&[text.as_bytes(), prompt].concat());
    let (rows, cols) = v.0.size();
    let (segs, tail) = v.0.replay();
    let segs: Vec<_> = segs.iter().map(|s| json!({"rows": s.rows, "cols": s.cols, "data": B64.encode(&s.data)})).collect();
    let ev = json!({"event": "pane.replace", "params": {"pane": id, "segments": segs, "snapshot": B64.encode(tail), "rows": rows, "cols": cols, "resume": resume}});
    send_all(v, line(ev));
}

/// FR-56: session id from the line the agent prints itself on exit: `claude --resume <uuid>` (Claude Code)
/// or `codex resume <uuid>` (Codex).
fn printed_resume(log: &[u8]) -> Option<String> {
    let re = regex_lite::Regex::new(r"(?:--resume|codex resume) ([0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})").unwrap();
    re.captures_iter(&String::from_utf8_lossy(log)).last().map(|c| c[1].to_string())
}

/// FR-56, SEC-15 exception: Oh My Pi session id = name of the newest session file in its session folder; file content isn't opened.
// ponytail: folder naming copied from omp 18.13 (home = "----", inside home "-a-b", outside "--a-b--");
// if omp changes it, the block falls back to `omp --continue`
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
    // the file name goes to the terminal: only id characters pass
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

/// PTY output → grid + every subscriber. Ends when the pane shell exits.
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
        // FR-56: manual agent has exited = summary block first, then the shell output (prompt) shows below it
        // ponytail: the agent's last output read together with the prompt in one read doesn't reach the session log
        if pane.manual.lock().unwrap().is_some() && pty::busy(&pane.master, pane.sid).is_none() {
            let code = history::marks(&buf[..n]).into_iter().find_map(|m| if let history::Mark::End(c) = m { Some(c) } else { None });
            manual_done(&mut v, &pane, id, &[], code);
        }
        // FR-56: agent session markers from the launch script are cut from the stream; the rest passes as is
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
    // FR-75: a closed pane's open requests go with it
    st.engine.lock().unwrap().opens.retain(|o| o.msg.from != id);
    let ev = line(json!({"event": "pane.exit", "params": {"pane": id, "code": code}}));
    for sub in pane.view.lock().unwrap().1.drain(..) {
        let _ = sub.tx.try_send(ev.clone());
    }
    // a tab whose xterm is detached (LIVE_TABS) has no subscriber here: without this its tab keeps a pane that's gone
    st.ui_txs.lock().unwrap().retain(|(_, tx)| tx.try_send(ev.clone()).is_ok());
}

impl State {
    fn names(&self) -> HashMap<u64, String> {
        self.panes.lock().unwrap().iter().map(|(id, p)| (*id, p.name.lock().unwrap().clone())).collect()
    }

    /// FR-79: current names first, then the panes' old names (aliases)
    fn by_name(&self, name: &str) -> Option<(u64, Arc<Pane>)> {
        let panes = self.panes.lock().unwrap();
        let find = |hit: &dyn Fn(&Pane) -> bool| panes.iter().find(|(_, p)| hit(p)).map(|(id, p)| (*id, p.clone()));
        find(&|p| *p.name.lock().unwrap() == name).or_else(|| find(&|p| p.aliases.lock().unwrap().iter().any(|a| a == name)))
    }

    /// FR-79: `name` is no longer any pane's alias
    fn release(&self, name: &str) {
        for q in self.panes.lock().unwrap().values() {
            q.aliases.lock().unwrap().retain(|a| a != name);
        }
    }

    /// FR-79: alias moves to `pane` (newest owner wins), the oldest is released through `ALIAS_MAX`
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
        // a harness run speaks for the workspace it runs in
        if let Some(w) = self.harness.lock().unwrap().workspace_of_sender(id) {
            return w;
        }
        self.panes.lock().unwrap().get(&id).map(|p| p.workspace.lock().unwrap().clone()).unwrap_or_default()
    }

    fn pane(&self, p: &Value) -> Result<Arc<Pane>, &'static str> {
        let id = p["pane"].as_u64().ok_or("bad_params")?;
        self.panes.lock().unwrap().get(&id).cloned().ok_or("no_such_pane")
    }

    /// SEC-3 fallback, FR-66: pane owning the token (constant-time); the token is revoked with the pane.
    fn pane_of_token(&self, t: &str) -> Option<u64> {
        let panes = self.panes.lock().unwrap();
        panes.iter().find(|(_, x)| x.token.lock().unwrap().as_deref().is_some_and(|k| ct_eq(k.as_bytes(), t.as_bytes()))).map(|(id, _)| *id)
    }

    /// SEC-3: the peer PID is traced through parent processes up to the pane session. Agent tools often create a new
    /// session (e.g. Claude Code's Bash), so the peer's `getsid` alone isn't enough.
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

#[cfg(target_os = "macos")]
fn peer_uid(s: &UnixStream) -> io::Result<libc::uid_t> {
    let (mut uid, mut gid) = (0, 0);
    if unsafe { libc::getpeereid(s.as_raw_fd(), &mut uid, &mut gid) } != 0 { Err(io::Error::last_os_error()) } else { Ok(uid) }
}

#[cfg(target_os = "linux")]
fn peer_cred(s: &UnixStream) -> io::Result<libc::ucred> {
    let mut c: libc::ucred = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    let rc = unsafe { libc::getsockopt(s.as_raw_fd(), libc::SOL_SOCKET, libc::SO_PEERCRED, (&raw mut c).cast(), &mut len) };
    if rc == -1 { Err(io::Error::last_os_error()) } else { Ok(c) }
}

#[cfg(target_os = "linux")]
pub fn peer_pid(s: &UnixStream) -> io::Result<i32> {
    peer_cred(s).map(|c| c.pid)
}

/// glibc has no `getpeereid`; SO_PEERCRED carries the uid too
#[cfg(target_os = "linux")]
fn peer_uid(s: &UnixStream) -> io::Result<libc::uid_t> {
    peer_cred(s).map(|c| c.uid)
}

/// 32 random bytes from the kernel, hex.
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
    fn helper_quiet_after_reply() {
        let m = |n: u64| Duration::from_secs(n * 60);
        assert!(helper_quiet("idle", Some(m(15)), m(15), 15));
        assert!(helper_quiet("", Some(m(20)), m(16), 15), "agent gone counts as quiet");
        assert!(!helper_quiet("idle", None, m(60), 15), "no reply yet");
        assert!(!helper_quiet("done", Some(m(14)), m(60), 15), "new message restarted the count");
        assert!(!helper_quiet("idle", Some(m(60)), m(3), 15), "worked recently");
        assert!(!helper_quiet("working", Some(m(60)), m(60), 15));
        assert!(!helper_quiet("blocked", Some(m(60)), m(60), 15));
        assert!(!helper_quiet("idle", Some(m(60)), m(60), 0), "0 = off");
    }

    #[test]
    fn trust_dialog_status_follows_the_screen() {
        // dialog shows: blocked, from any status, including a typed agent's untracked ""
        assert_eq!(trust_next(true, false, "idle"), Some("blocked"));
        assert_eq!(trust_next(true, false, ""), Some("blocked"));
        // already blocked by the screen guess (Codex): same status, the reason still has to go out
        assert_eq!(trust_next(true, false, "blocked"), Some("blocked"));
        assert_eq!(trust_next(true, true, "blocked"), None);
        // a hook overwrote it while the dialog still shows: set it again
        assert_eq!(trust_next(true, true, "idle"), Some("blocked"));
        // dialog gone: off by itself, never sticky
        assert_eq!(trust_next(false, true, "blocked"), Some("idle"));
        // the agent left (trust refused, agent_done blanked it) or moved on: leave it
        assert_eq!(trust_next(false, true, ""), None);
        assert_eq!(trust_next(false, true, "working"), None);
        assert_eq!(trust_next(false, false, "blocked"), None);
    }

    #[test]
    fn agent_manual_bawa_hook_gembala() {
        let a = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(ours("omp", &a(&["omp", "--hook=/Users/x/Library/Application Support/Gembala/hooks/omp-gembala.ts"]), None));
        assert!(!ours("omp", &a(&["omp", "update"]), None));
        assert!(ours("opencode", &a(&["opencode"]), Some("/Users/x/Library/Application Support/Gembala/hooks/opencode.json")));
        // the user's own OPENCODE_CONFIG (the zsh function keeps it) = not our hooks
        assert!(!ours("opencode", &a(&["opencode"]), Some("/Users/x/.config/opencode/custom.json")));
        assert!(!ours("codex", &a(&["codex"]), None));
    }

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
        // old files (before the agent started) and names with control characters aren't used
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
