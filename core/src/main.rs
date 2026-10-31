mod daemon;
mod grid;
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
        cmd @ ("send" | "wait" | "read" | "reply" | "hook") => {
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

fn usage() -> ExitCode {
    eprintln!("{USAGE}");
    ExitCode::from(2)
}
