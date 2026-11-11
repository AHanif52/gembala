//! FR-98 / SEC-20: `gembala jail -- <command…>` runs an agent inside a Landlock domain so a script in the agent's pane
//! can't reach the UI channel (an abstract socket, `ipc::bind_abstract`) and can't signal processes outside the domain.
//! Linux 6.12+ (Landlock ABI 6); on older kernels and on other systems the command runs unchanged — there the UI channel
//! is the path socket + secret + `ipc::peer_tainted` (TECH 21.8 lapis 2).
//!
//! Fail closed: a kernel that can hold a domain (ABI ≥ 6) but where the domain doesn't go up must not run the agent
//! unguarded — the daemon already chose lapis 1 there (`scope_supported`). It prints the failing step and exits 126.
//!
//! Only agents are wrapped: a plain pane shell keeps `sudo` (`no_new_privs` would break it).

use std::process::ExitCode;

/// `--` then the command: the wrapper text in `profile.rs` is the same on every system.
pub fn main(args: &[String]) -> ExitCode {
    match args.split_first() {
        Some((sep, cmd)) if sep == "--" && !cmd.is_empty() => {
            #[cfg(target_os = "linux")]
            if let Err((step, errno)) = restrict() {
                // the wrapper's caller reads stderr; the agent must not start outside the domain
                eprintln!("gembala jail: {step}: {errno}");
                return ExitCode::from(126);
            }
            exec(cmd)
        }
        _ => {
            eprintln!("usage: gembala jail -- <command...>");
            ExitCode::from(2)
        }
    }
}

/// `execvp`: the command replaces this process, same argv and environment (no extra pid in the tree).
fn exec(argv: &[String]) -> ExitCode {
    use std::os::unix::process::CommandExt;
    let err = std::process::Command::new(&argv[0]).args(&argv[1..]).exec();
    eprintln!("gembala jail: {}: {err}", argv[0]);
    ExitCode::from(127)
}

/// Whether this kernel can hold a pane's agents (Landlock scope, ABI 6). The daemon uses it to pick the UI channel.
#[cfg(target_os = "linux")]
pub fn scope_supported() -> bool {
    abi() >= 6
}

#[cfg(not(target_os = "linux"))]
pub fn scope_supported() -> bool {
    false
}

#[cfg(target_os = "linux")]
const VERSION: libc::c_uint = 1;
#[cfg(target_os = "linux")]
const SCOPE_ABSTRACT_UNIX_SOCKET: u64 = 1 << 0;
#[cfg(target_os = "linux")]
const SCOPE_SIGNAL: u64 = 1 << 1;

/// v1 `handled_access_fs`, v4 `handled_access_net`, v6 `scoped`; size 24 is what ABI 6 reads.
#[cfg(target_os = "linux")]
#[repr(C)]
struct RulesetAttr {
    handled_access_fs: u64,
    handled_access_net: u64,
    scoped: u64,
}

/// Landlock ABI of this kernel; 0 when the syscall is missing (ENOSYS) or disabled.
#[cfg(target_os = "linux")]
fn abi() -> libc::c_long {
    let r = unsafe { libc::syscall(libc::SYS_landlock_create_ruleset, std::ptr::null::<u8>(), 0usize, VERSION) };
    if r < 0 { 0 } else { r }
}

/// No filesystem rules: the pane must still read the project, its dotfiles and the agent's own data. The scopes only take
/// away what a script would use to reach the UI: abstract sockets and signals to processes outside the domain.
#[cfg(target_os = "linux")]
fn restrict() -> Result<(), (&'static str, i32)> {
    let abi = abi();
    let mut errno = [0; 3]; // per `STEPS`, 0 = passed or never reached
    if abi >= 6 {
        errno[0] = status(libc::c_long::from(unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) }));
        if errno[0] == 0 {
            let attr = RulesetAttr { handled_access_fs: 0, handled_access_net: 0, scoped: SCOPE_ABSTRACT_UNIX_SOCKET | SCOPE_SIGNAL };
            let fd = unsafe { libc::syscall(libc::SYS_landlock_create_ruleset, &raw const attr, std::mem::size_of::<RulesetAttr>(), 0 as libc::c_uint) };
            errno[1] = status(fd);
            if errno[1] == 0 {
                // the domain is inherited by every descendant and can't be lifted from inside
                errno[2] = status(unsafe { libc::syscall(libc::SYS_landlock_restrict_self, fd, 0 as libc::c_uint) });
                unsafe { libc::close(fd as libc::c_int) };
            }
        }
    }
    match refusal(abi, errno) {
        None => Ok(()),
        Some(failure) => Err(failure),
    }
}

/// `0` when the call succeeded, else its errno.
#[cfg(target_os = "linux")]
fn status(rc: libc::c_long) -> i32 {
    if rc >= 0 { 0 } else { std::io::Error::last_os_error().raw_os_error().unwrap_or(-1) }
}

/// The three kernel steps of lapis 1, in order; the name of the failing one goes to stderr.
// Only `restrict` (Linux) and `tes_keputusan` reach these two; a macOS build has no jail to decide.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub const STEPS: [&str; 3] = ["PR_SET_NO_NEW_PRIVS", "landlock_create_ruleset", "landlock_restrict_self"];

/// SEC-20, pure so every failure is a table row: the Landlock ABI and each step's errno (`0` = passed or not reached) →
/// the step that must stop the exec, with its errno. `None` = run the command — inside the domain when ABI ≥ 6 and all
/// steps passed, or unguarded below ABI 6, where the kernel can't hold a domain and lapis 2 (secret + injection check) is
/// what guards it.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn refusal(abi: libc::c_long, errno: [i32; 3]) -> Option<(&'static str, i32)> {
    if abi < 6 {
        return None;
    }
    let i = errno.iter().position(|e| *e != 0)?;
    Some((STEPS[i], errno[i]))
}

#[cfg(test)]
mod tes_keputusan {
    use super::*;

    /// ABI < 6: the kernel can't hold a domain, lapis 2 guards it, and no step may stop the command.
    #[test]
    fn tanpa_domain_tetap_jalan() {
        assert_eq!(refusal(5, [0, 0, 0]), None);
        assert_eq!(refusal(5, [1, 95, 1]), None);
        assert_eq!(refusal(0, [38, 38, 38]), None); // ENOSYS everywhere
    }

    /// ABI ≥ 6 with every step through: the command runs inside the domain.
    #[test]
    fn domain_lengkap_jalan() {
        assert_eq!(refusal(6, [0, 0, 0]), None);
        assert_eq!(refusal(10, [0, 0, 0]), None);
    }

    /// ABI ≥ 6 and one step fails: no exec, the step and its errno are named. Fail closed, not "run without the jail".
    #[test]
    fn langkah_gagal_tidak_jalan() {
        assert_eq!(refusal(6, [1, 0, 0]), Some(("PR_SET_NO_NEW_PRIVS", 1))); // EPERM
        assert_eq!(refusal(6, [0, 95, 0]), Some(("landlock_create_ruleset", 95))); // EOPNOTSUPP
        assert_eq!(refusal(6, [0, 38, 0]), Some(("landlock_create_ruleset", 38))); // ENOSYS
        assert_eq!(refusal(6, [0, 0, 1]), Some(("landlock_restrict_self", 1))); // EPERM
        assert_eq!(refusal(7, [22, 1, 0]), Some(("PR_SET_NO_NEW_PRIVS", 22))); // the first failing step is reported
    }
}

