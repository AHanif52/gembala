//! FR-62: per-pane block history on disk, so command blocks still show after the daemon stops or the laptop
//! restarts (like Warp's "Previous session"): command line, folder, time, exit code, and resume command, plus a
//! redacted log of the main screen (`.log`, exception to SEC-13).

use serde_json::{Value, json};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

const MAX: usize = 200;
const KEEP_DAYS: u64 = 14;
const LOG_MAX: u64 = 4 << 20;

pub struct Block {
    pub t: u64,
    pub cwd: String,
    pub cmd: String,
    pub code: i64,
    pub secs: u64,
    pub resume: Option<String>,
}

/// zsh block markers in the PTY stream: `OSC 133;C;<base64 command>` and `OSC 133;D;<exit code>`.
pub enum Mark {
    Start(String),
    End(i64),
}

fn dir() -> PathBuf {
    crate::config::path().with_file_name("history")
}

/// 16 hex digits: a pane's history key (the same across daemon restarts, so its files never depend on one daemon run),
/// or the daemon boot id of `{boot, pane}` in layouts saved before keys (8 Nov 2026)
pub fn hex16(s: &str) -> bool {
    s.len() == 16 && s.bytes().all(|c| c.is_ascii_hexdigit())
}

/// File name stem of the `history` the UI sends (untrusted): a pane key, or `<boot>-<pane>` from an old layout
pub fn stem(from: &Value) -> Option<String> {
    if let Some(k) = from.as_str() {
        return hex16(k).then(|| k.to_string());
    }
    let (boot, pane) = (from["boot"].as_str()?, from["pane"].as_u64()?);
    hex16(boot).then(|| format!("{boot}-{pane}"))
}

pub fn path(stem: &str) -> PathBuf {
    dir().join(format!("{stem}.jsonl"))
}

/// Pane screen log (colored lines) written whole through a temp file; write failures are ignored.
pub fn save_log(stem: &str, bytes: &[u8]) {
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    let file = dir().join(format!("{stem}.log"));
    let _ = std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir());
    let tmp = file.with_extension("tmp");
    // redacted per chunk between color codes: `\x1b[32mghp_…` still gets caught (the `m` isn't a word start)
    let raw = String::from_utf8_lossy(bytes);
    let (mut text, mut last) = (String::new(), 0);
    for m in regex_lite::Regex::new("\x1b\\[[0-9;]*m").unwrap().find_iter(&raw) {
        text += &crate::git::redact(&raw[last..m.start()]);
        text += m.as_str();
        last = m.end();
    }
    text += &crate::git::redact(&raw[last..]);
    let ok = std::fs::OpenOptions::new().create(true).write(true).truncate(true).mode(0o600).open(&tmp).and_then(|mut f| f.write_all(text.as_bytes()));
    if ok.is_ok() {
        let _ = std::fs::rename(&tmp, &file);
    }
}

/// Old pane log and when it was last written, if any.
pub fn load_log(stem: &str) -> Option<(Vec<u8>, u64)> {
    let file = dir().join(format!("{stem}.log"));
    let meta = std::fs::metadata(&file).ok().filter(|m| m.len() <= LOG_MAX)?;
    let t = meta.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs();
    Some((trim_separator(&std::fs::read(file).ok()?), t))
}

/// The "Sesi sebelumnya" (previous session) line and blank lines at the end of the log are dropped: a pane unused since the last restart
/// doesn't pile up lines, the latest one is enough
fn trim_separator(log: &[u8]) -> Vec<u8> {
    let text = String::from_utf8_lossy(log);
    let ansi = regex_lite::Regex::new("\x1b\\[[0-9;?]*[A-Za-z]").unwrap();
    let plain = |l: &str| ansi.replace_all(l, "").trim().to_string();
    let mut lines: Vec<&str> = text.split("\r\n").collect();
    // a "Sesi sebelumnya" line followed only by blank lines is dropped (they can stack)
    let last_sep = |lines: &[&str]| {
        lines.iter().rposition(|l| plain(l).trim_start_matches(['─', ' ']).starts_with("Sesi sebelumnya")).filter(|&i| lines[i + 1..].iter().all(|l| plain(l).is_empty()))
    };
    while let Some(i) = last_sep(&lines) {
        lines.truncate(i);
    }
    // the log always ends in three blank lines, same as block_text: the button row of the last block (two terminal rows, drawn
    // by the UI) isn't covered by the new line. The log is saved from the grid without trailing blank lines, so don't rely on its content
    while lines.last().is_some_and(|l| plain(l).is_empty()) {
        lines.pop();
    }
    if !lines.is_empty() {
        lines.extend(["", "", ""]);
    }
    let mut out = lines.join("\r\n");
    if !out.is_empty() {
        out += "\r\n";
    }
    out.into_bytes()
}

/// Separator line below the old log
pub fn separator(t: u64) -> Vec<u8> {
    format!("{GUTTER}\x1b[2mSesi sebelumnya · {}\x1b[0m\r\n\r\n", date(t)).into_bytes()
}

pub fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn json_line(b: &Block) -> String {
    json!({"t": b.t, "cwd": b.cwd, "cmd": b.cmd, "code": b.code, "secs": b.secs, "resume": b.resume}).to_string() + "\n"
}

/// Add one block; write failures are ignored (history must never get in the pane's way).
pub fn append(file: &Path, b: &Block) {
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    let Some(parent) = file.parent() else { return };
    let _ = std::fs::DirBuilder::new().recursive(true).mode(0o700).create(parent);
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).mode(0o600).open(file) {
        let _ = f.write_all(json_line(b).as_bytes());
    }
}

/// The file becomes exactly these blocks (the last 200 when a pane comes back, so a pane's file doesn't grow forever),
/// through a temp file: a daemon stopping halfway leaves the old file whole.
pub fn rewrite(file: &Path, blocks: &[Block]) {
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    let Some(parent) = file.parent() else { return };
    let _ = std::fs::DirBuilder::new().recursive(true).mode(0o700).create(parent);
    let tmp = file.with_extension("jsonl.tmp");
    let text: String = blocks.iter().map(json_line).collect();
    let ok = std::fs::OpenOptions::new().create(true).write(true).truncate(true).mode(0o600).open(&tmp).and_then(|mut f| f.write_all(text.as_bytes()));
    if ok.is_ok() {
        let _ = std::fs::rename(&tmp, file);
    }
}

/// Resume command shapes the daemon builds (FR-56); anything else is dropped.
fn safe_resume(r: &str) -> bool {
    const UUID: &str = "[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}";
    regex_lite::Regex::new(&format!(
        "^(claude(-[a-z0-9-]{{1,32}})? --resume {UUID}|codex resume ({UUID}|--last)|opencode --continue|opencode --session [A-Za-z0-9_-]{{1,80}}|omp --continue|omp --resume [A-Za-z0-9-]{{1,80}})$"
    ))
    .is_ok_and(|re| re.is_match(r))
}

fn clean(s: &str) -> String {
    crate::msg::sanitize(&s.chars().take(4096).collect::<String>()).unwrap_or_default()
}

/// Last 200 blocks of a pane (`stem` checked by [`stem`]), content sanitized: the file is written by the daemon but sits on disk.
pub fn load(stem: &str) -> Vec<Block> {
    let Ok(f) = std::fs::File::open(path(stem)) else { return Vec::new() };
    let lines: Vec<String> = std::io::BufReader::new(f).lines().map_while(Result::ok).collect();
    lines[lines.len().saturating_sub(MAX)..]
        .iter()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .map(|v| Block {
            t: v["t"].as_u64().unwrap_or_default(),
            cwd: clean(v["cwd"].as_str().unwrap_or_default()).replace('\n', " "),
            cmd: clean(v["cmd"].as_str().unwrap_or_default()),
            code: v["code"].as_i64().unwrap_or_default(),
            secs: v["secs"].as_u64().unwrap_or_default(),
            resume: v["resume"].as_str().filter(|r| safe_resume(r)).map(String::from),
        })
        .filter(|b| !b.cmd.trim().is_empty())
        .fold(Vec::new(), |mut out: Vec<Block>, b| {
            // one agent session recorded twice (shell block + agent block, before the 4 Nov 2026 fix):
            // same start and duration; the one with a resume command wins
            match out.last_mut() {
                Some(p) if p.t.abs_diff(b.t) <= 5 && p.secs.abs_diff(b.secs) <= 5 => {
                    if p.resume.is_none() && b.resume.is_some() {
                        *p = b;
                    }
                }
                _ => out.push(b),
            }
            out
        })
}

pub fn dur(secs: u64) -> String {
    match secs {
        0..60 => format!("{secs} dtk"),
        60..3600 => format!("{} mnt {} dtk", secs / 60, secs % 60),
        _ => format!("{} jam {} mnt", secs / 3600, secs % 3600 / 60),
    }
}

/// Timeline dots and lines are drawn by the UI outside the grid (pen screen 02), so block text starts at column 0
const GUTTER: &str = "";

/// One block like FR-56 (timeline): `<icon> <folder> · <duration>[ · exit n]` dim, command bold,
/// resume command dim if any (then one row for the UI buttons), then one blank row as spacing between blocks.
pub fn block_text(cwd: &str, secs: u64, code: &str, cmd: &str, resume: Option<&str>) -> String {
    let exit = if code == "0" { String::new() } else { format!(" · keluar {code}") };
    // agent icon colored like in the sidebar (FR-52); other commands have no icon
    let first = cmd.split_whitespace().next().unwrap_or_default();
    let icon = [("claude", "\x1b[38;2;217;119;87m✳"), ("omp", "\x1b[38;2;168;85;247mπ"), ("codex", "\x1b[38;2;16;163;127m◎"), ("opencode", "\x1b[38;2;96;165;250m◇")]
        .iter()
        .find(|(a, _)| first == *a || first.starts_with(&format!("{a}-")))
        .map_or(String::new(), |(_, g)| format!("{g}\x1b[0m\x1b[2m "));
    let cmd = cmd.replace('\n', &format!("\r\n{GUTTER}"));
    let mut t = format!("{GUTTER}\x1b[2m{icon}{cwd} · {}{exit}\x1b[0m\r\n{GUTTER}\x1b[1m{cmd}\x1b[0m\r\n", dur(secs));
    // pen screen 02: resume command dim without a marker, then two rows for the buttons the UI draws
    if let Some(r) = resume {
        t += &format!("{GUTTER}\x1b[2m{r}\x1b[0m\r\n\r\n\r\n");
    }
    t + "\r\n"
}

fn date(t: u64) -> String {
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    let secs = t as libc::time_t;
    let mut buf = [0u8; 64];
    let n = unsafe {
        libc::localtime_r(&secs, &mut tm);
        libc::strftime(buf.as_mut_ptr().cast(), buf.len(), c"%e %b %H.%M".as_ptr(), &tm)
    };
    String::from_utf8_lossy(&buf[..n]).trim().to_string()
}

/// Old pane blocks, then the "Sesi sebelumnya" line (the UI translates it), written to the grid before the new shell output.
pub fn render(blocks: &[Block]) -> Vec<u8> {
    let Some(first) = blocks.first() else { return Vec::new() };
    let mut out = String::new();
    for b in blocks {
        out += &block_text(&b.cwd, b.secs, &b.code.to_string(), &b.cmd, b.resume.as_deref());
    }
    let mut out = out.into_bytes();
    out.extend(separator(first.t));
    out
}

/// Block markers in an output chunk, in order.
// ponytail: a marker cut by the PTY read boundary is missed (that block isn't recorded)
pub fn marks(b: &[u8]) -> Vec<Mark> {
    use base64::Engine;
    let mut out = Vec::new();
    let mut i = 0;
    const OSC: &[u8] = b"\x1b]133;";
    while let Some(p) = b[i..].windows(OSC.len()).position(|w| w == OSC) {
        let s = i + p + OSC.len();
        let Some(end) = b[s..].iter().position(|&c| c == 0x07) else { break };
        let body = &b[s..s + end];
        if let Some(cmd) = body.strip_prefix(b"C;") {
            let cmd = base64::engine::general_purpose::STANDARD.decode(cmd).unwrap_or_default();
            out.push(Mark::Start(clean(&String::from_utf8_lossy(&cmd))));
        } else if let Some(code) = body.strip_prefix(b"D;") {
            out.push(Mark::End(std::str::from_utf8(code).ok().and_then(|c| c.trim().parse().ok()).unwrap_or_default()));
        }
        i = s + end + 1;
    }
    out
}

/// History and log files untouched for more than 14 days are deleted.
pub fn sweep() {
    let Ok(entries) = std::fs::read_dir(dir()) else { return };
    let limit = std::time::Duration::from_secs(KEEP_DAYS * 86400);
    for e in entries.flatten() {
        if e.metadata().and_then(|m| m.modified()).is_ok_and(|t| t.elapsed().is_ok_and(|d| d > limit)) {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// FR-61: the account owning a Claude session is guessed from the resume command in history (`claude-kantor --resume <id>` = "kantor",
/// `claude --resume <id>` = "" Utama), newest block wins. For sessions started before owners were recorded.
pub fn resume_account(id: &str) -> Option<String> {
    resume_account_in(&dir(), id)
}

fn resume_account_in(dir: &Path, id: &str) -> Option<String> {
    let tail = format!(" --resume {id}");
    let mut best: Option<(u64, String)> = None;
    for e in std::fs::read_dir(dir).ok()?.flatten().filter(|e| e.path().extension().is_some_and(|x| x == "jsonl")) {
        let Ok(f) = std::fs::File::open(e.path()) else { continue };
        for l in std::io::BufReader::new(f).lines().map_while(Result::ok) {
            let Ok(v) = serde_json::from_str::<Value>(&l) else { continue };
            let (Some(r), t) = (v["resume"].as_str(), v["t"].as_u64().unwrap_or_default()) else { continue };
            let Some(cmd) = r.strip_suffix(&tail).filter(|_| safe_resume(r)) else { continue };
            let acct = match cmd.strip_prefix("claude") {
                Some("") | Some("-utama") => String::new(),
                Some(a) => a.trim_start_matches('-').to_string(),
                None => continue,
            };
            if best.as_ref().is_none_or(|(bt, _)| t >= *bt) {
                best = Some((t, acct));
            }
        }
    }
    best.map(|(_, a)| a)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn penanda_blok() {
        let b = b"x\x1b]133;C;ZWNobyBoaQ==\x07hi\r\n\x1b]133;D;3\x07\x1b]133;A\x07";
        let m = marks(b);
        assert!(matches!(&m[0], Mark::Start(c) if c == "echo hi"));
        assert!(matches!(m[1], Mark::End(3)));
        assert_eq!(m.len(), 2);
    }

    #[test]
    fn perintah_lanjut_dari_file_disaring() {
        assert!(safe_resume("claude --resume 35dca435-228f-44db-ac71-9b16c6ef1eeb"));
        assert!(safe_resume("omp --continue"));
        assert!(safe_resume("claude-kantor --resume 35dca435-228f-44db-ac71-9b16c6ef1eeb"));
        assert!(!safe_resume("claude-x;id --resume 35dca435-228f-44db-ac71-9b16c6ef1eeb"));
        assert!(!safe_resume("claude --resume 35dca435-228f-44db-ac71-9b16c6ef1eeb; rm -rf ~"));
        assert!(!safe_resume("bash -c id"));
    }

    #[test]
    fn blok_kembar_digabung() {
        let d = std::env::temp_dir().join(format!("gembala-uji-{}", std::process::id()));
        // SAFETY: other tests using GEMBALA_CONFIG write the same value (test folder per process)
        unsafe { std::env::set_var("GEMBALA_CONFIG", d.join("config.toml")) };
        let f = path("00000000000000aa-1");
        let r = "claude --resume 35dca435-228f-44db-ac71-9b16c6ef1eeb";
        append(&f, &Block { t: 100, cwd: "~".into(), cmd: "claude".into(), code: 0, secs: 60, resume: Some(r.into()) });
        append(&f, &Block { t: 101, cwd: "~".into(), cmd: "claude".into(), code: 0, secs: 61, resume: None });
        append(&f, &Block { t: 500, cwd: "~".into(), cmd: "ls".into(), code: 0, secs: 0, resume: None });
        let b = load(&stem(&json!({"boot": "00000000000000aa", "pane": 1})).unwrap());
        assert_eq!(b.len(), 2);
        assert_eq!(b[0].resume.as_deref(), Some(r));
        // a pane coming back keeps its file, trimmed to these blocks
        rewrite(&f, &b[1..]);
        assert_eq!(load("00000000000000aa-1").len(), 1);
        let _ = std::fs::remove_file(f);
    }

    #[test]
    fn history_tidak_valid_diabaikan() {
        assert_eq!(stem(&json!("00000000000000aa")).as_deref(), Some("00000000000000aa"));
        assert_eq!(stem(&json!({"boot": "0123456789abcdef", "pane": 3})).as_deref(), Some("0123456789abcdef-3"));
        assert!(stem(&json!({"boot": "../../etc/passwd", "pane": 1})).is_none());
        assert!(stem(&json!({"boot": "0123456789abcdef"})).is_none());
        assert!(stem(&json!("../../etc/passwd")).is_none());
        assert!(stem(&json!("0123456789abcdef/")).is_none());
        assert!(stem(&json!(7)).is_none());
    }

    #[test]
    fn log_pulih_dan_disamarkan() {
        let d = std::env::temp_dir().join(format!("gembala-uji-{}", std::process::id()));
        // SAFETY: other tests using GEMBALA_CONFIG write the same value (test folder per process)
        unsafe { std::env::set_var("GEMBALA_CONFIG", d.join("config.toml")) };
        save_log("0123456789abcdef", b"\x1b[32mok\x1b[m\r\n\x1b[33mghp_abcd1234abcd1234\x1b[m\r\n");
        let (b, _) = load_log("0123456789abcdef").unwrap();
        let t = String::from_utf8(b).unwrap();
        assert!(t.contains("\x1b[32mok") && !t.contains("abcd1234abcd1234"), "{t}");
        assert!(load_log("fedcba9876543210").is_none());
        // an old "Sesi sebelumnya" line at the end of the log doesn't pile up
        let t = String::from_utf8(trim_separator(&[b"ls\r\nok\r\n".as_slice(), &separator(0), &separator(1), b"\r\n"].concat())).unwrap();
        assert_eq!(t, "ls\r\nok\r\n\r\n\r\n\r\n");
        // the blank rows reserved for buttons before the line stay
        let blk = block_text("~", 1, "0", "claude", Some("claude --resume x"));
        let t = String::from_utf8(trim_separator(&[blk.as_bytes(), &separator(0)].concat())).unwrap();
        assert_eq!(t, blk);
        // log from the grid (trailing blank rows gone): the last block's button rows are given back
        let t = String::from_utf8(trim_separator(blk.trim_end().as_bytes())).unwrap();
        assert_eq!(t, blk);
        let _ = std::fs::remove_file(d.join("history/0123456789abcdef.log")); // folder used by another test
    }

    #[test]
    fn render_blok_dan_garis() {
        let b = Block { t: 0, cwd: "~/x".into(), cmd: "claude".into(), code: 0, secs: 75, resume: Some("omp --continue".into()) };
        let t = String::from_utf8(render(&[b])).unwrap();
        assert!(t.contains("~/x · 1 mnt 15 dtk") && t.contains("claude") && t.contains("omp --continue") && t.contains("Sesi sebelumnya"), "{t}");
    }

    #[test]
    fn pemilik_sesi_dari_riwayat() {
        let d = std::env::temp_dir().join(format!("gembala-uji-pemilik-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let id = "9b7c267f-d248-4521-aa3c-fe3293169e49";
        let blk = |t, r: &str| Block { t, cwd: "~".into(), cmd: "claude".into(), code: 0, secs: 1, resume: Some(r.into()) };
        append(&d.join("a-1.jsonl"), &blk(10, &format!("claude --resume {id}")));
        assert_eq!(resume_account_in(&d, id).as_deref(), Some(""));
        append(&d.join("a-2.jsonl"), &blk(20, &format!("claude-kantor --resume {id}")));
        assert_eq!(resume_account_in(&d, id).as_deref(), Some("kantor"));
        append(&d.join("a-3.jsonl"), &blk(30, &format!("claude-utama --resume {id}")));
        assert_eq!(resume_account_in(&d, id).as_deref(), Some(""));
        // foreign shapes and other ids are ignored
        append(&d.join("a-4.jsonl"), &blk(40, &format!("claude-x;id --resume {id}")));
        assert_eq!(resume_account_in(&d, id).as_deref(), Some(""));
        assert_eq!(resume_account_in(&d, "00000000-0000-0000-0000-000000000000"), None);
        std::fs::remove_dir_all(&d).unwrap();
    }
}
