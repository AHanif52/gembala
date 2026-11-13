//! PLAN-CC-PANE phase 2: restricted non-PTY runs, bounded RAM replay and UI-only IPC.
//!
//! Survey: terminal fields were used by attach/write/resize/kill, screen/usage/Telegram
//! watchers, helper/harness delivery, message read/wait, history seeding/logging and
//! turn snapshots. Those callers now guard kind; layout remains opaque JSON and
//! restart/resume belongs to phase 4. Child sessions also participate in peer identity.
//! The existing profile names the executable `commandcode` (`cmd` is its alias).
//! `--print=<prompt>` is equivalent to `-p <prompt>` and prevents optional-argument
//! parsing from treating a prompt such as `--yolo` as an access flag.

use super::*;
use crate::commandcode::{Block, Envelope, Reader, RunResult};
use std::collections::VecDeque;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicI32, AtomicBool};
use std::time::Instant;

const RING_MAX: usize = 8 << 20;
const BLOCK_MAX: usize = 256 << 10;
const SNAP_MAX: usize = 8 << 20;
const REQUESTS_MAX: usize = 256;
/// History browsing limits: header-only reads, a bounded transcript and the newest messages of one branch.
const SESSIONS_PAGE: usize = 50;
const HEADER_MAX: u64 = 64 << 10;
const TRANSCRIPT_MAX: u64 = 32 << 20;
const HISTORY_MESSAGES_MAX: usize = 200;

pub(super) struct Pane {
    pub(super) pid: AtomicI32,
    pub(super) cwd: PathBuf,
    command: String,
    env: Vec<(String, String)>,
    model: String,
    effort: String,
    closed: AtomicBool,
    worker: Mutex<Option<thread::JoinHandle<()>>>,
    data: Mutex<Data>,
    /// Wide access is per pane, RAM only, and only after an explicit UI gate: it dies with the daemon.
    access: Mutex<Access>,
}

struct Access {
    mode: &'static str,
    revision: u64,
    /// The cwd and session the grant was made for; a different one means the grant no longer applies.
    cwd: PathBuf,
    session: String,
}

struct Data {
    seq: u64,
    run_id: String,
    /// Command Code session bound to this pane: the next run resumes it explicitly, never `--continue`.
    session: String,
    active: bool,
    cancel: Option<Instant>,
    requests: VecDeque<(String, String)>,
    ring: VecDeque<(Value, usize)>,
    bytes: usize,
    snapshot: Value,
    subscribers: Vec<Sub>,
}

fn snapshot() -> Value {
    json!({"state":"idle", "access":"restricted", "messages":[], "text":"", "thinking":"", "tools":{}, "truncated":false})
}

fn clip(text: &str, cap: usize) -> String {
    let mut end = text.len().min(cap);
    while !text.is_char_boundary(end) { end -= 1; }
    text[..end].to_string()
}

/// Bound every string before it becomes display state, including nested tool output.
fn bounded(v: &Value) -> Value {
    let value = match v {
        Value::String(s) => json!(clip(s, BLOCK_MAX)),
        Value::Array(a) => Value::Array(a.iter().take(128).map(bounded).collect()),
        Value::Object(o) => Value::Object(o.iter().take(128).map(|(k, v)| (clip(k, 256), bounded(v))).collect()),
        v => v.clone(),
    };
    let text = value.to_string();
    if text.len() > BLOCK_MAX {
        // JSON escaping can expand a byte up to sixfold inside the preview string.
        json!({"truncated":true, "preview":clip(&text, BLOCK_MAX / 6)})
    } else { value }
}

impl Data {
    /// Rolling dedup window: room for one more request id by dropping the oldest, so a long-lived pane never
    /// reaches a permanent `request_limit`.
    fn make_room(&mut self) {
        if self.requests.len() >= REQUESTS_MAX { self.requests.pop_front(); }
    }

    /// Bind the pane to a session id reported by the CLI or picked from history; validation keeps the value usable
    /// as an argument of the next `--resume`.
    fn bind_session(&mut self, id: Option<&str>) {
        if let Some(id) = id.filter(|id| valid_session_id(id)) {
            self.session = id.to_string();
            self.snapshot["sessionId"] = json!(self.session);
        }
    }

    fn emit(&mut self, epoch: &str, id: u64, kind: &str, payload: Value) {
        if kind == "agent" { self.reduce(&payload); }
        self.seq += 1;
        let ev = json!({"pane":id, "epoch":epoch, "runId":self.run_id, "seq":self.seq, "kind":kind, "payload":payload});
        let size = ev.to_string().len();
        while self.bytes + size > RING_MAX {
            let Some((_, n)) = self.ring.pop_front() else { break };
            self.bytes -= n;
        }
        if size <= RING_MAX { self.ring.push_back((ev.clone(), size)); self.bytes += size; }
        let line = line(json!({"event":"cc.event", "params":ev}));
        send_all(&mut self.subscribers, id, line);
    }

    fn reduce(&mut self, event: &Value) {
        if event.to_string().len() > BLOCK_MAX { self.snapshot["truncated"] = json!(true); }
        match event["type"].as_str().unwrap_or_default() {
            "run_start" => {
                self.snapshot["sessionId"] = event["sessionId"].clone();
                self.bind_session(event["sessionId"].as_str());
            }
            "message_start" => { self.snapshot["text"] = json!(""); self.snapshot["thinking"] = json!(""); },
            "thinking_start" => self.snapshot["thinking"] = json!(""),
            "text_delta" | "thinking_delta" => {
                let key = if event["type"] == "text_delta" { "text" } else { "thinking" };
                let mut text = self.snapshot[key].as_str().unwrap_or_default().to_string();
                text.push_str(event["delta"].as_str().unwrap_or_default());
                if text.len() > BLOCK_MAX { self.snapshot["truncated"] = json!(true); }
                self.snapshot[key] = json!(clip(&text, BLOCK_MAX));
            }
            "thinking_end" => self.snapshot["thinking"] = bounded(&event["text"]),
            "message_update" | "message_end" => {
                let content = bounded(&event["content"]);
                self.snapshot["content"] = content.clone();
                if event["type"] == "message_end"
                    && let Some(messages) = self.snapshot["messages"].as_array_mut() {
                        messages.push(content);
                        if messages.len() > 64 { messages.remove(0); }
                        self.snapshot["text"] = json!("");
                        self.snapshot["thinking"] = json!("");
                        self.snapshot["content"] = Value::Null;
                }
            }
            kind if kind.starts_with("tool_") => {
                if let Some(tool) = event["toolCallId"].as_str()
                    && let Some(tools) = self.snapshot["tools"].as_object_mut()
                    && (tools.len() < 128 || tools.contains_key(tool)) {
                            let value = tools.entry(clip(tool, 256)).or_insert_with(|| json!({}));
                            if let (Some(current), Some(update)) = (value.as_object_mut(), bounded(event).as_object()) {
                                current.extend(update.clone());
                            }
                }
            }
            "stream_restart" => {
                self.snapshot["text"] = json!(""); self.snapshot["thinking"] = json!("");
                self.snapshot["content"] = Value::Null;
            }
            _ => {}
        }
        if self.snapshot.to_string().len() > SNAP_MAX {
            self.snapshot["messages"] = json!([]);
            self.snapshot["tools"] = json!({});
            self.snapshot["content"] = Value::Null;
            self.snapshot["truncated"] = json!(true);
        }
    }
}

pub(super) fn create(st: &Arc<State>, p: &Value) -> Res {
    let max = st.config.lock().unwrap()["values"]["limits"]["max_panes"].as_u64().unwrap_or(32) as usize;
    if st.panes.lock().unwrap().len() >= max { return Err("max_panes"); }
    let profile_id = p["profile"].as_str().ok_or("bad_params")?;
    let profile = st.profiles.lock().unwrap().iter().find(|x| x.id == profile_id).cloned().ok_or("no_such_profile")?;
    if profile.agent != "commandcode" { return Err("wrong_kind"); }
    // Free-form profile flags/mods could undo restricted access. No shell or secret interpolation.
    if !profile.args.is_empty() || !profile.secret_env.is_empty() || profile.permission != "default" {
        return Err("unsupported_profile");
    }
    let cwd = p["cwd"].as_str().ok_or("bad_params")?;
    let cwd = Path::new(cwd).canonicalize().map_err(|_| "bad_dir")?;
    if !cwd.is_dir() { return Err("bad_dir"); }
    let workspace = p["workspace"].as_str().ok_or("bad_params")?;
    if workspace.len() > 64 { return Err("bad_params"); }
    let (model, effort) = model_effort(p)?;
    validate_model_effort(model, effort)?;
    let backend = Pane {
        pid: AtomicI32::new(0), cwd: cwd.clone(), command: profile.command, env: profile.env,
        model: model.into(), effort: effort.into(), closed: AtomicBool::new(false),
        worker: Mutex::new(None),
        data: Mutex::new(Data { seq:0, run_id:String::new(), session:String::new(), active:false, cancel:None,
            requests:VecDeque::new(), ring:VecDeque::new(), bytes:0, snapshot:snapshot(), subscribers:Vec::new() }),
        access: Mutex::new(Access { mode:"restricted", revision:0, cwd:cwd.clone(), session:String::new() }),
    };
    let key = new_secret().map_err(|_| "io")?[..16].to_string();
    let pane = Arc::new(make_pane(st, p, PaneKind::CommandCode(Box::new(backend)), "commandcode".into(), key));
    let id = st.next.fetch_add(1, Ordering::Relaxed);
    let name = pane.name.lock().unwrap().clone();
    st.panes.lock().unwrap().insert(id, pane);
    Ok(json!({"pane":id, "name":name, "kind":"commandcode", "agent":"commandcode", "access":"restricted"}))
}

pub(super) fn backend(pane: &super::Pane) -> Result<&Pane, &'static str> {
    match &pane.kind { PaneKind::CommandCode(cc) => Ok(cc), _ => Err("wrong_kind") }
}

pub(super) fn info(pane: &super::Pane) -> Res {
    let cc = backend(pane)?;
    let d = cc.data.lock().unwrap();
    let access = cc.access.lock().unwrap();
    Ok(json!({"kind":"commandcode", "agent":"commandcode", "access":access.mode, "access_revision":access.revision,
        "pane_name":*pane.name.lock().unwrap(), "status":*pane.status.lock().unwrap(),
        "busy":d.active, "cwd":cc.cwd, "pid":cc.pid.load(Ordering::Acquire),
        "session": if d.session.is_empty() { Value::Null } else { json!(d.session) },
        "runId":d.run_id, "aliases":*pane.aliases.lock().unwrap()}))
}

pub(super) fn attach(st: &State, p: &Value, conn: u64, tx: &SyncSender<String>, lagging: &Arc<Mutex<Vec<u64>>>) -> Res {
    let pane = st.pane(p)?;
    let cc = backend(&pane)?;
    let mut d = cc.data.lock().unwrap();
    let after = p["afterSeq"].as_u64();
    if cc.closed.load(Ordering::Acquire) { return Err("no_such_pane"); }
    let gap = after.is_some_and(|seq| seq > d.seq || d.ring.front().is_some_and(|(e, _)| seq.saturating_add(1) < e["seq"].as_u64().unwrap_or(0)));
    let replay: Vec<_> = d.ring.iter().filter(|(e, _)| after.is_some_and(|seq| e["seq"].as_u64().unwrap_or(0) > seq)).map(|(e, _)| e.clone()).collect();
    d.subscribers.retain(|s| s.conn != conn);
    d.subscribers.push(Sub { conn, tx:tx.clone(), lagging:lagging.clone() });
    Ok(json!({"epoch":st.boot, "snapshot":d.snapshot, "throughSeq":d.seq, "gap":gap, "events":replay}))
}

pub(super) fn detach(st: &State, p: &Value, conn: u64) -> Res {
    let pane = st.pane(p)?;
    backend(&pane)?.data.lock().unwrap().subscribers.retain(|s| s.conn != conn);
    Ok(json!({"ok":true}))
}

pub(super) fn unsubscribe(st: &State, conn: u64) {
    let panes: Vec<_> = st.panes.lock().unwrap().values().cloned().collect();
    for pane in panes {
        if let Ok(cc) = backend(&pane) { cc.data.lock().unwrap().subscribers.retain(|s| s.conn != conn); }
    }
}

pub(super) fn run(st: &Arc<State>, p: &Value) -> Res {
    let id = p["pane"].as_u64().ok_or("bad_params")?;
    let pane = st.pane(p)?;
    let cc = backend(&pane)?;
    let request = p["requestId"].as_str().filter(|s| !s.is_empty() && s.len() <= 128).ok_or("bad_params")?;
    let prompt = p["prompt"].as_str().filter(|s| !s.is_empty() && !s.contains('\0')).ok_or("bad_params")?;
    if cc.cwd.canonicalize().ok().as_ref() != Some(&cc.cwd) { return Err("bad_dir"); }
    if p.as_object().is_none_or(|o| o.keys().any(|k| !["pane", "requestId", "prompt", "model", "effort"].contains(&k.as_str()))) {
        return Err("bad_params");
    }
    let (model, effort) = model_effort(p)?;
    validate_model_effort(model, effort)?;
    let mut worker = cc.worker.lock().unwrap();
    let mut d = cc.data.lock().unwrap();
    if cc.closed.load(Ordering::Acquire) { return Err("no_such_pane"); }
    if let Some((_, run)) = d.requests.iter().find(|(r, _)| r == request) { return Ok(json!({"runId":run})); }
    if d.active { return Err("busy"); }
    // Dedup window: a retried id stays idempotent while it is one of the last REQUESTS_MAX runs.
    d.make_room();
    // A pane keeps one conversation: the next run resumes the bound session explicitly.
    let resume = (!d.session.is_empty()).then(|| d.session.clone());
    // Wide access only while a UI grant still covers this folder and the bound session.
    let yolo = {
        let mut access = cc.access.lock().unwrap();
        let valid = access.mode == "yolo" && access.cwd == cc.cwd
            && (access.session.is_empty() || access.session == d.session);
        if access.mode == "yolo" && !valid {
            // The folder or session moved under the grant: it no longer applies, and the UI must ask again.
            access.mode = "restricted"; access.session.clear(); access.revision += 1;
        }
        valid
    };
    // Never join while holding data: the worker completes its final state under that lock.
    drop(d);
    if let Some(old) = worker.take() { let _ = old.join(); }
    let mut d = cc.data.lock().unwrap();
    let run_id = new_secret().map_err(|_| "io")?;
    let mut cmd = Command::new(&st.hooks.exe);
    cmd.args(["jail", "--", &cc.command]).arg(format!("--print={prompt}")).args(["--output-format", "json"]);
    cmd.args(runner::flags("commandcode", if model.is_empty() { &cc.model } else { model }, if effort.is_empty() { &cc.effort } else { effort }, None));
    // `--resume <id>`, never `--continue`: the id is the pane's own conversation, not "the latest in this cwd".
    if let Some(session) = &resume { cmd.args(["--resume", session]); }
    // Only the default access path is restricted; a grant recorded by the UI gate unlocks this one flag.
    if yolo { cmd.arg("--yolo"); }
    pty::scrub_env(&mut cmd);
    cmd.envs(cc.env.iter().cloned()).current_dir(&cc.cwd).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    unsafe { cmd.pre_exec(|| { if libc::setsid() == -1 { Err(io::Error::last_os_error()) } else { Ok(()) } }); }
    // hello/pane_of_pid holds the same registry lock. Publish SID before the child can authenticate.
    let panes = st.panes.lock().unwrap();
    let mut child = cmd.spawn().map_err(|_| "spawn_failed")?;
    let pid = child.id() as i32;
    if nonblocking(&child).is_err() {
        unsafe { libc::killpg(pid, libc::SIGKILL); }
        let _ = child.wait();
        return Err("io");
    }
    cc.pid.store(pid, Ordering::Release);
    drop(panes);
    d.active = true; d.cancel = None; d.run_id = run_id.clone();
    d.snapshot["text"] = json!(""); d.snapshot["thinking"] = json!("");
    d.snapshot["content"] = Value::Null; d.snapshot["result"] = Value::Null;
    d.snapshot["diagnostic"] = Value::Null; d.snapshot["stderr"] = json!("");
    d.snapshot["state"] = json!("working"); d.snapshot["runId"] = json!(run_id);
    d.snapshot["access"] = json!(if yolo { "yolo" } else { "restricted" });
    d.snapshot["exitCode"] = Value::Null;
    d.requests.push_back((request.into(), run_id.clone()));
    d.emit(&st.boot, id, "state", json!({"state":"working", "access": if yolo { "yolo" } else { "restricted" }}));
    drop(d);
    *pane.status.lock().unwrap() = "working";
    let state = st.clone(); let running = pane.clone();
    *worker = Some(thread::spawn(move || pump(&state, id, &running, child)));
    Ok(json!({"runId":run_id}))
}

fn nonblocking(child: &Child) -> io::Result<()> {
    let stdout = child.stdout.as_ref().ok_or_else(|| io::Error::other("missing stdout"))?;
    let stderr = child.stderr.as_ref().ok_or_else(|| io::Error::other("missing stderr"))?;
    for fd in [stdout.as_raw_fd(), stderr.as_raw_fd()] {
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 { return Err(io::Error::last_os_error()); }
    }
    Ok(())
}

pub(super) fn cancel(st: &State, p: &Value) -> Res {
    let pane = st.pane(p)?;
    let cc = backend(&pane)?;
    let mut d = cc.data.lock().unwrap();
    if p["runId"].as_str().is_none_or(|id| id != d.run_id) || d.run_id.is_empty() { return Err("stale_run"); }
    if d.active && d.cancel.is_none() { d.cancel = Some(Instant::now()); }
    Ok(json!({"accepted":true}))
}

/// `cc.access`: the UI gate. Wide access is a grant held by the daemon for this pane, never an argument of `cc.run`;
/// only the UI channel reaches this (SEC-20). `revision` rejects a stale dialog, and revoking stops a live run first.
pub(super) fn access(st: &State, p: &Value) -> Res {
    let id = p["pane"].as_u64().ok_or("bad_params")?;
    let pane = st.pane(p)?;
    let cc = backend(&pane)?;
    if p.as_object().is_none_or(|o| o.keys().any(|k| !["pane", "mode", "gateId", "revision"].contains(&k.as_str()))) {
        return Err("bad_params");
    }
    let mode = p["mode"].as_str().filter(|m| *m == "restricted" || *m == "yolo").ok_or("bad_params")?;
    let revision = p["revision"].as_u64().ok_or("bad_params")?;
    let mut d = cc.data.lock().unwrap();
    let mut access = cc.access.lock().unwrap();
    if access.revision != revision { return Err("stale_revision"); }
    if mode == "yolo" {
        // The gate itself lives in the UI: the daemon only records the answered dialog and its scope.
        if p["gateId"].as_str().filter(|g| !g.is_empty() && g.len() <= 128).is_none() { return Err("bad_gate"); }
        access.mode = "yolo";
        access.cwd = cc.cwd.clone();
        access.session = d.session.clone();
    } else {
        // Revoking applies to the running request too, not just the next one.
        if d.active && d.cancel.is_none() { d.cancel = Some(Instant::now()); }
        access.mode = "restricted";
        access.session.clear();
    }
    access.revision += 1;
    let out = json!({"mode":access.mode, "revision":access.revision, "cwd":access.cwd,
        "session": if access.session.is_empty() { Value::Null } else { json!(access.session) }});
    d.snapshot["access"] = json!(access.mode);
    drop(access);
    d.emit(&st.boot, id, "state", json!({"access":out["mode"], "revision":out["revision"]}));
    Ok(out)
}

fn valid_session_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 80 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

struct SessionFile {
    id: String,
    path: PathBuf,
    updated: u64,
    title: Option<String>,
    unsupported: bool,
}

fn projects_root() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    std::fs::canonicalize(Path::new(&home).join(".commandcode").join("projects")).ok()
}

/// First line only: the transcript header names the session, its format version and the folder it belongs to.
fn read_header(path: &Path) -> Option<Value> {
    use std::io::Read;
    let mut buf = Vec::new();
    std::fs::File::open(path).ok()?.take(HEADER_MAX).read_to_end(&mut buf).ok()?;
    serde_json::from_slice(buf.split(|b| *b == b'\n').next()?).ok()
}

/// Sessions of this pane's folder: the `cwd` in each header is the truth, not the project-slug folder name.
/// Files that resolve outside the projects folder are not ours, and sidecars are not transcripts.
fn scan_sessions(cwd: &Path) -> Vec<SessionFile> {
    let Some(root) = projects_root() else { return Vec::new() };
    let mut found = Vec::new();
    let Ok(dirs) = std::fs::read_dir(&root) else { return found };
    for dir in dirs.flatten() {
        let Ok(entries) = std::fs::read_dir(dir.path()) else { continue };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if !name.ends_with(".jsonl") || name.ends_with(".checkpoints.jsonl") || name.ends_with(".prompts.jsonl") { continue; }
            let Ok(path) = std::fs::canonicalize(entry.path()) else { continue };
            if !path.starts_with(&root) { continue; }
            let Ok(meta) = std::fs::metadata(&path) else { continue };
            let updated = meta.modified().ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs());
            let Some(header) = read_header(&path) else { continue };
            let Some(header_cwd) = header["cwd"].as_str().and_then(|c| std::fs::canonicalize(c).ok()) else { continue };
            if header_cwd != cwd { continue; }
            let Some(id) = header["id"].as_str().filter(|id| valid_session_id(id)) else { continue };
            found.push(SessionFile {
                id: id.to_string(), path, updated,
                unsupported: header["type"] != "session" || header["version"].as_u64() != Some(3),
                title: header["title"].as_str().map(|t| t.chars().take(200).collect()),
            });
        }
    }
    found.sort_by(|a, b| b.updated.cmp(&a.updated).then_with(|| a.path.cmp(&b.path)));
    found
}

/// `cc.sessions`: a paged list of this pane's folder, newest first. Not a browser for the whole home folder.
pub(super) fn sessions(st: &State, p: &Value) -> Res {
    let pane = st.pane(p)?;
    let cc = backend(&pane)?;
    if p.as_object().is_none_or(|o| o.keys().any(|k| !["pane", "cursor"].contains(&k.as_str()))) { return Err("bad_params"); }
    let offset = match p.get("cursor") {
        None | Some(Value::Null) => 0,
        Some(Value::String(s)) => s.parse::<usize>().map_err(|_| "bad_params")?,
        Some(_) => return Err("bad_params"),
    };
    let all = scan_sessions(&cc.cwd);
    let page: Vec<Value> = all.iter().skip(offset).take(SESSIONS_PAGE).map(|s| {
        let mut v = json!({"id": s.id, "path": s.path.to_string_lossy(), "updatedAt": s.updated});
        if let Some(title) = &s.title { v["title"] = json!(title); }
        if s.unsupported { v["unsupported"] = json!(true); }
        v
    }).collect();
    let mut out = json!({"sessions": page});
    if offset + SESSIONS_PAGE < all.len() { out["next"] = json!((offset + SESSIONS_PAGE).to_string()); }
    Ok(out)
}

fn block_value(block: &Block) -> Value {
    match block {
        Block::Text(text) => json!({"type":"text", "text":clip(text, BLOCK_MAX)}),
        Block::Thinking(text) => json!({"type":"thinking", "thinking":clip(text, BLOCK_MAX)}),
        Block::ToolUse { id, name, input } => json!({"type":"tool_use", "id":id, "name":name, "input":bounded(input)}),
        Block::ToolResult { tool_use_id, content, is_error } =>
            json!({"type":"tool_result", "tool_use_id":tool_use_id, "content":bounded(content), "is_error":is_error}),
        Block::Unsupported(kind) => json!({"type":"unsupported", "name":kind}),
    }
}

/// `cc.resume`: bind the pane to a committed transcript and show its active branch. No spawn: the next `cc.run`
/// resumes it. A session has one writer, so a second pane cannot bind the same id.
pub(super) fn resume(st: &State, p: &Value) -> Res {
    let id = p["pane"].as_u64().ok_or("bad_params")?;
    let pane = st.pane(p)?;
    let cc = backend(&pane)?;
    if p.as_object().is_none_or(|o| o.keys().any(|k| !["pane", "sessionId"].contains(&k.as_str()))) { return Err("bad_params"); }
    let wanted = p["sessionId"].as_str().filter(|s| valid_session_id(s)).ok_or("bad_params")?;
    if cc.closed.load(Ordering::Acquire) { return Err("no_such_pane"); }
    if cc.data.lock().unwrap().active { return Err("busy"); }
    let others: Vec<(u64, Arc<super::Pane>)> = st.panes.lock().unwrap().iter().map(|(id, p)| (*id, Arc::clone(p))).collect();
    for (other, pane) in &others {
        if *other == id { continue; }
        if let Ok(backend) = backend(pane)
            && backend.data.lock().unwrap().session == wanted { return Err("session_in_use"); }
    }
    let found = scan_sessions(&cc.cwd).into_iter().find(|s| s.id == wanted).ok_or("no_such_session")?;
    if found.unsupported { return Err("unsupported_format"); }
    let meta = std::fs::metadata(&found.path).map_err(|_| "io")?;
    if meta.len() > TRANSCRIPT_MAX { return Err("too_large"); }
    let bytes = std::fs::read(&found.path).map_err(|_| "io")?;
    let history = crate::commandcode::transcript(&bytes, None).map_err(|_| "unsupported_format")?;
    let mut messages: Vec<Value> = history.entries.iter()
        .filter_map(|e| e.message.as_ref().map(|m| (e, m)))
        .map(|(e, m)| json!({"id":e.id, "role":m.role, "timestamp":e.timestamp,
            "content": m.content.iter().map(block_value).collect::<Vec<_>>() }))
        .collect();
    if messages.len() > HISTORY_MESSAGES_MAX { messages.drain(..messages.len() - HISTORY_MESSAGES_MAX); }
    let partial = history.partial;
    let mut d = cc.data.lock().unwrap();
    d.bind_session(Some(wanted));
    // Reopened history starts restricted again: wide access needs a fresh gate.
    let mut access = cc.access.lock().unwrap();
    if access.mode != "restricted" { access.mode = "restricted"; access.session.clear(); access.revision += 1; }
    let revision = access.revision;
    drop(access);
    d.snapshot["access"] = json!("restricted");
    let state = d.snapshot["state"].clone();
    d.emit(&st.boot, id, "state", json!({"state":state, "sessionId":wanted, "access":"restricted", "revision":revision}));
    drop(d);
    Ok(json!({"sessionId":wanted, "path":found.path.to_string_lossy(),
        "history":{"messages":messages, "partial":partial}}))
}

pub(super) fn close(st: &State, id: u64, pane: &super::Pane) {
    let Ok(cc) = backend(pane) else { return };
    let mut worker = cc.worker.lock().unwrap();
    cc.closed.store(true, Ordering::Release);
    { let mut d = cc.data.lock().unwrap(); if d.active && d.cancel.is_none() { d.cancel = Some(Instant::now()); } }
    if let Some(worker) = worker.take() { let _ = worker.join(); }
    st.panes.lock().unwrap().remove(&id);
    st.engine.lock().unwrap().opens.retain(|o| o.msg.from != id);
    st.engine.lock().unwrap().closes.retain(|c| c.msg.from != id && c.msg.to != id);
    cc.data.lock().unwrap().subscribers.clear();
    let ev = line(json!({"event":"pane.exit", "params":{"pane":id, "code":null}}));
    st.ui_txs.lock().unwrap().retain(|(_, tx)| tx.try_send(ev.clone()).is_ok());
}

/// Daemon shutdown: stop and reap every live Command Code run. A pane without a PTY doesn't get the kernel's
/// SIGHUP when the daemon exits, so its setsid child would outlive us; cancel + join is bounded by the pump's
/// escalation (SIGINT, SIGTERM at 500 ms, SIGKILL at 2 s, drain cap 1 s).
pub(super) fn shutdown(st: &State) {
    let panes: Vec<(u64, Arc<super::Pane>)> = st.panes.lock().unwrap().iter().map(|(id, p)| (*id, Arc::clone(p))).collect();
    for (id, pane) in panes.iter().filter(|(_, p)| matches!(p.kind, PaneKind::CommandCode(_))) {
        close(st, *id, pane);
    }
}

fn result_value(r: &RunResult) -> Value {
    json!({"subtype":r.subtype, "sessionId":r.session_id, "stopReason":r.stop_reason,
        "finalText":r.final_text.as_deref().map(|s| clip(s, BLOCK_MAX)), "error":r.error.as_ref().map(bounded), "durationMs":r.duration_ms,
        "usage":{"inputTokens":r.usage.input_tokens,"outputTokens":r.usage.output_tokens,
            "cacheReadTokens":r.usage.cache_read_tokens,"cacheWriteTokens":r.usage.cache_write_tokens}})
}

fn bind_grant(cc: &Pane, d: &Data) {
    if d.session.is_empty() { return; }
    let mut access = cc.access.lock().unwrap();
    // A grant made before the first run follows the session that run created.
    if access.mode == "yolo" && access.session.is_empty() { access.session = d.session.clone(); }
}

fn envelopes(st: &State, id: u64, cc: &Pane, values: Vec<Envelope>, result: &mut Option<RunResult>) {
    let mut d = cc.data.lock().unwrap();
    for value in values {
        match value {
            Envelope::Event(event) => d.emit(&st.boot, id, "agent", event),
            Envelope::Result(r) => *result = Some(r),
        }
    }
    bind_grant(cc, &d);
}

fn exit_kind(code: Option<i32>) -> &'static str {
    match code { Some(0) => "done", Some(3) => "auth", Some(4) => "permission", Some(5) => "rate_limit",
        Some(6) => "network", Some(7) => "server", Some(8) => "max_turns", Some(9) => "no_response",
        Some(10) => "credits", Some(130) | None => "interrupted", _ => "error" }
}

fn pump(st: &State, id: u64, pane: &super::Pane, mut child: Child) {
    let Ok(cc) = backend(pane) else { let _ = child.kill(); let _ = child.wait(); return };
    let mut reader = Reader::default();
    let mut result = None;
    let mut failure = None;
    let mut stderr = Vec::new();
    let mut out_eof = false; let mut err_eof = false;
    let mut exit = None;
    let mut exited_at = None;
    let mut signal = 0;
    let pid = child.id() as i32;
    loop {
        let cancel = cc.data.lock().unwrap().cancel;
        if let Some(at) = cancel {
            let next = if at.elapsed() >= Duration::from_secs(2) { libc::SIGKILL } else if at.elapsed() >= Duration::from_millis(500) { libc::SIGTERM } else { libc::SIGINT };
            if next != signal { unsafe { libc::killpg(pid, next); } signal = next; }
        }
        let mut buf = [0u8; 16 * 1024];
        if !out_eof {
            match child.stdout.as_mut().map(|out| out.read(&mut buf)) {
                Some(Ok(0)) | None => out_eof = true,
                Some(Ok(n)) if failure.is_none() => match reader.feed(&buf[..n]) {
                    Ok(values) => envelopes(st, id, cc, values, &mut result),
                    Err(e) => failure = Some(e),
                },
                Some(Err(e)) if e.kind() != io::ErrorKind::WouldBlock && e.kind() != io::ErrorKind::Interrupted => { failure = Some("stdout read failed".into()); out_eof = true; },
                _ => {}
            }
        }
        if !err_eof {
            match child.stderr.as_mut().map(|err| err.read(&mut buf)) {
                Some(Ok(0)) | None => err_eof = true,
                Some(Ok(n)) => { let left = BLOCK_MAX.saturating_sub(stderr.len()); stderr.extend_from_slice(&buf[..n.min(left)]); },
                Some(Err(e)) if e.kind() != io::ErrorKind::WouldBlock && e.kind() != io::ErrorKind::Interrupted => err_eof = true,
                _ => {}
            }
        }
        if failure.is_some() { let mut d = cc.data.lock().unwrap(); if d.cancel.is_none() { d.cancel = Some(Instant::now()); } }
        if exit.is_none() {
            match child.try_wait() {
                Ok(Some(status)) => { exit = Some(status); exited_at = Some(Instant::now()); },
                Ok(None) => {},
                Err(_) => { failure = Some("child wait failed".into()); unsafe { libc::killpg(pid, libc::SIGKILL); } exit = child.wait().ok(); exited_at = Some(Instant::now()); },
            }
        }
        if exited_at.is_some() && out_eof && err_eof {
            if cancel.is_some() { unsafe { libc::killpg(pid, libc::SIGKILL); } }
            break;
        }
        // Descendants holding pipes cannot keep close/cancel or a dead child alive forever.
        if exited_at.is_some_and(|at| at.elapsed() >= Duration::from_secs(1)) {
            unsafe { libc::killpg(pid, libc::SIGKILL); }
            failure.get_or_insert_with(|| "output drain timed out".into());
            break;
        }
        thread::sleep(Duration::from_millis(5));
    }
    if failure.is_none() {
        match reader.finish() { Ok(values) => envelopes(st, id, cc, values, &mut result), Err(e) => failure = Some(e) }
    }
    let code = exit.and_then(|status| status.code());
    let mut d = cc.data.lock().unwrap();
    let eof = failure.as_deref() == Some("EOF without result");
    let cancelled = d.cancel.is_some() && (failure.is_none() || eof);
    let mismatch = result.as_ref().is_some_and(|r| match r.subtype.as_str() {
        "success" => code != Some(0),
        "max_turns" => code != Some(8),
        _ => code == Some(0) || code == Some(8),
    });
    let state = if failure.is_some() && !eof { "protocol_error" }
        else if cancelled || (eof && code == Some(0)) { "interrupted" }
        else if eof { exit_kind(code) }
        else if mismatch { failure = Some("result/exit mismatch".into()); "protocol_error" }
        else { exit_kind(code) };
    if let Some(r) = result && state != "protocol_error" && !cancelled {
        if r.final_text.as_ref().is_some_and(|s| s.len() > BLOCK_MAX) { d.snapshot["truncated"] = json!(true); }
        let value = result_value(&r);
        d.bind_session(r.session_id.as_deref());
        bind_grant(cc, &d);
        d.snapshot["result"] = value.clone();
        d.emit(&st.boot, id, "result", value);
    }
    d.snapshot["state"] = json!(state); d.snapshot["exitCode"] = json!(code);
    d.snapshot["diagnostic"] = json!(failure);
    d.snapshot["stderr"] = json!(clip(&telegram::mask_secrets(&String::from_utf8_lossy(&stderr)), BLOCK_MAX));
    d.active = false;
    cc.pid.store(0, Ordering::Release);
    d.emit(&st.boot, id, "state", json!({"state":state,"exitCode":code}));
    *pane.status.lock().unwrap() = if state == "done" { "done" } else { "idle" };
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> Arc<State> {
        Arc::new(State {
            panes: Mutex::default(), next: AtomicU64::new(1), secret: None, ui_req: None,
            pane_sock: String::new(), ui_clients: AtomicUsize::new(0), ui_txs: Mutex::default(),
            ui_next: AtomicU64::new(1), config: Mutex::new(Value::Null),
            profiles: Mutex::new(profile::builtin()), accounts: Mutex::default(), addons: Mutex::default(),
            agents: Mutex::new(Value::Null), boot:"test-epoch".into(),
            hooks:profile::Hooks { exe:String::new(), dir:PathBuf::new() }, engine:Mutex::default(),
            pane_subs:Mutex::default(), audit:None, harness:Mutex::default(), tg:Mutex::default(),
            update:Mutex::default(), usage:Mutex::default(), models:Mutex::default(), report_shot:Mutex::default(),
        })
    }

    fn pane(st: &Arc<State>) -> (u64, Arc<super::super::Pane>) {
        let r = create(st, &json!({"workspace":"test", "cwd":std::env::temp_dir(), "profile":"commandcode"})).unwrap();
        let id = r["pane"].as_u64().unwrap();
        (id, st.pane(&json!({"pane":id})).unwrap())
    }

    #[test]
    fn request_ledger_rolls_instead_of_locking_the_pane() {
        let st = state(); let (_, pane) = pane(&st);
        let mut d = backend(&pane).unwrap().data.lock().unwrap();
        for i in 0..REQUESTS_MAX { d.requests.push_back((format!("r{i}"), format!("run{i}"))); }
        d.make_room();
        d.requests.push_back(("latest".into(), "run-latest".into()));
        assert_eq!(d.requests.len(), REQUESTS_MAX);
        assert_eq!(d.requests.front().map(|(r, _)| r.as_str()), Some("r1"), "oldest request id evicted");
        assert_eq!(d.requests.back().map(|(r, _)| r.as_str()), Some("latest"));
        assert_eq!(d.requests.iter().find(|(r, _)| r == "latest").map(|(_, run)| run.as_str()), Some("run-latest"));
    }

    #[test]
    fn output_strings_are_bounded_at_utf8_boundary() {
        assert_eq!(clip("a🌱b", 4), "a");
        let large = bounded(&json!({"text":"x".repeat(BLOCK_MAX + 1)}));
        assert!(large.to_string().len() <= BLOCK_MAX);
        assert!(large["truncated"] == true || large["text"]["truncated"] == true);
    }

    #[test]
    fn reducer_replaces_message_snapshots_and_keeps_tool_input() {
        let st = state(); let (_, pane) = pane(&st);
        let mut d = backend(&pane).unwrap().data.lock().unwrap();
        d.reduce(&json!({"type":"text_delta","delta":"draft"}));
        d.reduce(&json!({"type":"message_update","content":[{"type":"text","text":"draft"}]}));
        d.reduce(&json!({"type":"message_end","content":[{"type":"text","text":"final"}]}));
        assert_eq!(d.snapshot["messages"], json!([[{"type":"text","text":"final"}]]));
        assert_eq!(d.snapshot["text"], "");
        d.reduce(&json!({"type":"tool_queued","toolCallId":"one","toolName":"read_file","input":{"path":"a"}}));
        d.reduce(&json!({"type":"tool_completed","toolCallId":"one","result":"read"}));
        assert_eq!(d.snapshot["tools"]["one"]["input"]["path"], "a");
        assert_eq!(d.snapshot["tools"]["one"]["type"], "tool_completed");
        d.reduce(&json!({"type":"text_delta","delta":"discard"}));
        d.reduce(&json!({"type":"stream_restart"}));
        assert_eq!(d.snapshot["text"], "");
        assert_eq!(d.snapshot["messages"].as_array().unwrap().len(), 1);
        assert_eq!(d.snapshot["tools"]["one"]["result"], "read");
    }

    #[test]
    fn bounded_ring_reduces_before_eviction_and_drops_slow_subscribers() {
        let st = state(); let (id, pane) = pane(&st);
        let mut d = backend(&pane).unwrap().data.lock().unwrap();
        let (tx, _rx) = sync_channel(1);
        let lagging = Arc::new(Mutex::new(Vec::new()));
        d.subscribers.push(Sub { conn:7, tx, lagging:lagging.clone() });
        for _ in 0..34 { d.emit(&st.boot, id, "agent", json!({"type":"text_delta","delta":"x".repeat(BLOCK_MAX)})); }
        assert!(d.bytes <= RING_MAX);
        assert!(d.ring.front().unwrap().0["seq"].as_u64().unwrap() > 1);
        assert_eq!(d.snapshot["text"].as_str().unwrap().len(), BLOCK_MAX);
        assert_eq!(d.snapshot["truncated"], true);
        assert!(d.subscribers.is_empty());
        assert_eq!(*lagging.lock().unwrap(), vec![id]);
    }

    #[test]
    fn attach_and_snapshot_have_no_race_gap() {
        let st = state(); let (id, pane) = pane(&st);
        let (tx, rx) = sync_channel(2048);
        let running = pane.clone(); let epoch = st.boot.clone();
        let writer = thread::spawn(move || {
            for _ in 0..1000 { backend(&running).unwrap().data.lock().unwrap().emit(&epoch, id, "agent", json!({"type":"text_delta","delta":"a"})); }
        });
        let r = attach(&st, &json!({"pane":id}), 1, &tx, &Arc::default()).unwrap();
        let through = r["throughSeq"].as_u64().unwrap();
        assert_eq!(r["snapshot"]["text"].as_str().unwrap().len() as u64, through);
        writer.join().unwrap();
        let events: Vec<Value> = rx.try_iter().map(|s| serde_json::from_str(&s).unwrap()).collect();
        assert_eq!(events.len() as u64, 1000 - through);
        for (i, event) in events.iter().enumerate() { assert_eq!(event["params"]["seq"], through + i as u64 + 1); }
        unsubscribe(&st, 1);
        assert!(backend(&pane).unwrap().data.lock().unwrap().subscribers.is_empty());
    }

    #[test]
    fn telegram_remote_and_harness_capability_guards() {
        let st = state(); let (id, pane) = pane(&st);
        assert_eq!(pane.terminal().err(), Some("wrong_kind"));
        assert_eq!(telegram::checked_target(Some((id, pane.clone())), "no_pane").err(), Some("unsupported_kind"));
        assert_eq!(pane_method(&st, id, "send", &json!({})).unwrap_err(), "unsupported_kind");
        assert_eq!(runner::method(&st, "harness.start", &json!({"from_pane":id})).unwrap_err(), "unsupported_kind");
        let m = bus::Msg { id:"test".into(), from:0, from_name:"remote".into(), to:id, to_name:"commandcode-1".into(), body:"do not execute".into(), reply_to:None, hop:0, at:std::time::SystemTime::now() };
        assert_eq!(try_send(&st, m, true, true)["reason"], "unsupported_kind");
        assert!(backend(&pane).unwrap().data.lock().unwrap().requests.is_empty());
    }

    #[test]
    fn unsafe_profile_and_invalid_directory_fail_before_spawn() {
        let st = state();
        let p = json!({"workspace":"test", "cwd":std::env::temp_dir(), "profile":"commandcode"});
        st.profiles.lock().unwrap().iter_mut().find(|p| p.id == "commandcode").unwrap().args.push("--yolo".into());
        assert_eq!(create(&st, &p).unwrap_err(), "unsupported_profile");
        st.profiles.lock().unwrap().iter_mut().find(|p| p.id == "commandcode").unwrap().args.clear();
        assert_eq!(create(&st, &json!({"workspace":"test", "cwd":"/gembala-missing-test-directory", "profile":"commandcode"})).unwrap_err(), "bad_dir");
        assert!(st.panes.lock().unwrap().is_empty());
    }
}
