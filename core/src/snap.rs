//! FR-70: restore point per agent turn. The working folder is snapshotted into a separate git repo owned by Gembala
//! (`<config folder>/snap/<key>.git`, work-tree = the user's repo). The user's index, HEAD, branches, and stash stay
//! untouched; the file list comes from the user's repo (read only), so the user's ignore rules apply.

use serde_json::{Value, json};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock, mpsc};
use std::time::Duration;

const BIG: u64 = 25 << 20;
/// A repo over this after prune loses its oldest turns, then itself (restore points only, never user data)
const CAP: u64 = 512 << 20;
const KEEP_DAYS: u64 = 7;
const MAX_DIFF: usize = 200 * 1024;
pub const MAX_TURNS: usize = 50;

/// Turn in progress
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
    /// FR-99 P5: the agent's last message of the turn (hook text, secrets already masked, clipped); empty without one
    pub summary: String,
    pub skipped: Vec<String>,
}

#[derive(Default)]
pub struct Turns {
    /// The pane's profile turned restore points off
    pub off: bool,
    pub open: Option<Open>,
    pub done: Vec<Turn>,
}

static LOCK: Mutex<()> = Mutex::new(());
/// Trees held by a live turn: (folder, tree) = how many turns use it. The first holder writes `refs/turns/<tree>`, the last
/// one deletes it; prune only keeps what a ref (or the index) reaches. Two turns can share a tree (before of N+1 = after of N).
static PINS: Mutex<Vec<((String, String), usize)>> = Mutex::new(Vec::new());

impl Drop for Turns {
    fn drop(&mut self) {
        // the pane is gone: its turns no longer hold anything
        self.open.take().into_iter().for_each(|o| unpin(&o.root, &o.before));
        self.done.drain(..).for_each(|t| unpin_turn(&t));
    }
}

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
    // Gembala's snapshot repo: hooks, fsmonitor, and auto gc off; paths literal (not patterns)
    c.args(["-c", "core.fsmonitor=false", "-c", "core.hooksPath=/dev/null", "-c", "gc.auto=0", "-c", "core.autocrlf=false", "--literal-pathspecs"])
        .arg(format!("--git-dir={}", gitdir(root).display()))
        .arg(format!("--work-tree={root}"))
        .args(args);
    c.current_dir(root).env("GIT_TERMINAL_PROMPT", "0").stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
    crate::pty::scrub_env(&mut c);
    crate::profile::run_with_timeout(c, limit)
}

/// git in the snapshot repo itself (no work tree): refs, prune, repack
fn gd_git(gd: &Path, args: &[&str], limit: Duration) -> Option<String> {
    let mut c = Command::new("git");
    c.args(["-c", "core.hooksPath=/dev/null", "-c", "gc.auto=0"]).arg(format!("--git-dir={}", gd.display())).args(args);
    c.current_dir(gd).env("GIT_TERMINAL_PROMPT", "0").stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
    crate::pty::scrub_env(&mut c);
    crate::profile::run_with_timeout(c, limit)
}

/// A live turn holds this tree: it survives prune until the last holder lets go
pub fn pin(root: &str, tree: &str) {
    let mut p = PINS.lock().unwrap();
    let key = (root.to_string(), tree.to_string());
    match p.iter_mut().find(|(k, _)| *k == key) {
        Some((_, n)) => *n += 1,
        None => {
            p.push((key, 1));
            gd_git(&gitdir(root), &["update-ref", &format!("refs/turns/{tree}"), tree], Duration::from_secs(10));
        }
    }
}

pub fn unpin(root: &str, tree: &str) {
    let mut p = PINS.lock().unwrap();
    let key = (root.to_string(), tree.to_string());
    let Some(i) = p.iter().position(|(k, _)| *k == key) else { return };
    p[i].1 -= 1;
    if p[i].1 == 0 {
        p.swap_remove(i);
        gd_git(&gitdir(root), &["update-ref", "-d", &format!("refs/turns/{tree}")], Duration::from_secs(10));
    }
}

pub fn unpin_turn(t: &Turn) {
    unpin(&t.root, &t.before);
    unpin(&t.root, &t.after);
}

fn dir_size(p: &Path) -> u64 {
    std::fs::read_dir(p).into_iter().flatten().flatten().map(|e| e.metadata().map_or(0, |m| if m.is_dir() { dir_size(&e.path()) } else { m.len() })).sum()
}

/// Drop what no ref reaches and pack the rest into one pack. `prune` only touches loose objects; `repack -a -d` is what drops
/// unreachable objects from earlier packs (stored data is reused, not recompressed).
fn prune(gd: &Path) {
    gd_git(gd, &["prune", "--expire=now"], Duration::from_secs(300));
    gd_git(gd, &["-c", "pack.threads=1", "repack", "-a", "-d", "-q"], Duration::from_secs(600));
}

/// After a turn is dropped: prune, then keep the repo under `cap`. Oldest turns of this pane go first; a repo still over the cap
/// with nothing left to drop is deleted and starts over (turns of other panes in that folder then fail with `io`, like a missing repo).
fn compact_with(root: &str, turns: &Mutex<Turns>, cap: u64) {
    let _g = LOCK.lock().unwrap();
    let gd = gitdir(root);
    prune(&gd);
    while dir_size(&gd) > cap {
        let Some(t) = ({
            let mut cur = turns.lock().unwrap();
            (!cur.done.is_empty()).then(|| cur.done.remove(0))
        }) else {
            PINS.lock().unwrap().retain(|((r, _), _)| r != root);
            let _ = std::fs::remove_dir_all(&gd);
            return;
        };
        unpin_turn(&t);
        prune(&gd);
    }
}

pub fn compact(root: &str, turns: &Mutex<Turns>) {
    compact_with(root, turns, CAP);
}

/// Once per daemon run: no turn survives a restart, so every ref is stale. Prune what they held; a repo over the cap is deleted.
pub fn startup() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        queue(|| {
            let _g = LOCK.lock().unwrap();
            let Ok(entries) = std::fs::read_dir(dir()) else { return };
            for gd in entries.flatten().map(|e| e.path()).filter(|p| p.join("HEAD").exists()) {
                let _ = std::fs::remove_dir_all(gd.join("refs/turns"));
                prune(&gd);
                if dir_size(&gd) > CAP {
                    let _ = std::fs::remove_dir_all(&gd);
                }
            }
        })
    });
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

/// Root of the git repo at `cwd`, or None when it isn't a repo / is a repo with local filters (SEC-29)
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

/// Snapshot the working folder: (tree, big files skipped)
pub fn take(root: &str) -> Option<(String, Vec<String>)> {
    let _g = LOCK.lock().unwrap();
    init(root)?;
    let list = crate::git::git_for(Path::new(root), &["ls-files", "-co", "--exclude-standard", "-z"], Duration::from_secs(30))?;
    let (mut keep, mut skipped) = (Vec::new(), Vec::new());
    for p in list.split('\0').filter(|p| !p.is_empty()) {
        match std::fs::symlink_metadata(Path::new(root).join(p)) {
            Ok(m) if m.len() >= BIG && m.is_file() => skipped.push(p.to_string()),
            Ok(_) => keep.push(p.to_string()),
            Err(_) => {} // tracked files that were deleted
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

/// (file, lines added, lines removed) between two snapshots
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

/// Diff of one turn, same shape as `git::diff` (changes panel FR-22)
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

/// Files that change when the working folder goes back to `before`: (`ubah` | `hapus`, path)
pub fn preview(root: &str, before: &str) -> Option<Vec<(&'static str, String)>> {
    let (now, _) = take(root)?;
    // the fresh tree has no ref: a prune from the queue must not take it before the diff
    pin(root, &now);
    let out = sgit(root, &["diff", "--name-status", "--no-renames", "-z", before, &now], Duration::from_secs(10));
    unpin(root, &now);
    let out = out?;
    let mut it = out.split('\0').filter(|s| !s.is_empty());
    let mut files = Vec::new();
    while let (Some(st), Some(p)) = (it.next(), it.next()) {
        // A = file new after that point: deleted; M/D/T = old content written back
        files.push((if st == "A" { "hapus" } else { "ubah" }, p.to_string()));
    }
    Some(files)
}

/// Restore the working folder to `before` for `files` from `preview`. Ignored and skipped files aren't touched.
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

/// Turn done = snapshot after
pub fn finish(o: Open, end: u64, title: String, summary: String) -> Option<Turn> {
    let Some((after, skipped)) = take(&o.root) else {
        unpin(&o.root, &o.before);
        return None;
    };
    pin(&o.root, &after);
    Some(Turn { start: o.start, end, root: o.root, before: o.before, after, head: o.head, title, summary, skipped })
}

pub const SUMMARY_MAX: usize = 300;

/// FR-99 P5: one short line from an agent's last message: control characters dropped, whitespace collapsed, cut at
/// `SUMMARY_MAX` characters (on a word when it can, with an ellipsis). Masking secrets is the caller's job.
pub fn clip_summary(text: &str) -> String {
    let flat = text.chars().filter(|c| !c.is_control() || c.is_whitespace()).collect::<String>().split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= SUMMARY_MAX {
        return flat;
    }
    let cut: String = flat.chars().take(SUMMARY_MAX).collect();
    let word = cut.rsplit_once(' ').map_or(cut.as_str(), |(a, _)| a);
    format!("{}…", word.trim_end_matches([',', ';', ':', '.']))
}

/// One queue for every snapshot: turn start/end order holds, agent hooks don't wait for git
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

/// Snapshot repos unused for 7 days are deleted
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
    fn summary_is_one_clipped_line() {
        assert_eq!(clip_summary("  Bikin layar\n\nharness.\t48 tes lewat. \x1b[0m"), "Bikin layar harness. 48 tes lewat. [0m");
        assert_eq!(clip_summary(""), "");
        let long = "kata ".repeat(100);
        let s = clip_summary(&long);
        assert!(s.ends_with('…') && s.chars().count() <= SUMMARY_MAX + 1, "{s}");
        assert!(!s.contains("kat…"), "cut on a word: {s}");
        assert_eq!(clip_summary(&"x".repeat(500)).chars().count(), SUMMARY_MAX + 1, "one long word is cut hard");
    }

    #[test]
    fn giliran_dipotret_dan_dikembalikan() {
        let base = std::env::temp_dir().join(format!("gembala-snap-{}", std::process::id()));
        let repo = base.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        // SAFETY: other tests using GEMBALA_CONFIG write the same value (test folder per process)
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
        // agent turn: change a.txt, create b.txt, ignored file
        std::fs::write(repo.join("a.txt"), "dua\n").unwrap();
        std::fs::write(repo.join("b.txt"), "baru\n").unwrap();
        std::fs::write(repo.join("rahasia.txt"), "x").unwrap();
        let turn = &finish(open, 2, "uji".into(), String::new()).unwrap();
        assert_eq!(stat(turn), (2, 2, 1));
        assert!(diff(turn)["diff"].as_str().unwrap().contains("+dua"));
        let files = preview(&root, &turn.before).unwrap();
        assert!(files.contains(&("ubah", "a.txt".into())) && files.contains(&("hapus", "b.txt".into())) && files.len() == 2, "{files:?}");
        restore(&root, &turn.before, &files).unwrap();
        assert_eq!(std::fs::read_to_string(repo.join("a.txt")).unwrap(), "satu\n");
        assert!(!repo.join("b.txt").exists() && repo.join("rahasia.txt").exists());
        // the user's index and HEAD stay untouched
        assert_eq!(head(&root), head0);
        let st = Command::new("git").arg("-C").arg(&repo).args(["status", "--porcelain"]).output().unwrap().stdout;
        assert!(String::from_utf8_lossy(&st).trim().is_empty(), "{}", String::from_utf8_lossy(&st));
        let _ = std::fs::remove_dir_all(base);
    }

    /// git repo with one committed file in a temp folder, snapshot config pointed at a temp folder (same value in every test)
    fn temp_repo(tag: &str) -> (PathBuf, String) {
        let base = std::env::temp_dir().join(format!("gembala-snap-{tag}-{}", std::process::id()));
        let repo = base.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        // SAFETY: other tests using GEMBALA_CONFIG write the same value (test folder per process)
        unsafe { std::env::set_var("GEMBALA_CONFIG", std::env::temp_dir().join(format!("gembala-uji-{}/config.toml", std::process::id()))) };
        assert!(Command::new("git").arg("-C").arg(&repo).args(["init", "-q"]).output().unwrap().status.success());
        let root = root_of(&repo).unwrap();
        (repo, root)
    }

    /// snapshot of a folder where `a.bin` holds `bytes` pseudo-random (incompressible) bytes, as a pinned finished turn
    fn turn_of(repo: &Path, root: &str, bytes: usize, seed: u8) -> Turn {
        let mut x = 0x9e3779b97f4a7c15u64 ^ u64::from(seed);
        let data: Vec<u8> = (0..bytes)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                (x >> 24) as u8
            })
            .collect();
        std::fs::write(repo.join("a.bin"), data).unwrap();
        let (tree, _) = take(root).unwrap();
        pin(root, &tree); // before and after, as `Open` and `finish` do
        pin(root, &tree);
        Turn { start: 1, end: 2, root: root.into(), before: tree.clone(), after: tree, head: String::new(), title: String::new(), summary: String::new(), skipped: vec![] }
    }

    fn loose_and_packed(root: &str) -> u64 {
        dir_size(&gitdir(root).join("objects"))
    }

    #[test]
    fn dropped_turn_is_pruned_and_a_kept_one_survives() {
        let (repo, root) = temp_repo("prune");
        let old = turn_of(&repo, &root, 300_000, 1);
        let keep = turn_of(&repo, &root, 300_000, 2);
        let with_both = loose_and_packed(&root);
        let turns = Mutex::new(Turns::default());
        turns.lock().unwrap().done = vec![keep.clone()];
        unpin_turn(&old);
        compact_with(&root, &turns, u64::MAX);
        assert!(loose_and_packed(&root) < with_both - 200_000, "old blob still there");
        // the kept turn still diffs against itself and restores
        let kind = |t: &str| sgit(&root, &["cat-file", "-t", t], Duration::from_secs(10)).unwrap_or_default().trim().to_string();
        assert_eq!(kind(&keep.after), "tree");
        assert_eq!(kind(&old.after), "", "dropped tree still there");
        drop(turns); // Drop unpins the kept turn: its ref goes
        assert!(PINS.lock().unwrap().iter().all(|((r, _), _)| *r != root));
        let _ = std::fs::remove_dir_all(repo.parent().unwrap());
    }

    #[test]
    fn a_shared_tree_stays_pinned_until_the_last_turn_lets_go() {
        let (repo, root) = temp_repo("shared");
        let a = turn_of(&repo, &root, 1000, 3);
        let refs = || gd_git(&gitdir(&root), &["for-each-ref", "refs/turns"], Duration::from_secs(10)).unwrap_or_default();
        pin(&root, &a.after);
        unpin(&root, &a.after);
        assert!(refs().contains(&a.after), "still held by the turn");
        unpin_turn(&a); // before and after are the same tree here
        assert!(refs().trim().is_empty(), "{}", refs());
        let _ = std::fs::remove_dir_all(repo.parent().unwrap());
    }

    #[test]
    fn files_of_25_mb_or_more_are_skipped() {
        let (repo, root) = temp_repo("big");
        std::fs::write(repo.join("small.bin"), vec![1u8; 24 << 20]).unwrap();
        let f = std::fs::File::create(repo.join("big.bin")).unwrap();
        f.set_len(25 << 20).unwrap(); // sparse: no disk used
        let (_, skipped) = take(&root).unwrap();
        assert_eq!(skipped, ["big.bin"]);
        let _ = std::fs::remove_dir_all(repo.parent().unwrap());
    }

    #[test]
    fn a_repo_over_the_cap_loses_oldest_turns_then_itself() {
        let (repo, root) = temp_repo("cap");
        let t1 = turn_of(&repo, &root, 300_000, 4);
        let t2 = turn_of(&repo, &root, 300_000, 5);
        let t3 = turn_of(&repo, &root, 300_000, 6);
        let turns = Mutex::new(Turns::default());
        turns.lock().unwrap().done = vec![t1, t2, t3];
        // room for about two blobs: the oldest goes, the newer two stay
        compact_with(&root, &turns, 700_000);
        assert_eq!(turns.lock().unwrap().done.len(), 2, "oldest dropped, then it fits");
        // the cap lowered below one blob: the pane's turns are all dropped, the repo goes and a later snapshot starts over
        compact_with(&root, &turns, 100_000);
        assert!(turns.lock().unwrap().done.is_empty());
        assert!(!gitdir(&root).exists());
        assert!(take(&root).is_some() && gitdir(&root).exists());
        let _ = std::fs::remove_dir_all(repo.parent().unwrap());
    }
}
