//! PTY pane: spawn lewat login shell, baca/tulis, resize (TECH bagian 6, pekerjaan 0.1).

use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};

pub struct Pty {
    /// Master PTY; baca = output pane, tulis = input pane. `try_clone` untuk thread baca terpisah.
    pub master: File,
    pub child: Child,
}

fn check(rc: libc::c_int) -> io::Result<()> {
    if rc == -1 { Err(io::Error::last_os_error()) } else { Ok(()) }
}

fn winsize(rows: u16, cols: u16) -> libc::winsize {
    libc::winsize { ws_row: rows, ws_col: cols, ws_xpixel: 0, ws_ypixel: 0 }
}

fn cloexec(fd: libc::c_int) -> io::Result<()> {
    check(unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) })
}

// ponytail: daftar tetap dari env sesi Claude Code 2.1 yang teramati; tambah prefix saat agent lain ketahuan menurunkan penanda
const INHERITED_DROP: &[&str] = &["CLAUDECODE", "CLAUDE_CODE_", "CLAUDE_PID", "CLAUDE_EFFORT", "CODEX_", "OPENCODE_", "GEMBALA_"];

pub fn login_shell() -> String {
    std::env::var("SHELL").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "/bin/zsh".into())
}

impl Pty {
    /// `$SHELL -l -c <cmd>`, atau `$SHELL -l` interaktif bila `cmd` kosong.
    pub fn spawn(cmd: Option<&str>, rows: u16, cols: u16, env: &[(&str, &str)]) -> io::Result<Pty> {
        let (mut m, mut s) = (0, 0);
        let mut ws = winsize(rows, cols);
        check(unsafe { libc::openpty(&mut m, &mut s, std::ptr::null_mut(), std::ptr::null_mut(), &mut ws) })?;
        let (master, slave) = unsafe { (OwnedFd::from_raw_fd(m), OwnedFd::from_raw_fd(s)) };
        // master jangan sampai diwarisi proses pane (agent tidak boleh pegang PTY pane lain)
        cloexec(m)?;
        cloexec(s)?;

        let mut c = Command::new(login_shell());
        c.arg("-l");
        if let Some(cmd) = cmd {
            c.arg("-c").arg(cmd);
        }
        // SEC-35: penanda sesi agent yang menjalankan Gembala (termasuk socket + token pesannya) tidak ikut ke pane
        for (k, _) in std::env::vars_os() {
            let k = k.to_string_lossy();
            if INHERITED_DROP.iter().any(|p| k.starts_with(p)) {
                c.env_remove(&*k);
            }
        }
        c.env("TERM", "xterm-256color")
            .envs(env.iter().copied())
            .stdin(Stdio::from(slave.try_clone()?))
            .stdout(Stdio::from(slave.try_clone()?))
            .stderr(Stdio::from(slave));
        // setsid + jadikan slave terminal pengendali, supaya Ctrl+C/job control jalan
        unsafe {
            c.pre_exec(|| {
                check(libc::setsid())?;
                check(libc::ioctl(0, libc::TIOCSCTTY as _, 0))
            });
        }
        let child = c.spawn()?;
        drop(c); // tutup salinan slave di daemon; tanpa ini baca master tidak pernah EOF
        Ok(Pty { master: File::from(master), child })
    }

}

pub fn resize(master: &File, rows: u16, cols: u16) -> io::Result<()> {
    let ws = winsize(rows, cols);
    check(unsafe { libc::ioctl(master.as_raw_fd(), libc::TIOCSWINSZ as _, &ws) })
}

/// Baca master sampai pane tutup. macOS/Linux memberi EIO, bukan EOF, saat slave sudah tertutup.
#[cfg(test)]
pub fn read_to_end(mut r: impl io::Read) -> io::Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        match r.read(&mut buf) {
            Ok(0) => return Ok(out),
            Ok(n) => out.extend_from_slice(&buf[..n]),
            Err(e) if e.raw_os_error() == Some(libc::EIO) => return Ok(out),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn run(mut p: Pty) -> String {
        let out = read_to_end(&p.master).unwrap();
        p.child.wait().unwrap();
        String::from_utf8_lossy(&out).into_owned()
    }

    #[test]
    fn echo_lewat_pty() {
        let out = run(Pty::spawn(Some("echo gembala-ok"), 24, 80, &[]).unwrap());
        assert!(out.contains("gembala-ok"), "{out:?}");
    }

    #[test]
    fn ukuran_awal_dan_resize() {
        assert!(run(Pty::spawn(Some("stty size"), 30, 100, &[]).unwrap()).contains("30 100"));
        let p = Pty::spawn(Some("sleep 0.3; stty size"), 24, 80, &[]).unwrap();
        resize(&p.master, 40, 120).unwrap();
        let out = run(p);
        assert!(out.contains("40 120"), "{out:?}");
    }

    #[test]
    fn tulis_ke_pty() {
        let p = Pty::spawn(Some("read x; echo dapat-$x"), 24, 80, &[]).unwrap();
        (&p.master).write_all(b"halo\r").unwrap();
        let out = run(p);
        assert!(out.contains("dapat-halo"), "{out:?}");
    }

    #[test]
    fn punya_tty_pengendali() {
        // /dev/tty hanya bisa dibuka bila setsid + TIOCSCTTY berhasil
        let out = run(Pty::spawn(Some("tty; echo x > /dev/tty"), 24, 80, &[]).unwrap());
        assert!(!out.contains("Device not configured"), "{out:?}");
        assert!(out.contains("/dev/ttys") || out.contains("/dev/pts/"), "{out:?}");
    }
}
