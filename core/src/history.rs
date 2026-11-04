//! FR-62: riwayat blok per pane di disk, supaya blok perintah tetap tampil setelah daemon berhenti atau laptop
//! restart (ala "Previous session" Warp): baris perintah, folder, waktu, kode keluar, dan perintah lanjut, plus log
//! layar utama yang disamarkan (`.log`, pengecualian SEC-13).

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

fn log_path(from: &Value) -> Option<PathBuf> {
    let (boot, pane) = (from["boot"].as_str()?, from["pane"].as_u64()?);
    (boot.len() == 16 && boot.bytes().all(|c| c.is_ascii_hexdigit())).then(|| dir().join(format!("{boot}-{pane}.log")))
}

/// Log layar pane (baris berwarna) ditulis utuh lewat file sementara; gagal tulis diabaikan.
pub fn save_log(boot: &str, pane: u64, bytes: &[u8]) {
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    let Some(file) = log_path(&json!({"boot": boot, "pane": pane})) else { return };
    let _ = std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir());
    let tmp = file.with_extension("tmp");
    // disamarkan per potongan di antara kode warna: `\x1b[32mghp_…` tetap kena (huruf `m` bukan awal kata)
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

/// Log pane lama dan waktu terakhir ditulis, bila ada.
pub fn load_log(from: &Value) -> Option<(Vec<u8>, u64)> {
    let file = log_path(from)?;
    let meta = std::fs::metadata(&file).ok().filter(|m| m.len() <= LOG_MAX)?;
    let t = meta.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs();
    Some((trim_separator(&std::fs::read(file).ok()?), t))
}

/// Garis "Sesi sebelumnya" (dan baris kosong) di ujung log dibuang: pane yang tidak dipakai sejak restart terakhir
/// tidak menumpuk garis, cukup satu yang terbaru
fn trim_separator(log: &[u8]) -> Vec<u8> {
    let text = String::from_utf8_lossy(log);
    let ansi = regex_lite::Regex::new("\x1b\\[[0-9;?]*[A-Za-z]").unwrap();
    let plain = |l: &str| ansi.replace_all(l, "").trim().to_string();
    let mut lines: Vec<&str> = text.split("\r\n").collect();
    // garis "Sesi sebelumnya" yang hanya diikuti baris kosong dibuang (bisa bertumpuk)
    let last_sep = |lines: &[&str]| {
        lines.iter().rposition(|l| plain(l).trim_start_matches(['─', ' ']).starts_with("Sesi sebelumnya")).filter(|&i| lines[i + 1..].iter().all(|l| plain(l).is_empty()))
    };
    while let Some(i) = last_sep(&lines) {
        lines.truncate(i);
    }
    // ujung log selalu tiga baris kosong, sama dengan block_text: baris tombol blok terakhir (dua baris terminal, digambar
    // UI) tidak tertutup garis baru. Log disimpan dari grid tanpa baris kosong di ujung, jadi jangan andalkan isinya
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

/// Garis pemisah di bawah log lama
pub fn separator(t: u64) -> Vec<u8> {
    format!("{GUTTER}\x1b[2mSesi sebelumnya · {}\x1b[0m\r\n\r\n", date(t)).into_bytes()
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
        "^(claude(-[a-z0-9-]{{1,32}})? --resume {UUID}|codex resume ({UUID}|--last)|opencode --continue|opencode --session [A-Za-z0-9_-]{{1,80}}|omp --continue|omp --resume [A-Za-z0-9-]{{1,80}})$"
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
        .fold(Vec::new(), |mut out: Vec<Block>, b| {
            // satu sesi agent yang tercatat dua kali (blok shell + blok agent, sebelum perbaikan 4 Nov 2026):
            // mulai dan durasinya sama; yang punya perintah lanjut dipakai
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

/// Titik dan garis linimasa digambar UI di luar grid (pen layar 02), jadi teks blok mulai di kolom 0
const GUTTER: &str = "";

/// Satu blok seperti FR-56 (linimasa): `<ikon> <folder> · <durasi>[ · keluar n]` redup, perintah tebal,
/// perintah lanjut redup bila ada (lalu satu baris untuk tombol UI), lalu satu baris kosong sebagai jarak antarblok.
pub fn block_text(cwd: &str, secs: u64, code: &str, cmd: &str, resume: Option<&str>) -> String {
    let exit = if code == "0" { String::new() } else { format!(" · keluar {code}") };
    // ikon agent berwarna seperti di sidebar (FR-52); perintah lain tanpa ikon
    let first = cmd.split_whitespace().next().unwrap_or_default();
    let icon = [("claude", "\x1b[38;2;217;119;87m✳"), ("omp", "\x1b[38;2;168;85;247mπ"), ("codex", "\x1b[38;2;16;163;127m◎"), ("opencode", "\x1b[38;2;96;165;250m◇")]
        .iter()
        .find(|(a, _)| first == *a || first.starts_with(&format!("{a}-")))
        .map_or(String::new(), |(_, g)| format!("{g}\x1b[0m\x1b[2m "));
    let cmd = cmd.replace('\n', &format!("\r\n{GUTTER}"));
    let mut t = format!("{GUTTER}\x1b[2m{icon}{cwd} · {}{exit}\x1b[0m\r\n{GUTTER}\x1b[1m{cmd}\x1b[0m\r\n", dur(secs));
    // pen layar 02: perintah lanjut redup tanpa tanda, lalu dua baris untuk tombol yang digambar UI
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

/// Blok-blok pane lama lalu garis "Sesi sebelumnya", ditulis ke grid sebelum output shell baru.
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

/// File riwayat dan log yang tidak disentuh lebih dari 14 hari dihapus.
pub fn sweep() {
    let Ok(entries) = std::fs::read_dir(dir()) else { return };
    let limit = std::time::Duration::from_secs(KEEP_DAYS * 86400);
    for e in entries.flatten() {
        if e.metadata().and_then(|m| m.modified()).is_ok_and(|t| t.elapsed().is_ok_and(|d| d > limit)) {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// FR-61: akun pemilik sesi Claude ditebak dari perintah lanjut di riwayat (`claude-kantor --resume <id>` = "kantor",
/// `claude --resume <id>` = "" Utama), blok terbaru menang. Untuk sesi yang dimulai sebelum pemilik dicatat.
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
        // SAFETY: test lain yang memakai GEMBALA_CONFIG menulis nilai yang sama (folder uji per proses)
        unsafe { std::env::set_var("GEMBALA_CONFIG", d.join("config.toml")) };
        let f = path("00000000000000aa", 1);
        let r = "claude --resume 35dca435-228f-44db-ac71-9b16c6ef1eeb";
        append(&f, &Block { t: 100, cwd: "~".into(), cmd: "claude".into(), code: 0, secs: 60, resume: Some(r.into()) });
        append(&f, &Block { t: 101, cwd: "~".into(), cmd: "claude".into(), code: 0, secs: 61, resume: None });
        append(&f, &Block { t: 500, cwd: "~".into(), cmd: "ls".into(), code: 0, secs: 0, resume: None });
        let b = load(&json!({"boot": "00000000000000aa", "pane": 1}));
        assert_eq!(b.len(), 2);
        assert_eq!(b[0].resume.as_deref(), Some(r));
        let _ = std::fs::remove_file(f);
    }

    #[test]
    fn boot_tidak_valid_diabaikan() {
        assert!(load(&json!({"boot": "../../etc/passwd", "pane": 1})).is_empty());
        assert!(load(&json!({"boot": "0123456789abcdef"})).is_empty());
    }

    #[test]
    fn log_pulih_dan_disamarkan() {
        let d = std::env::temp_dir().join(format!("gembala-uji-{}", std::process::id()));
        // SAFETY: test lain yang memakai GEMBALA_CONFIG menulis nilai yang sama (folder uji per proses)
        unsafe { std::env::set_var("GEMBALA_CONFIG", d.join("config.toml")) };
        save_log("0123456789abcdef", 7, b"\x1b[32mok\x1b[m\r\n\x1b[33mghp_abcd1234abcd1234\x1b[m\r\n");
        let (b, _) = load_log(&json!({"boot": "0123456789abcdef", "pane": 7})).unwrap();
        let t = String::from_utf8(b).unwrap();
        assert!(t.contains("\x1b[32mok") && !t.contains("abcd1234abcd1234"), "{t}");
        assert!(load_log(&json!({"boot": "../../etc/passwd", "pane": 7})).is_none());
        // garis "Sesi sebelumnya" lama di ujung log tidak menumpuk
        let t = String::from_utf8(trim_separator(&[b"ls\r\nok\r\n".as_slice(), &separator(0), &separator(1), b"\r\n"].concat())).unwrap();
        assert_eq!(t, "ls\r\nok\r\n\r\n\r\n\r\n");
        // baris kosong jatah tombol sebelum garis tetap ada
        let blk = block_text("~", 1, "0", "claude", Some("claude --resume x"));
        let t = String::from_utf8(trim_separator(&[blk.as_bytes(), &separator(0)].concat())).unwrap();
        assert_eq!(t, blk);
        // log dari grid (baris kosong di ujung hilang): jatah tombol blok terakhir dikembalikan
        let t = String::from_utf8(trim_separator(blk.trim_end().as_bytes())).unwrap();
        assert_eq!(t, blk);
        let _ = std::fs::remove_file(d.join("history/0123456789abcdef-7.log")); // folder dipakai test lain
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
        // bentuk asing dan id lain diabaikan
        append(&d.join("a-4.jsonl"), &blk(40, &format!("claude-x;id --resume {id}")));
        assert_eq!(resume_account_in(&d, id).as_deref(), Some(""));
        assert_eq!(resume_account_in(&d, "00000000-0000-0000-0000-000000000000"), None);
        std::fs::remove_dir_all(&d).unwrap();
    }
}
