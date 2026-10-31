mod config;
mod daemon;
mod grid;
mod profile;
mod pty;
mod rundir;

use std::fs::File;
use std::io::Write;
use std::os::fd::FromRawFd;
use std::process::ExitCode;

const USAGE: &str = "pakai: gembala <daemon [--secret-fd N]|send|wait|read|reply|hook>";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str).unwrap_or_default() {
        "daemon" => daemon(&args[1..]),
        "hook" => hook(&args[1..]),
        cmd @ ("send" | "wait" | "read" | "reply") => {
            eprintln!("gembala {cmd}: belum ada (Fase 2)");
            ExitCode::from(2)
        }
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

fn usage() -> ExitCode {
    eprintln!("{USAGE}");
    ExitCode::from(2)
}
