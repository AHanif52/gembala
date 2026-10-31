#[allow(dead_code)] // dipakai daemon mulai pekerjaan 0.3; hapus allow saat itu
mod grid;
#[allow(dead_code)]
mod pty;
mod rundir;

use std::process::ExitCode;

const USAGE: &str = "pakai: gembala <daemon|send|wait|read|reply|hook>";

fn main() -> ExitCode {
    let cmd = std::env::args().nth(1).unwrap_or_default();
    match cmd.as_str() {
        "daemon" => daemon(),
        "send" | "wait" | "read" | "reply" | "hook" => {
            eprintln!("gembala {cmd}: belum ada (Fase 2)");
            ExitCode::from(2)
        }
        _ => {
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
    }
}

// ponytail: baru cek folder socket; PTY + socket + attach = kerja Fase 0
fn daemon() -> ExitCode {
    let dir = rundir::default_dir();
    let res = rundir::ensure_private_dir(&dir).and_then(|_| rundir::socket_path(&dir, "pane.sock"));
    match res {
        Ok(sock) => {
            println!("folder socket siap: {}", sock.display());
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("gembala daemon: {e}");
            ExitCode::FAILURE
        }
    }
}
