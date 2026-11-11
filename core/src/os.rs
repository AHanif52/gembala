//! Small per-OS differences (TECH 21.1): the commands and folders whose name changes between macOS and Linux.
//! Everything else lives in the shared code; a caller here must not need an `if cfg!` of its own.

use std::path::PathBuf;

/// System `curl`: the macOS one; on Linux from PATH (Debian/Ubuntu package it, a newer one may sit elsewhere).
pub fn curl() -> &'static str {
    if cfg!(target_os = "macos") { "/usr/bin/curl" } else { "curl" }
}

/// Shell command printing a stored secret (FR-33, SEC-32: the pane shell reads it, the daemon never sees the value).
/// macOS Keychain through `security`; Linux Secret Service through `secret-tool` (same service/account words).
/// `service`/`account` come from `profile::ok_ref`, which allows no quote.
pub fn secret_cmd(service: &str, account: &str) -> String {
    if cfg!(target_os = "macos") {
        format!("security find-generic-password -w -s '{service}' -a '{account}'")
    } else {
        format!("secret-tool lookup service '{service}' account '{account}'")
    }
}

/// Config folder: `~/Library/Application Support/Gembala` on macOS, `$XDG_CONFIG_HOME/gembala` (`~/.config/gembala`)
/// on Linux. `None` without a home folder.
pub fn config_dir() -> Option<PathBuf> {
    if cfg!(target_os = "macos") {
        let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| "/".into());
        return Some(home.join("Library/Application Support/Gembala"));
    }
    if let Some(x) = std::env::var_os("XDG_CONFIG_HOME").filter(|x| !x.is_empty()) {
        return Some(PathBuf::from(x).join("gembala"));
    }
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config/gembala"))
}

/// Run folder: `$TMPDIR/gembala` on macOS (already per user); `$XDG_RUNTIME_DIR/gembala` on Linux, `$TMPDIR/gembala`
/// as the fallback when the session has no runtime folder. `ensure_private_dir` still checks the owner and mode.
pub fn runtime_dir() -> PathBuf {
    if cfg!(target_os = "macos") {
        return std::env::var_os("TMPDIR").map(PathBuf::from).unwrap_or_else(|| "/tmp".into()).join("gembala");
    }
    if let Some(x) = std::env::var_os("XDG_RUNTIME_DIR").filter(|x| !x.is_empty()) {
        return PathBuf::from(x).join("gembala");
    }
    std::env::var_os("TMPDIR").map(PathBuf::from).unwrap_or_else(|| "/tmp".into()).join("gembala")
}
