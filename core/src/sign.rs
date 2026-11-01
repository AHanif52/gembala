//! SEC-20: kanal UI dikenali dari tanda tangan kode peer, tanpa rahasia di Keychain. Item Keychain milik app
//! bersertifikat self-signed (tanpa TeamIdentifier) terikat ke cdhash build, jadi tiap build baru ditanya ulang.
//! Peer lolos bila prosesnya (audit token saat connect, kebal exec dan PID dipakai ulang) memenuhi
//! `identifier "dev.gembala.app" and certificate leaf = <sertifikat daemon sendiri>`. Daemon tanpa sertifikat
//! (build dev, uji) = `None`: hanya jalur rahasia.

use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;

/// Requirement dari tanda tangan binary daemon ini sendiri (sidecar di bundle app yang sama dengan UI).
#[cfg(target_os = "macos")]
pub fn ui_requirement() -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    let out = std::process::Command::new("/usr/bin/codesign").args(["-d", "-r-"]).arg(exe).output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let leaf = regex_lite::Regex::new(r#"certificate leaf = H"([0-9a-fA-F]{40})""#).ok()?.captures(&text)?.get(1)?.as_str().to_string();
    Some(format!(r#"identifier "dev.gembala.app" and certificate leaf = H"{leaf}""#))
}

/// Audit token peer saat connect (`LOCAL_PEERTOKEN`).
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

#[cfg(not(target_os = "macos"))]
pub fn ui_requirement() -> Option<String> {
    None // ponytail: Linux belum; kanal UI tetap memakai rahasia
}

#[cfg(not(target_os = "macos"))]
pub fn peer_ok(_s: &UnixStream, _requirement: &str) -> bool {
    false
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    /// Proses uji (tanpa sertifikat Gembala) tidak boleh lolos; requirement rusak juga ditolak.
    #[test]
    fn peer_tanpa_tanda_tangan_ditolak() {
        let (a, _b) = UnixStream::pair().unwrap();
        assert!(!peer_ok(&a, r#"identifier "dev.gembala.app" and certificate leaf = H"0000000000000000000000000000000000000000""#));
        assert!(!peer_ok(&a, "bukan requirement"));
        // binary uji tidak bertanda tangan sertifikat: tidak ada requirement, daemon hanya memakai rahasia
        assert_eq!(ui_requirement(), None);
    }
}
