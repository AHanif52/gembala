mod bus;
mod config;
mod daemon;
mod git;
mod grid;
mod msg;
mod profile;
mod pty;
mod rundir;
mod sign;

use std::fs::File;
use std::io::Write;
use std::os::fd::FromRawFd;
use std::process::ExitCode;

const USAGE: &str = "pakai: gembala daemon [--secret-fd N]
       gembala send <pane> <pesan…> [--queue] [--reply-to <msg_id>]
       gembala wait [<pane>] [--reply <msg_id>] [--pattern <regex>] [--timeout <detik>]
       gembala read <pane> [-n <baris>]
       gembala reply <msg_id> <teks…>
       gembala status
       gembala watch";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str).unwrap_or_default() {
        "daemon" => daemon(&args[1..]),
        "hook" => hook(&args[1..]),
        cmd @ ("send" | "wait" | "read" | "reply" | "status" | "watch") => agent_cli(cmd, &args[1..]),
        _ => {
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
    }
}

/// `--secret-fd N`: rahasia kanal UI ditulis ke descriptor warisan peluncur (UI), lalu ditutup.
/// Tanpa flag ini kanal UI menolak semua koneksi.
fn daemon(args: &[String]) -> ExitCode {
    let secret_fd = match args {
        [] => None,
        [f, n] if f == "--secret-fd" => match n.parse::<i32>() {
            Ok(n) if n > 2 => Some(n),
            _ => return usage(),
        },
        _ => return usage(),
    };
    let dir = daemon::default_dir();
    let res = rundir::ensure_private_dir(&dir).and_then(|_| {
        let secret = secret_fd.map(|_| daemon::new_secret()).transpose()?;
        let handoff = secret.clone();
        daemon::run(&dir, secret, move || {
            if let (Some(fd), Some(s)) = (secret_fd, handoff) {
                let mut f = unsafe { File::from_raw_fd(fd) }; // ditutup saat drop
                if let Err(e) = writeln!(f, "{s}") {
                    // tanpa serah-terima, kanal UI tidak bisa dipakai siapa pun: daemon tidak berguna
                    eprintln!("gembala daemon: tulis rahasia ke fd {fd}: {e}");
                    std::process::exit(1);
                }
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

/// `gembala hook <agent> <event> [payload]`, dipanggil hook agent di dalam pane (SEC-26, FR-4).
/// Selalu keluar 0 dan tidak menulis ke stdout: hook yang gagal tidak boleh mengganggu agent
/// (stdout hook UserPromptSubmit Claude Code ikut masuk ke prompt).
fn hook(args: &[String]) -> ExitCode {
    let (Some(agent), Some(event)) = (args.first(), args.get(1)) else { return ExitCode::SUCCESS };
    let mut payload = args.get(2).cloned().unwrap_or_default();
    if agent == "claude" {
        use std::io::Read;
        let _ = std::io::stdin().take(64 * 1024).read_to_string(&mut payload);
    }
    let Some(status) = profile::hook_status(agent, event, &payload) else { return ExitCode::SUCCESS };
    let Some(sock) = std::env::var_os("GEMBALA_SOCKET") else { return ExitCode::SUCCESS };
    let send = || -> std::io::Result<()> {
        use std::io::{BufRead, BufReader};
        let mut s = std::os::unix::net::UnixStream::connect(sock)?;
        s.set_read_timeout(Some(std::time::Duration::from_secs(1)))?;
        let mut r = BufReader::new(s.try_clone()?);
        let mut l = String::new();
        writeln!(s, "{}", serde_json::json!({"id": 1, "method": "hello", "params": {"proto": daemon::PROTO, "client": "hook"}}))?;
        r.read_line(&mut l)?;
        writeln!(s, "{}", serde_json::json!({"id": 2, "method": "hook", "params": {"status": status}}))?;
        r.read_line(&mut l)?;
        Ok(())
    };
    let _ = send();
    ExitCode::SUCCESS
}

/// Fase 2 (O2): CLI untuk agent di dalam pane. Satu baris JSON ke stdout; kode keluar 0 = diterima/berhasil,
/// 1 = ditolak/timeout, 2 = salah pakai atau tidak bisa tersambung. Identitas dari kernel (SEC-3), bukan argumen.
fn agent_cli(cmd: &str, args: &[String]) -> ExitCode {
    use serde_json::{Value, json};
    let mut rest: Vec<String> = Vec::new();
    let mut opt = std::collections::HashMap::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--queue" => drop(opt.insert("queue", Value::Bool(true))),
            "--reply-to" | "--reply" | "--pattern" | "--timeout" | "-n" => match it.next() {
                Some(v) => drop(opt.insert(a.trim_start_matches('-'), json!(v))),
                None => return usage(),
            },
            _ => rest.push(a.clone()),
        }
    }
    let text = |from: usize| rest.get(from..).map(|r| r.join(" ")).unwrap_or_default();
    let params = match cmd {
        "send" if rest.len() >= 2 => json!({"to": rest[0], "body": text(1), "queue": opt.get("queue"), "reply_to": opt.get("reply-to")}),
        "reply" if rest.len() >= 2 => json!({"msg_id": rest[0], "body": text(1)}),
        "read" if rest.len() == 1 => json!({"target": rest[0], "lines": opt.get("n").and_then(|v| v.as_str()).and_then(|v| v.parse::<u64>().ok())}),
        "wait" if rest.len() <= 1 => json!({
            "target": rest.first(), "reply": opt.get("reply"), "pattern": opt.get("pattern"),
            "timeout_ms": opt.get("timeout").and_then(|v| v.as_str()).and_then(|v| v.parse::<u64>().ok()).map(|s| s * 1000),
        }),
        "status" | "watch" => json!({}),
        _ => return usage(),
    };
    let Some(sock) = std::env::var_os("GEMBALA_SOCKET") else {
        eprintln!("gembala {cmd}: GEMBALA_SOCKET tidak ada; jalankan dari dalam pane Gembala");
        return ExitCode::from(2);
    };
    let run = || -> std::io::Result<Value> {
        use std::io::{BufRead, BufReader};
        let mut s = std::os::unix::net::UnixStream::connect(sock)?;
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
            // FR-8: satu baris JSON per perubahan status pane yang boleh dibaca, sampai dihentikan
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
            let out = v.get("result").cloned().unwrap_or_else(|| v.get("error").cloned().unwrap_or_default());
            println!("{out}");
            let ok = v.get("result").is_some_and(|r| r["result"] != "rejected" && r.get("timeout").is_none());
            ExitCode::from(if ok { 0 } else { 1 })
        }
        Err(e) => {
            eprintln!("gembala {cmd}: {e}");
            ExitCode::from(2)
        }
    }
}

fn usage() -> ExitCode {
    eprintln!("{USAGE}");
    ExitCode::from(2)
}
