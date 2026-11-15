//! SEC-20: the UI channel is recognized by the peer's code signature, no secret in the Keychain. Keychain items of an app
//! with a self-signed certificate (no TeamIdentifier) are tied to the build's cdhash, so every new build asks again.
//! A peer passes when its process (audit token at connect, immune to exec and PID reuse) satisfies
//! `identifier "dev.gembala.app" and certificate leaf = <the daemon's own certificate>`. A daemon without a certificate
//! (dev build, tests) = `None`: secret path only.

#[cfg(target_os = "macos")]
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;

/// Requirement from the signature of this daemon binary (sidecar in the same app bundle as the UI).
#[cfg(target_os = "macos")]
pub fn ui_requirement() -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    let out = std::process::Command::new("/usr/bin/codesign").args(["-d", "-r-"]).arg(exe).output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let leaf = regex_lite::Regex::new(r#"certificate leaf = H"([0-9a-fA-F]{40})""#).ok()?.captures(&text)?.get(1)?.as_str().to_string();
    Some(format!(r#"identifier "dev.gembala.app" and certificate leaf = H"{leaf}""#))
}

/// SEC-41: the signing key alone doesn't make a UI (a process that can use the self-signed key can sign any binary with
/// the identifier), so the peer must also be the very UI binary installed next to this daemon: its cdhash is read from
/// the file on disk at each connect, so a UI updated by `pasang.sh` while this daemon lives still passes.
/// Daemon: `X.app/Contents/Helpers/Gembala Daemon.app/Contents/MacOS/gembala`; UI: `X.app/Contents/MacOS/gembala-ui`.
#[cfg(target_os = "macos")]
pub fn ui_pinned(s: &UnixStream) -> bool {
    let Ok(exe) = std::env::current_exe() else { return false };
    let Some(ui) = exe.ancestors().nth(5).map(|c| c.join("MacOS/gembala-ui")) else { return false };
    cdhash_requirement(&ui).is_some_and(|r| peer_ok(s, &r))
}

#[cfg(not(target_os = "macos"))]
pub fn ui_pinned(_s: &UnixStream) -> bool {
    false
}

/// `cdhash H"…"` of the code at `path` (`codesign -d -vvv` prints `CDHash=<hex>` on stderr).
#[cfg(target_os = "macos")]
pub fn cdhash_requirement(path: &std::path::Path) -> Option<String> {
    let out = std::process::Command::new("/usr/bin/codesign").args(["-d", "-vvv"]).arg(path).output().ok()?;
    let text = String::from_utf8_lossy(&out.stderr);
    let hash = text.lines().find_map(|l| l.strip_prefix("CDHash="))?.trim();
    (hash.len() >= 40 && hash.bytes().all(|b| b.is_ascii_hexdigit())).then(|| format!(r#"cdhash H"{hash}""#))
}

/// Peer audit token at connect (`LOCAL_PEERTOKEN`).
#[cfg(target_os = "macos")]
pub fn peer_ok(s: &UnixStream, requirement: &str) -> bool {
    use core_foundation::base::TCFType;
    use core_foundation::data::CFData;
    use security_framework::os::macos::code_signing::{Flags, GuestAttributes, SecCode, SecRequirement};
    let mut token = [0u8; 32];
    let mut len = token.len() as libc::socklen_t;
    let rc = unsafe { libc::getsockopt(s.as_raw_fd(), libc::SOL_LOCAL, libc::LOCAL_PEERTOKEN, token.as_mut_ptr().cast(), &mut len) };
    if rc != 0 || len as usize != token.len() {
        return false;
    }
    let data = CFData::from_buffer(&token);
    let mut attrs = GuestAttributes::new();
    attrs.set_audit_token(data.as_concrete_TypeRef());
    let Ok(req) = requirement.parse::<SecRequirement>() else { return false };
    SecCode::copy_guest_with_attribues(None, &attrs, Flags::NONE).is_ok_and(|code| code.check_validity(Flags::NONE, &req).is_ok())
}

/// Linux has no code identity here either, and a binary path proves nothing (`setsid env LD_PRELOAD=… /usr/bin/gembala-ui`
/// matches it): the secret is the proof on both of its UI-channel paths — the abstract socket, which a pane's Landlock
/// domain can't cross, and the path socket on a kernel without that scope — plus `ipc::peer_tainted` (TECH 21.8).
/// Other systems: secret only.
#[cfg(not(target_os = "macos"))]
pub fn ui_requirement() -> Option<String> {
    None
}

#[cfg(not(target_os = "macos"))]
pub fn peer_ok(_s: &UnixStream, _requirement: &str) -> bool {
    false
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    /// A test process (no Gembala certificate) must not pass; a broken requirement is rejected too.
    #[test]
    fn peer_tanpa_tanda_tangan_ditolak() {
        let (a, _b) = UnixStream::pair().unwrap();
        assert!(!peer_ok(&a, r#"identifier "dev.gembala.app" and certificate leaf = H"0000000000000000000000000000000000000000""#));
        assert!(!peer_ok(&a, "bukan requirement"));
        // the test binary isn't signed with the certificate: no requirement, the daemon only uses the secret
        assert_eq!(ui_requirement(), None);
        assert!(!ui_pinned(&a));
    }

    /// The pin is the cdhash of the binary: the process running that binary passes, any other cdhash doesn't.
    #[test]
    fn peer_dengan_cdhash_biner_sendiri_lolos() {
        let (a, _b) = UnixStream::pair().unwrap();
        let req = cdhash_requirement(&std::env::current_exe().unwrap()).expect("test binary carries a cdhash (linker ad-hoc signature)");
        assert!(peer_ok(&a, &req), "{req}");
        assert!(!peer_ok(&a, r#"cdhash H"0000000000000000000000000000000000000000""#));
        assert_eq!(cdhash_requirement(std::path::Path::new("/nonexistent")), None);
    }
}
