//! FR-62: riwayat blok per pane di disk, supaya blok perintah tetap tampil setelah daemon berhenti atau laptop
//! restart (ala "Previous session" Warp). Hanya baris perintah, folder, waktu, kode keluar, dan perintah lanjut;
//! output tidak pernah ditulis (pengecualian SEC-13).

use serde_json::{Value, json};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

const MAX: usize = 200;
const KEEP_DAYS: u64 = 30;

pub struct Block {
    pub t: u64,
    pub cwd: String,
    pub cmd: String,
    pub code: i64,
    pub secs: u64,
    pub resume: Option<String>,
}

/// Penanda blok zsh di stream PTY: `OSC 133;C;<perintah base64>` dan `OSC 133;D;<kode keluar>`.
pub enum Mark {
    Start(String),
    End(i64),
}

fn dir() -> PathBuf {
    crate::config::path().with_file_name("history")
}

pub fn path(boot: &str, pane: u64) -> PathBuf {
    dir().join(format!("{boot}-{pane}.jsonl"))
}

pub fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Tambah satu blok; gagal tulis diabaikan (riwayat tidak boleh mengganggu pane).
pub fn append(file: &Path, b: &Block) {
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    let Some(parent) = file.parent() else { return };
    let _ = std::fs::DirBuilder::new().recursive(true).mode(0o700).create(parent);
    let line = json!({"t": b.t, "cwd": b.cwd, "cmd": b.cmd, "code": b.code, "secs": b.secs, "resume": b.resume});
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).mode(0o600).open(file) {
        let _ = writeln!(f, "{line}");
    }
}

/// Bentuk perintah lanjut yang dibuat daemon (FR-56); selain itu dibuang.
fn safe_resume(r: &str) -> bool {
    const UUID: &str = "[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}";
    regex_lite::Regex::new(&format!(
        "^(claude --resume {UUID}|codex resume ({UUID}|--last)|opencode --continue|opencode --session [A-Za-z0-9_-]{{1,80}}|omp --continue|omp --resume [A-Za-z0-9-]{{1,80}})$"
    ))
    .is_ok_and(|re| re.is_match(r))
}

fn clean(s: &str) -> String {
    crate::msg::sanitize(&s.chars().take(4096).collect::<String>()).unwrap_or_default()
}

/// `{boot, pane}` dari tata letak UI (tidak tepercaya): 200 blok terakhir pane lama, isinya dibersihkan.
pub fn load(from: &Value) -> Vec<Block> {
    let (Some(boot), Some(pane)) = (from["boot"].as_str(), from["pane"].as_u64()) else { return Vec::new() };
    if boot.len() != 16 || !boot.bytes().all(|c| c.is_ascii_hexdigit()) {
        return Vec::new();
    }
    let Ok(f) = std::fs::File::open(path(boot, pane)) else { return Vec::new() };
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
        .collect()
}

pub fn dur(secs: u64) -> String {
    match secs {
        0..60 => format!("{secs} dtk"),
        60..3600 => format!("{} mnt {} dtk", secs / 60, secs % 60),
        _ => format!("{} jam {} mnt", secs / 3600, secs % 3600 / 60),
    }
}

/// Satu blok seperti FR-56: `<folder> (<durasi>)` redup, perintah tebal, lalu perintah lanjut bila ada.
pub fn block_text(cwd: &str, secs: u64, code: &str, cmd: &str, resume: Option<&str>) -> String {
    let exit = if code == "0" { String::new() } else { format!(" · keluar {code}") };
    let mut t = format!("\x1b[2m{cwd} ({}){exit}\x1b[0m\r\n\x1b[1m{}\x1b[0m\r\n", dur(secs), cmd.replace('\n', "\r\n"));
    if let Some(r) = resume {
        t += &format!("\r\nResume this session with:\r\n{r}\r\n");
    }
    t
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

/// Blok-blok pane lama lalu garis "Sesi sebelumnya", ditulis ke grid sebelum output shell baru.
pub fn render(blocks: &[Block]) -> Vec<u8> {
    let Some(first) = blocks.first() else { return Vec::new() };
    let mut out = String::new();
    for b in blocks {
        out += &block_text(&b.cwd, b.secs, &b.code.to_string(), &b.cmd, b.resume.as_deref());
    }
    out += &format!("\x1b[2m── Sesi sebelumnya · {} ──\x1b[0m\r\n\r\n", date(first.t));
    out.into_bytes()
}

/// Penanda blok dalam potongan output, berurutan.
// ponytail: penanda yang terpotong batas baca PTY terlewat (blok itu tidak tercatat)
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

/// File riwayat yang tidak disentuh lebih dari 30 hari dihapus.
pub fn sweep() {
    let Ok(entries) = std::fs::read_dir(dir()) else { return };
    let limit = std::time::Duration::from_secs(KEEP_DAYS * 86400);
    for e in entries.flatten() {
        if e.metadata().and_then(|m| m.modified()).is_ok_and(|t| t.elapsed().is_ok_and(|d| d > limit)) {
            let _ = std::fs::remove_file(e.path());
        }
    }
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
        assert!(!safe_resume("claude --resume 35dca435-228f-44db-ac71-9b16c6ef1eeb; rm -rf ~"));
        assert!(!safe_resume("bash -c id"));
    }

    #[test]
    fn boot_tidak_valid_diabaikan() {
        assert!(load(&json!({"boot": "../../etc/passwd", "pane": 1})).is_empty());
        assert!(load(&json!({"boot": "0123456789abcdef"})).is_empty());
    }

    #[test]
    fn render_blok_dan_garis() {
        let b = Block { t: 0, cwd: "~/x".into(), cmd: "claude".into(), code: 0, secs: 75, resume: Some("omp --continue".into()) };
        let t = String::from_utf8(render(&[b])).unwrap();
        assert!(t.contains("~/x (1 mnt 15 dtk)") && t.contains("claude") && t.contains("omp --continue") && t.contains("Sesi sebelumnya"), "{t}");
    }
}
