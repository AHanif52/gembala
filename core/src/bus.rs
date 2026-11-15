//! Message engine state (BRD section 8) and audit log (SEC-11). No PTY: the daemon writes to panes.

use serde_json::{Value, json};
use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::io::{self, Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// FR-95: what a message asks of its recipient
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Kind {
    /// do it, then answer with `gembala reply`
    #[default]
    Request,
    /// information only, no answer wanted
    Note,
    /// the answer to a `Request`
    Result,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::Request => "request",
            Kind::Note => "note",
            Kind::Result => "result",
        }
    }
}

/// FR-95 task numbering, FR-89 edit mark; filled by the daemon, never taken from the sender (BRD section 8)
#[derive(Clone, Debug, Default)]
pub struct Tag {
    pub kind: Kind,
    /// id of the first request of the task; empty until the daemon sets it
    pub task: String,
    /// the request the sender was working on
    pub parent: Option<String>,
    /// FR-89: the user changed the body while it was queued
    pub edited: bool,
}

#[derive(Clone, Debug)]
pub struct Msg {
    pub id: String,
    pub from: u64,
    pub from_name: String,
    pub to: u64,
    pub to_name: String,
    /// already through the SEC-7/23 filter
    pub body: String,
    pub reply_to: Option<String>,
    pub is_reply: bool,
    pub hop: u32,
    pub at: SystemTime,
    pub tag: Tag,
}

impl Msg {
    pub fn summary(&self) -> Value {
        let preview: String = crate::git::redact(&self.body).chars().take(80).collect();
        // FR-99 P2: seconds since the epoch, so the "Butuh lu" strip can put the longest wait on top
        let at = self.at.duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        json!({"id": self.id, "from": self.from_name, "to": self.to_name, "hop": self.hop, "reply_to": self.reply_to, "preview": preview, "at": at,
               "kind": self.tag.kind.name(), "task": self.tag.task, "parent": self.tag.parent, "edited": self.tag.edited})
    }
}

pub struct Queued {
    pub msg: Msg,
    pub since: Instant,
    /// already approved by a human (confirm card) before queueing
    pub approved: bool,
}

/// FR-75: an agent sent to an agent not open yet; waits for the human's card (open + send, or reject)
pub struct Open {
    /// `to` = 0 and `to_name` = the name the agent typed until the pane exists
    pub msg: Msg,
    pub profile: String,
    /// "" = the workspace default account (the UI picks it, like any new pane)
    pub account: String,
    pub model: String,
    pub effort: String,
    /// the requester's folder when it asked
    pub cwd: String,
    pub workspace: String,
    pub since: Instant,
}

/// FR-86: an agent asked to close another pane; only the human's card closes it. `msg.to` = the target pane and
/// `msg.to_name` its name when it asked (nothing is typed into it), so `gembala wait --reply <id>` finds the answer.
pub struct Close {
    pub msg: Msg,
    pub since: Instant,
}

#[derive(Default)]
pub struct Engine {
    seq: u64,
    /// messages received so far, for hops and reply owners (capped at the last 1000)
    known: VecDeque<Msg>,
    /// ids written to the recipient's pane (same cap): only these can be replied to (TECH 18 lead 7)
    delivered: VecDeque<String>,
    pub replies: HashMap<String, Msg>,
    pub waiting: HashSet<String>,
    pub undelivered_replies: HashMap<String, Msg>,
    pub reply_alerted: HashSet<String>,
    /// FR-95: ids typed into a pane / ids that ended another way (answered, rejected, cancelled): a request still open
    /// for its recipient is delivered and not closed. Kept in step with `known`.
    delivered: HashSet<String>,
    closed: HashSet<String>,
    pub queues: HashMap<u64, VecDeque<Queued>>,
    /// FR-89: the message `deliver_loop` took off a queue and is deciding on right now; cancel/edit wait for it to settle
    pub limbo: Option<Msg>,
    /// FR-89: cancelled ids, for the sender's `wait --reply` (taken by the first waiter)
    pub cancelled: HashSet<String>,
    sent: HashMap<u64, VecDeque<Instant>>,
    pub paused: bool,
    pub muted: HashSet<u64>,
    /// waiting for a human's Approve/Reject (FR-12)
    pub pending: Vec<Msg>,
    pub opens: Vec<Open>,
    /// FR-86: close requests waiting for the human's card
    pub closes: Vec<Close>,
    /// FR-100: problem reports an agent drafted with `gembala report`; only the user sends them (SEC-40)
    pub reports: Vec<Msg>,
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

    /// Hops are counted by the daemon: hop(reply_to) + 1; an unknown reply_to = a new chain.
    pub fn hop_for(&self, reply_to: Option<&str>) -> u32 {
        reply_to.and_then(|r| self.find(r)).map_or(0, |m| m.hop + 1)
    }

    /// FR-66, FR-95: `request`s typed into `to` that are not answered yet: not a reply, not answered or cancelled, younger
    /// than `max_age`. The newest first. A queued, held, or rejected message was never typed, so it is not here.
    pub fn open_requests(&self, to: u64, max_age: Duration) -> Vec<&Msg> {
        self.known
            .iter()
            .rev()
            .filter(|m| {
                m.to == to && m.tag.kind == Kind::Request && m.reply_to.is_none() && self.delivered.contains(&m.id) && !self.closed.contains(&m.id)
                    && !self.replies.contains_key(&m.id) && m.at.elapsed().is_ok_and(|a| a < max_age)
            })
            .collect()
    }

    /// FR-95: number, task and parent for a new message from `me`. The request `me` is working on (exactly one open) becomes
    /// the parent; more than one and no `parent` = a new task. An explicit `parent` has to be a request sent to `me`.
    pub fn tag_for(&self, me: u64, kind: Kind, parent: Option<&str>, own_id: &str) -> Result<Tag, &'static str> {
        let open = self.open_requests(me, Duration::from_secs(86_400));
        let up = match parent {
            Some(p) => Some(self.find(p).filter(|m| m.to == me && m.tag.kind == Kind::Request).ok_or("bad_params")?),
            None if open.len() == 1 => Some(open[0]),
            None => None,
        };
        Ok(match up {
            Some(up) => Tag { kind, task: if up.tag.task.is_empty() { up.id.clone() } else { up.tag.task.clone() }, parent: Some(up.id.clone()), edited: false },
            None => Tag { kind, task: own_id.to_string(), parent: None, edited: false },
        })
    }

    pub fn mark_delivered(&mut self, id: &str) {
        self.delivered.push_back(id.to_string());
        while self.delivered.len() > 1000 {
            self.delivered.pop_front();
        }
    }

    pub fn is_delivered(&self, id: &str) -> bool {
        self.delivered.iter().any(|d| d == id)
    }

    /// FR-95: this request is answered (or ended another way), whatever happens to the reply later
    pub fn close(&mut self, id: &str) {
        self.closed.insert(id.to_string());
    }

    pub fn remember(&mut self, m: &Msg) {
        // Results cannot start another reply chain.
        if m.is_reply { return; }
        self.known.push_back(m.clone());
        while self.known.len() > 1000 {
            if let Some(old) = self.known.pop_front() {
                self.delivered.remove(&old.id);
                self.closed.remove(&old.id);
            }
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
        match kind {
            "sent" => drop(self.delivered.insert(m.id.clone())),
            "rejected" | "cancelled" => drop(self.closed.insert(m.id.clone())),
            _ => {}
        }
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

    pub fn overdue_replies(&mut self) -> Vec<Msg> {
        let old: Vec<Msg> = self.undelivered_replies.values().filter(|m| m.at.elapsed().is_ok_and(|age| age > Duration::from_secs(120))).cloned().collect();
        old.into_iter().filter(|m| self.reply_alerted.insert(m.id.clone())).collect()
    }
}

/// FR-89: what cancel/edit found
pub enum Held {
    /// it was queued (or a confirm card) and is now cancelled / changed; the message as it is now
    Msg(Box<Msg>),
    /// the delivery loop holds it this instant: ask again in a moment
    InFlight,
    /// it was typed into the pane already (or ended another way); never touched
    Gone,
}

impl Engine {
    fn held_ref(&self, id: &str) -> Option<&Msg> {
        self.queues.values().flatten().map(|q| &q.msg).chain(self.pending.iter()).find(|m| m.id == id)
    }

    /// who may change it: `by` = the asking pane, `None` = the user through the UI channel (any message)
    fn owner_check(by: Option<u64>, m: &Msg) -> Result<(), &'static str> {
        if by.is_some_and(|p| p != m.from) { Err("not_allowed") } else { Ok(()) }
    }

    /// Where is `id` right now? Decided under the caller's engine lock, so a message is never reported as sent while it
    /// still waits, nor changed after it left.
    fn locate(&self, id: &str, by: Option<u64>) -> Result<Option<Held>, &'static str> {
        if let Some(m) = self.held_ref(id) {
            return Self::owner_check(by, m).map(|()| None);
        }
        if let Some(m) = self.limbo.as_ref().filter(|m| m.id == id) {
            return Self::owner_check(by, m).map(|()| Some(Held::InFlight));
        }
        let m = self.find(id).ok_or("no_such_msg")?;
        Self::owner_check(by, m).map(|()| Some(Held::Gone))
    }

    pub fn cancel(&mut self, id: &str, by: Option<u64>) -> Result<Held, &'static str> {
        if let Some(h) = self.locate(id, by)? {
            return Ok(h);
        }
        let queued = self.queues.values_mut().find_map(|q| q.iter().position(|x| x.msg.id == id).and_then(|i| q.remove(i))).map(|q| q.msg);
        let m = match queued {
            Some(m) => m,
            None => self.pending.remove(self.pending.iter().position(|m| m.id == id).ok_or("no_such_msg")?),
        };
        self.cancelled.insert(m.id.clone());
        self.undelivered_replies.remove(id);
        Ok(Held::Msg(Box::new(m)))
    }

    /// New text for a queued message (already through `msg::sanitize`). Place in the queue and age stay; a change by the
    /// user is marked, one by the sender is not.
    pub fn edit(&mut self, id: &str, body: String, by: Option<u64>) -> Result<Held, &'static str> {
        if let Some(h) = self.locate(id, by)? {
            return Ok(h);
        }
        let m = self.queues.values_mut().flatten().map(|q| &mut q.msg).chain(self.pending.iter_mut()).find(|m| m.id == id).ok_or("no_such_msg")?;
        m.body = body;
        m.tag.edited = by.is_none();
        Ok(Held::Msg(Box::new(m.clone())))
    }
}

/// SEC-11: append-only audit log per day, `0600`, HMAC-SHA256 of the content with a random per-install key,
/// 80-character excerpt after SEC-12 redaction, 14-day retention.
pub struct Audit {
    dir: PathBuf,
    key: Vec<u8>,
}

impl Audit {
    pub fn open(dir: PathBuf) -> io::Result<Audit> {
        crate::config::write_atomic(&dir.join(".probe"), b"")?; // creates the 0700 folders along the way
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

    fn waiting(e: &Engine, to: u64, age: Duration) -> Option<&Msg> {
        e.open_requests(to, age).into_iter().next()
    }

    fn msg(id: &str, hop: u32) -> Msg {
        Msg {
            id: id.into(),
            from: 1,
            from_name: "claude-1".into(),
            to: 2,
            to_name: "oc-1".into(),
            body: "kunci sk-ant-rahasia jangan bocor".into(),
            reply_to: None,
            is_reply: false,
            hop,
            at: SystemTime::now(),
            tag: Tag::default(),
        }
    }

    /// FR-99 P2: a pending card / FR-75 request carries its age, so the strip can put the longest wait on top and never
    /// chains a message written a minute ago above one from an hour ago.
    #[test]
    fn ringkasan_pesan_membawa_umur() {
        let old = Msg { at: SystemTime::now() - Duration::from_secs(3600), ..msg("lama", 0) };
        let s = old.summary();
        let at = s["at"].as_u64().unwrap();
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
        assert!((now - 3700..=now - 3500).contains(&at), "{s}");
        assert_eq!(s["id"], "lama");
        assert!(!s["preview"].as_str().unwrap().contains("sk-ant"), "{s}");
    }

    /// FR-66: only real messages that aren't answered or held get an automatic answer
    #[test]
    fn pesan_menunggu_balasan() {
        let mut e = Engine::default();
        let day = Duration::from_secs(86_400);
        let sent = |e: &mut Engine, m: &Msg| drop(e.log("sent", m, None));
        let a = msg("a", 0);
        e.remember(&a);
        assert!(waiting(&e, 2, day).is_none(), "belum diketik ke pane");
        sent(&mut e, &a);
        e.remember(&Msg { reply_to: Some("x".into()), ..msg("balasan", 1) });
        assert_eq!(waiting(&e, 2, day).map(|m| m.id.as_str()), Some("a"));
        assert!(waiting(&e, 1, day).is_none(), "bukan penerimanya");
        e.replies.insert("a".into(), msg("r", 1));
        assert!(waiting(&e, 2, day).is_none(), "sudah dibalas");
        e.replies.clear();
        assert!(waiting(&e, 2, day).is_some());
        e.close("a");
        assert!(waiting(&e, 2, day).is_none(), "balasannya sudah diambil, tetap tidak menunggu");
        let held = msg("tertahan", 0);
        e.remember(&held);
        e.pending.push(held);
        assert!(waiting(&e, 2, day).is_none(), "menunggu persetujuan manusia");
        let old = Msg { at: SystemTime::now() - Duration::from_secs(3600), ..msg("lama", 0) };
        e.remember(&old);
        sent(&mut e, &old);
        assert!(waiting(&e, 2, Duration::from_secs(1800)).is_none(), "terlalu lama");
        let note = Msg { tag: Tag { kind: Kind::Note, ..Tag::default() }, ..msg("catatan", 0) };
        e.remember(&note);
        sent(&mut e, &note);
        assert_eq!(e.open_requests(2, day).len(), 1, "note tidak menunggu balasan (FR-95)");
    }

    /// FR-95: the one open request becomes the parent; two or none make a new task; `parent` must be a request to the sender
    #[test]
    fn nomor_tugas_dan_parent() {
        let mut e = Engine::default();
        let day = Duration::from_secs(86_400);
        // pane 2 got two requests from pane 1
        for id in ["a", "b"] {
            let m = Msg { tag: Tag { task: id.into(), ..Tag::default() }, ..msg(id, 0) };
            e.remember(&m);
            drop(e.log("sent", &m, None));
            if id == "a" {
                let t = e.tag_for(2, Kind::Request, None, "n1").unwrap();
                assert_eq!((t.task.as_str(), t.parent.as_deref()), ("a", Some("a")), "tepat satu = anak otomatis");
            }
        }
        let t = e.tag_for(2, Kind::Request, None, "n2").unwrap();
        assert_eq!((t.task.as_str(), t.parent.as_deref()), ("n2", None), "dua terbuka tanpa --parent = tugas baru");
        let t = e.tag_for(2, Kind::Note, Some("b"), "n3").unwrap();
        assert_eq!((t.kind, t.task.as_str(), t.parent.as_deref()), (Kind::Note, "b", Some("b")));
        assert_eq!(e.tag_for(2, Kind::Request, Some("tidak-ada"), "n4").unwrap_err(), "bad_params");
        assert_eq!(e.tag_for(1, Kind::Request, Some("a"), "n5").unwrap_err(), "bad_params", "bukan permintaan ke pengirim");
        assert!(e.open_requests(2, day).len() == 2);
        e.close("a");
        let t = e.tag_for(2, Kind::Request, None, "n6").unwrap();
        assert_eq!(t.parent.as_deref(), Some("b"), "yang sudah dijawab tidak dihitung");
    }

    fn queued(e: &mut Engine, id: &str) {
        e.queues.entry(2).or_default().push_back(Queued { msg: msg(id, 0), since: Instant::now(), approved: false });
    }

    /// FR-89: only the sender (or the user) can cancel or edit; sent = untouched; a message the delivery loop holds this
    /// instant is "in flight", not "sent", until it settles
    #[test]
    fn batal_dan_ubah_pesan_antre() {
        let mut e = Engine::default();
        queued(&mut e, "a");
        e.remember(&msg("a", 0));
        assert_eq!(e.cancel("a", Some(9)).err(), Some("not_allowed"), "bukan pengirim");
        assert_eq!(e.edit("a", "x".into(), Some(9)).err(), Some("not_allowed"));
        assert!(matches!(e.edit("a", "baru".into(), Some(1)), Ok(Held::Msg(m)) if m.body == "baru" && !m.tag.edited), "pengirim: tanpa tanda");
        assert!(matches!(e.edit("a", "versi pengguna".into(), None), Ok(Held::Msg(m)) if m.tag.edited));
        assert_eq!(e.queues[&2][0].msg.body, "versi pengguna");
        // delivery loop took it: neither Gone nor changed
        let q = e.queues.get_mut(&2).unwrap().pop_front().unwrap();
        e.limbo = Some(q.msg.clone());
        assert!(matches!(e.cancel("a", None), Ok(Held::InFlight)));
        assert!(matches!(e.edit("a", "z".into(), None), Ok(Held::InFlight)));
        // it went back (target busy): now it can be cancelled
        e.queues.get_mut(&2).unwrap().push_front(q);
        e.limbo = None;
        assert!(matches!(e.cancel("a", Some(1)), Ok(Held::Msg(m)) if m.id == "a"));
        assert!(e.cancelled.contains("a") && e.queue_len(2) == 0);
        // it was typed into the pane: refused, body kept
        assert!(matches!(e.cancel("a", None), Ok(Held::Gone)));
        assert!(matches!(e.edit("a", "z".into(), None), Ok(Held::Gone)));
        assert_eq!(e.cancel("tidak-ada", None).err(), Some("no_such_msg"));
        // a confirm card is a queued message too
        e.remember(&msg("kartu", 0));
        e.pending.push(msg("kartu", 0));
        assert!(matches!(e.edit("kartu", "ubah".into(), None), Ok(Held::Msg(_))));
        assert!(matches!(e.cancel("kartu", None), Ok(Held::Msg(_))) && e.pending.is_empty());
    }

    #[test]
    fn hanya_pesan_terkirim_bisa_dibalas() {
        let mut e = Engine::default();
        e.remember(&msg("tertolak", 0));
        assert!(!e.is_delivered("tertolak"));
        e.mark_delivered("tertolak");
        assert!(e.is_delivered("tertolak"));
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
    fn balasan_tertahan_diperingatkan_sekali_setelah_dua_menit() {
        let mut e = Engine::default();
        e.undelivered_replies.insert("new".into(), msg("new", 1));
        e.undelivered_replies.insert("old".into(), Msg { at: SystemTime::now() - Duration::from_secs(121), ..msg("old", 1) });
        e.undelivered_replies.insert("delivered".into(), Msg { at: SystemTime::now() - Duration::from_secs(121), ..msg("delivered", 1) });
        e.undelivered_replies.remove("delivered");
        let old = e.overdue_replies();
        assert_eq!(old.len(), 1);
        assert_eq!(old[0].id, "old");
        assert!(e.overdue_replies().is_empty());
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
