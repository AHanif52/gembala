//! FR-82: is there a newer Gembala? One GET to GitHub's latest release when the daemon starts and every 6 hours, only
//! while `[update] check` is on (or when the user presses "Cek sekarang"). No user data is sent; nothing is downloaded or
//! installed: the UI shows the version and the `brew upgrade` command, the user runs it.

use super::*;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

const URL: &str = "https://api.github.com/repos/AHanif52/gembala/releases/latest";
const EVERY: Duration = Duration::from_secs(6 * 3600);
const NOTES_MAX: usize = 1500;

pub fn start(st: &Arc<State>) {
    let st = st.clone();
    thread::spawn(move || {
        loop {
            if on(&st) {
                check(&st);
            }
            thread::sleep(EVERY);
        }
    });
}

fn on(st: &State) -> bool {
    st.config.lock().unwrap()["values"]["update"]["check"].as_bool().unwrap_or(true)
}

/// last result + the switch; `latest` absent = never checked (or every check failed)
pub fn status(st: &State) -> Value {
    let mut v = st.update.lock().unwrap().clone();
    if !v.is_object() {
        v = json!({});
    }
    v["check"] = json!(on(st));
    // FR-81: installed by Homebrew = the card shows `brew upgrade gembala`, otherwise the source steps
    v["brew"] = json!(["/opt/homebrew/Caskroom/gembala", "/usr/local/Caskroom/gembala"].iter().any(|p| std::path::Path::new(p).exists()));
    v
}

/// "Cek sekarang": in the background, the result comes as `update.changed` (a UI request never waits on the network)
pub fn check_now(st: &Arc<State>) {
    let st = st.clone();
    thread::spawn(move || check(&st));
}

fn check(st: &State) {
    // -4: IPv6 to GitHub/Telegram hangs on some networks (FR-80); the URL is fixed, nothing from the user goes in argv
    let out = Command::new("/usr/bin/curl")
        .args(["-4fsSL", "--max-time", "15", "-H", "Accept: application/vnd.github+json", "-H", &format!("User-Agent: gembala/{}", env!("CARGO_PKG_VERSION")), URL])
        .output();
    let at = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or_default();
    let parsed = out.ok().filter(|o| o.status.success()).and_then(|o| serde_json::from_slice::<Value>(&o.stdout).ok()).and_then(|r| release(&r));
    {
        let mut cur = st.update.lock().unwrap();
        match parsed {
            Some(mut r) => {
                r["at"] = json!(at);
                *cur = r;
            }
            // offline or GitHub down: stay quiet, keep what was known (BRD FR-82)
            None if cur.is_object() => cur["failed_at"] = json!(at),
            None => *cur = json!({"failed_at": at}),
        }
    }
    let ev = line(json!({"event": "update.changed", "params": status(st)}));
    st.ui_txs.lock().unwrap().retain(|(_, tx)| tx.try_send(ev.clone()).is_ok());
}

/// GitHub release JSON → what the UI shows. The text comes from the network: version must look like a version, the
/// link must point at this repo's releases, notes are plain text (the UI never renders them as HTML)
fn release(r: &Value) -> Option<Value> {
    let tag = r["tag_name"].as_str()?.trim_start_matches('v');
    if tag.is_empty() || tag.len() > 20 || !tag.chars().all(|c| c.is_ascii_digit() || c == '.') {
        return None;
    }
    let url = r["html_url"].as_str().filter(|u| u.starts_with("https://github.com/AHanif52/gembala/releases/")).unwrap_or("https://github.com/AHanif52/gembala/releases");
    let notes: String = r["body"].as_str().unwrap_or_default().chars().take(NOTES_MAX).collect();
    Some(json!({"latest": tag, "published": r["published_at"].as_str().unwrap_or_default(), "url": url, "notes": notes}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rilis_dari_github() {
        let r = json!({"tag_name": "v0.1.1", "html_url": "https://github.com/AHanif52/gembala/releases/tag/v0.1.1", "published_at": "2026-11-10T02:00:00Z", "body": "- Notif update"});
        assert_eq!(release(&r), Some(json!({"latest": "0.1.1", "published": "2026-11-10T02:00:00Z", "url": "https://github.com/AHanif52/gembala/releases/tag/v0.1.1", "notes": "- Notif update"})));
        // a tag that isn't a version, or a link to somewhere else, isn't taken as is
        assert_eq!(release(&json!({"tag_name": "v1; rm -rf ~"})), None);
        assert_eq!(release(&json!({"tag_name": "0.2.0", "html_url": "https://evil.example/x"})).unwrap()["url"], "https://github.com/AHanif52/gembala/releases");
    }
}
