mod account;
mod addon;
mod bus;
mod config;
mod daemon;
mod diag;
mod git;
mod grid;
mod harness;
mod history;
mod ipc;
mod jail;
mod msg;
mod os;
mod profile;
mod pty;
mod recipe;
mod rundir;
mod saver;
mod sign;
mod snap;

use std::fs::File;
use std::io::Write;
use std::os::fd::FromRawFd;
use std::process::ExitCode;

const USAGE: &str = "usage: gembala daemon [--secret-fd N | --secret-sock <path>]
       gembala send <pane> <message…> [--queue] [--reply-to <msg_id>] [--model <id>] [--effort <level>]
       gembala wait [<pane>] [--reply <msg_id>] [--pattern <regex>] [--timeout <seconds>]
       gembala read <pane> [-n <lines>]
       gembala reply <msg_id> <text…>
       gembala close <pane>
       gembala report <title> [text…] [--screenshot]
       gembala status
       gembala watch";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str).unwrap_or_default() {
        "daemon" => daemon(&args[1..]),
        "hook" => hook(&args[1..]),
        // FR-98, SEC-20: runs an agent inside a Landlock domain (internal, not in USAGE; a wrapper prefix)
        "jail" => jail::main(&args[1..]),
        // FR-102: Claude Code status line wrapper, installed per launch through `--settings` (internal, not in USAGE)
        "statusline" => statusline(),
        // FR-61: called by the zsh `claude` function before Claude for another account runs (not in USAGE: internal)
        "trust-sync" => {
            // only accounts in config with trust on (FR-61 (8): "Trusted folders" can be Own)
            let text = std::fs::read_to_string(config::path()).ok();
            let on = |d: &str| config::load(text.as_deref()).accounts.iter().any(|a| a.trust && account::expand(&a.dir) == account::expand(d));
            if let Some(dir) = args.get(1).filter(|d| on(d)) {
                account::sync_trust(dir);
            }
            ExitCode::SUCCESS
        }
        // FR-78: `addons <agent>` is called by the zsh `claude`/`codex` functions (internal, not in USAGE)
        cmd @ ("send" | "wait" | "read" | "reply" | "close" | "report" | "status" | "watch" | "token" | "addons") => agent_cli(cmd, &args[1..]),
        _ => {
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
    }
}

enum Handoff {
    Fd(i32),
    Sock(std::path::PathBuf),
}

/// The UI channel secret is handed to the launcher, then the channel is closed. Without a flag the UI channel rejects every connection.
/// `--secret-fd N`: inherited descriptor (test scripts, UI fallback). `--secret-sock <path>`: socket owned by the UI when the daemon
/// runs as a helper app through `open` (FR-20); the secret is only written when the socket owner is this bundle's `gembala-ui`.
fn daemon(args: &[String]) -> ExitCode {
    let handoff = match args {
        [] => None,
        [f, n] if f == "--secret-fd" => match n.parse::<i32>() {
            Ok(n) if n > 2 => Some(Handoff::Fd(n)),
            _ => return usage(),
        },
        [f, p] if f == "--secret-sock" => Some(Handoff::Sock(p.into())),
        _ => return usage(),
    };
    let dir = daemon::default_dir();
    let res = rundir::ensure_private_dir(&dir).and_then(|_| {
        let secret = handoff.as_ref().map(|_| daemon::new_secret()).transpose()?;
        let s = secret.clone();
        daemon::run(&dir, secret, move || {
            let (Some(h), Some(s)) = (handoff, s) else { return };
            if let Err(e) = hand_over(h, &s) {
                // without a handover nobody can use the UI channel: the daemon is useless
                eprintln!("gembala daemon: secret handover: {e}");
                std::process::exit(1);
            }
        })
    });
    match res {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("gembala daemon: {e}");
            ExitCode::FAILURE
        }
    }
}

/// `gembala hook <agent> <event> [payload]`, called by agent hooks inside a pane (SEC-26, FR-4).
/// Always exits 0 and never writes to stdout: a failing hook must not disturb the agent
/// (Claude Code's UserPromptSubmit hook stdout ends up in the prompt).
fn hook(args: &[String]) -> ExitCode {
    let (Some(agent), Some(event)) = (args.first(), args.get(1)) else { return ExitCode::SUCCESS };
    let mut payload = args.get(2).cloned().unwrap_or_default();
    // Claude's hook input is on stdin; omp's plugin passes a lone `-` for the same reason (its answer would otherwise
    // sit in the process arguments)
    if agent == "claude" || payload == "-" {
        use std::io::Read;
        let _ = std::io::stdin().take(64 * 1024).read_to_string(&mut payload);
    }
    let Some(status) = profile::hook_status(agent, event, &payload) else { return ExitCode::SUCCESS };
    // FR-66: last message of a Codex turn; the daemon only uses it when a message is waiting for a reply
    let reply = (agent == "codex")
        .then(|| serde_json::from_str::<serde_json::Value>(&payload).ok())
        .flatten()
        .filter(|v| v["type"] == "agent-turn-complete")
        .and_then(|v| v["last-assistant-message"].as_str().map(|m| m.chars().take(4000).collect::<String>()))
        .filter(|m| !m.trim().is_empty());
    // FR-68: session id for exact resume (Codex `thread-id` in the notify payload, OpenCode `sessionID` from the plugin)
    let json = || serde_json::from_str::<serde_json::Value>(&payload).unwrap_or_default();
    let session = match agent.as_str() {
        "codex" => json()["thread-id"].as_str().map(String::from),
        "opencode" => Some(payload.trim().to_string()).filter(|p| !p.is_empty()),
        // FR-61: Claude `session_id` in every event, `source` in SessionStart; account folder from the hook process env
        "claude" => json()["session_id"].as_str().map(String::from),
        _ => None,
    };
    let source = json()["source"].as_str().map(String::from);
    // FR-80: the turn's last answer for the Telegram "done" message, from the hook input itself (see `hook_last`)
    let last = profile::hook_last(agent, &payload);
    let dir = std::env::var("CLAUDE_CONFIG_DIR").ok().filter(|_| agent == "claude");
    let Some(sock) = std::env::var_os("GEMBALA_SOCKET") else { return ExitCode::SUCCESS };
    let send = || -> std::io::Result<()> {
        use std::io::{BufRead, BufReader};
        let mut s = std::os::unix::net::UnixStream::connect(sock)?;
        s.set_read_timeout(Some(std::time::Duration::from_secs(1)))?;
        let mut r = BufReader::new(s.try_clone()?);
        let mut l = String::new();
        writeln!(s, "{}", serde_json::json!({"id": 1, "method": "hello", "params": {"proto": daemon::PROTO, "client": "hook"}}))?;
        r.read_line(&mut l)?;
        writeln!(s, "{}", serde_json::json!({"id": 2, "method": "hook", "params": {"status": status, "reply": reply, "last": last, "agent": agent, "session": session, "source": source, "dir": dir}}))?;
        r.read_line(&mut l)?;
        Ok(())
    };
    let _ = send();
    ExitCode::SUCCESS
}

/// FR-102: Claude Code's status line wrapper, installed per launch through `--settings`. Claude runs it with the session
/// JSON on stdin: the `rate_limits` numbers go to the daemon, then the user's own `statusLine.command` — that field only,
/// from that account's settings.json (SEC-15 exception 3) — gets the same stdin and its stdout is printed as is. Any
/// problem prints nothing: a failing wrapper must not break Claude's status line.
fn statusline() -> ExitCode {
    use std::io::Read;
    let mut input = String::new();
    let _ = std::io::stdin().take(64 * 1024).read_to_string(&mut input);
    let payload: serde_json::Value = serde_json::from_str(&input).unwrap_or_default();
    send_usage(&payload);
    if let Some(cmd) = user_statusline()
        && let Some(out) = run_statusline(&cmd, &input)
    {
        print!("{out}");
        let _ = std::io::stdout().flush();
    }
    ExitCode::SUCCESS
}

/// The rate limits reach the daemon as numbers through the pane channel; the rest of the status line JSON stays here.
fn send_usage(payload: &serde_json::Value) {
    let rate = &payload["rate_limits"];
    if !rate.is_object() {
        return;
    }
    let Some(sock) = std::env::var_os("GEMBALA_SOCKET") else { return };
    let dir = std::env::var("CLAUDE_CONFIG_DIR").ok();
    let token = std::env::var("GEMBALA_TOKEN").ok();
    let send = || -> std::io::Result<()> {
        use std::io::{BufRead, BufReader};
        let mut s = std::os::unix::net::UnixStream::connect(&sock)?;
        s.set_read_timeout(Some(std::time::Duration::from_secs(1)))?;
        let mut r = BufReader::new(s.try_clone()?);
        let mut l = String::new();
        writeln!(s, "{}", serde_json::json!({"id": 1, "method": "hello", "params": {"proto": daemon::PROTO, "client": "statusline", "token": token}}))?;
        r.read_line(&mut l)?;
        writeln!(s, "{}", serde_json::json!({"id": 2, "method": "usage", "params": {"dir": dir, "rate_limits": rate}}))?;
        r.read_line(&mut l)?;
        Ok(())
    };
    let _ = send();
}

/// SEC-15 exception 3: only `statusLine.command` is read, and nothing is written.
fn user_statusline() -> Option<String> {
    let dir = std::env::var("CLAUDE_CONFIG_DIR").map(|d| account::expand(&d)).unwrap_or_else(|_| account::main_dir());
    let path = dir.join("settings.json");
    // follows the account's link to Utama; a device or a giant file is not a settings file
    let meta = std::fs::metadata(&path).ok()?;
    if !meta.is_file() || meta.len() > 1024 * 1024 {
        return None;
    }
    let text = std::fs::read_to_string(path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    let cmd = v["statusLine"]["command"].as_str()?.trim();
    (!cmd.is_empty()).then(|| cmd.to_string())
}

/// Runs the user's command with the same stdin; 2 second limit, failure or timeout = nothing.
fn run_statusline(cmd: &str, input: &str) -> Option<String> {
    use std::process::{Command, Stdio};
    let mut c = Command::new("/bin/sh");
    c.args(["-c", cmd]).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
    let mut child = c.spawn().ok()?;
    let mut stdin = child.stdin.take()?;
    let stdout = child.stdout.take()?;
    let data = input.as_bytes().to_vec();
    let writer = std::thread::spawn(move || {
        use std::io::Write;
        let _ = stdin.write_all(&data);
    });
    let reader = std::thread::spawn(move || {
        use std::io::Read;
        let mut out = String::new();
        let _ = stdout.take(64 * 1024).read_to_string(&mut out);
        out
    });
    let start = std::time::Instant::now();
    while child.try_wait().ok()?.is_none() {
        if start.elapsed() > std::time::Duration::from_secs(2) {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let _ = writer.join();
    // a command that wrote too much is cut, not an error
    reader.join().ok()
}

/// Phase 2 (O2): CLI for agents inside a pane. One JSON line to stdout; exit code 0 = accepted/succeeded,
/// 1 = rejected/timeout, 2 = misuse or can't connect. Identity comes from the kernel (SEC-3), not arguments.
fn agent_cli(cmd: &str, args: &[String]) -> ExitCode {
    use serde_json::{Value, json};
    let mut rest: Vec<String> = Vec::new();
    let mut opt = std::collections::HashMap::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--queue" => drop(opt.insert("queue", Value::Bool(true))),
            "--screenshot" => drop(opt.insert("screenshot", Value::Bool(true))),
            "--reply-to" | "--reply" | "--pattern" | "--timeout" | "-n" | "--model" | "--effort" => match it.next() {
                Some(v) => drop(opt.insert(a.trim_start_matches('-'), json!(v))),
                None => return usage(),
            },
            _ => rest.push(a.clone()),
        }
    }
    let text = |from: usize| rest.get(from..).map(|r| r.join(" ")).unwrap_or_default();
    let params = match cmd {
        "send" if rest.len() >= 2 => json!({"to": rest[0], "body": text(1), "queue": opt.get("queue"), "reply_to": opt.get("reply-to"), "model": opt.get("model"), "effort": opt.get("effort")}),
        "reply" if rest.len() >= 2 => json!({"msg_id": rest[0], "body": text(1)}),
        "close" if rest.len() == 1 => json!({"target": rest[0]}),
        // FR-100: a draft for the user to review; Gembala never sends it anywhere (SEC-40)
        "report" if !rest.is_empty() => json!({"title": rest[0], "body": text(1), "screenshot": opt.get("screenshot")}),
        "read" if rest.len() == 1 => json!({"target": rest[0], "lines": opt.get("n").and_then(|v| v.as_str()).and_then(|v| v.parse::<u64>().ok())}),
        "wait" if rest.len() <= 1 => json!({
            "target": rest.first(), "reply": opt.get("reply"), "pattern": opt.get("pattern"),
            "timeout_ms": opt.get("timeout").and_then(|v| v.as_str()).and_then(|v| v.parse::<u64>().ok()).map(|s| s * 1000),
        }),
        "status" | "watch" | "token" => json!({}),
        "addons" if rest.len() == 1 => json!({"agent": rest[0]}),
        _ => return usage(),
    };
    let Some(sock) = std::env::var_os("GEMBALA_SOCKET") else {
        eprintln!("gembala {cmd}: GEMBALA_SOCKET isn't set; run this from inside a Gembala pane");
        return ExitCode::from(2);
    };
    let run = || -> std::io::Result<Value> {
        use std::io::{BufRead, BufReader};
        let mut s = match std::os::unix::net::UnixStream::connect(&sock) {
            Ok(s) => s,
            // FR-66: the sandbox (Codex) blocks sockets: go through a spool file with the pane token
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied && cmd != "watch" => {
                let token = std::env::var("GEMBALA_SPOOL").or_else(|_| std::env::var("GEMBALA_TOKEN")).map_err(|_| e)?;
                return spool_call(std::path::Path::new(&sock), &token, cmd, &params);
            }
            Err(e) => return Err(e),
        };
        let mut r = BufReader::new(s.try_clone()?);
        let mut l = String::new();
        let token = std::env::var("GEMBALA_TOKEN").ok();
        writeln!(s, "{}", json!({"id": 1, "method": "hello", "params": {"proto": daemon::PROTO, "client": "cli", "token": token}}))?;
        r.read_line(&mut l)?;
        let hello: Value = serde_json::from_str(&l).unwrap_or_default();
        if hello.get("result").is_none() {
            return Ok(hello);
        }
        if cmd == "watch" {
            // FR-8: one JSON line per status change of a readable pane, until stopped
            writeln!(s, "{}", json!({"id": 2, "method": "subscribe", "params": {}}))?;
            for line in r.lines() {
                let v: Value = serde_json::from_str(&line?).unwrap_or_default();
                if v.get("event").is_some() {
                    println!("{}", v["params"]);
                }
            }
            return Ok(json!({"result": {"closed": true}}));
        }
        writeln!(s, "{}", json!({"id": 2, "method": cmd, "params": params}))?;
        l.clear();
        r.read_line(&mut l)?;
        Ok(serde_json::from_str(&l).unwrap_or_default())
    };
    match run() {
        Ok(v) => {
            if v["result"]["note"] == "model_ignored" {
                eprintln!("gembala send: model and effort only apply when opening a new pane");
            }
            let out = v.get("result").cloned().unwrap_or_else(|| v.get("error").cloned().unwrap_or_default());
            match out["token"].as_str() {
                // `GEMBALA_SPOOL="$(gembala token)"`: token polos
                Some(t) if cmd == "token" => println!("{t}"),
                // one argument per line for zsh `${(f)…}`; an error (older daemon) prints nothing, so no stray argument
                _ if cmd == "addons" => out["args"].as_array().into_iter().flatten().filter_map(Value::as_str).for_each(|a| println!("{a}")),
                _ => println!("{out}"),
            }
            let ok = v.get("result").is_some_and(|r| r["result"] != "rejected" && r.get("timeout").is_none());
            ExitCode::from(if ok { 0 } else { 1 })
        }
        Err(e) => {
            eprintln!("gembala {cmd}: {e}");
            ExitCode::from(2)
        }
    }
}

/// FR-66: request through `<run>/spool/<id>.req` (write then rename), the answer is awaited as `<id>.res`.
fn spool_call(sock: &std::path::Path, token: &str, method: &str, params: &serde_json::Value) -> std::io::Result<serde_json::Value> {
    use std::time::{Duration, Instant};
    let dir = sock.parent().unwrap_or(std::path::Path::new(".")).join("spool");
    let id = daemon::new_secret()?[..16].to_string();
    let (tmp, req, res) = (dir.join(format!("{id}.tmp")), dir.join(format!("{id}.req")), dir.join(format!("{id}.res")));
    std::fs::write(&tmp, serde_json::json!({"token": token, "method": method, "params": params}).to_string())?;
    std::fs::rename(&tmp, &req)?;
    // `wait` waits on its own in the daemon; the limit is a bit above its timeout (default 10 minutes)
    let limit = match params["timeout_ms"].as_u64() {
        Some(ms) => Duration::from_millis(ms) + Duration::from_secs(10),
        None if method == "wait" => Duration::from_secs(600),
        None => Duration::from_secs(30),
    };
    let start = Instant::now();
    loop {
        if let Ok(text) = std::fs::read_to_string(&res) {
            let _ = std::fs::remove_file(&res);
            return Ok(serde_json::from_str(&text).unwrap_or_default());
        }
        if start.elapsed() > limit {
            let _ = std::fs::remove_file(&req);
            return Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "the daemon didn't answer through the spool"));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn hand_over(h: Handoff, secret: &str) -> std::io::Result<()> {
    match h {
        Handoff::Fd(fd) => writeln!(unsafe { File::from_raw_fd(fd) }, "{secret}"), // closed on drop
        Handoff::Sock(p) => {
            let mut s = std::os::unix::net::UnixStream::connect(p)?;
            // the socket owner must be this install's UI: macOS `Gembala.app/Contents/Helpers/Gembala Daemon.app/Contents/MacOS/gembala`
            // → `Gembala.app/Contents/MacOS/gembala-ui`; Linux/Windows side by side with the daemon binary
            let me = std::env::current_exe()?.canonicalize()?;
            #[cfg(target_os = "macos")]
            let ui = me.ancestors().nth(5).map(|c| c.join("MacOS/gembala-ui")).and_then(|p| p.canonicalize().ok());
            #[cfg(not(target_os = "macos"))]
            let ui = me.with_file_name("gembala-ui").canonicalize().ok();
            let peer = pty::proc_path(ipc::peer_pid(&s)?).and_then(|p| p.canonicalize().ok());
            if ui.is_none() || peer != ui {
                return Err(std::io::Error::other(format!("socket owner isn't this bundle's gembala-ui: {peer:?}")));
            }
            writeln!(s, "{secret}")?;
            // stay connected until the UI is done checking who the sender is (LOCAL_PEERPID fails after close)
            s.set_read_timeout(Some(std::time::Duration::from_secs(5)))?;
            let _ = std::io::Read::read(&mut s, &mut [0u8; 1]);
            Ok(())
        }
    }
}

fn usage() -> ExitCode {
    eprintln!("{USAGE}");
    ExitCode::from(2)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// FR-102: the user's command gets the status line stdin as is; one that doesn't answer in 2 seconds prints nothing.
    #[test]
    fn statusline_perintah_pengguna() {
        assert_eq!(run_statusline("/bin/cat", "{\"a\":1}"), Some("{\"a\":1}".into()));
        let start = std::time::Instant::now();
        assert_eq!(run_statusline("sleep 5; echo lama", "x"), None);
        assert!(start.elapsed() < std::time::Duration::from_secs(4), "batas waktu tidak jalan");
        // a command that prints nothing is not an error
        assert_eq!(run_statusline("/bin/cat >/dev/null", "x"), Some(String::new()));
    }
}
