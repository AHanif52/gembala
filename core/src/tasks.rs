//! FR-92: task notes for the task panel. One user request to one agent = one task owned by that agent; what the owner then
//! asks other agents is a child. Only what the panel cannot rebuild is kept (owner, title, the children's ends); the live part
//! of a child (running, waiting, held, limit) is read from the daemon when the panel asks. Written to disk like the outbox
//! (SEC-39: `config::write_atomic`, 0600; titles and results pass the secret filter), owners named by history key (FR-62).

use crate::bus::{Kind, Msg, result_status};
use serde_json::{Value, json};
use std::path::PathBuf;

/// tasks kept: done ones go first when the list is longer
const KEEP: usize = 40;

#[derive(Clone, Debug, PartialEq)]
pub struct Child {
    /// id of the `request` the owner (or a helper of it) sent
    pub id: String,
    pub from_name: String,
    /// history key of the target; empty while the target is still an FR-75 open card (found by name then)
    pub to_key: String,
    pub to_name: String,
    pub title: String,
    pub at: u64,
    /// first line of the answer
    pub result: Option<String>,
    /// `done` / `partial` / `blocked` / `unspecified`, as the answer states itself (FR-95)
    pub outcome: Option<String>,
    /// why it failed (a code)
    pub end: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Task {
    /// first request of the task (FR-95): the user's message id, or the owner's first request when the user typed it in the pane
    pub id: String,
    pub owner_key: String,
    pub owner_name: String,
    pub title: String,
    pub at: u64,
    pub done: Option<u64>,
    /// nobody told Gembala what the user asked: made from the owner's first request
    pub implicit: bool,
    pub kids: Vec<Child>,
}

#[derive(Default)]
pub struct Tasks {
    pub list: Vec<Task>,
    /// text last written, so an unchanged list is not written again
    last: String,
}

/// the first non-empty line, secrets filtered, 80 characters
pub fn first_line(body: &str) -> String {
    let red = crate::git::redact(body);
    red.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or_default().chars().take(80).collect()
}

impl Tasks {
    /// the user sent `body` to `owner`
    pub fn open(&mut self, id: &str, owner_key: &str, owner_name: &str, body: &str, now: u64) {
        if id.is_empty() || owner_key.is_empty() || self.list.iter().any(|t| t.id == id) {
            return;
        }
        self.push(Task { id: id.into(), owner_key: owner_key.into(), owner_name: owner_name.into(), title: first_line(body), at: now, done: None, implicit: false, kids: vec![] });
    }

    /// an agent sent a `request`. Answers to something a helper was asked go under the same task; a request with no known
    /// parent goes to the sender's newest open task, or starts one owned by the sender.
    pub fn attach(&mut self, m: &Msg, from_key: &str, to_key: &str, now: u64) {
        if m.tag.kind != Kind::Request || m.is_reply || m.id.is_empty() || self.list.iter().any(|t| t.kids.iter().any(|k| k.id == m.id)) {
            return;
        }
        let kid = Child { id: m.id.clone(), from_name: m.from_name.clone(), to_key: to_key.into(), to_name: m.to_name.clone(), title: first_line(&m.body), at: now, result: None, outcome: None, end: None };
        let by_parent = m.tag.parent.as_ref().and_then(|p| self.list.iter().position(|t| t.kids.iter().any(|k| &k.id == p)));
        let by_owner = || self.list.iter().rposition(|t| t.done.is_none() && t.owner_key == from_key && !from_key.is_empty());
        match by_parent.or_else(by_owner) {
            Some(i) => self.list[i].kids.push(kid),
            None if from_key.is_empty() => {}
            None => self.push(Task { id: m.id.clone(), owner_key: from_key.into(), owner_name: m.from_name.clone(), title: kid.title.clone(), at: now, done: None, implicit: true, kids: vec![kid] }),
        }
    }

    /// the target answered request `id`
    pub fn result(&mut self, id: &str, body: &str) -> bool {
        let Some(k) = self.kid(id) else { return false };
        // the `Status:` line (FR-91) is the outcome, not the summary: the summary is the first line after it
        let rest = body.lines().skip_while(|l| l.trim().is_empty() || result_status(l).is_some()).collect::<Vec<_>>().join("\n");
        k.result = Some(first_line(&rest));
        k.outcome = Some(result_status(body).unwrap_or("unspecified").into());
        true
    }

    /// request `id` ended without an answer
    pub fn end(&mut self, id: &str, reason: &str) -> bool {
        let reason = if reason == "owner_cancelled" { "cancelled" } else { reason };
        self.kid(id).is_some_and(|k| k.result.is_none() && { k.end = Some(reason.into()); true })
    }

    /// the user marks a task done (`task.done`)
    pub fn finish(&mut self, id: &str, now: u64) -> bool {
        self.list.iter_mut().find(|t| t.id == id).is_some_and(|t| {
            t.done.get_or_insert(now);
            true
        })
    }

    /// the owner answered the user (Telegram reply): its open tasks are done
    pub fn answered(&mut self, owner_key: &str, now: u64) -> bool {
        let mut any = false;
        for t in self.list.iter_mut().filter(|t| t.owner_key == owner_key && t.done.is_none()) {
            t.done = Some(now);
            any = true;
        }
        any
    }

    fn kid(&mut self, id: &str) -> Option<&mut Child> {
        self.list.iter_mut().flat_map(|t| t.kids.iter_mut()).find(|k| k.id == id)
    }

    fn push(&mut self, t: Task) {
        self.list.push(t);
        while self.list.len() > KEEP {
            let i = self.list.iter().position(|t| t.done.is_some()).unwrap_or(0);
            self.list.remove(i);
        }
    }

    /// the text for the file; no tasks = empty (no file)
    pub fn encode(&self) -> String {
        if self.list.is_empty() { String::new() } else { Value::Array(self.list.iter().map(task_json).collect()).to_string() }
    }

    /// write if changed
    pub fn save(&mut self, path: &std::path::Path) {
        let text = self.encode();
        if text != self.last && crate::outbox::write(path, &text).is_ok() {
            self.last = text;
        }
    }

    pub fn load(path: &std::path::Path) -> Tasks {
        let list = match std::fs::read_to_string(path).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()) {
            Some(Value::Array(a)) => a.iter().filter_map(task_from).collect(),
            _ => Vec::new(),
        };
        Tasks { list, last: String::new() }
    }
}

/// `<config folder>/outbox/tasks.json`, next to the outbox (same folder rules)
pub fn path() -> PathBuf {
    crate::outbox::path().with_file_name("tasks.json")
}

fn task_json(t: &Task) -> Value {
    json!({"id": t.id, "owner_key": t.owner_key, "owner_name": t.owner_name, "title": t.title, "at": t.at, "done": t.done, "implicit": t.implicit,
           "kids": t.kids.iter().map(|k| json!({"id": k.id, "from_name": k.from_name, "to_key": k.to_key, "to_name": k.to_name, "title": k.title, "at": k.at,
                                                "result": k.result, "outcome": k.outcome, "end": k.end})).collect::<Vec<_>>()})
}

fn task_from(v: &Value) -> Option<Task> {
    let s = |v: &Value, k: &str| v[k].as_str().map(String::from);
    let kids = v["kids"].as_array()?.iter().map(|k| Some(Child {
        id: s(k, "id").filter(|i| !i.is_empty())?,
        from_name: s(k, "from_name")?,
        to_key: s(k, "to_key")?,
        to_name: s(k, "to_name")?,
        title: s(k, "title")?,
        at: k["at"].as_u64()?,
        result: s(k, "result"),
        outcome: s(k, "outcome"),
        end: s(k, "end"),
    })).collect::<Option<Vec<_>>>()?;
    Some(Task {
        id: s(v, "id").filter(|i| !i.is_empty())?,
        owner_key: s(v, "owner_key").filter(|k| !k.is_empty())?,
        owner_name: s(v, "owner_name")?,
        title: s(v, "title")?,
        at: v["at"].as_u64()?,
        done: v["done"].as_u64(),
        implicit: v["implicit"].as_bool().unwrap_or(false),
        kids,
    })
}

/// where a child stands right now. `held` = a restart left it on a card (FR-90), `limit` = its target hit a usage limit (FR-93),
/// `queued` = waiting in a queue or on a card, `target` = the target pane's status (`None` = no such pane).
pub fn kid_state(k: &Child, held: bool, limit: bool, queued: bool, target: Option<&str>) -> &'static str {
    if k.result.is_some() {
        return match k.outcome.as_deref() {
            Some("blocked") => "stuck",
            Some("partial") => "partial",
            _ => "done",
        };
    }
    if k.end.is_some() {
        return "failed";
    }
    if held {
        return "held";
    }
    if target.is_none() && !queued {
        return "failed";
    }
    if limit {
        return "limit";
    }
    match (queued, target) {
        (true, _) => "waiting",
        (_, Some("working")) => "running",
        _ => "waiting",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::Tag;
    use std::time::SystemTime;

    fn req(id: &str, from: &str, to: &str, body: &str, parent: Option<&str>) -> Msg {
        Msg { id: id.into(), from: 1, from_name: from.into(), to: 2, to_name: to.into(), body: body.into(), reply_to: None, is_reply: false, hop: 0, at: SystemTime::now(),
              tag: Tag { kind: Kind::Request, task: id.into(), parent: parent.map(String::from), edited: false } }
    }

    #[test]
    fn tugas_dari_pesan_pengguna_dan_anak_dari_pemilik() {
        let mut t = Tasks::default();
        t.open("msg_1", "keyA", "claude-1", "\n  Migrasi tabel bucket\nlanjutan", 100);
        t.open("msg_1", "keyA", "claude-1", "dobel", 101); // the same id twice is one task
        assert_eq!(t.list.len(), 1);
        assert_eq!(t.list[0].title, "Migrasi tabel bucket");
        t.attach(&req("msg_2", "claude-1", "omp-1", "tulis tes", None), "keyA", "keyB", 110);
        assert_eq!(t.list.len(), 1, "permintaan pemilik masuk ke tugas pengguna");
        assert_eq!(t.list[0].kids.len(), 1);
        // a helper's sub-request goes under the same task through the parent
        t.attach(&req("msg_3", "omp-1", "codex-1", "cek diff", Some("msg_2")), "keyB", "keyC", 120);
        assert_eq!(t.list.len(), 1);
        assert_eq!(t.list[0].kids[1].from_name, "omp-1");
        // a note or a repeat is not a child
        t.attach(&req("msg_2", "claude-1", "omp-1", "x", None), "keyA", "keyB", 130);
        assert_eq!(t.list[0].kids.len(), 2);
    }

    #[test]
    fn permintaan_tanpa_tugas_pengguna_bikin_tugas_tersirat() {
        let mut t = Tasks::default();
        t.attach(&req("msg_9", "claude-1", "omp-1", "audit repo ini", None), "keyA", "keyB", 5);
        assert_eq!(t.list.len(), 1);
        assert!(t.list[0].implicit);
        assert_eq!(t.list[0].owner_name, "claude-1");
        // a sender that is not a pane (harness) starts nothing
        t.attach(&req("msg_10", "harness", "omp-1", "x", None), "", "keyB", 6);
        assert_eq!(t.list.len(), 1);
        // a finished task is not attached to: the next request starts a new one
        t.finish("msg_9", 7);
        t.attach(&req("msg_11", "claude-1", "omp-1", "lagi", None), "keyA", "keyB", 8);
        assert_eq!(t.list.len(), 2);
    }

    #[test]
    fn selesai_lewat_balasan_ke_pengguna_dan_tombol_bukan_giliran() {
        let mut t = Tasks::default();
        t.open("a", "keyA", "claude-1", "satu", 1);
        t.open("b", "keyB", "omp-1", "dua", 2);
        t.attach(&req("r1", "claude-1", "omp-2", "bantu", None), "keyA", "keyC", 3);
        // a helper finishing does not finish the task
        assert!(t.result("r1", "Status: done\nSudah beres\ndetail"));
        assert_eq!(t.list[0].done, None);
        assert_eq!(t.list[0].kids[0].result.as_deref(), Some("Sudah beres"), "baris Status dilewati");
        assert_eq!(t.list[0].kids[0].outcome.as_deref(), Some("done"));
        // the owner answering the user finishes only its own tasks
        assert!(t.answered("keyA", 50));
        assert_eq!(t.list[0].done, Some(50));
        assert_eq!(t.list[1].done, None);
        assert!(!t.answered("keyA", 60), "sudah selesai: tidak ada yang berubah");
        // the user marks the other one; a second press keeps the first time
        assert!(t.finish("b", 70));
        assert!(t.finish("b", 99));
        assert_eq!(t.list[1].done, Some(70));
        assert!(!t.finish("nope", 1));
    }

    #[test]
    fn akhir_anak_dan_keadaan() {
        let mut t = Tasks::default();
        t.open("a", "keyA", "claude-1", "satu", 1);
        for id in ["r1", "r2", "r3"] {
            t.attach(&req(id, "claude-1", "omp-1", "x", None), "keyA", "keyB", 2);
        }
        assert!(t.end("r1", "expired"));
        assert!(t.end("r2", "owner_cancelled"));
        assert!(t.result("r3", "**Status:** blocked\n\nbutuh izin"));
        assert_eq!(t.list[0].kids[2].result.as_deref(), Some("butuh izin"));
        assert!(!t.end("r3", "expired"), "yang sudah dijawab tidak gagal belakangan");
        let k = |i: usize| t.list[0].kids[i].clone();
        assert_eq!(kid_state(&k(0), false, false, false, Some("idle")), "failed");
        assert_eq!(k(1).end.as_deref(), Some("cancelled"));
        assert_eq!(kid_state(&k(2), false, false, false, Some("idle")), "stuck");
        let part = Child { outcome: Some("partial".into()), ..k(2) };
        assert_eq!(kid_state(&part, false, false, false, Some("idle")), "partial");
        let plain = Child { outcome: Some("unspecified".into()), ..k(2) };
        assert_eq!(kid_state(&plain, false, false, false, Some("idle")), "done");
        let open = Child { end: None, result: None, outcome: None, ..k(0) };
        assert_eq!(kid_state(&open, false, false, false, Some("working")), "running");
        assert_eq!(kid_state(&open, false, false, false, Some("idle")), "waiting");
        assert_eq!(kid_state(&open, false, false, true, Some("working")), "waiting");
        assert_eq!(kid_state(&open, false, true, false, Some("working")), "limit");
        assert_eq!(kid_state(&open, true, true, false, None), "held");
        assert_eq!(kid_state(&open, false, false, false, None), "failed", "pane hilang");
        assert_eq!(kid_state(&open, false, false, true, None), "waiting", "kartu buka pane belum jalan");
    }

    #[test]
    fn simpan_dan_muat_dari_disk_pribadi() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("gembala-tasks-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("cfg/outbox/tasks.json");
        let mut t = Tasks::default();
        t.open("a", "keyA", "claude-1", "pakai token sk-ant-api03-abcdefghijklmnopqrstuvwxyz0123456789 ya", 1);
        t.attach(&req("r1", "claude-1", "omp-1", "x", None), "keyA", "keyB", 2);
        t.result("r1", "Status: partial\nsetengah");
        t.finish("a", 9);
        t.save(&path);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("abcdefghijklmnopqrstuvwxyz0123456789"), "rahasia disaring dari judul");
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(std::fs::metadata(path.parent().unwrap()).unwrap().permissions().mode() & 0o777, 0o700);
        let back = Tasks::load(&path);
        assert_eq!(back.list, t.list);
        // nothing changed = no write; nothing left = no file
        t.list.clear();
        t.save(&path);
        assert!(!path.exists());
        // a broken file reads as nothing
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("x.json"), "[1, {\"id\":\"q\"}]").unwrap();
        assert!(Tasks::load(&dir.join("x.json")).list.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn daftar_dibatasi_yang_selesai_dibuang_dulu() {
        let mut t = Tasks::default();
        for i in 0..KEEP + 5 {
            t.open(&format!("t{i}"), "k", "claude-1", "x", i as u64);
            if i == 3 {
                t.finish("t3", 9);
            }
        }
        assert_eq!(t.list.len(), KEEP);
        assert!(t.list.iter().all(|x| x.id != "t3"), "yang selesai dibuang lebih dulu");
        assert!(t.list.iter().any(|x| x.id == "t44"), "yang terbaru tetap ada");
    }
}
