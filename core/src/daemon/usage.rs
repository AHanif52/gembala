//! FR-102: how much of each agent login's allowance is used. Numbers only from what the agents hand out themselves:
//! the Codex footer drawn on a pane (FR-85) and `codex app-server` `account/rateLimits/read` (Codex reads its own
//! login), `omp usage --json`, the `quota` item of the Gemini footer, and the `rate_limits` Claude sends to its status
//! line through the wrapper Gembala installs per launch. No token, `auth.json`, Keychain, or provider API is read
//! (SEC-15, SEC-32), and nothing from a login is logged.
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
pub const AGENTS: &[&str] = &["claude", "codex", "omp", "opencode", "gemini", "commandcode"];

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

#[derive(Clone, Debug, PartialEq)]
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
}

#[derive(Default)]
pub struct Store {
    rows: Vec<Row>,
    /// A Codex pane ran in this daemon run: the app-server probe is for logins in use (FR-102).
    codex: bool,
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
                "source": r.source, "read_at": r.read_at, "limited": r.limited,
            })
        })
        .collect();
    json!({"rows": rows})
}

/// New numbers for one agent login. The row's age is refreshed on every reading; the UI is only told when a number
/// actually changes (the Codex footer is read every second).
fn put(st: &State, mut row: Row) {
    sort(&mut row.windows);
    let tell = {
        let mut s = st.usage.lock().unwrap();
        match s.rows.iter_mut().find(|r| r.agent == row.agent && r.login == row.login) {
            None => {
                s.rows.push(row);
                true
            }
            Some(cur) => {
                // A footer has no reset time: keep the one the app-server reading gave for the same window while it's ahead.
                if row.source == "codex_footer" {
                    for w in row.windows.iter_mut().filter(|w| w.resets_at.is_none()) {
                        if let Some(t) = cur.windows.iter().find(|o| o.kind == w.kind && o.model == w.model).and_then(|o| o.resets_at).filter(|t| *t > now()) {
                            w.resets_at = Some(t);
                        }
                    }
                }
                let tell = cur.windows != row.windows || cur.source != row.source || cur.limited != row.limited || cur.login_name != row.login_name;
                *cur = row;
                tell
            }
        }
    };
    if tell {
        changed(st);
    }
}

fn changed(st: &State) {
    let ev = line(json!({"event": "usage.changed", "params": status(st)}));
    st.ui_txs.lock().unwrap().retain(|(_, tx)| tx.try_send(ev.clone()).is_ok());
}

/// A Codex pane ran in this daemon run (footer or profile); the 10-minute app-server probe only runs for its logins.
pub fn note_codex(st: &State) {
    st.usage.lock().unwrap().codex = true;
}

/// FR-102: started once by the daemon; probes the agents that can answer for themselves.
pub fn start(st: &Arc<State>) {
    // Isolated run folders (tests, the bench script) never start provider processes or touch the network.
    if std::env::var_os("GEMBALA_RUNDIR").is_some() {
        return;
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
                    put(st, Row { agent: "codex", login: "utama".into(), login_name: "Utama".into(), windows, source: "codex_footer", read_at: now(), limited });
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
            put(st, Row { agent: "gemini", login: "utama".into(), login_name: "Utama".into(), windows, source: "gemini_footer", read_at: now(), limited });
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
    let installed = |agent: &str| -> Option<String> {
        let agents = st.agents.lock().unwrap();
        agents.as_array()?.iter().find(|a| a["agent"] == agent && a["installed"] == true)?["path"].as_str().map(String::from)
    };
    if let Some(path) = installed("omp") {
        match omp_usage(&path) {
            Some((windows, limited)) => put(st, Row { agent: "omp", login: "utama".into(), login_name: "Utama".into(), windows, source: "omp_cli", read_at: now(), limited }),
            // offline, logged out, or a changed shape: keep the last numbers (FR-102: nothing is guessed)
            None => eprintln!("gembala daemon: usage omp: no numbers"),
        }
    }
    if st.usage.lock().unwrap().codex
        && let Some(path) = installed("codex")
    {
        for (login, name, home) in codex_logins(st) {
            match probe_codex(&path, home.as_deref()) {
                Some((windows, limited)) => put(st, Row { agent: "codex", login, login_name: name, windows, source: "codex_app_server", read_at: now(), limited }),
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
    put(st, Row { agent: "claude", login, login_name: if name.is_empty() { "Utama".into() } else { name }, windows, source: "claude_statusline", read_at: now(), limited });
    Ok(json!({}))
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
        let (w, limited) = omp_usage(&path.to_string_lossy()).unwrap();
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
}
