//! Keadaan mesin pesan (BRD bagian 8) dan log audit (SEC-11). Tanpa PTY: daemon yang menulis ke pane.

use serde_json::{Value, json};
use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::io::{self, Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[derive(Clone, Debug)]
pub struct Msg {
    pub id: String,
    pub from: u64,
    pub from_name: String,
    pub to: u64,
    pub to_name: String,
    /// sudah lewat penyaring SEC-7/23
    pub body: String,
    pub reply_to: Option<String>,
    pub hop: u32,
    pub at: SystemTime,
}

impl Msg {
    pub fn summary(&self) -> Value {
        let preview: String = crate::git::redact(&self.body).chars().take(80).collect();
        json!({"id": self.id, "from": self.from_name, "to": self.to_name, "hop": self.hop, "reply_to": self.reply_to, "preview": preview})
    }
}

pub struct Queued {
    pub msg: Msg,
    pub since: Instant,
    /// sudah disetujui manusia (kartu konfirmasi) sebelum antre
    pub approved: bool,
}

#[derive(Default)]
pub struct Engine {
    seq: u64,
    /// pesan yang pernah diterima, untuk hop dan pemilik balasan (dibatasi 1000 terakhir)
    known: VecDeque<Msg>,
    pub replies: HashMap<String, Msg>,
    pub queues: HashMap<u64, VecDeque<Queued>>,
    sent: HashMap<u64, VecDeque<Instant>>,
    pub paused: bool,
    pub muted: HashSet<u64>,
    /// menunggu Setujui/Tolak manusia (FR-12)
    pub pending: Vec<Msg>,
    pub history: VecDeque<Value>,
}

impl Engine {
    pub fn new_id(&mut self, salt: &str) -> String {
        self.seq += 1;
        format!("msg_{}{}", &salt[..salt.len().min(4)], self.seq)
    }

    pub fn find(&self, id: &str) -> Option<&Msg> {
        self.known.iter().rev().find(|m| m.id == id)
    }

    /// Hop dihitung daemon: hop(reply_to) + 1; reply_to yang tidak dikenal = rantai baru.
    pub fn hop_for(&self, reply_to: Option<&str>) -> u32 {
        reply_to.and_then(|r| self.find(r)).map_or(0, |m| m.hop + 1)
    }

    pub fn remember(&mut self, m: &Msg) {
        self.known.push_back(m.clone());
        while self.known.len() > 1000 {
            self.known.pop_front();
        }
    }

    pub fn sent_last_minute(&mut self, from: u64) -> usize {
        let q = self.sent.entry(from).or_default();
        while q.front().is_some_and(|t| t.elapsed() > Duration::from_secs(60)) {
            q.pop_front();
        }
        q.len()
    }

    pub fn note_sent(&mut self, from: u64) {
        self.sent.entry(from).or_default().push_back(Instant::now());
    }

    pub fn log(&mut self, kind: &str, m: &Msg, reason: Option<&str>) -> Value {
        let ev = json!({"kind": kind, "reason": reason, "msg": m.summary(), "at": m.at.duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)});
        self.history.push_back(ev.clone());
        while self.history.len() > 200 {
            self.history.pop_front();
        }
        ev
    }

    pub fn queue_len(&self, to: u64) -> usize {
        self.queues.get(&to).map_or(0, VecDeque::len)
    }
}

/// SEC-11: log audit append-only per hari, `0600`, HMAC-SHA256 isi dengan kunci acak per instalasi,
/// cuplikan 80 karakter setelah redaksi SEC-12, retensi 14 hari.
pub struct Audit {
    dir: PathBuf,
    key: Vec<u8>,
}

impl Audit {
    pub fn open(dir: PathBuf) -> io::Result<Audit> {
        crate::config::write_atomic(&dir.join(".probe"), b"")?; // membuat folder 0700 berantai
        let _ = fs::remove_file(dir.join(".probe"));
        let key_path = dir.join("audit.key");
        let key = match fs::read(&key_path) {
            Ok(k) if k.len() == 32 => k,
            _ => {
                let mut k = vec![0u8; 32];
                fs::File::open("/dev/urandom")?.read_exact(&mut k)?;
                crate::config::write_atomic(&key_path, &k)?;
                k
            }
        };
        let a = Audit { dir, key };
        a.prune();
        Ok(a)
    }

    fn prune(&self) {
        let Ok(entries) = fs::read_dir(&self.dir) else { return };
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let old = e.metadata().and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_some_and(|d| d > Duration::from_secs(14 * 86_400));
            if name.starts_with("audit-") && name.ends_with(".jsonl") && old {
                let _ = fs::remove_file(e.path());
            }
        }
    }

    pub fn write(&self, kind: &str, m: &Msg, reason: Option<&str>) {
        use hmac::{Hmac, KeyInit, Mac};
        let Ok(mut mac) = Hmac::<sha2::Sha256>::new_from_slice(&self.key) else { return };
        mac.update(m.body.as_bytes());
        let tag: String = mac.finalize().into_bytes().iter().map(|b| format!("{b:02x}")).collect();
        let secs = m.at.duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let snippet: String = crate::git::redact(&m.body).chars().take(80).collect();
        let row = json!({"at": secs, "id": m.id, "from": m.from_name, "to": m.to_name, "hop": m.hop,
                         "result": kind, "reason": reason, "hmac": tag, "snippet": snippet});
        let path = self.dir.join(format!("audit-{}.jsonl", crate::msg::ymd(secs)));
        let f = fs::OpenOptions::new().create(true).append(true).mode(0o600).open(path);
        if let Ok(mut f) = f {
            let _ = writeln!(f, "{row}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(id: &str, hop: u32) -> Msg {
        Msg {
            id: id.into(),
            from: 1,
            from_name: "claude-1".into(),
            to: 2,
            to_name: "oc-1".into(),
            body: "kunci sk-ant-rahasia jangan bocor".into(),
            reply_to: None,
            hop,
            at: SystemTime::now(),
        }
    }

    #[test]
    fn hop_dan_laju() {
        let mut e = Engine::default();
        e.remember(&msg("a", 0));
        e.remember(&msg("b", 2));
        assert_eq!(e.hop_for(Some("a")), 1);
        assert_eq!(e.hop_for(Some("b")), 3);
        assert_eq!(e.hop_for(Some("tidak-ada")), 0);
        for _ in 0..3 {
            e.note_sent(1);
        }
        assert_eq!(e.sent_last_minute(1), 3);
        assert_eq!(e.sent_last_minute(2), 0);
    }

    #[test]
    fn audit_tersamar_dan_privat() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("gembala-audit-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let a = Audit::open(dir.join("audit")).unwrap();
        a.write("sent", &msg("m1", 0), None);
        let f = fs::read_dir(dir.join("audit")).unwrap().flatten().find(|e| e.file_name().to_string_lossy().starts_with("audit-")).unwrap();
        let text = fs::read_to_string(f.path()).unwrap();
        assert!(!text.contains("sk-ant-rahasia") && text.contains("\"hmac\""), "{text}");
        assert_eq!(f.metadata().unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(fs::metadata(dir.join("audit/audit.key")).unwrap().permissions().mode() & 0o777, 0o600);
        let _ = fs::remove_dir_all(&dir);
    }
}
