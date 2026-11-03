//! Panel perubahan dan toolbar (FR-22, FR-55; SEC-29): git milik pengguna, baca saja, tanpa jaringan.
//! Tidak ada input pengguna yang menjadi argumen git; folder kerja diambil dari pane. Pengecualian FR-71: worktree
//! (nama tervalidasi, hook repo dimatikan) hanya dari kanal UI.

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
    // SEC-29: config milik repo tidak boleh menjalankan perintah (fsmonitor, hook, repo bare tersemat)
    c.args(["-c", "core.fsmonitor=false", "-c", "core.hooksPath=/dev/null", "-c", "safe.bareRepository=explicit", "-C"]).arg(cwd).args(args);
    c.env("GIT_TERMINAL_PROMPT", "0").env("GIT_OPTIONAL_LOCKS", "0").stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
    crate::pty::scrub_env(&mut c);
    crate::profile::run_with_timeout(c, limit)
}

/// FR-71: nama worktree = bagian folder dan branch `gembala/<nama>`
pub fn valid_name(n: &str) -> bool {
    (1..=40).contains(&n.len()) && n.chars().all(|c| c.is_ascii_alphanumeric() || "._-".contains(c)) && n.chars().next().is_some_and(|c| c.is_ascii_alphanumeric()) && !n.contains("..")
}

/// FR-71: worktree baru dari HEAD repo `cwd` di `dir` (bawaan `<repo>.<nama>` di samping repo), branch `gembala/<nama>`.
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
        return Err("worktree_failed"); // mis. branch sudah ada
    }
    Ok(json!({"dir": d, "branch": branch}))
}

/// FR-71: hapus worktree tambahan (bukan repo utama). Ada perubahan dan belum `force` = daftar file, tidak dihapus.
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

/// SEC-29: filter clean/smudge/process di config lokal repo berjalan saat diff membandingkan worktree dan tidak bisa
/// dimatikan lewat `-c`; repo seperti itu dianggap bukan repo. Filter dari config global (mis. git-lfs) tetap dipercaya.
pub(crate) fn repo_runs_filters(cwd: &Path) -> bool {
    git(cwd, &["config", "--list", "--show-scope", "--includes"]).is_none_or(|s| {
        s.lines().any(|l| {
            let (scope, kv) = l.split_once('\t').unwrap_or_default();
            let key = kv.split('=').next().unwrap_or_default();
            matches!(scope, "local" | "worktree") && key.starts_with("filter.") && [".clean", ".smudge", ".process"].iter().any(|s| key.ends_with(s))
        })
    })
}

/// `{repo:false}` bila bukan repo; selain itu repo, branch, ahead/behind upstream, jumlah file dan baris.
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

/// Diff terhadap HEAD (staged + belum) plus daftar file baru; disamarkan (SEC-12) dan dibatasi 200 KB.
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

/// SEC-12: kata yang diawali pola rahasia diganti; blok PRIVATE KEY dibuang.
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

        // audit run-1: config repo tidak boleh menjalankan perintah lewat panel (SEC-29)
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
