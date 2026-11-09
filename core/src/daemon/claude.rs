//! FR-83/84: read only the foreground Claude process's session metadata (SEC-15).
use super::*;
use std::time::SystemTime;

#[derive(Clone, Debug, PartialEq)]
struct Session {
    status: String,
    reason: String,
}

#[derive(Default)]
pub struct Cache {
    path: PathBuf,
    stamp: Option<(SystemTime, u64)>,
    data: Option<Session>,
    pub shell: bool,
    pub reason: String,
}

impl Cache {
    /// A new hook supersedes the last observation, until the file changes again.
    pub fn clear(&mut self) {
        self.shell = false;
        self.reason.clear();
        self.data = None;
    }

    fn apply(&mut self, status: &'static str) -> (&'static str, bool) {
        let old_shell = self.shell;
        self.shell = self.data.as_ref().is_some_and(|d| d.status == "shell" || (old_shell && d.status == "busy" && status == "done"));
        self.reason = self.data.as_ref().filter(|d| d.status == "waiting").map(|d| d.reason.clone()).unwrap_or_default();
        let next = match self.data.as_ref().map(|d| d.status.as_str()) {
            Some("shell") => "done",
            Some("waiting") => "blocked",
            _ => status,
        };
        // Busy belongs to a new turn: never release the previous turn's held notice.
        let release = old_shell && !self.shell && next == "done";
        (next, release)
    }

    pub fn overrides(&self) -> bool {
        self.shell || self.data.as_ref().is_some_and(|d| d.status == "waiting")
    }

    fn read(&mut self, path: PathBuf, pid: i32) {
        if self.path != path {
            *self = Self { path, ..Self::default() };
        }
        let Ok(meta) = fs::symlink_metadata(&self.path) else {
            self.data = None;
            self.stamp = None;
            return;
        };
        // Never follow a session-file symlink into a key, transcript, or another config file.
        if !meta.is_file() || meta.len() > MAX_LINE {
            return;
        }
        let Ok(mtime) = meta.modified() else { return };
        let stamp = Some((mtime, meta.len()));
        if self.stamp == stamp {
            return;
        }
        self.stamp = stamp;
        use std::os::unix::fs::OpenOptionsExt;
        let Ok(file) = fs::OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW).open(&self.path) else { return };
        let mut bytes = Vec::new();
        if file.take(MAX_LINE + 1).read_to_end(&mut bytes).is_ok()
            && let Some(data) = parse(&bytes, pid)
        {
            self.data = Some(data);
        }
    }
}

fn parse(bytes: &[u8], pid: i32) -> Option<Session> {
    if bytes.len() > MAX_LINE as usize {
        return None;
    }
    let v: Value = serde_json::from_slice(bytes).ok()?;
    if v["pid"].as_i64()? != i64::from(pid) {
        return None;
    }
    let version: Vec<u32> = v["version"].as_str()?.split('.').map(str::parse).collect::<Result<_, _>>().ok()?;
    if version.len() != 3 || version.as_slice() < [2, 1, 292].as_slice() {
        return None;
    }
    Some(Session { status: v["status"].as_str()?.into(), reason: reason(v["waitingFor"].as_str().unwrap_or_default()) })
}

pub fn reason(raw: &str) -> String {
    let lower = raw.to_lowercase();
    let text = if lower.contains("trust") {
        "minta percaya folder"
    } else if lower == "worker request" {
        "worker minta izin"
    } else if lower == "sandbox request" {
        "sandbox minta akses"
    } else if lower.contains("permission") || lower.contains("approve") || lower.contains("allow") {
        "minta izin"
    } else if lower == "input needed" {
        "nunggu jawaban"
    } else if lower == "dialog open" {
        "ada dialog kebuka"
    } else {
        raw
    };
    telegram::mask_secrets(text).chars().filter(|c| !c.is_control()).take(40).collect()
}

/// Called before a Stop hook is published, and once a second by the screen watcher.
pub fn refresh(st: &State, id: u64, pane: &Pane, hook: Option<&'static str>) {
    if pane.agent_name().as_deref() != Some("claude") && hook.is_none() {
        return;
    }
    let Some((pid, _)) = pty::busy(&pane.master, pane.sid) else {
        if let Some(status) = hook { set_status(st, id, pane, status, None); }
        return;
    };
    let dir = {
        let account = pane.account.lock().unwrap();
        st.accounts.lock().unwrap().iter().find(|a| a.id == *account && a.agent == "claude").map(|a| account::expand(&a.dir)).unwrap_or_else(account::main_dir)
    };
    let mut cache = pane.claude.lock().unwrap();
    let old = (cache.shell, cache.reason.clone());
    cache.read(dir.join("sessions").join(format!("{pid}.json")), pid);
    let current = *pane.status.lock().unwrap();
    let (next, release) = cache.apply(hook.unwrap_or(current));
    let changed = old != (cache.shell, cache.reason.clone());
    drop(cache);
    if hook.is_some() || next != current {
        set_status(st, id, pane, next, None);
        if next != current {
            return;
        }
    }
    if changed && (next != "done" || release || pane.claude.lock().unwrap().shell) {
        status_changed(st, id, pane, next);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_fields_and_bounds() {
        let bytes = br#"{"pid":42,"status":"shell","version":"2.1.311","messagingSocketPath":"ignored"}"#;
        assert_eq!(parse(bytes, 42).unwrap().status, "shell");
        assert!(parse(bytes, 43).is_none());
        assert!(parse(b"{", 42).is_none());
        assert!(parse(br#"{"pid":42,"status":"idle"}"#, 42).is_none());
        assert!(parse(&vec![b' '; 65537], 42).is_none());
        assert!(parse(br#"{"pid":42,"status":"shell","version":"2.1.291"}"#, 42).is_none());
        assert_eq!(parse(br#"{"pid":42,"status":"waiting","version":"2.1.311","waitingFor":"input needed"}"#, 42).unwrap().reason, "nunggu jawaban");
    }

    #[test]
    fn shell_holds_done_then_idle_releases_once() {
        let mut cache = Cache { data: Some(Session { status: "shell".into(), reason: String::new() }), ..Cache::default() };
        assert_eq!(cache.apply("working"), ("done", false));
        assert!(cache.shell);
        assert_eq!(cache.apply("done"), ("done", false));
        cache.data.as_mut().unwrap().status = "busy".into();
        assert_eq!(cache.apply("done"), ("done", false));
        assert!(cache.shell); // a new turn can precede its working hook; keep every notice channel held
        cache.data.as_mut().unwrap().status = "idle".into();
        assert_eq!(cache.apply("done"), ("done", true));
        assert!(!cache.shell);
        assert_eq!(cache.apply("done"), ("done", false));
        cache.data.as_mut().unwrap().status = "shell".into();
        cache.apply("done");
        cache.clear(); // new working hook supersedes the old shell result
        assert_eq!(cache.apply("working"), ("working", false));
        cache.data = Some(Session { status: "busy".into(), reason: String::new() });
        assert_eq!(cache.apply("blocked"), ("blocked", false));
        cache.data = Some(Session { status: "waiting".into(), reason: "nunggu jawaban".into() });
        assert_eq!(cache.apply("working"), ("blocked", false));
        assert_eq!(cache.reason, "nunggu jawaban");
    }

    #[test]
    fn cache_keeps_good_data_and_reads_only_the_pid_file() {
        let dir = std::env::temp_dir().join(format!("gbl-claude-cache-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("42.json");
        let good = br#"{"pid":42,"version":"2.1.311","status":"shell"}"#;
        fs::write(&path, good).unwrap();
        let mut cache = Cache::default();
        cache.read(path.clone(), 42);
        assert_eq!(cache.data.as_ref().unwrap().status, "shell");
        fs::write(&path, b"{").unwrap();
        cache.read(path.clone(), 42);
        assert_eq!(cache.data.as_ref().unwrap().status, "shell");
        fs::write(&path, vec![b' '; 65537]).unwrap();
        cache.read(path.clone(), 42);
        assert_eq!(cache.data.as_ref().unwrap().status, "shell");
        fs::remove_file(&path).unwrap();
        let key = dir.join("42.hash.key");
        fs::write(&key, good).unwrap();
        std::os::unix::fs::symlink(&key, &path).unwrap();
        let mut other = Cache::default();
        other.read(path.clone(), 42);
        assert!(other.data.is_none());
        fs::remove_file(&path).unwrap();
        cache.read(path, 42);
        assert!(cache.data.is_none());
        assert_eq!(fs::read(&key).unwrap(), good);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn waiting_reasons() {
        for (raw, expected) in [("tool permission", "minta izin"), ("approve tool", "minta izin"), ("allow tool", "minta izin"), ("input needed", "nunggu jawaban"), ("dialog open", "ada dialog kebuka"), ("folder trust", "minta percaya folder"), ("worker request", "worker minta izin"), ("sandbox request", "sandbox minta akses")] {
            assert_eq!(reason(raw), expected);
        }
        assert_eq!(reason("a\n\x1bb"), "ab");
        assert_eq!(reason(&"é".repeat(50)), "é".repeat(40));
    }
}
