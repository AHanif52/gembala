//! FR-70: titik kembali per giliran agent. Isi folder kerja dipotret ke repo git terpisah milik Gembala
//! (`<folder config>/snap/<kunci>.git`, work-tree = repo pengguna). Index, HEAD, branch, dan stash pengguna tidak
//! tersentuh; daftar file diambil dari repo pengguna (baca saja), jadi aturan ignore pengguna ikut.

use serde_json::{Value, json};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock, mpsc};
use std::time::Duration;

const BIG: u64 = 100 << 20;
const KEEP_DAYS: u64 = 7;
const MAX_DIFF: usize = 200 * 1024;
pub const MAX_TURNS: usize = 50;

/// Giliran yang sedang jalan
pub struct Open {
    pub start: u64,
    pub root: String,
    pub before: String,
    pub head: String,
}

#[derive(Clone)]
pub struct Turn {
    pub start: u64,
    pub end: u64,
    pub root: String,
    pub before: String,
    pub after: String,
    pub head: String,
    pub title: String,
    pub skipped: Vec<String>,
}

#[derive(Default)]
pub struct Turns {
    pub open: Option<Open>,
    pub done: Vec<Turn>,
}

static LOCK: Mutex<()> = Mutex::new(());

fn dir() -> PathBuf {
    crate::config::path().with_file_name("snap")
}

fn gitdir(root: &str) -> PathBuf {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    root.hash(&mut h);
    dir().join(format!("{:016x}.git", h.finish()))
}

fn sgit(root: &str, args: &[&str], limit: Duration) -> Option<String> {
    let mut c = Command::new("git");
    // repo potretan milik Gembala: hook, fsmonitor, dan gc otomatis mati; path apa adanya (bukan pola)
    c.args(["-c", "core.fsmonitor=false", "-c", "core.hooksPath=/dev/null", "-c", "gc.auto=0", "-c", "core.autocrlf=false", "--literal-pathspecs"])
        .arg(format!("--git-dir={}", gitdir(root).display()))
        .arg(format!("--work-tree={root}"))
        .args(args);
    c.current_dir(root).env("GIT_TERMINAL_PROMPT", "0").stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
    crate::pty::scrub_env(&mut c);
    crate::profile::run_with_timeout(c, limit)
}

fn init(root: &str) -> Option<()> {
    use std::os::unix::fs::DirBuilderExt;
    let gd = gitdir(root);
    if gd.join("HEAD").exists() {
        return Some(());
    }
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir()).ok()?;
    let ok = Command::new("git").args(["init", "-q", "--bare"]).arg(&gd).stdout(Stdio::null()).stderr(Stdio::null()).status().ok()?.success();
    let ok = ok && Command::new("git").arg(format!("--git-dir={}", gd.display())).args(["config", "core.bare", "false"]).status().ok()?.success();
    ok.then_some(())
}

/// Root repo git `cwd`, atau None bila bukan repo / repo dengan filter lokal (SEC-29)
pub fn root_of(cwd: &Path) -> Option<String> {
    let root = crate::git::git(cwd, &["rev-parse", "--show-toplevel"]).map(|s| s.trim().to_string()).filter(|s| !s.is_empty())?;
    (!crate::git::repo_runs_filters(cwd)).then_some(root)
}

pub fn head(root: &str) -> String {
    crate::git::git(Path::new(root), &["rev-parse", "HEAD"]).map(|s| s.trim().to_string()).unwrap_or_default()
}

fn write_list(name: &str, root: &str, paths: &[String]) -> Option<PathBuf> {
    let f = gitdir(root).join(name);
    std::fs::write(&f, paths.join("\0")).ok()?;
    Some(f)
}

/// Potret folder kerja: (tree, file besar yang dilewati)
pub fn take(root: &str) -> Option<(String, Vec<String>)> {
    let _g = LOCK.lock().unwrap();
    init(root)?;
    let list = crate::git::git_for(Path::new(root), &["ls-files", "-co", "--exclude-standard", "-z"], Duration::from_secs(30))?;
    let (mut keep, mut skipped) = (Vec::new(), Vec::new());
    for p in list.split('\0').filter(|p| !p.is_empty()) {
        match std::fs::symlink_metadata(Path::new(root).join(p)) {
            Ok(m) if m.len() >= BIG && m.is_file() => skipped.push(p.to_string()),
            Ok(_) => keep.push(p.to_string()),
            Err(_) => {} // file ter-track yang sudah dihapus
        }
    }
    keep.sort();
    keep.dedup();
    let set: HashSet<&String> = keep.iter().collect();
    let stale: Vec<String> = sgit(root, &["ls-files", "-z"], Duration::from_secs(30)).unwrap_or_default().split('\0').filter(|p| !p.is_empty() && !set.contains(&p.to_string())).map(String::from).collect();
    if !stale.is_empty() {
        let f = write_list("stale", root, &stale)?;
        sgit(root, &["rm", "--cached", "-q", "--ignore-unmatch", &format!("--pathspec-from-file={}", f.display()), "--pathspec-file-nul"], Duration::from_secs(60));
    }
    if !keep.is_empty() {
        let f = write_list("keep", root, &keep)?;
        sgit(root, &["add", "-f", "--ignore-errors", &format!("--pathspec-from-file={}", f.display()), "--pathspec-file-nul"], Duration::from_secs(180));
    }
    let tree = sgit(root, &["write-tree"], Duration::from_secs(30))?.trim().to_string();
    (tree.len() >= 40).then_some((tree, skipped))
}

/// (file, baris tambah, baris hapus) antara dua potretan
pub fn stat(t: &Turn) -> (u64, u64, u64) {
    let (mut f, mut a, mut d) = (0, 0, 0);
    for l in sgit(&t.root, &["diff", "--numstat", "--no-renames", &t.before, &t.after], Duration::from_secs(10)).unwrap_or_default().lines() {
        let mut it = l.split('\t');
        a += it.next().and_then(|n| n.parse::<u64>().ok()).unwrap_or(0);
        d += it.next().and_then(|n| n.parse::<u64>().ok()).unwrap_or(0);
        f += 1;
    }
    (f, a, d)
}

/// Diff satu giliran, bentuk sama dengan `git::diff` (panel perubahan FR-22)
pub fn diff(t: &Turn) -> Value {
    let mut text = sgit(&t.root, &["diff", "--no-ext-diff", "--no-textconv", "--no-renames", &t.before, &t.after], Duration::from_secs(10)).unwrap_or_default();
    let truncated = text.len() > MAX_DIFF;
    if truncated {
        let mut cut = MAX_DIFF;
        while !text.is_char_boundary(cut) {
            cut -= 1;
        }
        text.truncate(cut);
    }
    json!({"repo": true, "diff": crate::git::redact(&text), "truncated": truncated, "untracked": []})
}

/// File yang berubah bila folder kerja dikembalikan ke `before`: (`ubah` | `hapus`, path)
pub fn preview(root: &str, before: &str) -> Option<Vec<(&'static str, String)>> {
    let (now, _) = take(root)?;
    let out = sgit(root, &["diff", "--name-status", "--no-renames", "-z", before, &now], Duration::from_secs(10))?;
    let mut it = out.split('\0').filter(|s| !s.is_empty());
    let mut files = Vec::new();
    while let (Some(st), Some(p)) = (it.next(), it.next()) {
        // A = file baru sesudah titik itu: dihapus; M/D/T = isi lama ditulis lagi
        files.push((if st == "A" { "hapus" } else { "ubah" }, p.to_string()));
    }
    Some(files)
}

/// Kembalikan folder kerja ke `before` untuk `files` dari `preview`. File yang di-ignore dan yang dilewati tidak disentuh.
pub fn restore(root: &str, before: &str, files: &[(&str, String)]) -> Option<usize> {
    let _g = LOCK.lock().unwrap();
    let back: Vec<String> = files.iter().filter(|f| f.0 == "ubah").map(|f| f.1.clone()).collect();
    if !back.is_empty() {
        let f = write_list("restore", root, &back)?;
        sgit(root, &["checkout", before, &format!("--pathspec-from-file={}", f.display()), "--pathspec-file-nul"], Duration::from_secs(120))?;
    }
    let base = Path::new(root);
    for (_, p) in files.iter().filter(|f| f.0 == "hapus") {
        if Path::new(p).components().all(|c| matches!(c, std::path::Component::Normal(_))) {
            let _ = std::fs::remove_file(base.join(p));
        }
    }
    Some(files.len())
}

/// Giliran selesai = potret sesudah
pub fn finish(o: Open, end: u64, title: String) -> Option<Turn> {
    let (after, skipped) = take(&o.root)?;
    Some(Turn { start: o.start, end, root: o.root, before: o.before, after, head: o.head, title, skipped })
}

/// Satu antrean untuk semua potretan: urutan mulai/selesai giliran terjaga, hook agent tidak menunggu git
pub fn queue(job: impl FnOnce() + Send + 'static) {
    type Job = Box<dyn FnOnce() + Send>;
    static Q: OnceLock<Mutex<mpsc::Sender<Job>>> = OnceLock::new();
    let q = Q.get_or_init(|| {
        let (tx, rx) = mpsc::channel::<Job>();
        std::thread::spawn(move || rx.into_iter().for_each(|j| j()));
        Mutex::new(tx)
    });
    let _ = q.lock().unwrap().send(Box::new(job));
}

/// Repo potretan yang tidak dipakai 7 hari dihapus
pub fn sweep() {
    let Ok(entries) = std::fs::read_dir(dir()) else { return };
    let limit = Duration::from_secs(KEEP_DAYS * 86400);
    for e in entries.flatten() {
        let used = ["index", "HEAD"].iter().filter_map(|f| std::fs::metadata(e.path().join(f)).and_then(|m| m.modified()).ok()).max();
        if used.is_some_and(|t| t.elapsed().is_ok_and(|d| d > limit)) {
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn giliran_dipotret_dan_dikembalikan() {
        let base = std::env::temp_dir().join(format!("gembala-snap-{}", std::process::id()));
        let repo = base.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        // SAFETY: test lain yang memakai GEMBALA_CONFIG menulis nilai yang sama (folder uji per proses)
        unsafe { std::env::set_var("GEMBALA_CONFIG", std::env::temp_dir().join(format!("gembala-uji-{}/config.toml", std::process::id()))) };
        let sh = |args: &[&str]| assert!(Command::new("git").arg("-C").arg(&repo).args(args).output().unwrap().status.success(), "{args:?}");
        sh(&["init", "-q"]);
        std::fs::write(repo.join(".gitignore"), "rahasia.txt\n").unwrap();
        std::fs::write(repo.join("a.txt"), "satu\n").unwrap();
        sh(&["add", "."]);
        sh(&["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "a"]);
        let root = root_of(&repo).unwrap();
        let head0 = head(&root);
        let open = Open { start: 1, root: root.clone(), before: take(&root).unwrap().0, head: head0.clone() };
        // giliran agent: ubah a.txt, buat b.txt, file ignore
        std::fs::write(repo.join("a.txt"), "dua\n").unwrap();
        std::fs::write(repo.join("b.txt"), "baru\n").unwrap();
        std::fs::write(repo.join("rahasia.txt"), "x").unwrap();
        let turn = &finish(open, 2, "uji".into()).unwrap();
        assert_eq!(stat(turn), (2, 2, 1));
        assert!(diff(turn)["diff"].as_str().unwrap().contains("+dua"));
        let files = preview(&root, &turn.before).unwrap();
        assert!(files.contains(&("ubah", "a.txt".into())) && files.contains(&("hapus", "b.txt".into())) && files.len() == 2, "{files:?}");
        restore(&root, &turn.before, &files).unwrap();
        assert_eq!(std::fs::read_to_string(repo.join("a.txt")).unwrap(), "satu\n");
        assert!(!repo.join("b.txt").exists() && repo.join("rahasia.txt").exists());
        // index dan HEAD pengguna tidak tersentuh
        assert_eq!(head(&root), head0);
        let st = Command::new("git").arg("-C").arg(&repo).args(["status", "--porcelain"]).output().unwrap().stdout;
        assert!(String::from_utf8_lossy(&st).trim().is_empty(), "{}", String::from_utf8_lossy(&st));
        let _ = std::fs::remove_dir_all(base);
    }
}
