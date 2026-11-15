//! Message engine state (BRD section 8) and audit log (SEC-11). No PTY: the daemon writes to panes.

use serde_json::{Value, json};
use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::io::{self, Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// A message typed into an agent counts as the parent of what that agent sends next for this long (hop inheritance)
const HOP_WINDOW: Duration = Duration::from_secs(600);

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
#[derive(Clone, Debug, Default, PartialEq)]
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
               "kind": self.tag.kind.name(), "task": self.tag.task, "parent": self.tag.parent, "edited": self.tag.edited,
               "outcome": (self.tag.kind == Kind::Result).then(|| result_status(&self.body).unwrap_or("unspecified"))})
    }
}

/// FR-95 (5), FR-91: the outcome a `result` states itself, `Status: done|partial|blocked` on one of its first lines. Nothing is
/// guessed from prose: no such line = `None` (shown as "nggak disebut").
pub fn result_status(body: &str) -> Option<&'static str> {
    body.lines().take(6).find_map(|l| {
        let v = l.trim().trim_start_matches(['*', '#', '-', ' ']).to_ascii_lowercase();
        let v = v.strip_prefix("status")?.trim_start_matches(['*', ' ']).strip_prefix(':')?.trim().trim_matches('*').trim().to_string();
        ["done", "partial", "blocked"].into_iter().find(|s| v == *s || v.starts_with(&format!("{s} ")) || v.starts_with(&format!("{s}.")))
    })
}

/// FR-96: how long results of one group wait for the slowest helper
pub const GROUP_HOLD: Duration = Duration::from_secs(30 * 60);

/// FR-96: where one request to a helper stands
#[derive(Clone, Debug, PartialEq)]
pub enum Fin {
    Waiting,
    /// its `result` is held until the whole group has an end
    Answered { rid: String, body: String },
    /// the owner got it already (an explicit `wait --reply`, or a group of one)
    Delivered,
    /// failed or ended: rejected, expired, cancelled, pane closed, agent exited (reason as a code)
    Ended(String),
    /// the owner cancelled it itself: ends the wait, nothing to tell
    Silent,
}

#[derive(Clone, Debug)]
pub struct Member {
    pub id: String,
    pub to: u64,
    pub to_name: String,
    pub fin: Fin,
}

/// FR-96: requests one owner sent while serving the same parent request (or none) and before the first was answered
pub struct Group {
    owner: u64,
    parent: Option<String>,
    task: String,
    pub members: Vec<Member>,
    last_request: Instant,
}

/// FR-96: what goes to the owner as one message
#[derive(Debug)]
pub struct Flush {
    pub owner: u64,
    pub task: String,
    /// id of the first request, the one the combined message "replies" to
    pub first: String,
    pub items: Vec<Member>,
    /// helpers still waited on when the hold ran out
    pub pending: Vec<String>,
}

impl Group {
    fn flush(self, pending: bool) -> Flush {
        let first = self.members.first().map(|m| m.id.clone()).unwrap_or_default();
        let (done, wait): (Vec<Member>, Vec<Member>) = self.members.into_iter().partition(|m| m.fin != Fin::Waiting);
        Flush { owner: self.owner, task: self.task, first, items: done, pending: if pending { wait.into_iter().map(|m| m.to_name).collect() } else { vec![] } }
    }
}

impl Engine {
    /// FR-96: a `request` an agent sent is followed until it has an end
    pub fn track(&mut self, m: &Msg) {
        if m.tag.kind != Kind::Request || m.is_reply || m.from == 0 {
            return;
        }
        let member = Member { id: m.id.clone(), to: m.to, to_name: m.to_name.clone(), fin: Fin::Waiting };
        match self.groups.iter_mut().find(|g| g.owner == m.from && g.parent == m.tag.parent && g.members.iter().all(|x| x.fin == Fin::Waiting)) {
            Some(g) => {
                g.members.push(member);
                g.last_request = Instant::now();
            }
            None => self.groups.push(Group { owner: m.from, parent: m.tag.parent.clone(), task: m.tag.task.clone(), members: vec![member], last_request: Instant::now() }),
        }
        if self.groups.len() > 200 {
            self.groups.remove(0);
        }
    }

    /// FR-96: `id` is a request still followed
    pub fn is_tracked(&self, id: &str) -> bool {
        self.groups.iter().any(|g| g.members.iter().any(|m| m.id == id && m.fin == Fin::Waiting))
    }

    /// FR-96: the group `id` belongs to fans out to more than one helper (its results are held). Two requests to the same
    /// helper are a queue, not a fan-out: each answer goes straight back.
    pub fn grouped(&self, id: &str) -> bool {
        self.groups.iter().any(|g| {
            g.members.iter().any(|m| m.id == id && m.fin == Fin::Waiting) && g.members.iter().any(|m| m.to_name != g.members[0].to_name)
        })
    }

    /// FR-96: `id` got an end. `Some` when that was the last one the group waited for.
    pub fn settle(&mut self, id: &str, fin: Fin) -> Option<Flush> {
        let gi = self.groups.iter().position(|g| g.members.iter().any(|m| m.id == id && m.fin == Fin::Waiting))?;
        self.groups[gi].members.iter_mut().find(|m| m.id == id)?.fin = fin;
        if self.groups[gi].members.iter().all(|m| m.fin != Fin::Waiting) {
            return Some(self.groups.remove(gi).flush(false));
        }
        None
    }

    /// FR-96: a held `result` an explicit `wait --reply <id>` asks for now: handed over, no longer part of the combined message
    pub fn take_held(&mut self, id: &str) -> Option<(String, String, String, Option<Flush>)> {
        let gi = self.groups.iter().position(|g| g.members.iter().any(|m| m.id == id && matches!(m.fin, Fin::Answered { .. })))?;
        let m = self.groups[gi].members.iter_mut().find(|m| m.id == id)?;
        let Fin::Answered { rid, body } = std::mem::replace(&mut m.fin, Fin::Delivered) else { return None };
        let name = m.to_name.clone();
        let flush = self.groups[gi].members.iter().all(|m| m.fin != Fin::Waiting).then(|| self.groups.remove(gi).flush(false));
        Some((rid, name, body, flush))
    }

    /// FR-96: requests still waiting whose pane is `gone` (checked by the delivery loop); the ends they cause
    pub fn fail_where(&mut self, gone: impl Fn(&Member) -> Option<&'static str>) -> Vec<Flush> {
        let hits: Vec<(String, &'static str)> = self.groups.iter().flat_map(|g| g.members.iter()).filter(|m| m.fin == Fin::Waiting).filter_map(|m| gone(m).map(|r| (m.id.clone(), r))).collect();
        hits.into_iter().filter_map(|(id, r)| self.settle(&id, Fin::Ended(r.into()))).collect()
    }

    /// FR-96: groups that waited `GROUP_HOLD` since their last request and have something to say go with a "still waiting" list
    pub fn due_groups(&mut self) -> Vec<Flush> {
        let mut out = Vec::new();
        let mut i = 0;
        while i < self.groups.len() {
            let g = &self.groups[i];
            if g.last_request.elapsed() < GROUP_HOLD {
                i += 1;
            } else if g.members.iter().any(|m| m.fin != Fin::Waiting) {
                out.push(self.groups.remove(i).flush(true));
            } else if g.last_request.elapsed() > GROUP_HOLD * 4 {
                self.groups.remove(i); // nothing came back for two hours: stop following
            } else {
                i += 1;
            }
        }
        out
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
    pub replies: HashMap<String, Msg>,
    pub waiting: HashSet<String>,
    pub undelivered_replies: HashMap<String, Msg>,
    pub reply_alerted: HashSet<String>,
    /// FR-95 + TECH 18 lead 7: ids typed into a pane (only these can be replied to) / ids that ended another way (answered, rejected, cancelled): a request still open
    /// for its recipient is delivered and not closed. Kept in step with `known`.
    delivered: HashSet<String>,
    closed: HashSet<String>,
    pub queues: HashMap<u64, VecDeque<Queued>>,
    /// FR-89: the message `deliver_loop` took off a queue and is deciding on right now; cancel/edit wait for it to settle
    pub limbo: Option<Msg>,
    /// FR-89: cancelled ids, for the sender's `wait --reply` (taken by the first waiter)
    pub cancelled: HashSet<String>,
    /// FR-90: what was waiting when the daemon stopped; shown as cards, sent or dropped by the user, never sent by itself
    pub held: Vec<crate::outbox::Rec>,
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
    /// FR-96: requests agents sent, until each has an end (memory only: a restart makes them FR-90 cards)
    groups: Vec<Group>,
}

impl Engine {
    pub fn new_id(&mut self, salt: &str) -> String {
        self.seq += 1;
        format!("msg_{}{}", &salt[..salt.len().min(4)], self.seq)
    }

    pub fn find(&self, id: &str) -> Option<&Msg> {
        self.known.iter().rev().find(|m| m.id == id)
    }

    /// Hops are counted by the daemon: hop(reply_to) + 1. Without a known `reply_to` the sender inherits from the newest message
    /// another agent had typed into it within `HOP_WINDOW` (hop + 1), so dropping `reply_to` does not start a fresh chain; a
    /// sender nothing was typed into lately (or only the user, notes, or requests it already answered) starts at 0.
    pub fn hop_for(&self, from: u64, reply_to: Option<&str>) -> u32 {
        if let Some(m) = reply_to.and_then(|r| self.find(r)) {
            return m.hop + 1;
        }
        self.known.iter().rev()
            .find(|m| m.to == from && m.from != from && m.from_name != "user" && m.tag.kind != Kind::Note && self.delivered.contains(&m.id) && !self.closed.contains(&m.id) && m.at.elapsed().is_ok_and(|a| a < HOP_WINDOW))
            .map_or(0, |m| m.hop + 1)
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

    pub fn is_delivered(&self, id: &str) -> bool {
        self.delivered.contains(id)
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
        // replies never enter `known`, which is what evicts these ids: tracking them would only grow the sets
        match kind {
            _ if m.is_reply => {}
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

    /// FR-90: everything that waits in memory, as records for the outbox file: queued messages, confirm cards, the message the
    /// delivery loop is typing this instant (marked: it may have arrived), and the cards left from the last run. Once each.
    pub fn outbox_recs(&self, keys: &HashMap<u64, String>) -> Vec<crate::outbox::Rec> {
        let mut recs: Vec<crate::outbox::Rec> = Vec::new();
        let mut add = |r: Option<crate::outbox::Rec>| {
            if let Some(r) = r.filter(|r| !recs.iter().any(|x| x.id == r.id)) {
                recs.push(r);
            }
        };
        for q in self.queues.values().flatten() {
            add(crate::outbox::Rec::from_msg(&q.msg, keys, false, false));
        }
        for m in &self.pending {
            add(crate::outbox::Rec::from_msg(m, keys, true, false));
        }
        if let Some(m) = &self.limbo {
            add(crate::outbox::Rec::from_msg(m, keys, false, true));
        }
        for h in &self.held {
            add(Some(h.clone()));
        }
        recs
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

    /// FR-90: the snapshot has queue, cards and the message being typed (marked `maybe`), each once, and only what can come back
    #[test]
    fn foto_antrean_untuk_disk() {
        let keys: HashMap<u64, String> = [(1, "aaaaaaaaaaaaaaaa".to_string()), (2, "bbbbbbbbbbbbbbbb".to_string())].into();
        let mut e = Engine::default();
        queued(&mut e, "q1");
        e.pending.push(msg("kartu", 0));
        // popped by the delivery loop: not in the queue any more, but typed right now
        e.limbo = Some(msg("diketik", 0));
        let recs = e.outbox_recs(&keys);
        let flags: Vec<(&str, bool, bool)> = recs.iter().map(|r| (r.id.as_str(), r.card, r.maybe)).collect();
        assert_eq!(flags, [("q1", false, false), ("kartu", true, false), ("diketik", false, true)]);
        // back in the queue while still marked in flight: once, and not "maybe"
        queued(&mut e, "diketik");
        let recs = e.outbox_recs(&keys);
        assert_eq!(recs.iter().filter(|r| r.id == "diketik").map(|r| r.maybe).collect::<Vec<_>>(), [false]);
        // a message to a pane that is gone, or from the harness, can't be restored and isn't kept
        e.queues.get_mut(&2).unwrap().push_back(Queued { msg: Msg { to: 9, ..msg("hilang", 0) }, since: Instant::now(), approved: false });
        assert!(e.outbox_recs(&keys).iter().all(|r| r.id != "hilang"));
        // last run's cards stay until the user decides
        e.held.push(recs[0].clone());
        assert_eq!(e.outbox_recs(&keys).iter().filter(|r| r.id == recs[0].id).count(), 1);
    }

    #[test]
    fn hanya_pesan_terkirim_bisa_dibalas() {
        let mut e = Engine::default();
        let m = msg("tertolak", 0);
        e.remember(&m);
        assert!(!e.is_delivered("tertolak"));
        e.log("sent", &m, None);
        assert!(e.is_delivered("tertolak"));
        // a reply's own id is never tracked (nothing evicts it)
        let r = Msg { is_reply: true, ..msg("balasan", 1) };
        e.log("sent", &r, None);
        e.log("rejected", &r, Some("io"));
        assert!(!e.is_delivered("balasan") && e.delivered.len() == 1 && e.closed.is_empty());
    }

    #[test]
    fn hop_dan_laju() {
        let mut e = Engine::default();
        e.remember(&msg("a", 0));
        e.remember(&msg("b", 2));
        assert_eq!(e.hop_for(9, Some("a")), 1);
        assert_eq!(e.hop_for(9, Some("b")), 3);
        assert_eq!(e.hop_for(9, Some("tidak-ada")), 0);
        // no reply_to: the sender inherits from what was typed into it, not from a message that never arrived or from the user
        e.log("sent", &msg("b", 2), None); // b: 1 -> 2 at hop 2, delivered
        assert_eq!(e.hop_for(2, None), 3);
        assert_eq!(e.hop_for(2, Some("tidak-ada")), 3);
        assert_eq!(e.hop_for(1, None), 0, "claude-1 was only a sender");
        e.remember(&msg("c", 1));
        assert_eq!(e.hop_for(2, None), 3, "c to the same pane never arrived");
        let mut u = msg("u", 0);
        u.from_name = "user".into();
        u.from = 5;
        e.remember(&u);
        e.log("sent", &u, None);
        assert_eq!(e.hop_for(2, None), 3, "a user message is no parent");
        // answering the request ends that chain: the next message from the agent is a new one
        e.close("b");
        assert_eq!(e.hop_for(2, None), 0);
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

    fn req(id: &str, to: u64, name: &str) -> Msg {
        Msg { to, to_name: name.into(), body: "x".into(), ..msg(id, 0) }
    }

    /// FR-95 (5): only a stated `Status:` line counts; prose never decides
    #[test]
    fn hasil_menyebut_status_sendiri() {
        assert_eq!(result_status("Status: done
Goal: x"), Some("done"));
        assert_eq!(result_status("**Status:** blocked
Unblock: key"), Some("blocked"));
        assert_eq!(result_status("- status: Partial (2 of 3)"), Some("partial"));
        assert_eq!(result_status("Selesai semua, done!"), None);
        assert_eq!(result_status("Status: kayaknya beres"), None);
        let r = Msg { tag: Tag { kind: Kind::Result, ..Default::default() }, body: "Status: done".into(), ..msg("r", 0) };
        assert_eq!(r.summary()["outcome"], "done");
        assert_eq!(Msg { tag: Tag { kind: Kind::Result, ..Default::default() }, ..msg("r2", 0) }.summary()["outcome"], "unspecified");
        assert!(msg("q", 0).summary()["outcome"].is_null());
    }

    /// FR-96: results of one group wait for the last end, failures count as ends, then one message
    #[test]
    fn jawaban_pembantu_digabung() {
        let mut e = Engine::default();
        for (id, to, n) in [("a", 2, "h-1"), ("b", 3, "h-2"), ("c", 4, "h-3")] {
            e.track(&req(id, to, n));
        }
        assert!(e.grouped("a") && e.grouped("c"));
        assert!(e.settle("a", Fin::Answered { rid: "ra".into(), body: "hasil A".into() }).is_none());
        assert!(!e.grouped("a"), "sudah punya akhir");
        assert!(e.settle("b", Fin::Ended("expired".into())).is_none());
        let f = e.settle("c", Fin::Answered { rid: "rc".into(), body: "hasil C".into() }).unwrap();
        assert_eq!((f.owner, f.first.as_str(), f.items.len(), f.pending.len()), (1, "a", 3, 0));
        assert_eq!(f.items[1].fin, Fin::Ended("expired".into()));
        assert!(e.settle("a", Fin::Delivered).is_none(), "grup sudah bubar");
    }

    /// FR-96: a request sent after another one was answered starts its own group; other owners and parents stay apart
    #[test]
    fn kelompok_dipisah() {
        let mut e = Engine::default();
        e.track(&req("a", 2, "h-1"));
        e.track(&req("a2", 2, "h-1"));
        assert!(!e.grouped("a"), "same helper twice is no fan-out");
        e.track(&req("b", 3, "h-2"));
        assert!(e.grouped("a2"));
        assert!(e.settle("a", Fin::Delivered).is_none());
        e.track(&req("c", 4, "h-3"));
        assert!(!e.grouped("c"), "yang pertama sudah dijawab: grup baru");
        let child = Msg { tag: Tag { parent: Some("p".into()), ..Default::default() }, ..req("d", 5, "h-4") };
        e.track(&child);
        assert!(!e.grouped("d"));
        e.track(&Msg { from: 9, ..req("e", 5, "h-4") });
        assert!(!e.grouped("e"));
        // a note and a reply are never followed
        e.track(&Msg { tag: Tag { kind: Kind::Note, ..Default::default() }, ..req("n", 2, "h-1") });
        e.track(&Msg { is_reply: true, ..req("r", 2, "h-1") });
        assert!(e.settle("n", Fin::Delivered).is_none() && e.settle("r", Fin::Delivered).is_none());
    }

    /// FR-96: a group of one still reports its end; closed panes and exited agents end what waited on them
    #[test]
    fn akhir_tunggal_dan_pane_hilang() {
        let mut e = Engine::default();
        e.track(&req("a", 2, "h-1"));
        let f = e.settle("a", Fin::Ended("cancelled".into())).unwrap();
        assert_eq!(f.items.len(), 1);
        e.track(&req("b", 2, "h-1"));
        e.track(&req("c", 3, "h-2"));
        let out = e.fail_where(|m| (m.to == 2).then_some("pane_closed"));
        assert!(out.is_empty(), "c masih ditunggu");
        let f = e.settle("c", Fin::Answered { rid: "rc".into(), body: "ok".into() }).unwrap();
        assert_eq!(f.items.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), ["b", "c"]);
    }

    /// FR-96: a held result can be fetched by an explicit wait, and the hold ends after 30 minutes with a pending list
    #[test]
    fn hasil_tertahan_diambil_wait_dan_batas_waktu() {
        let mut e = Engine::default();
        e.track(&req("a", 2, "h-1"));
        e.track(&req("b", 3, "h-2"));
        e.settle("a", Fin::Answered { rid: "ra".into(), body: "hasil A".into() });
        let (rid, name, body, flush) = e.take_held("a").unwrap();
        assert_eq!((rid.as_str(), name.as_str(), body.as_str()), ("ra", "h-1", "hasil A"));
        assert!(flush.is_none() && e.take_held("a").is_none());
        assert!(e.due_groups().is_empty());
        e.groups[0].last_request = Instant::now() - GROUP_HOLD - Duration::from_secs(1);
        let due = e.due_groups();
        assert_eq!((due.len(), due[0].pending.clone()), (1, vec!["h-2".to_string()]));
        assert!(e.settle("b", Fin::Delivered).is_none(), "grup sudah dikirim");
    }
}
