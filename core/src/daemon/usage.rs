//! FR-102: how much of each agent login's allowance is used. Numbers from what the agents hand out themselves:
//! the Codex footer drawn on a pane (FR-85) and `codex app-server` `account/rateLimits/read` (Codex reads its own
//! login), `omp usage --json`, the `quota` item of the Gemini footer, and the `rate_limits` Claude sends to its status
//! line through the wrapper Gembala installs per launch. One exception (TECH 26.1, SEC-15 exception four, C-3): each
//! Claude account's Claude Code login is read from its Keychain item or `.credentials.json` and sent only to Anthropic's
//! OAuth usage endpoint; the token is never stored, logged, refreshed, or handed to a pane or the UI. No other token,
//! `auth.json`, or provider API is read (SEC-32).
//! FR-104: Command Code stays unknown: `/usage` is interactive; `status --json` reports authentication, not allowance.
use super::*;
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{SystemTime, UNIX_EPOCH};

/// How often the agents that can answer on their own are asked again (FR-102: every 10 minutes).
const EVERY: Duration = Duration::from_secs(10 * 60);
/// Agent detection starts on its own thread when the daemon opens; the first probe waits for it.
const FIRST: Duration = Duration::from_secs(20);
/// A probe never holds a process longer than this.
const PROBE: Duration = Duration::from_secs(6);
/// `omp usage` talks to the provider's API; a slower answer is given up rather than left running.
const OMP: Duration = Duration::from_secs(15);

/// Order the rows are listed in, also what the UI shows first.
pub const AGENTS: &[&str] = &["claude", "codex", "omp", "opencode", "gemini", "commandcode", "agy"];

#[derive(Clone, Debug, PartialEq)]
pub struct Window {
    /// "5h" | "week" | "month" | "day"
    pub kind: &'static str,
    /// 0-100, share used; the left share is `100 - used_pct`
    pub used_pct: f64,
    /// unix seconds
    pub resets_at: Option<i64>,
    pub model: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Row {
    pub agent: &'static str,
    /// FR-61 account id, or "utama"
    pub login: String,
    pub login_name: String,
    pub windows: Vec<Window>,
    pub source: &'static str,
    /// unix seconds of the last reading
    pub read_at: i64,
    /// the source says this login is at its limit right now
    pub limited: bool,
    /// TECH 26.1: Claude plan name from the login, e.g. "Max (20x)"
    pub plan: Option<String>,
    /// TECH 26.1: why these numbers may be old: "throttled" (Anthropic answered 429) or "last_run" (kept from disk)
    pub stale: Option<&'static str>,
}

#[derive(Default)]
pub struct Store {
    rows: Vec<Row>,
    /// A Codex pane ran in this daemon run: the app-server probe is for logins in use (FR-102).
    codex: bool,
    /// TECH 26.1: no OAuth usage reads before this unix second (Anthropic answered 429)
    paused_until: i64,
    /// TECH 26.1: Claude accounts with an OAuth read waiting or running; one at a time per account
    reading: Vec<String>,
    /// `usage-limits.json`, set by `start` outside isolated runs (tests never write it)
    saved: Option<PathBuf>,
}

fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or_default()
}

/// One decimal at most: `usedFraction` is a fraction, and `0.02 * 100.0` is not 2 in binary floating point.
fn pct(v: f64) -> f64 {
    (v.clamp(0.0, 100.0) * 10.0).round() / 10.0
}

fn window(kind: &'static str, used: f64, resets_at: Option<i64>, model: Option<String>) -> Window {
    Window { kind, used_pct: pct(used), resets_at, model }
}

/// The same window from two providers (omp reports one set per provider): the tighter one is what the ring shows.
fn merge_into(windows: &mut Vec<Window>, w: Window) {
    match windows.iter_mut().find(|o| o.kind == w.kind && o.model == w.model) {
        Some(cur) if w.used_pct > cur.used_pct => *cur = w,
        Some(_) => {}
        None => windows.push(w),
    }
}

fn sort(windows: &mut [Window]) {
    let rank = |k: &str| ["5h", "week", "month", "day"].iter().position(|x| *x == k).unwrap_or(9);
    windows.sort_by(|a, b| rank(a.kind).cmp(&rank(b.kind)).then_with(|| a.model.cmp(&b.model)));
}

/// Reset time in the user's clock; a day that isn't today gets its date too.
fn clock(t: i64) -> String {
    let tm = |secs: i64| {
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        let secs = secs as libc::time_t;
        unsafe { libc::localtime_r(&secs, &mut tm) };
        tm
    };
    let (a, b) = (tm(t), tm(now()));
    let same_day = a.tm_year == b.tm_year && a.tm_yday == b.tm_yday;
    let fmt = if same_day { c"%H:%M" } else { c"%d/%m %H:%M" };
    let mut buf = [0u8; 32];
    let n = unsafe { libc::strftime(buf.as_mut_ptr().cast(), buf.len(), fmt.as_ptr(), &a) };
    String::from_utf8_lossy(&buf[..n]).to_string()
}

pub fn rows(st: &State) -> Vec<Row> {
    let mut out = st.usage.lock().unwrap().rows.clone();
    let rank = |a: &str| AGENTS.iter().position(|x| *x == a).unwrap_or(AGENTS.len());
    out.sort_by(|a, b| rank(a.agent).cmp(&rank(b.agent)).then_with(|| a.login.cmp(&b.login)));
    out
}

/// `usage.list`: every number Gembala has, per agent login (FR-102 contract).
pub fn status(st: &State) -> Value {
    let rows: Vec<Value> = rows(st)
        .iter()
        .map(|r| {
            json!({
                "agent": r.agent, "login": r.login, "login_name": r.login_name,
                "windows": r.windows.iter().map(|w| json!({"kind": w.kind, "used_pct": w.used_pct, "resets_at": w.resets_at, "model": w.model})).collect::<Vec<_>>(),
                "source": r.source, "read_at": r.read_at, "limited": r.limited, "plan": r.plan, "stale": r.stale,
            })
        })
        .collect();
    json!({"rows": rows})
}

/// New numbers for one agent login. The row's age is refreshed on every reading; the UI is only told when a number
/// actually changes (the Codex footer is read every second).
fn put(st: &State, row: Row) {
    let tell = merge(&mut st.usage.lock().unwrap().rows, row);
    if tell {
        changed(st);
    }
}

/// One reading into the rows; true = a number the UI shows changed.
fn merge(rows: &mut Vec<Row>, mut row: Row) -> bool {
    sort(&mut row.windows);
    match rows.iter_mut().find(|r| r.agent == row.agent && r.login == row.login) {
        None => {
            rows.push(row);
            true
        }
        Some(cur) => {
            // TECH 26.1: per window the newest reading wins; the status line has no per-model weeks or plan, so
            // those stay from the last OAuth read (which brings the full set and replaces everything)
            if row.source == "claude_statusline" {
                for w in &cur.windows {
                    if !row.windows.iter().any(|n| n.kind == w.kind && n.model == w.model) {
                        row.windows.push(w.clone());
                    }
                }
                sort(&mut row.windows);
                row.plan = row.plan.take().or_else(|| cur.plan.clone());
            }
            // A footer has no reset time: keep the one the app-server reading gave for the same window while it's ahead.
            if row.source == "codex_footer" {
                for w in row.windows.iter_mut().filter(|w| w.resets_at.is_none()) {
                    if let Some(t) = cur.windows.iter().find(|o| o.kind == w.kind && o.model == w.model).and_then(|o| o.resets_at).filter(|t| *t > now()) {
                        w.resets_at = Some(t);
                    }
                }
            }
            if row.source == "agy_statusline" {
                for w in row.windows.iter_mut().filter(|w| w.resets_at.is_some()) {
                    if let Some(t) = cur.windows.iter().find(|o| o.kind == w.kind && o.model == w.model).and_then(|o| o.resets_at).filter(|t| *t > now()) {
                        w.resets_at = Some(t);
                    }
                }
            }
            let tell = cur.windows != row.windows
                || cur.source != row.source
                || cur.limited != row.limited
                || cur.login_name != row.login_name
                || cur.plan != row.plan
                || cur.stale != row.stale;
            *cur = row;
            tell
        }
    }
}

/// The UI is told and the numbers are kept for the next run (TECH 26.1: numbers, `read_at`, and plan only).
fn changed(st: &State) {
    let ev = line(json!({"event": "usage.changed", "params": status(st)}));
    super::ui_broadcast(st, &ev);
    let path = st.usage.lock().unwrap().saved.clone();
    if let Some(path) = path
        && let Err(e) = config::write_atomic(&path, saved_json(&rows(st)).to_string().as_bytes())
    {
        eprintln!("gembala daemon: usage save: {e}");
    }
}

fn saved_json(rows: &[Row]) -> Value {
    let rows: Vec<Value> = rows
        .iter()
        .map(|r| {
            json!({
                "agent": r.agent, "login": r.login, "login_name": r.login_name,
                "windows": r.windows.iter().map(|w| json!({"kind": w.kind, "used_pct": w.used_pct, "resets_at": w.resets_at, "model": w.model})).collect::<Vec<_>>(),
                "source": r.source, "read_at": r.read_at, "limited": r.limited, "plan": r.plan,
            })
        })
        .collect();
    json!({"rows": rows})
}

/// `usage-limits.json` from an earlier run: shown as `stale = "last_run"` until a fresh reading arrives. Unknown
/// agents, sources, or window kinds are dropped.
fn load_saved(v: &Value) -> Vec<Row> {
    const SOURCES: &[&str] = &["codex_footer", "codex_app_server", "omp_cli", "gemini_footer", "claude_statusline", "claude_oauth", "agy_statusline"];
    let pick = |list: &[&'static str], s: Option<&str>| list.iter().find(|x| Some(**x) == s).copied();
    let mut out = Vec::new();
    for r in v["rows"].as_array().into_iter().flatten() {
        let (Some(agent), Some(source), Some(login)) = (pick(AGENTS, r["agent"].as_str()), pick(SOURCES, r["source"].as_str()), r["login"].as_str()) else { continue };
        let windows: Vec<Window> = r["windows"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|w| {
                let kind = pick(&["5h", "week", "month", "day"], w["kind"].as_str())?;
                Some(window(kind, w["used_pct"].as_f64()?, w["resets_at"].as_i64(), w["model"].as_str().map(String::from)))
            })
            .collect();
        if windows.is_empty() {
            continue;
        }
        out.push(Row {
            agent,
            login: login.to_string(),
            login_name: r["login_name"].as_str().unwrap_or_default().to_string(),
            windows,
            source,
            read_at: r["read_at"].as_i64().unwrap_or_default(),
            limited: r["limited"].as_bool().unwrap_or_default(),
            plan: r["plan"].as_str().map(String::from),
            stale: Some("last_run"),
        });
    }
    out
}

/// A Codex pane ran in this daemon run (footer or profile); the 10-minute app-server probe only runs for its logins.
pub fn note_codex(st: &State) {
    st.usage.lock().unwrap().codex = true;
}

/// FR-102: started once by the daemon; probes the agents that can answer for themselves.
pub fn start(st: &Arc<State>) {
    // Isolated run folders (tests, the bench script) never start provider processes or touch the network.
    if isolated() {
        return;
    }
    let path = config::path().with_file_name("usage-limits.json");
    let kept = std::fs::read_to_string(&path).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()).map(|v| load_saved(&v)).unwrap_or_default();
    {
        let mut s = st.usage.lock().unwrap();
        for row in kept {
            merge(&mut s.rows, row);
        }
        s.saved = Some(path);
    }
    let st = st.clone();
    thread::spawn(move || {
        thread::sleep(FIRST);
        loop {
            poll(&st);
            thread::sleep(EVERY);
        }
    });
}

/// FR-102: Codex and Gemini numbers from the footer the agent already drew on the pane (no account data is read).
pub fn tick(st: &State, pane: &Pane) {
    let Ok(terminal) = pane.terminal() else { return };
    match pane.agent_name().as_deref() {
        Some("codex") => {
            note_codex(st);
            for line in footer_lines(pane) {
                if let Some((windows, limited)) = parse_codex_footer(&line) {
                    put(st, Row { agent: "codex", login: "utama".into(), login_name: "Utama".into(), windows, source: "codex_footer", read_at: now(), limited, ..Default::default() });
                    break;
                }
            }
        }
        // FR-88 isn't in this branch: a Gemini through a custom profile is found by the process name in front of the pane.
        Some("gemini") => gemini(st, pane),
        Some("custom") if pty::busy(&terminal.master, terminal.sid).and_then(|(pid, _)| pty::proc_name(pid)).as_deref() == Some("gemini") => gemini(st, pane),
        _ => {}
    }
}

fn gemini(st: &State, pane: &Pane) {
    for line in footer_lines(pane) {
        if let Some((windows, limited)) = parse_gemini_footer(&line) {
            put(st, Row { agent: "gemini", login: "utama".into(), login_name: "Utama".into(), windows, source: "gemini_footer", read_at: now(), limited, ..Default::default() });
            break;
        }
    }
}

/// Bottom screen rows, last line first (a footer is the bottom of the screen).
fn footer_lines(pane: &Pane) -> Vec<String> {
    let Ok(terminal) = pane.terminal() else { return Vec::new() };
    terminal.view.lock().unwrap().0.tail(3).lines().rev().map(str::to_string).collect()
}

fn poll(st: &Arc<State>) {
    // TECH 26.1: no Claude OAuth reads while no UI is connected
    if !st.ui_txs.lock().unwrap().is_empty() {
        claude_refresh(st);
    }
    let installed = |agent: &str| -> Option<String> {
        let agents = st.agents.lock().unwrap();
        agents.as_array()?.iter().find(|a| a["agent"] == agent && a["installed"] == true)?["path"].as_str().map(String::from)
    };
    if let Some(path) = installed("omp") {
        match omp_usage(&path) {
            Some((windows, limited)) => put(st, Row { agent: "omp", login: "utama".into(), login_name: "Utama".into(), windows, source: "omp_cli", read_at: now(), limited, ..Default::default() }),
            // offline, logged out, or a changed shape: keep the last numbers (FR-102: nothing is guessed)
            None => eprintln!("gembala daemon: usage omp: no numbers"),
        }
    }
    if st.usage.lock().unwrap().codex
        && let Some(path) = installed("codex")
    {
        for (login, name, home) in codex_logins(st) {
            match probe_codex(&path, home.as_deref()) {
                Some((windows, limited)) => put(st, Row { agent: "codex", login, login_name: name, windows, source: "codex_app_server", read_at: now(), limited, ..Default::default() }),
                // no login content in the log: the reason alone
                None => eprintln!("gembala daemon: usage codex app-server: no numbers"),
            }
        }
    }
}

/// Codex logins to probe: registered Codex accounts (FR-61 Codex is "menyusul", so today only Utama) and the logins a
/// Codex pane in this run already showed numbers for.
fn codex_logins(st: &State) -> Vec<(String, String, Option<String>)> {
    let mut out: Vec<(String, String, Option<String>)> = Vec::new();
    for a in st.accounts.lock().unwrap().iter().filter(|a| a.agent == "codex") {
        if !out.iter().any(|x| x.0 == a.id) {
            out.push((a.id.clone(), a.name.clone(), Some(account::expand(&a.dir).to_string_lossy().into_owned())));
        }
    }
    for r in st.usage.lock().unwrap().rows.iter().filter(|r| r.agent == "codex") {
        if !out.iter().any(|x| x.0 == r.login) {
            out.push((r.login.clone(), r.login_name.clone(), None));
        }
    }
    if out.is_empty() {
        out.push(("utama".into(), "Utama".into(), None));
    }
    out
}

/// `codex app-server` for one login: JSON-RPC over stdio, `initialize`, `initialized`, `account/rateLimits/read`,
/// then the process is closed. Codex reads its own login; Gembala only sees the numbers (SEC-15).
fn probe_codex(path: &str, codex_home: Option<&str>) -> Option<(Vec<Window>, bool)> {
    let mut c = Command::new(path);
    c.args(["-c", "approval_policy=never", "-c", "features.plugins=false", "-s", "read-only", "-a", "never", "app-server"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    pty::scrub_env(&mut c);
    if let Some(home) = codex_home {
        c.env("CODEX_HOME", home);
    }
    let mut child = c.spawn().ok()?;
    let mut stdin = child.stdin.take()?;
    let stdout = child.stdout.take()?;
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let init = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"clientInfo": {"name": "gembala", "version": env!("CARGO_PKG_VERSION")}}});
        if writeln!(stdin, "{init}").is_err() {
            return;
        }
        let mut sent = false;
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            let Ok(v) = serde_json::from_str::<Value>(line.trim()) else { continue };
            match v["id"].as_u64() {
                Some(1) if !sent => {
                    let ready = json!({"jsonrpc": "2.0", "method": "initialized", "params": {}});
                    let read = json!({"jsonrpc": "2.0", "id": 2, "method": "account/rateLimits/read", "params": {}});
                    if writeln!(stdin, "{ready}").and_then(|_| writeln!(stdin, "{read}")).is_err() {
                        return;
                    }
                    sent = true;
                }
                Some(2) => {
                    let _ = tx.send(v);
                    return;
                }
                _ => {}
            }
        }
    });
    let got = rx.recv_timeout(PROBE).ok();
    // the probe is over either way: the extra Codex process is closed and reaped
    let _ = child.kill();
    let _ = child.wait();
    parse_codex_rpc(&got?)
}

/// Codex app-server response → windows. `primary` is the 5-hour bucket, `secondary` the weekly one; the duration wins
/// when it's there (older builds drift by a minute), unknown durations keep that order.
pub fn parse_codex_rpc(v: &Value) -> Option<(Vec<Window>, bool)> {
    let limits = if v["result"]["rateLimits"].is_object() { &v["result"]["rateLimits"] } else if v["rateLimits"].is_object() { &v["rateLimits"] } else { return None };
    let bucket = |name: &str, fallback: &'static str| -> Option<Window> {
        let raw = &limits[name];
        let used = raw["usedPercent"].as_f64()?;
        let mins = raw["windowDurationMins"].as_f64();
        let kind = match mins.map(|m| m.round() as i64) {
            Some(m) if (m - 300).abs() <= 1 => "5h",
            Some(m) if (m - 10080).abs() <= 1 => "week",
            _ => fallback,
        };
        let resets = raw["resetsAt"].as_f64().map(|t| t.round() as i64).filter(|t| *t > 0);
        Some(window(kind, used, resets, None))
    };
    let mut windows: Vec<Window> = Vec::new();
    for (name, fallback) in [("primary", "5h"), ("secondary", "week")] {
        if let Some(w) = bucket(name, fallback) {
            merge_into(&mut windows, w);
        }
    }
    if windows.is_empty() {
        return None;
    }
    let limited = windows.iter().any(|w| w.used_pct >= 100.0);
    Some((windows, limited))
}

/// `omp usage --json --redact`: one report per provider, each with `rolling-5h` / `weekly` / `monthly` limits.
/// `usedFraction` is 0-1 and `window.resetsAt` is milliseconds.
pub fn parse_omp(v: &Value) -> Option<(Vec<Window>, bool)> {
    let mut windows: Vec<Window> = Vec::new();
    let mut limited = false;
    for report in v["reports"].as_array().into_iter().flatten() {
        for limit in report["limits"].as_array().into_iter().flatten() {
            let kind = match limit["id"].as_str() {
                Some("rolling-5h") => "5h",
                Some("weekly") => "week",
                Some("monthly") => "month",
                _ => continue,
            };
            let Some(used) = limit["amount"]["usedFraction"].as_f64() else { continue };
            let resets = limit["window"]["resetsAt"].as_f64().map(|ms| (ms / 1000.0).round() as i64);
            limited |= limit["status"].as_str().is_some_and(|s| s != "ok");
            merge_into(&mut windows, window(kind, used * 100.0, resets, None));
        }
    }
    if windows.is_empty() {
        return None;
    }
    Some((windows, limited))
}

fn omp_usage(path: &str) -> Option<(Vec<Window>, bool)> {
    let mut c = Command::new(path);
    c.args(["usage", "--json", "--redact"]).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
    pty::scrub_env(&mut c);
    let out = profile::run_with_timeout(c, OMP)?;
    parse_omp(&serde_json::from_str(&out).ok()?)
}

/// FR-102: the Codex footer line, e.g. `gpt-5.1-codex high · Context 87% left · 251K window · 5h 66% left · weekly 79% left`.
/// Only the 5-hour and weekly item are taken; the footer has no reset time.
pub fn parse_codex_footer(line: &str) -> Option<(Vec<Window>, bool)> {
    if !line.contains(" · ") {
        return None;
    }
    let mut windows: Vec<Window> = Vec::new();
    for part in line.split(" · ") {
        let low = part.trim().to_ascii_lowercase();
        let (kind, rest) = if let Some(r) = low.strip_prefix("5-hour ") {
            ("5h", r)
        } else if let Some(r) = low.strip_prefix("5h ") {
            ("5h", r)
        } else if let Some(r) = low.strip_prefix("weekly ") {
            ("week", r)
        } else {
            continue;
        };
        let Some(left) = rest.strip_suffix("% left").and_then(|n| n.trim().parse::<u32>().ok()).filter(|n| *n <= 100) else { continue };
        merge_into(&mut windows, window(kind, (100 - left) as f64, None, None));
    }
    if windows.is_empty() {
        return None;
    }
    let limited = windows.iter().any(|w| w.used_pct >= 100.0);
    Some((windows, limited))
}

/// FR-102: the Gemini footer `quota` item: `14% used`, `limit reached`, or `limit reached, resets in 2h`.
/// The footer's quota is pooled (not per model) and carries no reset time for the rings.
pub fn parse_gemini_footer(line: &str) -> Option<(Vec<Window>, bool)> {
    let low = line.to_ascii_lowercase();
    let used = if let Some(i) = low.rfind("% used") {
        let digits: String = low[..i].chars().rev().take_while(|c| c.is_ascii_digit()).collect::<String>().chars().rev().collect();
        digits.parse::<f64>().ok()?
    } else if low.contains("limit reached") {
        100.0
    } else {
        return None;
    };
    if !(0.0..=100.0).contains(&used) {
        return None;
    }
    Some((vec![window("day", used, None, None)], used >= 100.0))
}

/// `rate_limits` from Claude's status line JSON: `five_hour` and `seven_day` with `used_percentage` and `resets_at`.
pub fn parse_claude(rate: &Value) -> Vec<Window> {
    let mut windows = Vec::new();
    for (field, kind) in [("five_hour", "5h"), ("seven_day", "week")] {
        let w = &rate[field];
        let used = w["used_percentage"].as_f64().or_else(|| w["utilization"].as_f64());
        let Some(used) = used else { continue };
        let resets = w["resets_at"].as_f64().map(|t| t.round() as i64);
        merge_into(&mut windows, window(kind, used, resets, None));
    }
    windows
}

/// FR-102: the status line wrapper (`gembala statusline`) sends only these numbers, never the rest of the JSON.
pub fn from_claude(st: &State, p: &Value) -> Res {
    let windows = parse_claude(&p["rate_limits"]);
    if windows.is_empty() {
        return Ok(json!({}));
    }
    let login = acct_of_dir(st, p["dir"].as_str().map(String::from));
    let login = if login.is_empty() { "utama".to_string() } else { login };
    let name = acct_label(st, &login);
    let limited = windows.iter().any(|w| w.used_pct >= 100.0);
    put(st, Row { agent: "claude", login, login_name: if name.is_empty() { "Utama".into() } else { name }, windows, source: "claude_statusline", read_at: now(), limited, ..Default::default() });
    Ok(json!({}))
}

pub fn from_agy(st: &State, p: &Value) -> Res {
    let windows = parse_agy_quota(&p["quota"]);
    if windows.is_empty() { return Ok(json!({})); }
    let limited = windows.iter().any(|w| w.used_pct >= 100.0);
    put(st, Row { agent: "agy", login: "utama".into(), login_name: "Utama".into(), windows, source: "agy_statusline", read_at: now(), limited, ..Default::default() });
    Ok(json!({}))
}

fn parse_agy_quota(quota: &Value) -> Vec<Window> {
    let mut windows = Vec::new();
    for (bucket, value) in quota.as_object().into_iter().flatten() {
        if value["disabled"] == true { continue; }
        let lower = bucket.to_ascii_lowercase();
        let kind = if lower.ends_with("-5h") { "5h" } else if lower.ends_with("-weekly") { "week" } else { continue };
        let Some(remaining) = value["remaining_fraction"].as_f64().filter(|n| (0.0..=1.0).contains(n)) else { continue };
        let reset_at = value["reset_in_seconds"].as_i64().filter(|n| *n >= 0).map(|n| now() + n);
        let plain_bucket = if kind == "5h" { "gemini-5h" } else { "gemini-weekly" };
        let model = if lower.starts_with("gemini-") && lower != plain_bucket {
            Some(bucket.strip_prefix("gemini-").unwrap_or(bucket).trim_end_matches(if kind == "5h" { "-5h" } else { "-weekly" }).to_string())
        } else { None };
        merge_into(&mut windows, window(kind, (1.0 - remaining) * 100.0, reset_at, model));
    }
    windows
}

/// TECH 26.1: Anthropic's usage endpoint for Claude Code logins. It is unofficial: any failure leaves the status line
/// numbers (TECH 26) in place.
const OAUTH_URL: &str = "https://api.anthropic.com/api/oauth/usage";
/// Numbers older than this are read again when the UI asks or the 10-minute poll runs.
const OAUTH_FRESH: i64 = 10 * 60;
/// A Claude turn used allowance: its account is read again this long after the `Stop` hook.
const AFTER_STOP: Duration = Duration::from_secs(30);
const KEYCHAIN: Duration = Duration::from_secs(2);
const HTTP: Duration = Duration::from_secs(8);
/// No HTTPS client is in the build; curl ships with macOS and the usual Linux installs.
const CURL: &str = "/usr/bin/curl";

/// Isolated run folders (tests, the bench script, the demo daemon) never start provider processes or touch the network.
fn isolated() -> bool {
    std::env::var_os("GEMBALA_RUNDIR").is_some()
}

/// The Claude Code login fields Gembala uses; the rest of the stored JSON is dropped. No `Debug`, so the token can't
/// reach a format string. Lives for one request.
struct Login {
    token: String,
    /// unix milliseconds
    expires_ms: Option<i64>,
    profile: bool,
    plan: Option<String>,
}

/// The stored login: `.credentials.json`, or a Keychain item holding the JSON or its hex encoding.
fn parse_login(raw: &str) -> Option<Login> {
    let raw = raw.trim();
    let v: Value = serde_json::from_str(raw).ok().or_else(|| {
        let bytes = (0..raw.len()).step_by(2).map(|i| raw.get(i..i + 2).and_then(|h| u8::from_str_radix(h, 16).ok())).collect::<Option<Vec<u8>>>()?;
        serde_json::from_slice(&bytes).ok()
    })?;
    let o = &v["claudeAiOauth"];
    let token = o["accessToken"].as_str().filter(|t| !t.is_empty())?.to_string();
    let scopes: Vec<&str> = match &o["scopes"] {
        Value::Array(a) => a.iter().filter_map(Value::as_str).collect(),
        _ => o["scope"].as_str().map(|s| s.split_whitespace().collect()).unwrap_or_default(),
    };
    Some(Login {
        token,
        expires_ms: o["expiresAt"].as_i64(),
        // an old login without a scope list is tried; a token from `claude setup-token` lists only `user:inference`
        profile: scopes.is_empty() || scopes.contains(&"user:profile"),
        plan: plan_name(o["subscriptionType"].as_str(), o["rateLimitTier"].as_str()),
    })
}

/// "max" + "default_claude_max_20x" = "Max (20x)".
fn plan_name(subscription: Option<&str>, tier: Option<&str>) -> Option<String> {
    let sub = subscription.filter(|s| !s.is_empty())?;
    let mut name = sub[..1].to_uppercase() + &sub[1..];
    if let Some(x) = tier.and_then(|t| t.rsplit('_').next()).filter(|p| p.len() > 1 && p.ends_with('x') && p[..p.len() - 1].bytes().all(|c| c.is_ascii_digit())) {
        name.push_str(&format!(" ({x})"));
    }
    Some(name)
}

/// The file and the Keychain item can both exist (an old file left beside the item Claude Code keeps current): the one
/// that expires last is the live login, the Keychain item on a tie.
fn fresher(file: Option<Login>, keychain: Option<Login>) -> Option<Login> {
    match (file, keychain) {
        (Some(f), Some(k)) => Some(if f.expires_ms > k.expires_ms { f } else { k }),
        (f, k) => f.or(k),
    }
}

/// Why a login is not sent at all. Gembala never refreshes: the refresh token is single use and belongs to Claude Code,
/// so a login near its expiry waits for the next Claude session to renew it.
fn skip(l: &Login, now_ms: i64) -> Option<&'static str> {
    if !l.profile {
        return Some("no user:profile scope");
    }
    if l.expires_ms.is_some_and(|t| t < now_ms + 5 * 60 * 1000) {
        return Some("login due for refresh");
    }
    None
}

/// Claude Code's Keychain item for a config folder: the plain name for `~/.claude`, otherwise suffixed with the first
/// 8 hex digits of the folder path's sha256.
fn keychain_name(dir: &Path) -> String {
    use sha2::{Digest, Sha256};
    if dir == account::main_dir() {
        return "Claude Code-credentials".into();
    }
    let hash = Sha256::digest(dir.as_os_str().as_encoded_bytes());
    format!("Claude Code-credentials-{}", hash[..4].iter().map(|b| format!("{b:02x}")).collect::<String>())
}

#[cfg(target_os = "macos")]
fn keychain(dir: &Path) -> Option<String> {
    let user = std::env::var("USER").ok().filter(|u| !u.is_empty())?;
    let mut c = Command::new("/usr/bin/security");
    c.args(["find-generic-password", "-a", &user, "-w", "-s", &keychain_name(dir)]).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
    profile::run_with_timeout(c, KEYCHAIN)
}

#[cfg(not(target_os = "macos"))]
fn keychain(_dir: &Path) -> Option<String> {
    None
}

fn stored_login(dir: &Path) -> Option<Login> {
    let file = std::fs::read_to_string(dir.join(".credentials.json")).ok().and_then(|t| parse_login(&t));
    fresher(file, keychain(dir).and_then(|t| parse_login(&t)))
}

/// RFC 3339 (`2026-10-05T09:49:59.699314+00:00`) to unix seconds.
fn rfc3339(s: &str) -> Option<i64> {
    let n = |r: std::ops::Range<usize>| s.get(r)?.parse::<i64>().ok();
    let (y, mo, d, h, mi, se) = (n(0..4)?, n(5..7)?, n(8..10)?, n(11..13)?, n(14..16)?, n(17..19)?);
    let mut rest = s.get(19..)?;
    if let Some(r) = rest.strip_prefix('.') {
        rest = r.trim_start_matches(|c: char| c.is_ascii_digit());
    }
    let off = match rest {
        "Z" | "z" => 0,
        _ => {
            let sign = match rest.get(..1)? {
                "+" => 1,
                "-" => -1,
                _ => return None,
            };
            sign * (rest.get(1..3)?.parse::<i64>().ok()? * 3600 + rest.get(4..6)?.parse::<i64>().ok()? * 60)
        }
    };
    // days from the civil date (H. Hinnant's algorithm)
    let y = if mo <= 2 { y - 1 } else { y };
    let (era, yoe) = (y.div_euclid(400), y.rem_euclid(400));
    let doy = (153 * ((mo + 9) % 12) + 2) / 5 + d - 1;
    let days = era * 146_097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719_468;
    Some(days * 86_400 + h * 3600 + mi * 60 + se - off)
}

/// The usage answer: `five_hour` = 5h, `seven_day` = week, `limits[]` of kind `weekly_scoped` = a week per model,
/// then `seven_day_opus` / `seven_day_sonnet` for a model not listed there.
pub fn parse_oauth(v: &Value) -> Vec<Window> {
    let mut out = Vec::new();
    let mut add = |kind: &'static str, w: &Value, model: Option<String>| {
        if let Some(used) = w["utilization"].as_f64().or_else(|| w["percent"].as_f64()).filter(|u| u.is_finite()) {
            out.push(window(kind, used, w["resets_at"].as_str().and_then(rfc3339), model));
        }
    };
    add("5h", &v["five_hour"], None);
    add("week", &v["seven_day"], None);
    let mut models: Vec<String> = Vec::new();
    for e in v["limits"].as_array().into_iter().flatten().filter(|e| e["kind"] == "weekly_scoped") {
        let Some(m) = e.pointer("/scope/model/display_name").and_then(Value::as_str).map(str::trim).filter(|m| !m.is_empty()) else { continue };
        if !models.iter().any(|x| x == m) {
            models.push(m.to_string());
            add("week", e, Some(m.to_string()));
        }
    }
    for (m, key) in [("Opus", "seven_day_opus"), ("Sonnet", "seven_day_sonnet")] {
        if !models.iter().any(|x| x == m) {
            add("week", &v[key], Some(m.to_string()));
        }
    }
    out
}

enum Answer {
    Read(Vec<Window>),
    /// seconds to wait
    Throttled(i64),
    /// the reason for the log: never the body, a header, or the token
    Failed(String),
}

/// One GET through curl. The Authorization header goes in on stdin (`-H @-`), never in argv where `ps` shows it; `-q`
/// keeps a `.curlrc` from adding a trace or an output file; stderr is dropped.
fn fetch(curl: &str, token: &str, version: &str) -> Answer {
    let mut c = Command::new(curl);
    c.args(["-q", "-sS", "--proto", "=https", "--max-time", "8", "-D", "-", "-H", "@-", "-H", "Accept: application/json", "-H", "anthropic-beta: oauth-2025-04-20", "-A"])
        .arg(format!("claude-code/{version}"))
        .arg(OAUTH_URL)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    pty::scrub_env(&mut c);
    let header = format!("Authorization: Bearer {token}\n");
    match profile::run_with_input(c, Some(header.as_bytes()), HTTP + Duration::from_secs(1)) {
        Some(out) => answer(&out),
        None => Answer::Failed("no answer".into()),
    }
}

/// curl's output: header blocks (a proxy's `CONNECT` adds one), then the body.
fn answer(out: &str) -> Answer {
    let (mut rest, mut status, mut retry) = (out, 0, None);
    while rest.starts_with("HTTP/") {
        let Some((head, body)) = rest.split_once("\r\n\r\n") else { break };
        let mut lines = head.lines();
        status = lines.next().and_then(|l| l.split_whitespace().nth(1)).and_then(|c| c.parse::<u16>().ok()).unwrap_or(0);
        retry = lines.find_map(|l| l.split_once(':').filter(|(k, _)| k.trim().eq_ignore_ascii_case("retry-after")).and_then(|(_, v)| v.trim().parse::<i64>().ok()));
        rest = body;
    }
    match status {
        429 => Answer::Throttled(retry.filter(|s| *s > 0).unwrap_or(600).min(3600)),
        200..=299 => match serde_json::from_str::<Value>(rest) {
            Ok(v) => {
                let windows = parse_oauth(&v);
                if windows.is_empty() { Answer::Failed("answer without known windows".into()) } else { Answer::Read(windows) }
            }
            Err(_) => Answer::Failed("invalid JSON".into()),
        },
        0 => Answer::Failed("no HTTP answer".into()),
        s => Answer::Failed(format!("HTTP {s}")),
    }
}

/// Claude logins: Utama (`~/.claude`) and every FR-61 Claude account.
fn claude_logins(st: &State) -> Vec<(String, String, PathBuf)> {
    let mut out = vec![("utama".to_string(), "Utama".to_string(), account::main_dir())];
    for a in st.accounts.lock().unwrap().iter().filter(|a| a.agent == "claude") {
        out.push((a.id.clone(), a.name.clone(), account::expand(&a.dir)));
    }
    out
}

/// TECH 26.1: reads each Claude account whose numbers are older than 10 minutes or kept from the last run.
pub fn claude_refresh(st: &Arc<State>) {
    let at = now();
    for (id, name, dir) in claude_logins(st) {
        let fresh = st.usage.lock().unwrap().rows.iter().any(|r| r.agent == "claude" && r.login == id && r.stale.is_none() && at - r.read_at < OAUTH_FRESH);
        if !fresh {
            claude_later(st, id, name, dir, Duration::ZERO);
        }
    }
}

/// TECH 26.1: a Claude turn in this account (FR-61 id, empty = Utama) ended.
pub fn claude_after_stop(st: &Arc<State>, account: &str) {
    let id = if account.is_empty() { "utama" } else { account };
    if let Some((id, name, dir)) = claude_logins(st).into_iter().find(|l| l.0 == id) {
        claude_later(st, id, name, dir, AFTER_STOP);
    }
}

fn claude_later(st: &Arc<State>, id: String, name: String, dir: PathBuf, delay: Duration) {
    if isolated() {
        return;
    }
    {
        let mut s = st.usage.lock().unwrap();
        if s.reading.contains(&id) {
            return;
        }
        s.reading.push(id.clone());
    }
    let st = st.clone();
    thread::spawn(move || {
        thread::sleep(delay);
        claude_read(&st, &id, &name, &dir);
        st.usage.lock().unwrap().reading.retain(|x| *x != id);
    });
}

fn claude_read(st: &State, id: &str, name: &str, dir: &Path) {
    if now() < st.usage.lock().unwrap().paused_until {
        return;
    }
    // the User-Agent carries the installed Claude Code version (FR-99 detection); before it's known the next read tries
    let version = st.agents.lock().unwrap().as_array().and_then(|a| a.iter().find(|x| x["agent"] == "claude")?["version"].as_str().map(String::from));
    let Some(version) = version else { return };
    let Some(login) = stored_login(dir) else { return };
    if skip(&login, now() * 1000).is_some() {
        return;
    }
    match fetch(CURL, &login.token, &version) {
        Answer::Read(windows) => {
            // a per-model week at its limit leaves the other models usable
            let limited = windows.iter().any(|w| w.model.is_none() && w.used_pct >= 100.0);
            put(st, Row { agent: "claude", login: id.into(), login_name: name.into(), windows, source: "claude_oauth", read_at: now(), limited, plan: login.plan, stale: None });
        }
        Answer::Throttled(secs) => {
            throttle(&mut st.usage.lock().unwrap(), secs);
            eprintln!("gembala daemon: usage claude oauth: throttled for {secs} s");
            changed(st);
        }
        Answer::Failed(why) => eprintln!("gembala daemon: usage claude oauth ({id}): {why}"),
    }
}

/// A 429 pauses every account; the last numbers stay, marked old.
fn throttle(s: &mut Store, secs: i64) {
    s.paused_until = now() + secs;
    for r in s.rows.iter_mut().filter(|r| r.agent == "claude") {
        r.stale = Some("throttled");
    }
}

/// FR-102 `/status` block: numbers and reset times per login, nothing from the work (SEC-33/36). Windows whose reset
/// already passed are left out.
pub fn telegram_lines(st: &State, id_l: bool) -> Vec<(String, String, Vec<String>)> {
    let at = now();
    let mut out = Vec::new();
    for r in rows(st) {
        let parts: Vec<String> = r
            .windows
            .iter()
            .filter(|w| w.resets_at.is_none_or(|t| t > at))
            .map(|w| {
                let kind = match (w.kind, id_l) {
                    ("5h", true) => "5j",
                    ("week", true) => "7h",
                    ("month", true) => "bulan",
                    ("day", true) => "hari",
                    ("week", false) => "week",
                    ("month", false) => "month",
                    ("day", false) => "day",
                    (k, _) => k,
                };
                // TECH 26.1: per-model weeks next to the all-models week
                let kind = w.model.as_ref().map_or(kind.to_string(), |m| format!("{m} {kind}"));
                let head = format!("{kind} {}% {}", (100.0 - w.used_pct).round() as i64, if id_l { "sisa" } else { "left" });
                match w.resets_at {
                    Some(t) => format!("{head} ({}{})", if id_l { "reset " } else { "resets " }, clock(t)),
                    None => head,
                }
            })
            .collect();
        if !parts.is_empty() {
            out.push((r.agent.to_string(), r.login_name, parts));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agy_statusline_quota_only_keeps_public_numbers() {
        let quota = json!({
            "gemini-5h": {"remaining_fraction": 0.6, "reset_in_seconds": 3600},
            "gemini-weekly": {"remaining_fraction": 0.8},
            "gemini-argon-wtu-weekly": {"remaining_fraction": 0.5},
            "3p-5h": {"remaining_fraction": 0.9},
            "3p-weekly": {"remaining_fraction": 0.7},
            "disabled-weekly": {"remaining_fraction": 0.1, "disabled": true},
            "unknown": {"remaining_fraction": 0.1}
        });
        let rows = parse_agy_quota(&quota);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows.iter().find(|w| w.model.is_none() && w.kind == "5h").unwrap().used_pct, 40.0);
        assert!(rows.iter().any(|w| w.model.as_deref() == Some("argon-wtu") && w.kind == "week" && w.used_pct == 50.0));
        assert!(rows.iter().find(|w| w.model.is_none() && w.kind == "5h").unwrap().resets_at.unwrap() > now());
    }

    #[test]
    fn footer_codex_dari_teks_asli() {
        let line = "GPT-6.1 Terra medium · Context 87% left · 251K window · 5h 66% left · weekly 79% left · 12.3K used · ~/proj";
        let (w, limited) = parse_codex_footer(line).unwrap();
        assert_eq!(w, vec![window("5h", 34.0, None, None), window("week", 21.0, None, None)]);
        assert!(!limited);
        // the other spelling and an exhausted window
        let (w, limited) = parse_codex_footer("o3 high · 5-hour 0% left · weekly 100% left").unwrap();
        assert_eq!((w[0].used_pct, w[1].used_pct, limited), (100.0, 0.0, true));
        // the weekly item is optional (API key logins have no limits) and other text isn't a footer
        assert!(parse_codex_footer("o3 high · Context 50% left").is_none());
        assert!(parse_codex_footer("weekly 50% left").is_none());
    }

    #[test]
    fn footer_gemini_dari_kode_bundel() {
        let (w, limited) = parse_gemini_footer("~/proj  main  gemini-2.5-pro  14% used").unwrap();
        assert_eq!(w.len(), 1);
        assert_eq!((w[0].kind, w[0].used_pct, w[0].resets_at, limited), ("day", 14.0, None, false));
        let (w, limited) = parse_gemini_footer("quota  limit reached, resets in 2h").unwrap();
        assert_eq!((w[0].used_pct, limited), (100.0, true));
        assert!(parse_gemini_footer("~/proj  main  gemini-2.5-pro").is_none());
        assert!(parse_gemini_footer("120% used").is_none());
    }

    #[test]
    fn omp_usage_json() {
        let v = json!({"reports": [{"provider": "opencode-go", "limits": [
            {"id": "rolling-5h", "window": {"resetsAt": 1794388910272u64}, "amount": {"usedFraction": 0}, "status": "ok"},
            {"id": "weekly", "window": {"resetsAt": 1794388910272u64}, "amount": {"usedFraction": 0.02}, "status": "ok"},
            {"id": "monthly", "window": {}, "amount": {"usedFraction": 0.48}, "status": "ok"},
        ]}]});
        let (w, limited) = parse_omp(&v).unwrap();
        assert_eq!(w, vec![window("5h", 0.0, Some(1794388910), None), window("week", 2.0, Some(1794388910), None), window("month", 48.0, None, None)]);
        assert!(!limited);
        // two providers: the tighter window of a kind is kept
        let two = json!({"reports": [{"limits": [{"id": "weekly", "amount": {"usedFraction": 0.1}, "status": "ok"}]},
                                     {"limits": [{"id": "weekly", "amount": {"usedFraction": 0.9}, "status": "exceeded"}]}]});
        let (w, limited) = parse_omp(&two).unwrap();
        assert_eq!((w.len(), w[0].used_pct, limited), (1, 90.0, true));
        assert!(parse_omp(&json!({"reports": []})).is_none());
    }

    #[test]
    fn rate_limits_claude() {
        let rate = json!({"five_hour": {"used_percentage": 14, "resets_at": 1794380400}, "seven_day": {"used_percentage": 49, "resets_at": 1794380400}});
        assert_eq!(parse_claude(&rate), vec![window("5h", 14.0, Some(1794380400), None), window("week", 49.0, Some(1794380400), None)]);
        // only one window is fine; text is not a number
        assert_eq!(parse_claude(&json!({"seven_day": {"used_percentage": 49}})).len(), 1);
        assert!(parse_claude(&json!({"five_hour": {"used_percentage": "14"}})).is_empty());
        assert!(parse_claude(&json!({})).is_empty());
    }

    #[test]
    fn app_server_codex() {
        let v = json!({"jsonrpc": "2.0", "id": 2, "result": {"rateLimits": {
            "primary": {"usedPercent": 34, "windowDurationMins": 300, "resetsAt": 1794388910},
            "secondary": {"usedPercent": 12, "windowDurationMins": 10080, "resetsAt": 1794390000},
        }}});
        let (w, limited) = parse_codex_rpc(&v).unwrap();
        assert_eq!(w, vec![window("5h", 34.0, Some(1794388910), None), window("week", 12.0, Some(1794390000), None)]);
        assert!(!limited);
        // buckets without a duration keep the old primary/secondary order
        let v = json!({"result": {"rateLimits": {"primary": {"usedPercent": 100}, "secondary": {"usedPercent": 5}}}});
        let (w, limited) = parse_codex_rpc(&v).unwrap();
        assert_eq!((w[0].kind, w[1].kind, limited), ("5h", "week", true));
        assert!(parse_codex_rpc(&json!({"result": {}})).is_none());
    }

    /// The omp command path with a stand-in script (the real `omp` is never run in tests).
    #[test]
    fn omp_cli_skrip_palsu() {
        use std::os::unix::fs::PermissionsExt;
        let path = std::env::temp_dir().join(format!("gbl-102-omp-{}", std::process::id()));
        let _ = std::fs::remove_file(&path);
        std::fs::write(
            &path,
            r#"#!/bin/sh
printf '%s' '{"reports":[{"limits":[{"id":"rolling-5h","window":{"resetsAt":1794388910272},"amount":{"usedFraction":0.5},"status":"ok"}]}]}'
"#,
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        // Linux: a fork in another test thread can still hold the write fd, so exec fails with ETXTBSY; retry
        let (w, limited) = (0..3).find_map(|_| omp_usage(&path.to_string_lossy())).unwrap();
        assert_eq!(w, vec![window("5h", 50.0, Some(1794388910), None)]);
        assert!(!limited);
        let _ = std::fs::remove_file(&path);
    }

    /// The probe talks the real protocol to a stand-in script (the real `codex` is never run in tests).
    #[test]
    fn app_server_probe_skrip_palsu() {
        use std::os::unix::fs::PermissionsExt;
        let path = std::env::temp_dir().join(format!("gbl-102-codex-{}", std::process::id()));
        let _ = std::fs::remove_file(&path);
        std::fs::write(
            &path,
            r#"#!/bin/sh
read line
printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"userAgent":"gembala"}}'
read line
read line
printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{"rateLimits":{"primary":{"usedPercent":34,"windowDurationMins":300,"resetsAt":1794388910},"secondary":{"usedPercent":12,"windowDurationMins":10080,"resetsAt":1794390000}}}}'
"#,
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        let (w, limited) = probe_codex(&path.to_string_lossy(), Some("/tmp/gbl-102-home")).unwrap();
        assert_eq!(w, vec![window("5h", 34.0, Some(1794388910), None), window("week", 12.0, Some(1794390000), None)]);
        assert!(!limited);
        let _ = std::fs::remove_file(&path);
    }

    /// The answer shape from Kybern's `claude_usage.rs` test (TECH 26.1 fixture).
    fn oauth_fixture() -> Value {
        json!({
            "five_hour": {"utilization": 22.0, "resets_at": "2026-10-05T09:49:59.699314+00:00"},
            "seven_day": {"utilization": 33.0, "resets_at": "2026-10-10T12:59:59+00:00"},
            "seven_day_opus": null,
            "seven_day_sonnet": {"utilization": 4.0, "resets_at": null},
            "limits": [
                {"kind": "session", "percent": 22},
                {"kind": "weekly_scoped", "percent": 2, "resets_at": "2026-10-10T12:59:59+00:00", "scope": {"model": {"display_name": "Fable"}}}
            ]
        })
    }

    #[test]
    fn keychain_item_per_config_folder() {
        assert_eq!(keychain_name(&account::main_dir()), "Claude Code-credentials");
        // checked on the user's Mac 17 Nov 2026: this folder's item exists under this name
        assert_eq!(keychain_name(Path::new("/Users/aufahanif85gmail.com/.claude-kantor")), "Claude Code-credentials-9f8805fe");
    }

    #[test]
    fn login_fields_plan_and_choice() {
        let json = r#"{"claudeAiOauth":{"accessToken":"tok-a","refreshToken":"r","expiresAt":9000,"scopes":["user:inference","user:profile"],"subscriptionType":"max","rateLimitTier":"default_claude_max_20x"}}"#;
        let hex: String = json.bytes().map(|b| format!("{b:02x}")).collect();
        for raw in [json.to_string(), hex] {
            let l = parse_login(&raw).unwrap();
            assert_eq!((l.token.as_str(), l.expires_ms, l.profile, l.plan.as_deref()), ("tok-a", Some(9000), true, Some("Max (20x)")));
        }
        assert_eq!(plan_name(Some("pro"), Some("default_claude_pro")).as_deref(), Some("Pro"));
        assert_eq!(plan_name(None, Some("default_claude_max_5x")), None);
        assert!(parse_login(r#"{"claudeAiOauth":{"accessToken":""}}"#).is_none());
        assert!(parse_login("not a login").is_none());
        // the login that expires last wins; the Keychain item on a tie
        let at = |t: &str, exp: i64| parse_login(&format!(r#"{{"claudeAiOauth":{{"accessToken":"{t}","expiresAt":{exp}}}}}"#));
        assert_eq!(fresher(at("file", 9), at("kc", 5)).unwrap().token, "file");
        assert_eq!(fresher(at("file", 5), at("kc", 9)).unwrap().token, "kc");
        assert_eq!(fresher(at("file", 5), at("kc", 5)).unwrap().token, "kc");
        assert_eq!(fresher(None, at("kc", 5)).unwrap().token, "kc");
        assert!(fresher(None, None).is_none());
    }

    #[test]
    fn login_skipped_without_scope_or_near_expiry() {
        let now_ms = 1_000_000_000;
        let l = |scope: &str, exp: i64| parse_login(&format!(r#"{{"claudeAiOauth":{{"accessToken":"t","expiresAt":{exp},"scope":"{scope}"}}}}"#)).unwrap();
        assert_eq!(skip(&l("user:inference", now_ms + 3_600_000), now_ms), Some("no user:profile scope"));
        assert_eq!(skip(&l("user:inference user:profile", now_ms + 60_000), now_ms), Some("login due for refresh"));
        assert_eq!(skip(&l("user:inference user:profile", now_ms - 1), now_ms), Some("login due for refresh"));
        assert_eq!(skip(&l("user:inference user:profile", now_ms + 3_600_000), now_ms), None);
    }

    #[test]
    fn oauth_answer_to_windows() {
        assert_eq!(rfc3339("2026-10-05T09:49:59.699314+00:00"), Some(1_791_193_799));
        assert_eq!(rfc3339("2026-10-05T11:49:59+02:00"), Some(1_791_193_799));
        assert_eq!(rfc3339("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(rfc3339("2024-02-29T00:00:00Z"), Some(1_709_164_800));
        assert!(rfc3339("yesterday").is_none());
        let w = parse_oauth(&oauth_fixture());
        assert_eq!(
            w,
            vec![
                window("5h", 22.0, Some(1_791_193_799), None),
                window("week", 33.0, Some(1_791_637_199), None),
                window("week", 2.0, Some(1_791_637_199), Some("Fable".into())),
                window("week", 4.0, None, Some("Sonnet".into())),
            ]
        );
        assert!(parse_oauth(&json!({"error": "x"})).is_empty());
    }

    #[test]
    fn http_answer_status_and_retry_after() {
        let ok = format!("HTTP/1.1 200 Connection established\r\n\r\nHTTP/2 200\r\ncontent-type: application/json\r\n\r\n{}", oauth_fixture());
        assert!(matches!(answer(&ok), Answer::Read(w) if w.len() == 4));
        assert!(matches!(answer("HTTP/2 429\r\nretry-after: 120\r\n\r\n{}"), Answer::Throttled(120)));
        assert!(matches!(answer("HTTP/2 429\r\n\r\n"), Answer::Throttled(600)));
        assert!(matches!(answer("HTTP/2 429\r\nRetry-After: 99999\r\n\r\n"), Answer::Throttled(3600)));
        assert!(matches!(answer("HTTP/2 401\r\n\r\n{}"), Answer::Failed(r) if r == "HTTP 401"));
        assert!(matches!(answer("HTTP/2 200\r\n\r\n<html>"), Answer::Failed(r) if r == "invalid JSON"));
        assert!(matches!(answer("HTTP/2 200\r\n\r\n{}"), Answer::Failed(_)));
        assert!(matches!(answer(""), Answer::Failed(_)));
    }

    #[test]
    fn throttle_pauses_and_marks_claude_rows() {
        let mut s = Store::default();
        let row = |agent: &'static str| Row { agent, login: "utama".into(), windows: vec![window("5h", 10.0, None, None)], ..Default::default() };
        s.rows = vec![row("claude"), row("codex")];
        throttle(&mut s, 120);
        assert!(s.paused_until >= now() + 119);
        assert_eq!((s.rows[0].stale, s.rows[1].stale), (Some("throttled"), None));
    }

    #[test]
    fn sources_merge_per_window() {
        let mut rows = Vec::new();
        let oauth = Row {
            agent: "claude",
            login: "utama".into(),
            windows: parse_oauth(&oauth_fixture()),
            source: "claude_oauth",
            read_at: 100,
            plan: Some("Max (20x)".into()),
            stale: Some("last_run"),
            ..Default::default()
        };
        assert!(merge(&mut rows, oauth.clone()));
        // a newer status line reading replaces 5h and week; the per-model weeks and plan stay; the row is fresh again
        let sl = Row { agent: "claude", login: "utama".into(), windows: vec![window("5h", 50.0, None, None), window("week", 40.0, None, None)], source: "claude_statusline", read_at: 200, ..Default::default() };
        assert!(merge(&mut rows, sl));
        let r = &rows[0];
        assert_eq!((r.source, r.plan.as_deref(), r.stale, r.windows.len()), ("claude_statusline", Some("Max (20x)"), None, 4));
        assert_eq!(r.windows.iter().find(|w| w.kind == "5h").unwrap().used_pct, 50.0);
        assert!(r.windows.iter().any(|w| w.model.as_deref() == Some("Fable")));
        // the next OAuth read brings the full set
        let later = Row { windows: vec![window("5h", 60.0, None, None)], read_at: 300, stale: None, ..oauth };
        assert!(merge(&mut rows, later));
        assert_eq!(rows[0].windows.len(), 1);
    }

    #[test]
    fn saved_numbers_come_back_as_last_run() {
        let row = Row { agent: "claude", login: "kantor".into(), login_name: "Kantor".into(), windows: parse_oauth(&oauth_fixture()), source: "claude_oauth", read_at: 100, plan: Some("Pro".into()), stale: Some("throttled"), ..Default::default() };
        let saved = saved_json(std::slice::from_ref(&row));
        assert!(saved["rows"][0].get("stale").is_none());
        let back = load_saved(&serde_json::from_str(&saved.to_string()).unwrap());
        assert_eq!(back, vec![Row { stale: Some("last_run"), ..row }]);
        // unknown agents or sources are dropped
        assert!(load_saved(&json!({"rows": [{"agent": "x", "source": "claude_oauth", "login": "a", "windows": [{"kind": "5h", "used_pct": 1}]}]})).is_empty());
    }

    /// The request goes to a stand-in curl (the real endpoint is never called in tests). The token travels on stdin only,
    /// and no answer turns it into a log line, even when the body or stderr echo it back.
    #[test]
    fn token_only_on_stdin_never_in_log() {
        use std::os::unix::fs::PermissionsExt;
        let token = "sk-ant-oat01-FAKE-TOKEN-FOR-TEST";
        let dir = std::env::temp_dir().join(format!("gbl-261-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let curl = dir.join("curl");
        for (n, reply) in ["HTTP/2 500\\r\\n\\r\\n%s", "HTTP/2 200\\r\\n\\r\\n%s", "HTTP/2 401\\r\\nwww-authenticate: %s\\r\\n\\r\\n"].iter().enumerate() {
            let _ = std::fs::remove_file(&curl);
            std::fs::write(
                &curl,
                format!(
                    "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{d}/args'\nhead=$(cat)\nprintf '%s' \"$head\" > '{d}/stdin'\necho \"$head\" >&2\nprintf '{reply}' \"$head\"\n",
                    d = dir.display()
                ),
            )
            .unwrap();
            std::fs::set_permissions(&curl, std::fs::Permissions::from_mode(0o755)).unwrap();
            // Linux: a fork in another test thread can still hold the write fd (ETXTBSY); retry
            let got = (0..3).map(|_| fetch(&curl.to_string_lossy(), token, "2.1.0")).find(|a| !matches!(a, Answer::Failed(r) if r == "no answer")).unwrap();
            let Answer::Failed(reason) = got else { panic!("answer {n} should fail") };
            assert!(!reason.contains(token) && !reason.contains("Bearer"), "answer {n}: {reason}");
            let args = std::fs::read_to_string(dir.join("args")).unwrap();
            assert!(!args.contains(token) && args.contains("claude-code/2.1.0") && args.contains(OAUTH_URL) && args.starts_with("-q\n"));
            assert_eq!(std::fs::read_to_string(dir.join("stdin")).unwrap(), format!("Authorization: Bearer {token}"));
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
