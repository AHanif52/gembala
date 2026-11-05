//! Changes panel and toolbar (FR-22, FR-55; SEC-29): the user's git, read only, no network.
//! No user input becomes a git argument; the working folder comes from the pane. Exception FR-71: worktrees
//! (validated name, repo hooks off) only from the UI channel.
//! FR-23 exception: restore and commit one file (paths from the panel's own diff), UI channel only too.

use serde_json::{Value, json};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

const MAX_DIFF: usize = 200 * 1024;

pub(crate) fn git(cwd: &Path, args: &[&str]) -> Option<String> {
    git_for(cwd, args, Duration::from_secs(3))
}

pub(crate) fn git_for(cwd: &Path, args: &[&str], limit: Duration) -> Option<String> {
    let mut c = Command::new("git");
    // SEC-29: repo-owned config must not run commands (fsmonitor, hooks, embedded bare repos)
    c.args(["-c", "core.fsmonitor=false", "-c", "core.hooksPath=/dev/null", "-c", "safe.bareRepository=explicit", "-C"]).arg(cwd).args(args);
    c.env("GIT_TERMINAL_PROMPT", "0").env("GIT_OPTIONAL_LOCKS", "0").stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
    crate::pty::scrub_env(&mut c);
    crate::profile::run_with_timeout(c, limit)
}

/// FR-71: worktree name = part of the folder and of branch `gembala/<name>`
pub fn valid_name(n: &str) -> bool {
    (1..=40).contains(&n.len()) && n.chars().all(|c| c.is_ascii_alphanumeric() || "._-".contains(c)) && n.chars().next().is_some_and(|c| c.is_ascii_alphanumeric()) && !n.contains("..")
}

/// FR-71: new worktree from HEAD of the repo at `cwd` in `dir` (default `<repo>.<name>` next to the repo), branch `gembala/<name>`.
pub fn worktree_add(cwd: &Path, name: &str, dir: Option<&str>) -> Result<Value, &'static str> {
    if !valid_name(name) {
        return Err("bad_name");
    }
    let root = git(cwd, &["rev-parse", "--show-toplevel"]).map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).ok_or("not_repo")?;
    if repo_runs_filters(cwd) {
        return Err("repo_filter");
    }
    let home = std::env::var("HOME").unwrap_or_default();
    let dir = match dir.map(str::trim).filter(|d| !d.is_empty()) {
        Some(d) => std::path::PathBuf::from(d.strip_prefix('~').map_or(d.to_string(), |r| format!("{home}{r}"))),
        None => std::path::PathBuf::from(format!("{root}.{name}")),
    };
    if !dir.is_absolute() || dir.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
        return Err("bad_dir");
    }
    if dir.exists() {
        return Err("dir_exists");
    }
    let branch = format!("gembala/{name}");
    let d = dir.to_string_lossy().into_owned();
    git_for(Path::new(&root), &["worktree", "add", "-b", &branch, &d, "HEAD"], Duration::from_secs(120));
    if !dir.join(".git").exists() {
        return Err("worktree_failed"); // e.g. the branch already exists
    }
    Ok(json!({"dir": d, "branch": branch}))
}

/// FR-71: remove an extra worktree (not the main repo). Changes present and no `force` = file list, nothing removed.
pub fn worktree_remove(dir: &Path, force: bool) -> Result<Value, &'static str> {
    let list = git(dir, &["worktree", "list", "--porcelain"]).ok_or("not_repo")?;
    let trees: Vec<&str> = list.lines().filter_map(|l| l.strip_prefix("worktree ")).collect();
    let me = dir.canonicalize().map_err(|_| "io")?;
    let main = trees.first().ok_or("not_repo")?;
    if !trees[1..].iter().any(|t| Path::new(t).canonicalize().is_ok_and(|t| t == me)) {
        return Err("not_worktree");
    }
    let dirty: Vec<String> = git(dir, &["status", "--porcelain", "--untracked-files=all"]).unwrap_or_default().lines().map(|l| redact(l.get(3..).unwrap_or(l))).collect();
    if !dirty.is_empty() && !force {
        return Ok(json!({"removed": false, "dirty": dirty}));
    }
    let d = me.to_string_lossy().into_owned();
    let args: &[&str] = if force { &["worktree", "remove", "--force", &d] } else { &["worktree", "remove", &d] };
    git_for(Path::new(main), args, Duration::from_secs(60));
    if me.exists() {
        return Err("remove_failed");
    }
    Ok(json!({"removed": true}))
}

/// FR-23: toplevel of `cwd` for a write action; refused when it isn't a repo, the repo runs filters (SEC-29), or a
/// path isn't plain and relative to that toplevel (the panel only sends paths from its own diff).
fn act_root(cwd: &Path, paths: &[String]) -> Result<std::path::PathBuf, &'static str> {
    let root = git(cwd, &["rev-parse", "--show-toplevel"]).map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).ok_or("not_repo")?;
    if repo_runs_filters(cwd) {
        return Err("repo_filter");
    }
    let plain = |p: &String| !p.is_empty() && !p.contains('\0') && Path::new(p).components().all(|c| matches!(c, std::path::Component::Normal(_)));
    if paths.is_empty() || paths.len() > 500 || !paths.iter().all(plain) {
        return Err("bad_path");
    }
    Ok(root.into())
}

/// FR-23: whether HEAD has `path` as a file. A file HEAD doesn't have (untracked, or only staged) has no committed
/// version: "restoring" it would delete it for good, so restore refuses it.
fn in_head(root: &Path, path: &str) -> bool {
    git(root, &["--literal-pathspecs", "ls-tree", "-z", "HEAD", "--", path])
        .is_some_and(|s| s.split('\0').any(|e| e.split_once('\t').is_some_and(|(meta, name)| name == path && meta.contains(" blob "))))
}

/// FR-23: per path, `mod` (HEAD has it: restore and commit) or `new` (commit only), for the confirmation card.
pub fn file_kinds(cwd: &Path, paths: &[String]) -> Result<Value, &'static str> {
    let root = act_root(cwd, paths)?;
    Ok(json!(paths.iter().map(|p| json!({"path": p, "kind": if in_head(&root, p) { "mod" } else { "new" }})).collect::<Vec<_>>()))
}

/// FR-23 (SEC-29): one file back to HEAD, staged and unstaged changes both dropped. UI channel only.
pub fn restore_file(cwd: &Path, path: &str) -> Result<Value, &'static str> {
    let root = act_root(cwd, &[path.to_string()])?;
    if !in_head(&root, path) {
        return Err("new_file");
    }
    git_for(&root, &["--literal-pathspecs", "restore", "--source=HEAD", "--staged", "--worktree", "--", path], Duration::from_secs(30));
    if git(&root, &["--literal-pathspecs", "status", "--porcelain", "-z", "--", path]).is_none_or(|s| !s.is_empty()) {
        return Err("restore_failed");
    }
    Ok(json!({"restored": path}))
}

/// FR-23 (SEC-29): commit just `paths` (their worktree content; other staged files stay staged) with the user's
/// message. Unlike the read-only calls, the user's hooks and signing run as with their own `git commit`; fsmonitor
/// and embedded bare repos stay off. Failure = `commit_failed` with git's output (e.g. a hook saying why).
pub fn commit_files(cwd: &Path, paths: &[String], message: &str) -> Result<Value, crate::diag::Diag> {
    use crate::diag::Diag;
    if message.trim().is_empty() {
        return Err(Diag::new("empty_message"));
    }
    let root = act_root(cwd, paths).map_err(Diag::new)?;
    // `commit -- <path>` only takes paths git knows; new files are added first
    let new: Vec<&str> = paths.iter().filter(|p| !in_head(&root, p)).map(String::as_str).collect();
    if !new.is_empty() {
        let mut args = vec!["--literal-pathspecs", "add", "--"];
        args.extend(&new);
        git_for(&root, &args, Duration::from_secs(30));
    }
    let before = git(&root, &["rev-parse", "-q", "--verify", "HEAD"]);
    let msg = format!("--message={message}");
    let mut c = Command::new("git");
    c.args(["-c", "core.fsmonitor=false", "-c", "safe.bareRepository=explicit", "-C"]).arg(&root);
    c.args(["--literal-pathspecs", "commit", &msg, "--"]).args(paths);
    c.env("GIT_TERMINAL_PROMPT", "0").env("GIT_EDITOR", "true").stdin(Stdio::null());
    crate::pty::scrub_env(&mut c);
    let out = output_for(c, Duration::from_secs(120)).unwrap_or_default();
    let after = git(&root, &["rev-parse", "-q", "--verify", "HEAD"]);
    if after.is_none() || after == before {
        let tail: Vec<&str> = out.lines().rev().take(12).collect::<Vec<_>>().into_iter().rev().collect();
        return Err(Diag::new("commit_failed").arg("os", redact(&tail.join("\n"))));
    }
    Ok(json!({"commit": after.unwrap_or_default().trim().get(..7).unwrap_or_default()}))
}

/// stdout and stderr together, killed after `limit` (hooks and signing can take a while or hang)
fn output_for(mut c: Command, limit: Duration) -> Option<String> {
    use std::io::Read;
    let (mut r, w) = std::io::pipe().ok()?;
    c.stdout(w.try_clone().ok()?).stderr(w);
    let mut child = c.spawn().ok()?;
    drop(c); // the parent's copy of the pipe's write end, otherwise the read never ends
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut out = Vec::new();
        let _ = r.read_to_end(&mut out);
        let _ = tx.send(String::from_utf8_lossy(&out).into_owned());
    });
    let t0 = std::time::Instant::now();
    while child.try_wait().ok()?.is_none() {
        if t0.elapsed() > limit {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    // a hook's leftover background process can keep the pipe open: don't wait for it
    rx.recv_timeout(Duration::from_secs(2)).ok()
}

/// SEC-29: clean/smudge/process filters in the repo's local config run when diff compares the worktree and can't be
/// turned off with `-c`; a repo like that is treated as no repo. Filters from global config (e.g. git-lfs) are still trusted.
pub(crate) fn repo_runs_filters(cwd: &Path) -> bool {
    git(cwd, &["config", "--list", "--show-scope", "--includes"]).is_none_or(|s| {
        s.lines().any(|l| {
            let (scope, kv) = l.split_once('\t').unwrap_or_default();
            let key = kv.split('=').next().unwrap_or_default();
            matches!(scope, "local" | "worktree") && key.starts_with("filter.") && [".clean", ".smudge", ".process"].iter().any(|s| key.ends_with(s))
        })
    })
}

/// `{repo:false}` when it isn't a repo; otherwise repo, branch, ahead/behind upstream, file and line counts.
pub fn status(cwd: &Path) -> Value {
    let Some(root) = git(cwd, &["rev-parse", "--show-toplevel"]).map(|s| s.trim().to_string()).filter(|s| !s.is_empty()) else {
        return json!({"repo": false});
    };
    if repo_runs_filters(cwd) {
        return json!({"repo": false, "reason": "repo_filter"});
    }
    let branch = git(cwd, &["symbolic-ref", "--short", "-q", "HEAD"]).map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    let (ahead, behind) = git(cwd, &["rev-list", "--left-right", "--count", "HEAD...@{upstream}"])
        .and_then(|s| {
            let mut it = s.split_whitespace().map(|n| n.parse::<u64>().ok());
            Some((it.next()??, it.next()??))
        })
        .map_or((Value::Null, Value::Null), |(a, b)| (json!(a), json!(b)));
    let (mut files, mut add, mut del) = (0u64, 0u64, 0u64);
    for l in git(cwd, &["diff", "--numstat", "--no-textconv", "--no-ext-diff", "HEAD", "--"]).unwrap_or_default().lines() {
        let mut f = l.split('\t');
        add += f.next().and_then(|n| n.parse::<u64>().ok()).unwrap_or(0);
        del += f.next().and_then(|n| n.parse::<u64>().ok()).unwrap_or(0);
        files += 1;
    }
    let untracked = git(cwd, &["ls-files", "--others", "--exclude-standard", "--"]).unwrap_or_default().lines().count() as u64;
    json!({"repo": true, "root": root, "branch": branch, "ahead": ahead, "behind": behind,
           "files": files + untracked, "untracked": untracked, "add": add, "del": del})
}

/// Diff against HEAD (staged + unstaged) plus the list of new files; redacted (SEC-12) and capped at 200 KB.
pub fn diff(cwd: &Path) -> Value {
    if git(cwd, &["rev-parse", "--show-toplevel"]).is_none_or(|s| s.trim().is_empty()) {
        return json!({"repo": false});
    }
    if repo_runs_filters(cwd) {
        return json!({"repo": false, "reason": "repo_filter"});
    }
    let mut text = git(cwd, &["diff", "--no-color", "--no-ext-diff", "--no-textconv", "HEAD", "--"]).unwrap_or_default();
    let truncated = text.len() > MAX_DIFF;
    if truncated {
        let mut cut = MAX_DIFF;
        while !text.is_char_boundary(cut) {
            cut -= 1;
        }
        text.truncate(cut);
    }
    let untracked: Vec<String> = git(cwd, &["ls-files", "--others", "--exclude-standard", "--"]).unwrap_or_default().lines().map(redact).collect();
    json!({"repo": true, "diff": redact(&text), "truncated": truncated, "untracked": untracked})
}

/// SEC-12: words starting with a secret pattern are replaced; PRIVATE KEY blocks are dropped.
pub fn redact(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_key = false;
    for line in s.split_inclusive('\n') {
        if line.contains("-----BEGIN") && line.contains("PRIVATE KEY") {
            in_key = true;
            out += "[blok kunci privat disamarkan]\n";
        }
        if in_key {
            in_key = !(line.contains("-----END") && line.contains("PRIVATE KEY"));
            continue;
        }
        let mut rest = line;
        while let Some((i, pat)) = crate::profile::SECRET.iter().filter_map(|p| crate::profile::word_start(rest, p).map(|i| (i, p))).min() {
            out += &rest[..i];
            let tail = &rest[i + pat.len()..];
            let end = tail.find(|c: char| c.is_whitespace() || "\"'`,;)]}".contains(c)).unwrap_or(tail.len());
            out += "[disamarkan]";
            rest = &tail[end..];
        }
        out += rest;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worktree_buat_dan_hapus() {
        let base = std::env::temp_dir().join(format!("gembala-wt-{}", std::process::id()));
        let repo = base.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let sh = |args: &[&str]| assert!(Command::new("git").arg("-C").arg(&repo).args(args).output().unwrap().status.success(), "{args:?}");
        sh(&["init", "-q"]);
        std::fs::write(repo.join("a.txt"), "a").unwrap();
        sh(&["add", "a.txt"]);
        sh(&["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "a"]);
        assert_eq!(worktree_add(&repo, "../x", None), Err("bad_name"));
        let r = worktree_add(&repo, "fitur-1", None).unwrap();
        let dir = std::path::PathBuf::from(r["dir"].as_str().unwrap());
        assert!(dir.join("a.txt").exists() && r["branch"] == "gembala/fitur-1");
        assert_eq!(worktree_add(&repo, "fitur-1", None), Err("dir_exists"));
        assert_eq!(worktree_remove(&repo, false), Err("not_worktree"));
        std::fs::write(dir.join("b.txt"), "b").unwrap();
        assert_eq!(worktree_remove(&dir, false).unwrap()["dirty"][0], "b.txt");
        assert_eq!(worktree_remove(&dir, true).unwrap()["removed"], true);
        assert!(!dir.exists());
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn restore_and_commit_one_file() {
        let d = std::env::temp_dir().join(format!("gembala-act-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let run = |a: &[&str]| assert!(Command::new("git").arg("-C").arg(&d).args(a).output().unwrap().status.success(), "{a:?}");
        run(&["init", "-q", "-b", "main"]);
        run(&["config", "user.email", "t@t"]);
        run(&["config", "user.name", "t"]);
        for f in ["a.txt", "b c.txt", "-x.txt", "staged.txt"] {
            std::fs::write(d.join(f), "awal\n").unwrap();
        }
        run(&["add", "--", "."]);
        run(&["commit", "-q", "-m", "awal"]);
        let p = |s: &str| s.to_string();
        let read = |f: &str| std::fs::read_to_string(d.join(f)).unwrap();

        // modified, path with a space, path starting with '-', staged and then modified again
        for f in ["a.txt", "b c.txt", "-x.txt", "staged.txt"] {
            std::fs::write(d.join(f), "ubah\n").unwrap();
        }
        run(&["add", "staged.txt"]);
        std::fs::write(d.join("staged.txt"), "ubah lagi\n").unwrap();
        for f in ["b c.txt", "-x.txt", "staged.txt"] {
            assert_eq!(restore_file(&d, f).unwrap()["restored"], f);
            assert_eq!(read(f), "awal\n", "{f}");
        }
        assert_eq!(read("a.txt"), "ubah\n", "only the asked file goes back");
        run(&["diff", "--cached", "--quiet"]); // staged change dropped too

        // new files: untracked and only staged are refused (restore would delete them), pathspec magic is literal
        std::fs::write(d.join("baru.txt"), "x").unwrap();
        std::fs::write(d.join("idx.txt"), "x").unwrap();
        run(&["add", "idx.txt"]);
        assert_eq!(restore_file(&d, "baru.txt"), Err("new_file"));
        assert_eq!(restore_file(&d, "idx.txt"), Err("new_file"));
        assert_eq!(restore_file(&d, ":(glob)*"), Err("new_file"));
        assert!(d.join("baru.txt").exists() && d.join("idx.txt").exists());
        assert_eq!(restore_file(&d, "../luar"), Err("bad_path"));
        assert_eq!(restore_file(&d, "/etc/hosts"), Err("bad_path"));
        assert_eq!(file_kinds(&d, &[p("a.txt"), p("baru.txt")]).unwrap(), json!([{"path": "a.txt", "kind": "mod"}, {"path": "baru.txt", "kind": "new"}]));

        // commit: message required, only the given paths go in, an untracked file is added
        assert_eq!(commit_files(&d, &[p("a.txt")], "  ").unwrap_err().code, "empty_message");
        let r = commit_files(&d, &[p("a.txt"), p("baru.txt")], "-m pesan").unwrap();
        assert_eq!(r["commit"].as_str().unwrap().len(), 7);
        let log = Command::new("git").arg("-C").arg(&d).args(["show", "--name-only", "--format=%s", "HEAD"]).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&log.stdout).split_whitespace().collect::<Vec<_>>(), ["-m", "pesan", "a.txt", "baru.txt"]);
        // idx.txt still staged, not committed
        assert!(String::from_utf8_lossy(&Command::new("git").arg("-C").arg(&d).args(["status", "--porcelain"]).output().unwrap().stdout).contains("A  idx.txt"));

        // the user's hook runs and can refuse; git's output comes back
        std::fs::write(d.join(".git/hooks/pre-commit"), "#!/bin/sh\necho ditolak hook >&2\nexit 1\n").unwrap();
        std::fs::set_permissions(d.join(".git/hooks/pre-commit"), std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        let e = commit_files(&d, &[p("idx.txt")], "x").unwrap_err();
        assert_eq!(e.code, "commit_failed");
        assert!(e.args[0].1.contains("ditolak hook"), "{e:?}");
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn redaksi_rahasia() {
        let s = "+ANTHROPIC_API_KEY=\"sk-ant-api03-abc\" token ghp_123 aman\n-----BEGIN OPENSSH PRIVATE KEY-----\nAAAA\n-----END OPENSSH PRIVATE KEY-----\nakhir\n";
        let r = redact(s);
        assert!(!r.contains("sk-ant-api03") && !r.contains("ghp_123") && !r.contains("AAAA"), "{r}");
        assert!(r.contains("aman") && r.contains("akhir") && r.contains("[disamarkan]"), "{r}");
        assert_eq!(redact("task-list risk-free mask-on\n"), "task-list risk-free mask-on\n");
    }

    #[test]
    fn repo_nyata_dan_bukan_repo() {
        let d = std::env::temp_dir().join(format!("gembala-git-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        assert_eq!(status(&d)["repo"], false);
        let run = |a: &[&str]| assert!(Command::new("git").arg("-C").arg(&d).args(a).output().unwrap().status.success(), "{a:?}");
        run(&["init", "-q", "-b", "main"]);
        std::fs::write(d.join("a.txt"), "satu\n").unwrap();
        run(&["add", "a.txt"]);
        run(&["-c", "user.email=t@t", "-c", "user.name=t", "commit", "-q", "-m", "awal"]);
        std::fs::write(d.join("a.txt"), "satu\ndua\nKEY=sk-rahasia\n").unwrap();
        std::fs::write(d.join("baru.txt"), "x").unwrap();
        let s = status(&d);
        assert_eq!((s["branch"].as_str(), s["files"].as_u64(), s["add"].as_u64(), s["untracked"].as_u64()), (Some("main"), Some(2), Some(2), Some(1)), "{s}");
        let df = diff(&d);
        assert!(df["diff"].as_str().unwrap().contains("+dua") && !df["diff"].as_str().unwrap().contains("sk-rahasia"), "{df}");
        assert_eq!(df["untracked"], json!(["baru.txt"]));

        // audit run-1: repo config must not run commands through the panel (SEC-29)
        let jejak = |n: &str| format!("touch '{}'; false", d.join(n).display());
        run(&["config", "core.fsmonitor", &jejak("FSMON")]);
        run(&["config", "diff.x.textconv", &jejak("TEXTCONV")]);
        std::fs::write(d.join(".gitattributes"), "a.txt diff=x\n").unwrap();
        std::fs::write(d.join("a.txt"), "tiga\n").unwrap();
        assert_eq!(status(&d)["repo"], true);
        assert!(diff(&d)["diff"].as_str().unwrap().contains("+tiga"));
        assert!(!d.join("FSMON").exists() && !d.join("TEXTCONV").exists(), "fsmonitor/textconv dijalankan");
        run(&["config", "filter.x.clean", &jejak("CLEAN")]);
        std::fs::write(d.join(".gitattributes"), "a.txt filter=x\n").unwrap();
        std::fs::write(d.join("a.txt"), "empat\n").unwrap();
        assert_eq!((status(&d)["repo"].clone(), diff(&d)["repo"].clone()), (json!(false), json!(false)));
        assert!(!d.join("CLEAN").exists(), "filter clean dijalankan");
    }
}
