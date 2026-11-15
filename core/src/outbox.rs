//! FR-90, SEC-39: messages waiting in memory (queue, confirm cards) are written to disk so a daemon restart doesn't lose
//! them silently. After a restart they come back as `held` records the user sends or drops by hand; nothing is sent by itself.
//! Only what is needed to show and resend one is stored: no replies' text, no task notes (FR-92 doesn't exist yet).

use crate::bus::{Kind, Msg, Tag};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// a queued message is stale after the queue's own 10 minutes (BRD section 8); a confirm card waits for a human, so it keeps a day
const CARD_KEEP: Duration = Duration::from_secs(86_400);

/// One message that was waiting when the daemon stopped. Panes are named by their history key (FR-62: the same across
/// restarts), never by id: an id from another run, or a new pane with the old name, is not the same pane (FR-90).
#[derive(Clone, Debug, PartialEq)]
pub struct Rec {
    pub id: String,
    pub from_key: String,
    pub from_name: String,
    pub to_key: String,
    pub to_name: String,
    pub body: String,
    pub reply_to: Option<String>,
    pub is_reply: bool,
    pub hop: u32,
    pub at: u64,
    pub tag: Tag,
    /// it was waiting on a confirm card (needs the human's say-so again), not in a queue
    pub card: bool,
    /// the daemon was typing it into the pane when it stopped: it may have arrived, so it is never offered again
    pub maybe: bool,
}

impl Rec {
    /// `keys` = live pane id to history key. A message nobody can resolve again (the harness as sender, a pane that is
    /// gone, no id) isn't kept: it could not be sent safely.
    pub fn from_msg(m: &Msg, keys: &HashMap<u64, String>, card: bool, maybe: bool) -> Option<Rec> {
        let to_key = keys.get(&m.to)?.clone();
        let from_key = match keys.get(&m.from) {
            Some(k) => k.clone(),
            None if m.is_reply && m.from == 0 => String::new(), // a note from Gembala itself
            None => return None,
        };
        if m.id.is_empty() {
            return None;
        }
        let at = m.at.duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        Some(Rec {
            id: m.id.clone(),
            from_key,
            from_name: m.from_name.clone(),
            to_key,
            to_name: m.to_name.clone(),
            body: m.body.clone(),
            reply_to: m.reply_to.clone(),
            is_reply: m.is_reply,
            hop: m.hop,
            at,
            tag: m.tag.clone(),
            card,
            maybe,
        })
    }

    /// back to a message once both panes are found again (`from` = 0 for Gembala's own notes)
    pub fn to_msg(&self, from: (u64, String), to: (u64, String)) -> Msg {
        Msg {
            id: self.id.clone(),
            from: from.0,
            from_name: from.1,
            to: to.0,
            to_name: to.1,
            body: self.body.clone(),
            reply_to: self.reply_to.clone(),
            is_reply: self.is_reply,
            hop: self.hop,
            at: SystemTime::now(),
            tag: self.tag.clone(),
        }
    }

    pub fn expired(&self, now: u64) -> bool {
        let keep = if self.card { CARD_KEEP } else { crate::msg::QUEUE_TTL };
        now.saturating_sub(self.at) > keep.as_secs()
    }

    /// for `msg.list`: the card shows exactly what would be sent (same exception as the confirm card, SEC-12)
    pub fn summary(&self) -> Value {
        json!({"id": self.id, "from": self.from_name, "to": self.to_name, "body": self.body, "maybe": self.maybe, "card": self.card,
               "at": self.at, "kind": self.tag.kind.name(), "task": self.tag.task})
    }

    fn to_json(&self) -> Value {
        json!({"id": self.id, "from_key": self.from_key, "from_name": self.from_name, "to_key": self.to_key, "to_name": self.to_name,
               "body": self.body, "reply_to": self.reply_to, "is_reply": self.is_reply, "hop": self.hop, "at": self.at,
               "kind": self.tag.kind.name(), "task": self.tag.task, "parent": self.tag.parent, "edited": self.tag.edited,
               "card": self.card, "maybe": self.maybe})
    }

    fn from_json(v: &Value) -> Option<Rec> {
        let s = |k: &str| v[k].as_str().map(String::from);
        let kind = [Kind::Request, Kind::Note, Kind::Result].into_iter().find(|k| Some(k.name()) == v["kind"].as_str())?;
        Some(Rec {
            id: s("id").filter(|i| !i.is_empty())?,
            from_key: s("from_key")?,
            from_name: s("from_name")?,
            to_key: s("to_key").filter(|k| !k.is_empty())?,
            to_name: s("to_name")?,
            body: s("body")?,
            reply_to: s("reply_to"),
            is_reply: v["is_reply"].as_bool()?,
            hop: u32::try_from(v["hop"].as_u64()?).ok()?,
            at: v["at"].as_u64()?,
            tag: Tag { kind, task: s("task")?, parent: s("parent"), edited: v["edited"].as_bool().unwrap_or(false) },
            card: v["card"].as_bool().unwrap_or(false),
            maybe: v["maybe"].as_bool().unwrap_or(false),
        })
    }
}

/// `<config folder>/outbox/queue.json`: the folder is made `0700` and the file `0600` by `config::write_atomic` (SEC-39)
pub fn path() -> PathBuf {
    crate::config::path().with_file_name("outbox").join("queue.json")
}

/// The file's text for these records; empty list = empty string (no file)
pub fn encode(recs: &[Rec]) -> String {
    if recs.is_empty() { String::new() } else { Value::Array(recs.iter().map(Rec::to_json).collect()).to_string() }
}

/// Write `text` atomically; empty = remove the file, so no message text stays behind once nothing waits (SEC-39)
pub fn write(path: &std::path::Path, text: &str) -> std::io::Result<()> {
    if text.is_empty() {
        return match std::fs::remove_file(path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        };
    }
    crate::config::write_atomic(path, text.as_bytes())
}

/// Records of the last run. A broken or foreign file reads as nothing: it is never trusted past what `from_json` accepts.
pub fn load(path: &std::path::Path) -> Vec<Rec> {
    let Ok(text) = std::fs::read_to_string(path) else { return Vec::new() };
    match serde_json::from_str::<Value>(&text) {
        Ok(Value::Array(a)) => a.iter().filter_map(Rec::from_json).collect(),
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn rec(id: &str) -> Rec {
        Rec {
            id: id.into(),
            from_key: "aaaaaaaaaaaaaaaa".into(),
            from_name: "claude-1".into(),
            to_key: "bbbbbbbbbbbbbbbb".into(),
            to_name: "omp-1".into(),
            body: "tulis test\nbaris dua".into(),
            reply_to: None,
            is_reply: false,
            hop: 1,
            at: 1_793_491_200,
            tag: Tag { kind: Kind::Note, task: "msg_t1".into(), parent: Some("msg_t1".into()), edited: true },
            card: false,
            maybe: false,
        }
    }

    #[test]
    fn simpan_dan_baca_balik() {
        let dir = std::env::temp_dir().join(format!("gembala-outbox-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("cfg/outbox/queue.json");
        let all = vec![rec("a"), Rec { card: true, maybe: true, is_reply: true, reply_to: Some("x".into()), ..rec("b") }];
        write(&path, &encode(&all)).unwrap();
        assert_eq!(load(&path), all);
        // SEC-39: private folder and file
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(std::fs::metadata(path.parent().unwrap()).unwrap().permissions().mode() & 0o777, 0o700);
        // nothing waits = no file, no text left
        write(&path, &encode(&[])).unwrap();
        assert!(!path.exists());
        write(&path, "").unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn berkas_rusak_atau_asing_dibaca_kosong() {
        let dir = std::env::temp_dir().join(format!("gembala-outbox-bad-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("queue.json");
        for text in ["", "bukan json", "{\"a\":1}", "[1, {\"id\": \"x\"}]"] {
            std::fs::write(&path, text).unwrap();
            assert!(load(&path).is_empty(), "{text}");
        }
        // one good row among bad ones survives
        let good = rec("ok").to_json();
        std::fs::write(&path, json!([{"id": ""}, good]).to_string()).unwrap();
        assert_eq!(load(&path).len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn kedaluwarsa_antre_sepuluh_menit_kartu_sehari() {
        let now = 1_793_491_200 + 3600;
        assert!(rec("a").expired(now), "antre lewat 10 menit");
        assert!(!rec("a").expired(1_793_491_200 + 599));
        assert!(!Rec { card: true, ..rec("a") }.expired(now), "kartu menunggu manusia");
        assert!(Rec { card: true, ..rec("a") }.expired(1_793_491_200 + 86_401));
    }

    #[test]
    fn pesan_yang_tak_bisa_dipulihkan_tidak_disimpan() {
        let keys: HashMap<u64, String> = [(1, "aaaaaaaaaaaaaaaa".to_string()), (2, "bbbbbbbbbbbbbbbb".to_string())].into();
        let m = Msg { id: "m".into(), from: 1, from_name: "a".into(), to: 2, to_name: "b".into(), body: "x".into(), reply_to: None, is_reply: false, hop: 0, at: SystemTime::now(), tag: Tag::default() };
        assert!(Rec::from_msg(&m, &keys, false, false).is_some());
        assert!(Rec::from_msg(&Msg { from: 9, ..m.clone() }, &keys, false, false).is_none(), "pengirim bukan pane (harness)");
        assert!(Rec::from_msg(&Msg { to: 9, ..m.clone() }, &keys, false, false).is_none(), "pane tujuan sudah tidak ada");
        assert!(Rec::from_msg(&Msg { id: String::new(), ..m.clone() }, &keys, false, false).is_none(), "tanpa id tidak bisa dialamati");
        assert!(Rec::from_msg(&Msg { from: 0, is_reply: true, ..m.clone() }, &keys, false, false).is_some_and(|r| r.from_key.is_empty()), "note dari Gembala");
    }
}
