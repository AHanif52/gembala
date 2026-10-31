//! PTY pane: spawn lewat login shell, baca/tulis, resize (TECH bagian 6, pekerjaan 0.1).

use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::path::Path;
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

/// SEC-35: penanda sesi agent yang menjalankan Gembala (termasuk socket + token pesannya) tidak ikut ke pane.
pub fn scrub_env(c: &mut Command) {
    for (k, _) in std::env::vars_os() {
        let k = k.to_string_lossy();
        if INHERITED_DROP.iter().any(|p| k.starts_with(p)) {
            c.env_remove(&*k);
        }
    }
}

pub fn login_shell() -> String {
    std::env::var("SHELL").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "/bin/zsh".into())
}

impl Pty {
    /// `$SHELL -l -c <cmd>`, atau `$SHELL -l` interaktif bila `cmd` kosong.
    pub fn spawn(cmd: Option<&str>, rows: u16, cols: u16, env: &[(&str, &str)], cwd: Option<&Path>) -> io::Result<Pty> {
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
        scrub_env(&mut c);
        // app dari Finder/Dock mewarisi cwd `/` dari launchd; tanpa folder dari profil/pane, mulai di home
        match cwd.filter(|d| d.is_dir()) {
            Some(d) => drop(c.current_dir(d)),
            None => {
                if let Some(home) = std::env::var_os("HOME") {
                    c.current_dir(home);
                }
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

/// Proses yang sedang jalan di pane (FR-38, FR-50): `(pid, grup)`. Job foreground bila shell
/// memakai job control; anak langsung shell bila tidak (`$SHELL -l -c`). `None` = shell diam.
pub fn busy(master: &File, shell: i32) -> Option<(i32, bool)> {
    let fg = unsafe { libc::tcgetpgrp(master.as_raw_fd()) };
    if fg > 0 && fg != shell {
        return Some((fg, true));
    }
    children(shell).first().map(|&c| (c, false))
}

/// Sinyal ke proses `busy`; `0` = cek masih hidup.
pub fn signal((pid, group): (i32, bool), sig: libc::c_int) -> bool {
    unsafe { if group { libc::killpg(pid, sig) == 0 } else { libc::kill(pid, sig) == 0 } }
}

#[cfg(target_os = "macos")]
pub fn proc_name(pid: i32) -> Option<String> {
    let mut buf = [0u8; 4 * libc::MAXPATHLEN as usize];
    let n = unsafe { libc::proc_pidpath(pid, buf.as_mut_ptr().cast(), buf.len() as u32) };
    let path = std::str::from_utf8(&buf[..usize::try_from(n).ok().filter(|&n| n > 0)?]).ok()?;
    path.rsplit('/').next().map(str::to_string)
}

/// Folder kerja proses (FR-30: pane baru ikut folder pane yang fokus).
#[cfg(target_os = "macos")]
pub fn cwd_of(pid: i32) -> Option<std::path::PathBuf> {
    let mut info: libc::proc_vnodepathinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_vnodepathinfo>() as libc::c_int;
    let n = unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDVNODEPATHINFO, 0, (&raw mut info).cast(), size) };
    if n != size {
        return None;
    }
    let raw: &[libc::c_char] = info.pvi_cdir.vip_path.as_flattened();
    let bytes: Vec<u8> = raw.iter().take_while(|&&c| c != 0).map(|&c| c as u8).collect();
    use std::os::unix::ffi::OsStringExt;
    Some(std::ffi::OsString::from_vec(bytes).into()).filter(|p: &std::path::PathBuf| p.is_absolute())
}

#[cfg(target_os = "linux")]
pub fn cwd_of(pid: i32) -> Option<std::path::PathBuf> {
    std::fs::read_link(format!("/proc/{pid}/cwd")).ok()
}

#[cfg(target_os = "macos")]
fn children(pid: i32) -> Vec<i32> {
    let mut buf = [0 as libc::pid_t; 64];
    let size = std::mem::size_of_val(&buf) as libc::c_int;
    let n = unsafe { libc::proc_listchildpids(pid, buf.as_mut_ptr().cast(), size) };
    buf[..n.clamp(0, 64) as usize].to_vec()
}

#[cfg(target_os = "linux")]
pub fn proc_name(pid: i32) -> Option<String> {
    std::fs::read_to_string(format!("/proc/{pid}/comm")).ok().map(|s| s.trim().to_string())
}

#[cfg(target_os = "linux")]
fn children(pid: i32) -> Vec<i32> {
    let s = std::fs::read_to_string(format!("/proc/{pid}/task/{pid}/children")).unwrap_or_default();
    s.split_whitespace().filter_map(|p| p.parse().ok()).collect()
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
        let out = run(Pty::spawn(Some("echo gembala-ok"), 24, 80, &[], None).unwrap());
        assert!(out.contains("gembala-ok"), "{out:?}");
    }

    #[test]
    fn ukuran_awal_dan_resize() {
        assert!(run(Pty::spawn(Some("stty size"), 30, 100, &[], None).unwrap()).contains("30 100"));
        let p = Pty::spawn(Some("sleep 0.3; stty size"), 24, 80, &[], None).unwrap();
        resize(&p.master, 40, 120).unwrap();
        let out = run(p);
        assert!(out.contains("40 120"), "{out:?}");
    }

    #[test]
    fn tulis_ke_pty() {
        let p = Pty::spawn(Some("read x; echo dapat-$x"), 24, 80, &[], None).unwrap();
        (&p.master).write_all(b"halo\r").unwrap();
        let out = run(p);
        assert!(out.contains("dapat-halo"), "{out:?}");
    }

    #[test]
    fn punya_tty_pengendali() {
        // /dev/tty hanya bisa dibuka bila setsid + TIOCSCTTY berhasil
        let out = run(Pty::spawn(Some("tty; echo x > /dev/tty"), 24, 80, &[], None).unwrap());
        assert!(!out.contains("Device not configured"), "{out:?}");
        assert!(out.contains("/dev/ttys") || out.contains("/dev/pts/"), "{out:?}");
    }
}
