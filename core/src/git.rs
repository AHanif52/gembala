//! Panel perubahan dan toolbar (FR-22, FR-55; SEC-29): git milik pengguna, baca saja, tanpa jaringan.
//! Tidak ada input pengguna yang menjadi argumen git; folder kerja diambil dari pane.

use serde_json::{Value, json};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

const MAX_DIFF: usize = 200 * 1024;

fn git(cwd: &Path, args: &[&str]) -> Option<String> {
    let mut c = Command::new("git");
    c.arg("-C").arg(cwd).args(args);
    c.env("GIT_TERMINAL_PROMPT", "0").env("GIT_OPTIONAL_LOCKS", "0").stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
    crate::pty::scrub_env(&mut c);
    let out = crate::profile::run_with_timeout(c, Duration::from_secs(3))?;
    Some(out)
}

/// `{repo:false}` bila bukan repo; selain itu repo, branch, ahead/behind upstream, jumlah file dan baris.
pub fn status(cwd: &Path) -> Value {
    let Some(root) = git(cwd, &["rev-parse", "--show-toplevel"]).map(|s| s.trim().to_string()).filter(|s| !s.is_empty()) else {
        return json!({"repo": false});
    };
    let branch = git(cwd, &["symbolic-ref", "--short", "-q", "HEAD"]).map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    let (ahead, behind) = git(cwd, &["rev-list", "--left-right", "--count", "HEAD...@{upstream}"])
        .and_then(|s| {
            let mut it = s.split_whitespace().map(|n| n.parse::<u64>().ok());
            Some((it.next()??, it.next()??))
        })
        .map_or((Value::Null, Value::Null), |(a, b)| (json!(a), json!(b)));
    let (mut files, mut add, mut del) = (0u64, 0u64, 0u64);
    for l in git(cwd, &["diff", "--numstat", "HEAD", "--"]).unwrap_or_default().lines() {
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
    let mut text = git(cwd, &["diff", "--no-color", "--no-ext-diff", "HEAD", "--"]).unwrap_or_default();
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
    }
}
