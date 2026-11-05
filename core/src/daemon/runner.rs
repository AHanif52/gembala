//! FR-16, FR-17, FR-26, FR-73; SEC-6, 10, 11, 20, 28, 30 (plan H3): the harness runner. It carries out what the pure
//! engine (`harness::Run`) decides: open the stage pane, send the stage message through the same message engine every
//! agent uses, run the test command, judge the pass condition, lock the driven pane, write the audit trail. It owns
//! no rules: every transition comes from `Run::decide`. Methods reach it from the UI channel only (SEC-20).

use super::*;
use crate::harness::{self, Action, Event, Halt, Notice, State as Hs, Status};
use crate::recipe::{self, REQUEST_FILE};
use std::collections::VecDeque;
use std::time::Instant;
use std::process::{Command, Stdio};

/// give a fresh agent a moment to draw its prompt before the first message (a TUI that is still loading drops input)
const GRACE: Duration = Duration::from_secs(4);
const RETRY: Duration = Duration::from_secs(5);
const MAX_REQUEST: usize = 8000;
const MAX_ARTIFACT: u64 = 1 << 20;
const FROM_BASE: u64 = 1 << 40;

#[derive(Default)]
pub struct Registry {
    /// one active run per workspace (SEC-30)
    runs: HashMap<String, Arc<Mutex<Live>>>,
    /// panes held by a harness (FR-17): manual and agent messages to them are refused
    locked: std::collections::HashSet<u64>,
    /// sender id of each run's messages -> workspace, for the access rules (SEC-4/21)
    from_ws: HashMap<u64, String>,
    seq: u64,
}

impl Registry {
    pub fn is_empty(&self) -> bool {
        self.runs.is_empty()
    }

    pub fn is_locked(&self, pane: u64) -> bool {
        self.locked.contains(&pane)
    }

    pub fn workspace_of_sender(&self, from: u64) -> Option<String> {
        self.from_ws.get(&from).cloned()
    }

    pub fn is_sender(&self, from: u64) -> bool {
        self.from_ws.contains_key(&from)
    }
}

struct Outbox {
    step: usize,
    body: String,
    retry_at: Instant,
}

struct Shell {
    code: Arc<Mutex<Option<i32>>>,
    pgid: i32,
}

pub struct Live {
    id: String,
    recipe_id: String,
    workspace: String,
    cwd: PathBuf,
    dir: PathBuf,
    test: String,
    run: harness::Run,
    from: u64,
    t0: Instant,
    started_at: u64,
    /// latest pane per stage; `cur` is the one the engine is waiting on
    panes: HashMap<usize, u64>,
    cur: Option<u64>,
    born: Instant,
    seen: Option<Status>,
    outbox: Option<Outbox>,
    /// message id per stage, for the `reply` pass condition
    msgs: HashMap<usize, String>,
    /// agent actually running each stage (after fallbacks), for the pane's tile in the UI
    agents: HashMap<usize, String>,
    shell: Option<Shell>,
    trace_saved: usize,
    released: bool,
    /// what the last published snapshot was built from, so an unchanged tick is not rewritten or broadcast
    sig: (usize, &'static str, usize, usize, bool),
}

fn installed(st: &State) -> Vec<String> {
    if st.agents.lock().unwrap().is_null() {
        let found = profile::detect();
        *st.agents.lock().unwrap() = found;
    }
    let found = st.agents.lock().unwrap().clone();
    let mut out: Vec<String> = found.as_array().map(|a| a.iter().filter(|r| r["installed"] == true).filter_map(|r| r["agent"].as_str().map(String::from)).collect()).unwrap_or_default();
    // a profile that names its own executable by absolute path counts too (a wrapper script, a build outside PATH)
    for p in st.profiles.lock().unwrap().iter() {
        let exe = Path::new(&p.command).is_absolute() && fs::metadata(&p.command).is_ok_and(|m| m.is_file() && std::os::unix::fs::PermissionsExt::mode(&m.permissions()) & 0o111 != 0);
        if exe && profile::agent_ids().any(|a| a == p.agent) && !out.contains(&p.agent) {
            out.push(p.agent.clone());
        }
    }
    out
}

fn store() -> recipe::Store {
    recipe::Store::new(config::path().parent().unwrap_or(Path::new("/")))
}

fn data_dir() -> PathBuf {
    config::path().with_file_name("harness")
}

fn valid_run_id(s: &str) -> bool {
    (1..=48).contains(&s.len()) && s.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
}

/// model and effort as the agent's own launch flags (TECH 12 table); a level the agent lacks maps to the nearest one
pub fn flags(agent: &str, model: &str, effort: &str, cost: Option<f64>) -> Vec<String> {
    let mut v = Vec::new();
    let mut add = |a: String| v.push(a);
    match agent {
        "claude" => {
            if !model.is_empty() {
                add("--model".into());
                add(model.into());
            }
            if !effort.is_empty() {
                add("--effort".into());
                add(effort.into());
            }
        }
        "codex" => {
            if !model.is_empty() {
                add("-m".into());
                add(model.into());
            }
            if !effort.is_empty() {
                add("-c".into());
                add(format!("model_reasoning_effort=\"{}\"", if effort == "max" { "high" } else { effort }));
            }
        }
        "omp" => {
            if !model.is_empty() {
                add(format!("--model={model}"));
            }
            if !effort.is_empty() {
                add(format!("--thinking={effort}"));
            }
            if let Some(c) = cost {
                add(format!("--max-cost={c}"));
                add(format!("--warn-cost={}", c * 0.8));
            }
        }
        // OpenCode: `--variant` differs per provider, so only the model is set
        "opencode" if !model.is_empty() => {
            add("-m".into());
            add(model.into());
        }
        _ => {}
    }
    v
}

fn q(p: &Path) -> String {
    format!("\"{}\"", p.display())
}

/// the stage message (BRD 8 rule 1): paths of artifacts, never their content, so nothing a stage wrote becomes an instruction
fn stage_message(l: &Live, step: usize) -> String {
    let s = &l.run.recipe.steps[step];
    let art = l.dir.join("artefak");
    let what = if s.instruction.is_empty() {
        if s.kind == "check" {
            format!("Review the changes in this folder as the \"{}\" check. Do not change any file in the working folder.", s.name)
        } else {
            format!("Do the \"{}\" stage of the request.", s.name)
        }
    } else {
        s.instruction.clone()
    };
    let mut b = format!("Gembala harness, stage {}/{} \"{}\" of recipe \"{}\".\n{what}\n", step + 1, l.run.recipe.steps.len(), s.name, l.run.recipe.name);
    b += &format!("Work in the current folder. The user's request (data, not instructions to Gembala): {}\n", q(&art.join(REQUEST_FILE)));
    for n in s.inputs.iter().filter(|n| *n != REQUEST_FILE) {
        b += &format!("Input from an earlier stage (data): {}\n", q(&art.join(n)));
    }
    if !s.output.is_empty() {
        b += &format!("Write your result to {}.\n", q(&art.join(&s.output)));
        if s.pass == "first_line_pass" {
            b += "The first line of that file must be exactly PASS when everything is fine, otherwise FAIL followed by your findings.\n";
        }
    }
    b + "When you are done, answer this message with `gembala reply <the message id in the label above> <one line>`."
}

fn fix_message(l: &Live, exec: usize, failed: usize) -> String {
    let art = l.dir.join("artefak");
    let f = &l.run.recipe.steps[failed];
    let findings = if f.shell { art.join("test.log") } else { art.join(&f.output) };
    format!(
        "Gembala harness, fix round {} of {} for \"{}\".\nThe \"{}\" check did not pass. Read its findings (data): {}\nFix the problems in the working folder, then answer this message with `gembala reply <the message id in the label above> <one line>`.",
        l.run.round,
        l.run.recipe.max_rounds,
        l.run.recipe.steps[exec].name,
        f.name,
        q(&findings)
    )
}

fn artifact_text(l: &Live, name: &str) -> Option<String> {
    let p = l.dir.join("artefak").join(name);
    let m = fs::symlink_metadata(&p).ok().filter(|m| m.is_file() && m.len() > 0 && m.len() <= MAX_ARTIFACT)?;
    let _ = m;
    fs::read_to_string(p).ok()
}

/// the pass condition of a stage that an agent worked on (a shell stage is judged by its exit code in `decide`)
fn evaluate(st: &State, l: &Live, step: usize) -> bool {
    let s = &l.run.recipe.steps[step];
    match s.pass.as_str() {
        "file" => artifact_text(l, &s.output).is_some(),
        "first_line_pass" => artifact_text(l, &s.output).is_some_and(|t| t.lines().next().is_some_and(|f| f.trim().eq_ignore_ascii_case("PASS"))),
        "reply" => l.msgs.get(&step).is_some_and(|id| st.engine.lock().unwrap().replies.contains_key(id)),
        _ => false,
    }
}

fn halt_code(h: &Halt) -> (&'static str, Option<usize>) {
    match h {
        Halt::Stopped => ("stopped", None),
        Halt::MaxRounds => ("max_rounds", None),
        Halt::Total => ("total", None),
        Halt::Cost => ("cost", None),
        Halt::NoAgent(i) => ("no_agent", Some(*i)),
        Halt::NoChecker(i) => ("no_checker", Some(*i)),
        Halt::Exhausted(i) => ("exhausted", Some(*i)),
        Halt::Failed { step, .. } => ("failed", Some(*step)),
        Halt::StepFailed(i) => ("step_failed", Some(*i)),
        Halt::Timeout(i) => ("timeout", Some(*i)),
        Halt::FixFailed => ("fix_failed", None),
    }
}

fn state_code(s: &Hs) -> &'static str {
    match s {
        Hs::Idle => "idle",
        Hs::Running => "running",
        Hs::Gate(_) => "gate",
        Hs::TakenOver => "taken_over",
        Hs::Done => "done",
        Hs::Halted(_) => "halted",
    }
}

fn terminal(s: &Hs) -> bool {
    matches!(s, Hs::Done | Hs::Halted(_))
}

fn snapshot(st: &State, l: &Live) -> Value {
    let names = st.names();
    let steps: Vec<Value> = l.run.recipe.steps.iter().enumerate().map(|(i, s)| {
        let pane = l.panes.get(&i).copied();
        json!({"id": s.id, "name": s.name, "kind": s.kind, "human": s.human, "shell": s.shell, "gate": s.gate, "agents": s.agents, "output": s.output,
               "pane": pane, "pane_name": pane.and_then(|p| names.get(&p)), "pane_agent": l.agents.get(&i), "message": l.msgs.get(&i)})
    }).collect();
    let (halt, halt_step) = match &l.run.state {
        Hs::Halted(h) => {
            let (c, s) = halt_code(h);
            (Some(c), s)
        }
        _ => (None, None),
    };
    let trace: Vec<Value> = l.run.trace.iter().map(|e| json!({"at": e.at, "code": e.code, "step": e.step, "agent": e.agent})).collect();
    json!({
        "run": l.id, "recipe": l.recipe_id, "name": l.run.recipe.name, "workspace": l.workspace, "cwd": l.cwd, "started_at": l.started_at,
        "state": state_code(&l.run.state), "gate": if let Hs::Gate(i) = l.run.state { Some(i) } else { None }, "current": l.run.current(),
        "halt": halt, "halt_step": halt_step, "round": l.run.round, "max_rounds": l.run.recipe.max_rounds, "steps": steps, "trace": trace,
    })
}

fn save(st: &State, l: &Live) {
    let _ = config::write_atomic(&l.dir.join("run.json"), format!("{}\n", snapshot(st, l)).as_bytes());
}

fn ui_event(st: &State, method: &str, params: Value) {
    let ev = line(json!({"event": method, "params": params}));
    st.ui_txs.lock().unwrap().retain(|(_, tx)| tx.try_send(ev.clone()).is_ok());
}

fn audit(st: &State, l: &Live, code: &str, step: Option<usize>, agent: Option<&str>) {
    let Some(a) = &st.audit else { return };
    // SEC-11 rows are built around a message; a trail row is a message with the event code as its body
    let to = step.and_then(|s| l.panes.get(&s)).copied().unwrap_or(0);
    let m = bus::Msg {
        id: l.id.clone(),
        from: l.from,
        from_name: "harness".into(),
        to,
        to_name: format!("{}{}", agent.unwrap_or(""), step.map_or(String::new(), |s| format!("#{s}"))),
        body: code.into(),
        reply_to: None,
        hop: 0,
        at: std::time::SystemTime::now(),
    };
    a.write("harness", &m, None);
}

fn close_shell(l: &mut Live) {
    if let Some(s) = l.shell.take()
        && s.code.lock().unwrap().is_none()
    {
        unsafe { libc::killpg(s.pgid, libc::SIGTERM) };
    }
}

fn spawn_shell(l: &mut Live) -> Result<(), ()> {
    use std::os::unix::fs::OpenOptionsExt;
    use std::os::unix::process::CommandExt;
    let log = fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(l.dir.join("artefak/test.log")).map_err(drop)?;
    let err = log.try_clone().map_err(drop)?;
    // output is capped by the shell's file-size limit (512-byte blocks) so a runaway test cannot fill the disk
    let script = format!("ulimit -f 40000 2>/dev/null; {}", l.test);
    let mut c = Command::new(pty::login_shell());
    c.args(["-l", "-c", &script]).current_dir(&l.cwd).stdin(Stdio::null()).stdout(log).stderr(err).process_group(0);
    pty::scrub_env(&mut c);
    let mut child = c.spawn().map_err(drop)?;
    let code = Arc::new(Mutex::new(None));
    let c2 = code.clone();
    let pgid = child.id() as i32;
    thread::spawn(move || {
        let r = child.wait().ok().and_then(|s| s.code()).unwrap_or(-1);
        *c2.lock().unwrap() = Some(r);
    });
    l.shell = Some(Shell { code, pgid });
    Ok(())
}

/// carry out one action; an action may produce the next event (a verdict, a failed launch)
fn act(st: &Arc<State>, l: &mut Live, a: Action) -> Option<Event> {
    match a {
        Action::Send { step, agent } => {
            let s = l.run.recipe.steps[step].clone();
            if !s.output.is_empty() {
                // a result left by an earlier attempt must not pass for this one
                let _ = fs::remove_file(l.dir.join("artefak").join(&s.output));
            }
            let extra = flags(&agent, &s.model, &s.effort, s.max_cost);
            let params = json!({"profile": agent, "workspace": l.workspace, "cwd": l.cwd, "rows": 40, "cols": 120});
            let id = match create_pane(st, &params, &extra) {
                Ok(v) => v["pane"].as_u64()?,
                Err(_) => return Some(Event::Exit(1)),
            };
            l.panes.insert(step, id);
            l.agents.insert(step, agent.clone());
            l.cur = Some(id);
            l.born = Instant::now();
            l.seen = None;
            st.harness.lock().unwrap().locked.insert(id);
            l.outbox = Some(Outbox { step, body: stage_message(l, step), retry_at: Instant::now() });
            None
        }
        Action::SendFix { step, failed, .. } => {
            let Some(id) = l.panes.get(&step).copied().filter(|p| st.panes.lock().unwrap().contains_key(p)) else { return Some(Event::Exit(1)) };
            l.cur = Some(id);
            l.seen = None;
            l.born = Instant::now().checked_sub(GRACE).unwrap_or_else(Instant::now);
            st.harness.lock().unwrap().locked.insert(id);
            l.outbox = Some(Outbox { step, body: fix_message(l, step, failed), retry_at: Instant::now() });
            None
        }
        Action::RunShell { .. } => {
            let _ = fs::remove_file(l.dir.join("artefak/test.log"));
            l.cur = None;
            match spawn_shell(l) {
                Ok(()) => None,
                Err(()) => Some(Event::Exit(127)),
            }
        }
        Action::Evaluate { step } => Some(Event::Verdict(evaluate(st, l, step))),
        Action::Unlock => {
            let mut reg = st.harness.lock().unwrap();
            for p in l.panes.values() {
                reg.locked.remove(p);
            }
            None
        }
        Action::Notify(n) => {
            let (kind, step) = match n {
                Notice::Gate(i) => ("gate", Some(i)),
                Notice::Blocked(i) => ("blocked", Some(i)),
                Notice::Done => ("done", None),
                Notice::Halted => ("halted", None),
            };
            ui_event(st, "harness.notice", json!({"run": l.id, "workspace": l.workspace, "notice": kind, "step": step}));
            None
        }
    }
}

/// feed an event and everything it causes, then persist and publish
fn feed(st: &Arc<State>, l: &mut Live, ev: Event) {
    let now = l.t0.elapsed().as_secs();
    let mut q = VecDeque::from([ev]);
    while let Some(ev) = q.pop_front() {
        for a in l.run.decide(now, ev) {
            if let Some(next) = act(st, l, a) {
                q.push_back(next);
            }
        }
    }
    for e in &l.run.trace[l.trace_saved..] {
        audit(st, l, e.code, e.step, e.agent.as_deref());
    }
    l.trace_saved = l.run.trace.len();
    if terminal(&l.run.state) {
        close_shell(l);
        l.outbox = None;
        // the workspace is free again as soon as the run ends
        if !l.released {
            l.released = true;
            let mut reg = st.harness.lock().unwrap();
            reg.runs.remove(&l.workspace);
            reg.from_ws.remove(&l.from);
        }
    }
    let sig = (l.run.trace.len(), state_code(&l.run.state), l.panes.len(), l.msgs.len(), l.outbox.is_some());
    if sig != l.sig {
        l.sig = sig;
        save(st, l);
        ui_event(st, "harness.state", snapshot(st, l));
    }
}

fn map_status(s: &str) -> Status {
    match s {
        "working" => Status::Working,
        "blocked" => Status::Blocked,
        "idle" | "done" => Status::Idle,
        _ => Status::Unknown,
    }
}

/// one pass of the runner: deliver the waiting message, then report what changed in the stage's pane or shell
fn tick(st: &Arc<State>, l: &mut Live) {
    if !matches!(l.run.state, Hs::Running) {
        return;
    }
    let mut events = Vec::new();
    if let Some(ob) = &l.outbox
        && Instant::now() >= ob.retry_at
    {
        let pane = l.cur.and_then(|id| st.panes.lock().unwrap().get(&id).cloned().map(|p| (id, p)));
        if let Some((id, p)) = pane {
            let ready = l.born.elapsed() >= GRACE && matches!(*p.status.lock().unwrap(), "idle" | "done");
            if ready {
                let (step, body) = (ob.step, ob.body.clone());
                match send(st, l, id, &p, step, &body) {
                    Sent::Ok => l.outbox = None,
                    Sent::Later => l.outbox.as_mut().unwrap().retry_at = Instant::now() + RETRY,
                    Sent::Failed => {
                        l.outbox = None;
                        events.push(Event::Exit(1));
                    }
                }
            }
        }
    }
    let code = l.shell.as_ref().and_then(|s| *s.code.lock().unwrap());
    if let Some(code) = code {
        l.shell = None;
        events.push(Event::Exit(code));
    }
    if l.shell.is_none()
        && let Some(id) = l.cur
    {
        match st.panes.lock().unwrap().get(&id).cloned() {
            None => events.push(Event::Exit(1)),
            Some(p) => {
                let raw = *p.status.lock().unwrap();
                if raw.is_empty() {
                    // an agent pane starts "idle" and is only blanked when the agent process leaves (agent_done), so
                    // empty = the agent exited; the pane itself stays as a shell
                    if l.seen != Some(Status::Unknown) {
                        l.seen = Some(Status::Unknown);
                        events.push(Event::Exit(1));
                    }
                } else {
                    let s = map_status(raw);
                    if l.seen != Some(s) {
                        l.seen = Some(s);
                        events.push(Event::Status(s));
                    }
                }
            }
        }
    }
    events.push(Event::Tick);
    for ev in events {
        feed(st, l, ev);
        if terminal(&l.run.state) {
            break;
        }
    }
}

enum Sent {
    Ok,
    Later,
    Failed,
}

/// the stage message goes through the same decision as any agent's message (SEC-4 to 10, 28); the label names the harness
fn send(st: &Arc<State>, l: &mut Live, to: u64, pane: &Pane, step: usize, body: &str) -> Sent {
    let Ok(clean) = msg::sanitize(body) else { return Sent::Failed };
    let id = st.engine.lock().unwrap().new_id(&st.boot);
    let m = bus::Msg {
        id: id.clone(),
        from: l.from,
        from_name: "harness".into(),
        to,
        to_name: pane.name.lock().unwrap().clone(),
        body: clean,
        reply_to: None,
        // SEC-30: the hop count starts at 1 per stage, so an agent's answer is hop 2
        hop: 1,
        at: std::time::SystemTime::now(),
    };
    let r = try_send(st, m, true, false);
    match (r["result"].as_str(), r["reason"].as_str()) {
        (Some("accepted" | "queued"), _) => {
            l.msgs.insert(step, id);
            Sent::Ok
        }
        (_, Some("paused" | "rate_limited" | "needs_confirmation")) => Sent::Later,
        _ => Sent::Failed,
    }
}

fn spawn_ticker(st: Arc<State>, live: Arc<Mutex<Live>>) {
    thread::spawn(move || {
        loop {
            thread::sleep(Duration::from_millis(500));
            let mut l = live.lock().unwrap();
            tick(&st, &mut l);
            if terminal(&l.run.state) {
                return;
            }
        }
    });
}

fn pane_cwd(st: &State, p: &Value) -> Option<PathBuf> {
    if let Some(c) = p["cwd"].as_str().filter(|c| c.starts_with('/')) {
        return Some(PathBuf::from(c));
    }
    let f = st.pane(&json!({"pane": p["from_pane"]})).ok()?;
    let pid = pty::busy(&f.master, f.sid).map_or(f.sid, |(pid, _)| pid);
    pty::cwd_of(pid).or_else(|| pty::cwd_of(f.sid))
}

fn item(check: &str, ok: bool, blocking: bool, detail: impl ToString) -> Value {
    json!({"check": check, "ok": ok, "blocking": blocking, "detail": detail.to_string()})
}

/// FR-73: the list the "New task" dialog shows; the run starts only when every blocking item is ok
pub fn preflight(st: &State, p: &Value) -> Result<(Vec<Value>, Option<recipe::Recipe>), &'static str> {
    let id = p["recipe"].as_str().ok_or("bad_params")?;
    let inst = installed(st);
    let mut items = Vec::new();
    let recipe = match store().approved(id, Some(&inst)) {
        Ok(r) => {
            items.push(item("recipe_approved", true, true, ""));
            Some(r)
        }
        Err(d) => {
            items.push(item("recipe_approved", false, true, d.code));
            None
        }
    };
    if let Some(r) = &recipe {
        let run = harness::Run::new(r.clone(), &inst);
        let missing: Vec<&str> = run.recipe.steps.iter().filter(|s| !s.human && !s.shell && s.agents.is_empty()).map(|s| s.id.as_str()).collect();
        items.push(item("agents_installed", missing.is_empty(), true, missing.join(", ")));
        let need_test = r.steps.iter().any(|s| s.shell);
        let test = p["test"].as_str().unwrap_or_default().trim();
        items.push(item("test_command", !need_test || (!test.is_empty() && test.len() <= 500 && !test.chars().any(char::is_control)), true, ""));
        if r.steps.iter().any(|s| s.agents.iter().any(|a| a == "claude")) {
            items.push(item("claude_auto", true, false, "api_key_advice"));
        }
    }
    let cwd = pane_cwd(st, p);
    let repo = cwd.as_deref().is_some_and(|c| snap::root_of(c).is_some());
    items.push(item("git_repo", repo, true, ""));
    if let Some(c) = cwd.as_deref().filter(|_| repo) {
        let files = git::status(c)["files"].as_u64().unwrap_or(0);
        items.push(item("clean_tree", files == 0, false, files));
    }
    let ws = p["workspace"].as_str().unwrap_or_default();
    let busy = st.harness.lock().unwrap().runs.contains_key(ws);
    items.push(item("no_other_run", !busy, true, ""));
    Ok((items, recipe))
}

pub fn method(st: &Arc<State>, m: &str, p: &Value) -> Res {
    match m {
        "harness.preflight" => {
            let (items, _) = preflight(st, p)?;
            let ok = items.iter().all(|i| i["ok"] == true || i["blocking"] == false);
            Ok(json!({"ok": ok, "items": items}))
        }
        "harness.start" => start(st, p),
        "harness.state" => state(st, p),
        _ => command(st, m, p),
    }
}

fn start(st: &Arc<State>, p: &Value) -> Res {
    let (items, recipe) = preflight(st, p)?;
    if let Some(bad) = items.iter().find(|i| i["ok"] == false && i["blocking"] == true) {
        return Ok(json!({"error": "preflight", "args": {"check": bad["check"], "detail": bad["detail"]}}));
    }
    let recipe = recipe.ok_or("bad_params")?;
    let request = msg::sanitize(p["request"].as_str().ok_or("bad_params")?)?;
    if request.trim().is_empty() || request.len() > MAX_REQUEST {
        return Err("bad_params");
    }
    let cwd = pane_cwd(st, p).filter(|c| c.is_dir()).ok_or("bad_params")?;
    let workspace: String = p["workspace"].as_str().unwrap_or_default().chars().take(64).collect();
    let (run_id, from) = {
        let mut reg = st.harness.lock().unwrap();
        if reg.runs.contains_key(&workspace) {
            return Ok(json!({"error": "preflight", "args": {"check": "no_other_run", "detail": ""}}));
        }
        reg.seq += 1;
        (format!("run-{}-{}", history::now(), reg.seq), FROM_BASE + reg.seq)
    };
    let _ = config::write_atomic(&data_dir().join(".keep"), b"");
    let dir = data_dir().join(&run_id);
    config::write_atomic(&dir.join("artefak").join(REQUEST_FILE), request.as_bytes()).map_err(|_| "io")?;
    let run = harness::Run::new(recipe, &installed(st));
    let live = Live {
        id: run_id.clone(),
        recipe_id: p["recipe"].as_str().unwrap_or_default().to_string(),
        workspace: workspace.clone(),
        cwd,
        dir,
        test: p["test"].as_str().unwrap_or_default().trim().to_string(),
        run,
        from,
        t0: Instant::now(),
        started_at: history::now(),
        panes: HashMap::new(),
        cur: None,
        born: Instant::now(),
        seen: None,
        outbox: None,
        msgs: HashMap::new(),
        agents: HashMap::new(),
        shell: None,
        trace_saved: 0,
        released: false,
        sig: (usize::MAX, "", 0, 0, false),
    };
    let live = Arc::new(Mutex::new(live));
    {
        let mut reg = st.harness.lock().unwrap();
        reg.runs.insert(workspace.clone(), live.clone());
        reg.from_ws.insert(from, workspace);
    }
    let snap = {
        let mut l = live.lock().unwrap();
        feed(st, &mut l, Event::Start);
        snapshot(st, &l)
    };
    spawn_ticker(st.clone(), live);
    Ok(snap)
}

fn find(st: &State, p: &Value) -> Option<Arc<Mutex<Live>>> {
    // the registry lock is released before any run is locked: the ticker takes them in the other order
    let reg = st.harness.lock().unwrap();
    let by_ws = p["workspace"].as_str().and_then(|ws| reg.runs.get(ws).cloned());
    let all: Vec<_> = reg.runs.values().cloned().collect();
    drop(reg);
    match p["run"].as_str() {
        Some(id) => all.into_iter().find(|l| l.lock().unwrap().id == id),
        None => by_ws,
    }
}

fn state(st: &Arc<State>, p: &Value) -> Res {
    if let Some(live) = find(st, p) {
        let l = live.lock().unwrap();
        return Ok(snapshot(st, &l));
    }
    // not live: the saved row; a run that was still going when the daemon stopped is reported as halted, never resumed
    let id = p["run"].as_str().filter(|r| valid_run_id(r)).ok_or("no_such_run")?;
    let file = data_dir().join(id).join("run.json");
    let mut v: Value = fs::read_to_string(&file).ok().and_then(|t| serde_json::from_str(&t).ok()).ok_or("no_such_run")?;
    if !matches!(v["state"].as_str(), Some("done" | "halted")) {
        v["state"] = json!("halted");
        v["halt"] = json!("daemon_restart");
        let _ = config::write_atomic(&file, format!("{v}\n").as_bytes());
    }
    Ok(v)
}

/// stop, gate, takeover, release: the user's own buttons (UI channel); each only means something in the right state
fn command(st: &Arc<State>, m: &str, p: &Value) -> Res {
    let live = find(st, p).ok_or("no_such_run")?;
    let mut l = live.lock().unwrap();
    let ev = match m {
        "harness.stop" => Event::Stop,
        "harness.gate" => {
            if !matches!(l.run.state, Hs::Gate(_)) {
                return Err("not_at_gate");
            }
            Event::GateOk
        }
        "harness.takeover" => Event::Takeover,
        "harness.release" => {
            let panes: Vec<u64> = l.panes.values().copied().collect();
            if matches!(l.run.state, Hs::TakenOver) {
                st.harness.lock().unwrap().locked.extend(panes);
            }
            Event::Release
        }
        _ => return Err("unknown_method"),
    };
    feed(st, &mut l, ev);
    Ok(snapshot(st, &l))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launch_flags_per_agent() {
        assert_eq!(flags("claude", "opus", "high", None), ["--model", "opus", "--effort", "high"]);
        assert_eq!(flags("claude", "", "", None), Vec::<String>::new());
        assert_eq!(flags("codex", "gpt-5", "max", None), ["-m", "gpt-5", "-c", "model_reasoning_effort=\"high\""]);
        assert_eq!(flags("omp", "x", "low", Some(2.5)), ["--model=x", "--thinking=low", "--max-cost=2.5", "--warn-cost=2"]);
        assert_eq!(flags("opencode", "p/m", "high", None), ["-m", "p/m"]);
        assert!(flags("shell", "x", "y", None).is_empty());
    }

    #[test]
    fn run_ids() {
        assert!(valid_run_id("run-1730000000-1"));
        for bad in ["", "../x", "a b", "a/b", &"x".repeat(49)] {
            assert!(!valid_run_id(bad), "{bad}");
        }
    }
}
