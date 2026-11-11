//! FR-98 / SEC-20: `gembala jail -- <command…>` runs an agent inside a Landlock domain so a script in the agent's pane
//! can't reach the UI channel (an abstract socket, `ipc::bind_abstract`) and can't signal processes outside the domain.
//! Linux 6.12+ (Landlock ABI 6); on older kernels and on other systems the command runs unchanged — there the UI channel
//! is the path socket + secret + `ipc::peer_tainted` (TECH 21.8 lapis 2).
//!
//! Only agents are wrapped: a plain pane shell keeps `sudo` (`no_new_privs` would break it).

use std::process::ExitCode;

/// `--` then the command: the wrapper text in `profile.rs` is the same on every system.
pub fn main(args: &[String]) -> ExitCode {
    match args.split_first() {
        Some((sep, cmd)) if sep == "--" && !cmd.is_empty() => {
            #[cfg(target_os = "linux")]
            restrict();
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
fn restrict() {
    if abi() < 6 {
        return;
    }
    let attr = RulesetAttr { handled_access_fs: 0, handled_access_net: 0, scoped: SCOPE_ABSTRACT_UNIX_SOCKET | SCOPE_SIGNAL };
    unsafe {
        if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {
            return;
        }
        let fd = libc::syscall(libc::SYS_landlock_create_ruleset, &raw const attr, std::mem::size_of::<RulesetAttr>(), 0 as libc::c_uint);
        if fd < 0 {
            return;
        }
        // the domain is inherited by every descendant and can't be lifted from inside
        libc::syscall(libc::SYS_landlock_restrict_self, fd, 0 as libc::c_uint);
        libc::close(fd as libc::c_int);
    }
}
