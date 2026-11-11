//! Pane PTY: spawn through the login shell, read/write, resize (TECH section 6, work item 0.1).

use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};

pub struct Pty {
    /// PTY master; read = pane output, write = pane input. `try_clone` for a separate read thread.
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

// ponytail: fixed list from the observed Claude Code 2.1 session env; add a prefix when another agent turns out to pass down markers
// FR-61: `CLAUDE_CONFIG_DIR` of an app opened from another account's session must not become the pane account; accounts only come from the profile/`claude-<account>`
// Color switches too: Oh My Pi's shell tool sets `NO_COLOR=1`, so a daemon started from an agent turned every pane's
// Claude Code black and white (11 Nov). A user's own setting comes back from their rc files, panes run `$SHELL -l`.
const INHERITED_DROP: &[&str] =
    &["CLAUDECODE", "CLAUDE_CODE_", "CLAUDE_PID", "CLAUDE_EFFORT", "CLAUDE_CONFIG_DIR", "CODEX_", "OPENCODE_", "GEMBALA_", "NO_COLOR", "FORCE_COLOR", "CLICOLOR"];

/// SEC-35: session markers of the agent running Gembala (including its message socket + token) don't reach panes.
pub fn scrub_env(c: &mut Command) {
    for (k, _) in std::env::vars_os() {
        let k = k.to_string_lossy();
        if INHERITED_DROP.iter().any(|p| k.starts_with(p)) {
            c.env_remove(&*k);
        }
    }
}

/// `$SHELL`, else the account's shell from the user database (a daemon started without a login env), else `/bin/sh`
/// (zsh isn't on every Linux)
pub fn login_shell() -> String {
    std::env::var("SHELL").ok().filter(|s| !s.is_empty()).or_else(passwd_shell).unwrap_or_else(|| "/bin/sh".into())
}

fn passwd_shell() -> Option<String> {
    let pw = unsafe { libc::getpwuid(libc::getuid()) };
    let sh = unsafe { pw.as_ref().filter(|p| !p.pw_shell.is_null()).map(|p| std::ffi::CStr::from_ptr(p.pw_shell)) }?;
    Some(sh.to_str().ok()?.to_owned()).filter(|s| !s.is_empty())
}

impl Pty {
    /// `$SHELL -l -c <cmd>`, or interactive `$SHELL -l` when `cmd` is empty.
    pub fn spawn(cmd: Option<&str>, rows: u16, cols: u16, env: &[(&str, &str)], cwd: Option<&Path>) -> io::Result<Pty> {
        let (mut m, mut s) = (0, 0);
        let mut ws = winsize(rows, cols);
        check(unsafe { libc::openpty(&mut m, &mut s, std::ptr::null_mut(), std::ptr::null_mut(), &raw mut ws) })?;
        let (master, slave) = unsafe { (OwnedFd::from_raw_fd(m), OwnedFd::from_raw_fd(s)) };
        // the master must not be inherited by pane processes (an agent must not hold another pane's PTY)
        cloexec(m)?;
        cloexec(s)?;

        let mut c = Command::new(login_shell());
        c.arg("-l");
        if let Some(cmd) = cmd {
            c.arg("-c").arg(cmd);
        }
        scrub_env(&mut c);
        // an app from Finder/Dock inherits cwd `/` from launchd; without a folder from the profile/pane, start in home
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
        // setsid + make the slave the controlling terminal, so Ctrl+C/job control work
        unsafe {
            c.pre_exec(|| {
                check(libc::setsid())?;
                check(libc::ioctl(0, libc::TIOCSCTTY as _, 0))
            });
        }
        let child = c.spawn()?;
        drop(c); // close the daemon's copy of the slave; without this reading the master never hits EOF
        Ok(Pty { master: File::from(master), child })
    }
}

pub fn resize(master: &File, rows: u16, cols: u16) -> io::Result<()> {
    let ws = winsize(rows, cols);
    check(unsafe { libc::ioctl(master.as_raw_fd(), libc::TIOCSWINSZ as _, &ws) })
}

/// Process running in the pane (FR-38, FR-50): `(pid, group)`. The foreground job when the shell
/// uses job control; the shell's direct child when it doesn't (`$SHELL -l -c`). `None` = shell idle.
pub fn busy(master: &File, shell: i32) -> Option<(i32, bool)> {
    let fg = unsafe { libc::tcgetpgrp(master.as_raw_fd()) };
    if fg > 0 && fg != shell {
        return Some((fg, true));
    }
    children(shell).first().map(|&c| (c, false))
}

/// FR-40: terminal waiting for a password = canonical (line) mode but echo off, the `sudo`/`read -s` pattern.
/// Agent TUIs use raw (non-canonical) mode, so they aren't detected.
pub fn wants_password(master: &File) -> bool {
    let mut t: libc::termios = unsafe { std::mem::zeroed() };
    if unsafe { libc::tcgetattr(master.as_raw_fd(), &mut t) } != 0 {
        return false;
    }
    t.c_lflag & libc::ICANON != 0 && t.c_lflag & libc::ECHO == 0
}

/// Signal to the `busy` process; `0` = check it's still alive.
pub fn signal((pid, group): (i32, bool), sig: libc::c_int) -> bool {
    unsafe { if group { libc::killpg(pid, sig) == 0 } else { libc::kill(pid, sig) == 0 } }
}

#[cfg(target_os = "macos")]
pub fn proc_name(pid: i32) -> Option<String> {
    proc_path(pid)?.file_name()?.to_str().map(bare)
}

/// OpenCode from npm (`opencode-ai`) runs as `opencode.exe` on macOS too (9 Nov, another user's Mac: the pane wasn't an
/// agent, `@@` found nobody); the name without `.exe` is what agent detection and the tab title compare
fn bare(name: &str) -> String {
    name.strip_suffix(".exe").unwrap_or(name).to_string()
}

/// Binary path of a process (secret handover: the other side must be the right Gembala binary).
#[cfg(target_os = "macos")]
pub fn proc_path(pid: i32) -> Option<std::path::PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    let mut buf = [0u8; 4 * libc::MAXPATHLEN as usize];
    let n = unsafe { libc::proc_pidpath(pid, buf.as_mut_ptr().cast(), buf.len() as u32) };
    Some(std::ffi::OsStr::from_bytes(&buf[..usize::try_from(n).ok().filter(|&n| n > 0)?]).into())
}

/// Working folder of a process (FR-30: a new pane follows the focused pane's folder).
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

/// Proses induk (SEC-3: penelusuran peer PID sampai shell pane).
#[cfg(target_os = "macos")]
pub fn ppid(pid: i32) -> Option<i32> {
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
    let n = unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDTBSDINFO, 0, (&raw mut info).cast(), size) };
    (n == size).then_some(info.pbi_ppid as i32)
}

#[cfg(target_os = "linux")]
pub fn ppid(pid: i32) -> Option<i32> {
    let s = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    s.rsplit_once(')')?.1.split_whitespace().nth(1)?.parse().ok()
}

#[cfg(target_os = "linux")]
pub fn cwd_of(pid: i32) -> Option<std::path::PathBuf> {
    std::fs::read_link(format!("/proc/{pid}/cwd")).ok()
}

/// Process arguments (SEC-5: skip-permission flags on an agent typed by hand in a shell pane).
#[cfg(target_os = "macos")]
pub fn args_of(pid: i32) -> Vec<String> {
    procargs(pid).0
}

/// One env variable of a process (FR-61: `CLAUDE_CONFIG_DIR` of a typed agent = which account). Nothing else is kept.
#[cfg(target_os = "macos")]
pub fn env_of(pid: i32, key: &str) -> Option<String> {
    procargs(pid).1.into_iter().find_map(|kv| kv.strip_prefix(key)?.strip_prefix('=').map(String::from))
}

/// (argv, env) from KERN_PROCARGS2
#[cfg(target_os = "macos")]
fn procargs(pid: i32) -> (Vec<String>, Vec<String>) {
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid];
    let mut buf = vec![0u8; 64 * 1024];
    let mut len = buf.len();
    let rc = unsafe { libc::sysctl(mib.as_mut_ptr(), 3, buf.as_mut_ptr().cast(), &mut len, std::ptr::null_mut(), 0) };
    if rc != 0 || len < 4 {
        return (vec![], vec![]);
    }
    // layout: argc (i32), exec path, NUL padding, then argv[0..argc], then env
    let argc = i32::from_ne_bytes([buf[0], buf[1], buf[2], buf[3]]).max(0) as usize;
    let rest = &buf[4..len];
    let mut parts = rest.split(|&b| b == 0).filter(|p| !p.is_empty()).skip(1); // lewati path exec
    let s = |p: &[u8]| String::from_utf8_lossy(p).into_owned();
    let argv = (0..argc).filter_map(|_| parts.next()).map(s).collect();
    (argv, parts.map(s).collect())
}

#[cfg(target_os = "linux")]
pub fn args_of(pid: i32) -> Vec<String> {
    std::fs::read(format!("/proc/{pid}/cmdline")).unwrap_or_default().split(|&b| b == 0).filter(|p| !p.is_empty()).map(|p| String::from_utf8_lossy(p).into_owned()).collect()
}

#[cfg(target_os = "linux")]
pub fn env_of(pid: i32, key: &str) -> Option<String> {
    let env = std::fs::read(format!("/proc/{pid}/environ")).ok()?;
    env.split(|&b| b == 0).find_map(|kv| Some(String::from_utf8_lossy(kv.strip_prefix(key.as_bytes())?.strip_prefix(b"=")?).into_owned()))
}

#[cfg(target_os = "macos")]
fn children(pid: i32) -> Vec<i32> {
    let mut buf = [0 as libc::pid_t; 64];
    let size = std::mem::size_of_val(&buf) as libc::c_int;
    let n = unsafe { libc::proc_listchildpids(pid, buf.as_mut_ptr().cast(), size) };
    buf[..n.clamp(0, 64) as usize].to_vec()
}

#[cfg(target_os = "linux")]
pub fn proc_path(pid: i32) -> Option<std::path::PathBuf> {
    std::fs::read_link(format!("/proc/{pid}/exe")).ok()
}

#[cfg(target_os = "linux")]
pub fn proc_name(pid: i32) -> Option<String> {
    std::fs::read_to_string(format!("/proc/{pid}/comm")).ok().map(|s| bare(s.trim()))
}

#[cfg(target_os = "linux")]
fn children(pid: i32) -> Vec<i32> {
    let s = std::fs::read_to_string(format!("/proc/{pid}/task/{pid}/children")).unwrap_or_default();
    s.split_whitespace().filter_map(|p| p.parse().ok()).collect()
}

/// Read the master until the pane closes. macOS/Linux give EIO, not EOF, once the slave is closed.
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

    #[test]
    fn nama_proses_tanpa_exe() {
        assert_eq!(bare("opencode.exe"), "opencode");
        assert_eq!(bare("opencode"), "opencode");
        assert_eq!(bare("omp"), "omp");
    }

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
    fn argumen_proses_terbaca() {
        let mut c = std::process::Command::new("perl").args(["-e", "sleep 5", "--", "--yolo"]).spawn().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(200));
        let args = args_of(c.id() as i32);
        let _ = c.kill();
        let _ = c.wait();
        assert_eq!(args, ["perl", "-e", "sleep 5", "--", "--yolo"]);
    }

    #[test]
    fn punya_tty_pengendali() {
        // /dev/tty can only be opened when setsid + TIOCSCTTY succeeded
        let out = run(Pty::spawn(Some("tty; echo x > /dev/tty"), 24, 80, &[], None).unwrap());
        assert!(!out.contains("Device not configured"), "{out:?}");
        assert!(out.contains("/dev/ttys") || out.contains("/dev/pts/"), "{out:?}");
    }

    /// called by `env_of_membaca_folder_akun` as a child process
    #[test]
    #[ignore]
    fn tidur() {
        std::thread::sleep(std::time::Duration::from_secs(5));
    }

    #[test]
    fn env_of_membaca_folder_akun() {
        // macOS hides the env of Apple binaries (e.g. /bin/sleep); agents (claude, node) aren't Apple binaries
        let exe = std::env::current_exe().unwrap();
        let mut c = std::process::Command::new(exe).args(["--ignored", "--exact", "pty::tests::tidur", "-q"]).env("CLAUDE_CONFIG_DIR", "/rumah/.claude-kantor")
            .stdout(std::process::Stdio::null()).spawn().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(300));
        let pid = c.id() as i32;
        let (dir, none) = (env_of(pid, "CLAUDE_CONFIG_DIR"), env_of(pid, "CLAUDE_CONFIG"));
        let _ = c.kill();
        let _ = c.wait();
        assert_eq!(dir.as_deref(), Some("/rumah/.claude-kantor"));
        assert_eq!(none, None);
    }
}
