//! Folder socket privat (BRD SEC-1, SEC-25).

use std::fs::{self, DirBuilder};
use std::io::{self, ErrorKind};
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::path::{Path, PathBuf};

/// `sun_path` macOS 104 byte termasuk NUL.
const SUN_PATH_MAX: usize = 103;

/// `$TMPDIR/gembala`. macOS TMPDIR is already per user; the `/tmp` fallback is still safe
/// because `ensure_private_dir` rejects folders owned by someone else.
pub fn default_dir() -> PathBuf {
    std::env::var_os("TMPDIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("gembala")
}

/// Create the `0700` folder if missing, then reject it if it's a symlink, not a folder,
/// not owned by this UID, or accessible to group/others.
pub fn ensure_private_dir(dir: &Path) -> io::Result<()> {
    match fs::symlink_metadata(dir) {
        Err(e) if e.kind() == ErrorKind::NotFound => DirBuilder::new().mode(0o700).create(dir)?,
        Err(e) => return Err(e),
        Ok(_) => {}
    }
    let m = fs::symlink_metadata(dir)?;
    let deny = |why: &str| Err(io::Error::new(ErrorKind::PermissionDenied, format!("{}: {why}", dir.display())));
    if m.file_type().is_symlink() || !m.is_dir() {
        return deny("bukan folder biasa (symlink?)");
    }
    if m.uid() != unsafe { libc::getuid() } {
        return deny("bukan milik pengguna ini");
    }
    if m.mode() & 0o077 != 0 {
        return deny("izin harus 0700");
    }
    Ok(())
}

pub fn socket_path(dir: &Path, name: &str) -> io::Result<PathBuf> {
    let p = dir.join(name);
    if p.as_os_str().len() > SUN_PATH_MAX {
        return Err(io::Error::new(ErrorKind::InvalidInput, format!("path socket > {SUN_PATH_MAX} byte: {}", p.display())));
    }
    Ok(p)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{symlink, PermissionsExt};

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("gembala-test-{}-{tag}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir(&d).unwrap();
        d
    }

    #[test]
    fn creates_0700() {
        let d = scratch("new").join("run");
        ensure_private_dir(&d).unwrap();
        assert_eq!(fs::metadata(&d).unwrap().mode() & 0o777, 0o700);
        ensure_private_dir(&d).unwrap(); // idempoten
    }

    #[test]
    fn rejects_loose_mode() {
        let d = scratch("loose").join("run");
        fs::create_dir(&d).unwrap();
        fs::set_permissions(&d, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(ensure_private_dir(&d).unwrap_err().kind(), ErrorKind::PermissionDenied);
    }

    #[test]
    fn rejects_symlink() {
        let base = scratch("link");
        let real = base.join("real");
        DirBuilder::new().mode(0o700).create(&real).unwrap();
        let link = base.join("run");
        symlink(&real, &link).unwrap();
        assert_eq!(ensure_private_dir(&link).unwrap_err().kind(), ErrorKind::PermissionDenied);
    }

    #[test]
    fn rejects_long_socket_path() {
        let long = PathBuf::from("/").join("a".repeat(120));
        assert!(socket_path(&long, "pane.sock").is_err());
        assert!(socket_path(Path::new("/tmp/gembala"), "pane.sock").is_ok());
    }
}
