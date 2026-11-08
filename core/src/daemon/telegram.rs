//! FR-80, SEC-36, SEC-37 (TECH 7e): the user's own Telegram bot. Notices go out while the Mac is away (locked or idle),
//! the user answers permission prompts with buttons and types into agent panes by replying. Long polling goes out through
//! `/usr/bin/curl` with the token on stdin, never in argv; no port, no new dependency. The bot is a human channel with
//! fewer rights than the UI channel: no pane create/close/rename, no settings, no Gembala cards, never a shell pane or a
//! pane waiting for a password. Turned on and off only from the UI channel (SEC-20).

use super::*;
use std::collections::{HashSet, VecDeque};
use std::process::{Command, Stdio};
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::Receiver;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

/// messages older than this are dropped: a daemon that just started doesn't run stale commands
const MAX_AGE: i64 = 120;
const CODE_TTL: Duration = Duration::from_secs(600);
const CODE_TRIES: u32 = 5;
const MAX_TEXT: usize = 4000;
const MAX_SNIPPET: usize = 3500;
const MAP_MAX: usize = 500;
/// a hook can say "blocked" a moment before the dialog is drawn: the screen is read this long after the event
const SETTLE: Duration = Duration::from_secs(1);

pub type Slot = Mutex<Option<Arc<Bot>>>;

/// telegram.json (SEC-36, T9): token and bound chat, `0600` next to config.toml
#[derive(Clone, Debug, PartialEq)]
pub struct Link {
    pub token: String,
    pub chat: i64,
    pub user: i64,
    pub name: String,
}

pub struct Bot {
    token: String,
    /// bot username; a bot started from telegram.json learns it from getMe
    bot: Mutex<String>,
    link: Mutex<Option<Link>>,
    /// pairing code, when it was made, wrong tries
    code: Mutex<Option<(String, Instant, u32)>>,
    out: SyncSender<Out>,
    stop: AtomicBool,
    state: Mutex<(&'static str, String)>,
    /// bot message id -> pane, oldest first (reply = type into that pane)
    sent: Mutex<VecDeque<(i64, u64)>>,
    /// (pane, status_seq) already sent: one notice per event
    noted: Mutex<HashSet<(u64, u64)>>,
    away: AtomicBool,
    /// telegram.json mtime after our own write; anything else = changed from outside
    stamp: Mutex<Option<SystemTime>>,
    id_lang: bool,
    /// active pane: last typed into or last to send a notice; plain text and photos go there
    last: Mutex<Option<u64>>,
    /// plain text waiting for the user to pick a pane (text, message id)
    pending: Mutex<Option<(String, i64)>>,
}

enum Out {
    Api(&'static str, Value),
    Notice(u64, Kind, Instant),
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Kind {
    Blocked(u64),
    Done,
    Password,
    Long(u64),
}

#[derive(Debug, PartialEq)]
enum In {
    Ignore,
    Start { code: String, chat: i64, user: i64, name: String },
    Status,
    Look(String),
    /// `to = None`: plain text goes to the active pane (last used, or picked from buttons)
    Type { to: Option<To>, text: String, msg: i64 },
    /// `/ke <pane>`: switch the active pane
    Use(String),
    /// pane picked from the buttons offered for plain text without an active pane
    Pick { pane: u64, cb: String, msg: i64 },
    /// photo or file: saved like a pasted image (FR-54, SEC-34), its path typed into the pane after the caption
    Media { to: Option<To>, caption: String, file: String, ext: String, msg: i64 },
    Key { pane: u64, seq: u64, key: String, cb: String, msg: i64 },
    Help,
}

#[derive(Debug, PartialEq)]
enum To {
    Name(String),
    /// bot message id + the pane name on its first line (`claude-1 · gembala kelar`), for replies after a daemon restart
    Reply(i64, String),
}

#[derive(Debug, PartialEq)]
enum Why {
    Shell,
    Blocked,
    Password,
    TooLong,
    Bad,
}

pub fn file() -> PathBuf {
    config::path().with_file_name("telegram.json")
}

fn load() -> Option<Link> {
    let v: Value = serde_json::from_str(&fs::read_to_string(file()).ok()?).ok()?;
    let token = v["token"].as_str().filter(|t| valid_token(t))?.to_string();
    Some(Link { token, chat: v["chat"].as_i64()?, user: v["user"].as_i64()?, name: v["name"].as_str().unwrap_or_default().chars().take(80).collect() })
}

fn mtime() -> Option<SystemTime> {
    fs::metadata(file()).ok()?.modified().ok()
}

/// `<digits>:<30+ of [A-Za-z0-9_-]>`, the shape BotFather hands out
fn valid_token(t: &str) -> bool {
    let Some((id, key)) = t.split_once(':') else { return false };
    let id_ok = !id.is_empty() && id.len() <= 20 && id.bytes().all(|b| b.is_ascii_digit());
    id_ok && (30..=100).contains(&key.len()) && key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// value inside a double-quoted curl config string
fn curl_quote(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Telegram HTML: text from panes and the screen must not read as markup (T-14 pattern)
fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// last `lines` rows, trailing blank rows dropped, at most `MAX_SNIPPET` characters (cut from the top)
fn snippet(screen: &str, lines: usize) -> String {
    let rows: Vec<&str> = screen.lines().map(str::trim_end).collect();
    let end = rows.iter().rposition(|r| !r.is_empty()).map_or(0, |i| i + 1);
    let text = rows[end.saturating_sub(lines)..end].join("\n");
    let n = text.chars().count();
    text.chars().skip(n.saturating_sub(MAX_SNIPPET)).collect()
}

/// FR-69 label without the spinner and generic titles (agent name)
fn label(title: &str, agent: &str) -> Option<String> {
    // spinner (braille), Claude's ✳, omp's `π >` prefix
    let t = title.trim_start_matches(|c: char| ('\u{2800}'..='\u{28ff}').contains(&c) || matches!(c, '✳' | '✻' | 'π' | '>') || c.is_whitespace()).trim();
    let generic = t.eq_ignore_ascii_case(agent) || t.eq_ignore_ascii_case("claude code");
    (!t.is_empty() && !generic).then(|| t.chars().take(120).collect())
}

fn keys(agent: &str) -> &'static [&'static str] {
    // ponytail: only Claude's numbered dialog is known; Codex/omp/OpenCode get Esc until their keys are checked (TECH 7e)
    if agent == "claude" { &["1", "2", "3", "esc"] } else { &["esc"] }
}

fn key_bytes(key: &str) -> &'static [u8] {
    match key {
        "1" => b"1",
        "2" => b"2",
        "3" => b"3",
        _ => b"\x1b",
    }
}

fn cb_data(pane: u64, seq: u64, key: &str) -> String {
    format!("k|{pane}|{seq}|{key}")
}

fn cb_parse(d: &str) -> Option<(u64, u64, String)> {
    let mut it = d.split('|');
    if it.next()? != "k" {
        return None;
    }
    let (pane, seq, key) = (it.next()?.parse().ok()?, it.next()?.parse().ok()?, it.next()?);
    (it.next().is_none() && ["1", "2", "3", "esc"].contains(&key)).then(|| (pane, seq, key.to_string()))
}

/// Update -> what to do. Only the bound private chat counts (SEC-36); while pairing only `/start <code>`.
fn parse(u: &Value, link: Option<&Link>, now: i64) -> In {
    if let Some(q) = u.get("callback_query") {
        let Some(l) = link else { return In::Ignore };
        let m = &q["message"];
        if q["from"]["id"].as_i64() != Some(l.user) || m["chat"]["id"].as_i64() != Some(l.chat) {
            return In::Ignore;
        }
        let (data, cb, msg) = (q["data"].as_str().unwrap_or_default(), q["id"].as_str().unwrap_or_default().to_string(), m["message_id"].as_i64().unwrap_or_default());
        if let Some(pane) = data.strip_prefix("p|").and_then(|p| p.parse().ok()) {
            return In::Pick { pane, cb, msg };
        }
        let Some((pane, seq, key)) = cb_parse(data) else { return In::Ignore };
        return In::Key { pane, seq, key, cb, msg };
    }
    let m = &u["message"];
    if m["chat"]["type"] != "private" || now - m["date"].as_i64().unwrap_or_default() > MAX_AGE {
        return In::Ignore;
    }
    let (chat, user) = (m["chat"]["id"].as_i64().unwrap_or_default(), m["from"]["id"].as_i64().unwrap_or_default());
    let text = m["text"].as_str().unwrap_or_default().trim();
    let Some(l) = link else {
        let Some(code) = text.strip_prefix("/start ") else { return In::Ignore };
        let first = m["from"]["first_name"].as_str().unwrap_or_default();
        let name = match m["from"]["username"].as_str() {
            Some(u) => format!("{first} (@{u})"),
            None => first.to_string(),
        };
        return In::Start { code: code.trim().to_string(), chat, user, name: name.chars().take(80).collect() };
    };
    if chat != l.chat || user != l.user {
        return In::Ignore;
    }
    let msg = m["message_id"].as_i64().unwrap_or_default();
    let quoted = &m["reply_to_message"];
    let first = quoted["text"].as_str().or(quoted["caption"].as_str()).and_then(|t| t.lines().next()).unwrap_or_default();
    let name = first.split(" · ").next().filter(|n| *n != first).unwrap_or_default().trim().to_string();
    let reply = quoted["message_id"].as_i64().map(|r| To::Reply(r, name));
    // largest photo size, or a document; Telegram's public API serves files up to 20 MB
    let media = match (m["photo"].as_array().and_then(|p| p.last()), m.get("document")) {
        (Some(p), _) => Some((p, "jpg".to_string())),
        (None, Some(d)) => {
            let name = d["file_name"].as_str().unwrap_or_default();
            let ext: String = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
            let ok = !ext.is_empty() && ext.len() <= 8 && ext.bytes().all(|b| b.is_ascii_alphanumeric());
            Some((d, if ok { ext } else { "bin".into() }))
        }
        _ => None,
    };
    if let Some((f, ext)) = media {
        if f["file_size"].as_u64().unwrap_or_default() > 20 << 20 {
            return In::Help;
        }
        let caption = m["caption"].as_str().unwrap_or_default().trim().to_string();
        let (to, caption) = match caption.strip_prefix("/kirim ").or_else(|| caption.strip_prefix("/send ")) {
            Some(rest) => match rest.trim().split_once(char::is_whitespace) {
                Some((n, c)) => (Some(To::Name(n.into())), c.trim().to_string()),
                None => (Some(To::Name(rest.trim().into())), String::new()),
            },
            None => (reply, caption),
        };
        return In::Media { to, caption, file: f["file_id"].as_str().unwrap_or_default().to_string(), ext, msg };
    }
    if !text.is_empty() && !text.starts_with('/') {
        return In::Type { to: reply, text: text.to_string(), msg };
    }
    let (cmd, rest) = text.split_once(char::is_whitespace).unwrap_or((text, ""));
    let rest = rest.trim();
    match cmd.split('@').next().unwrap_or_default() {
        "/status" => In::Status,
        "/lihat" | "/look" if !rest.is_empty() => In::Look(rest.to_string()),
        "/ke" | "/to" if !rest.is_empty() => In::Use(rest.to_string()),
        "/kirim" | "/send" => match rest.split_once(char::is_whitespace) {
            Some((name, body)) if !body.trim().is_empty() => In::Type { to: Some(To::Name(name.to_string())), text: body.trim().to_string(), msg },
            _ => In::Help,
        },
        _ => In::Help,
    }
}

/// SEC-36 (4): text from the phone only goes to an agent pane that isn't asking for permission or a password
fn typed(agent: bool, status: &str, password: bool, text: &str) -> Result<Vec<u8>, Why> {
    if !agent {
        return Err(Why::Shell);
    }
    if password {
        return Err(Why::Password);
    }
    if status == "blocked" {
        return Err(Why::Blocked);
    }
    if text.chars().count() > MAX_TEXT {
        return Err(Why::TooLong);
    }
    let clean = msg::sanitize(text).map_err(|_| Why::Bad)?;
    if clean.trim().is_empty() {
        return Err(Why::Bad);
    }
    Ok(msg::paste(&clean))
}

/// `"HIDIdleTime" = <ns>` from `ioreg -c IOHIDSystem`
fn idle_secs(ioreg: &str) -> Option<u64> {
    let line = ioreg.lines().find(|l| l.contains("\"HIDIdleTime\""))?;
    line.split('=').nth(1)?.trim().parse::<u64>().ok().map(|ns| ns / 1_000_000_000)
}

fn locked(ioreg: &str) -> bool {
    ioreg.contains("\"CGSSessionScreenIsLocked\"=Yes")
}

/// 8 characters, base32 without look-alikes
fn new_code() -> io::Result<String> {
    const ABC: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
    let mut b = [0u8; 8];
    File::open("/dev/urandom")?.read_exact(&mut b)?;
    Ok(b.iter().map(|x| ABC[*x as usize % ABC.len()] as char).collect())
}

fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

fn dur(secs: u64) -> String {
    match secs {
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m {}s", s / 60, s % 60),
        s => format!("{}j {}m", s / 3600, s % 3600 / 60),
    }
}

/// Bot replies in the UI language: `[ui] language`, `system` = first of id/en in the macOS list (TECH 20)
fn lang_id(values: &Value) -> bool {
    match values["ui"]["language"].as_str().unwrap_or("system") {
        "id" => true,
        "en" => false,
        _ => {
            let out = Command::new("/usr/bin/defaults").args(["read", "-g", "AppleLanguages"]).output();
            let text = out.map(|o| String::from_utf8_lossy(&o.stdout).to_lowercase()).unwrap_or_default();
            let at = |p: &str| text.find(&format!("\"{p}")).unwrap_or(usize::MAX);
            at("id") < at("en")
        }
    }
}

fn t(id: bool, k: &str) -> &'static str {
    let (i, e) = match k {
        "blocked" => ("butuh izin", "needs permission"),
        "done" => ("kelar", "done"),
        "working" => ("lagi kerja", "working"),
        "idle" => ("nunggu perintah", "idle"),
        "password" => ("nunggu password", "waiting for a password"),
        "long" => ("perintah kelar", "command finished"),
        "shell_st" => ("shell", "shell"),
        "linked" => ("Nyambung ke Gembala di", "Connected to Gembala on"),
        "gone" => ("udah lewat", "already answered"),
        "sent" => ("terkirim", "sent"),
        "answered" => ("dijawab", "answered"),
        "no_pane" => ("Pane itu nggak ada.", "No such pane."),
        "shell" => ("Pane shell cuma bisa dilihat dari HP.", "Shell panes are view-only from the phone."),
        "need_ok" => ("Lagi nunggu izin, pakai tombolnya.", "It's waiting for permission, use the buttons."),
        "pw" => ("Lagi nunggu password, jawab di Mac.", "It's waiting for a password, answer on the Mac."),
        "too_long" => ("Kepanjangan, maks 4.000 karakter.", "Too long, 4,000 characters max."),
        "bad" => ("Teksnya kosong setelah dibersihin.", "Nothing left after cleaning the text."),
        "old_reply" => ("Pane pesan itu udah nggak ada. Ketik biasa buat ke pane aktif, atau /ke <pane>.", "That message's pane is gone. Type plainly for the active pane, or /to <pane>."),
        "snip_off" => ("Potongan layar dimatiin buat workspace ini.", "Screen snippets are off for this workspace."),
        "pick" => ("Ke pane mana?", "Which pane?"),
        "no_agent" => ("Belum ada pane agent.", "No agent pane yet."),
        "now_to" => ("Sekarang ngetik ke", "Now typing into"),
        "dl_fail" => ("Gagal ngunduh filenya (maks 20 MB).", "Couldn't download the file (20 MB max)."),
        "none" => ("Belum ada pane.", "No panes yet."),
        _ => (
            "Ketik biasa = masuk ke pane aktif (yang terakhir dipakai atau terakhir ngabarin).\nBales pesan bot = ke pane pesan itu.\nFoto/file = path-nya diketik ke pane aktif.\n/ke <pane> — ganti pane aktif\n/status — semua pane\n/lihat <pane> — layar terbaru",
            "Plain text = goes to the active pane (last used or last to send a notice).\nReply to a bot message = that message's pane.\nPhoto/file = its path is typed into the active pane.\n/to <pane> — switch the active pane\n/status — every pane\n/look <pane> — latest screen",
        ),
    };
    if id { i } else { e }
}

/// curl with its options (URL with the token included) on stdin, so nothing secret shows in argv
fn curl(cfg: &str, secs: u64) -> io::Result<std::process::Output> {
    let mut c = Command::new("/usr/bin/curl")
        .args(["-sS", "-4", "--max-time", &secs.to_string(), "-K", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    if let Some(mut w) = c.stdin.take() {
        let _ = w.write_all(cfg.as_bytes());
    }
    c.wait_with_output()
}

/// one Bot API call
fn api(token: &str, method: &str, body: &Value, secs: u64) -> Result<Value, (i64, Option<u64>)> {
    let url = format!("https://api.telegram.org/bot{token}/{method}");
    let cfg = format!("url = \"{}\"\nheader = \"Content-Type: application/json\"\ndata = \"{}\"\n", curl_quote(&url), curl_quote(&body.to_string()));
    let o = curl(&cfg, secs).map_err(|_| (0, None))?;
    let v: Value = serde_json::from_slice(&o.stdout).map_err(|_| (0, None))?;
    if v["ok"] == true {
        Ok(v["result"].clone())
    } else {
        Err((v["error_code"].as_i64().unwrap_or_default(), v["parameters"]["retry_after"].as_u64()))
    }
}

/// photo/file from the phone into Gembala's paste folder (`0700`, files `0600`, swept after 24 h like pasted images, SEC-34)
fn download(st: &State, b: &Bot, file: &str, ext: &str) -> Option<PathBuf> {
    let f = api(&b.token, "getFile", &json!({"file_id": file}), 15).ok()?;
    let fp = f["file_path"].as_str().filter(|p| !p.contains("..") && !p.starts_with('/'))?;
    let dir = Path::new(&st.pane_sock).parent()?.join("paste");
    crate::rundir::ensure_private_dir(&dir).ok()?;
    let path = dir.join(format!("{}.{ext}", &new_secret().ok()?[..24]));
    let url = format!("https://api.telegram.org/file/bot{}/{fp}", b.token);
    let cfg = format!("url = \"{}\"\noutput = \"{}\"\nfail\nmax-filesize = {}\n", curl_quote(&url), curl_quote(&path.to_string_lossy()), 20 << 20);
    let ok = curl(&cfg, 120).is_ok_and(|o| o.status.success());
    if !ok || fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).is_err() {
        let _ = fs::remove_file(&path);
        return None;
    }
    Some(path)
}

fn debug() -> bool {
    std::env::var("GEMBALA_TG_DEBUG").as_deref() == Ok("1")
}

/// stderr line that never panics: `eprintln!` panics on a broken stderr and would take the bot thread with it
fn log(line: std::fmt::Arguments) {
    let _ = writeln!(io::stderr(), "telegram: {line}");
}

impl Bot {
    fn react(&self, msg: i64, emoji: &str) {
        let r = json!({"chat_id": self.chat(), "message_id": msg, "reaction": [{"type": "emoji", "emoji": emoji}]});
        let _ = self.out.try_send(Out::Api("setMessageReaction", r));
    }

    fn linked(&self) -> Option<Link> {
        self.link.lock().unwrap().clone()
    }

    fn chat(&self) -> i64 {
        self.linked().map_or(0, |l| l.chat)
    }

    fn say(&self, html: &str) {
        let _ = self.out.try_send(Out::Api("sendMessage", json!({"chat_id": self.chat(), "text": html, "parse_mode": "HTML"})));
    }

    fn tell(&self, plain: &str) {
        self.say(&esc(plain));
    }

    fn remember(&self, msg: i64, pane: u64) {
        let mut s = self.sent.lock().unwrap();
        s.push_back((msg, pane));
        if s.len() > MAP_MAX {
            s.pop_front();
        }
    }

    /// first time this (pane, event) is seen
    fn first(&self, pane: u64, seq: u64) -> bool {
        let mut n = self.noted.lock().unwrap();
        // ponytail: forget everything past 2,000 events; a repeat after that is one extra message at worst
        if n.len() > 2000 {
            n.clear();
        }
        n.insert((pane, seq))
    }

    fn set(&self, st: &State, state: &'static str, err: &str) {
        let mut s = self.state.lock().unwrap();
        if s.0 != state || s.1 != err {
            log(format_args!("{state} {err}"));
            *s = (state, err.to_string());
            drop(s);
            changed(st);
        }
    }
}

fn values(st: &State) -> Value {
    st.config.lock().unwrap()["values"].clone()
}

pub fn status(st: &State) -> Value {
    let Some(b) = st.tg.lock().unwrap().clone() else { return json!({"state": "off"}) };
    let (state, err) = b.state.lock().unwrap().clone();
    let bot = b.bot.lock().unwrap().clone();
    // pairing code goes to the UI channel only (SEC-20); expired or used up = null, the UI offers to start again
    let code = b.code.lock().unwrap().as_ref().filter(|(_, at, n)| at.elapsed() < CODE_TTL && *n < CODE_TRIES).map(|(c, at, _)| (c.clone(), (CODE_TTL - at.elapsed()).as_secs()));
    json!({"state": state, "bot": bot, "name": b.linked().map(|l| l.name), "error": (!err.is_empty()).then_some(err),
           "code": code.as_ref().map(|c| &c.0), "code_secs": code.as_ref().map(|c| c.1),
           "link": code.as_ref().map(|c| format!("https://t.me/{bot}?start={}", c.0))})
}

fn changed(st: &State) {
    let ev = line(json!({"event": "telegram.changed", "params": status(st)}));
    st.ui_txs.lock().unwrap().retain(|(_, tx)| tx.try_send(ev.clone()).is_ok());
}

fn spawn(st: &Arc<State>, token: String, bot: String, link: Option<Link>) -> Arc<Bot> {
    let (tx, rx) = sync_channel(64);
    let paired = link.is_some();
    let b = Arc::new(Bot {
        token,
        bot: Mutex::new(bot),
        link: Mutex::new(link),
        code: Mutex::new(None),
        out: tx,
        stop: AtomicBool::new(false),
        state: Mutex::new((if paired { "on" } else { "pairing" }, String::new())),
        sent: Mutex::default(),
        noted: Mutex::default(),
        away: AtomicBool::new(false),
        stamp: Mutex::new(if paired { mtime() } else { None }),
        id_lang: lang_id(&values(st)),
        last: Mutex::new(None),
        pending: Mutex::new(None),
    });
    if let Some(old) = st.tg.lock().unwrap().replace(b.clone()) {
        old.stop.store(true, Ordering::Relaxed);
    }
    let (s1, b1) = (st.clone(), b.clone());
    thread::spawn(move || poll(s1, b1));
    let (s2, b2) = (st.clone(), b.clone());
    thread::spawn(move || send_loop(s2, b2, rx));
    let (s3, b3) = (st.clone(), b.clone());
    thread::spawn(move || watch(s3, b3));
    changed(st);
    b
}

/// daemon start: a bot bound before keeps running (FR-80)
pub fn start_saved(st: &Arc<State>) {
    if let Some(l) = load() {
        spawn(st, l.token.clone(), String::new(), Some(l));
    }
}

/// `telegram.pair {token}` (UI channel): answers at once (UI requests run in order on one connection), then checks the
/// token with getMe in the background and starts polling with a fresh one-time code; the result comes as `telegram.changed`.
/// A rejected token leaves a running bot as it was.
pub fn pair(st: &Arc<State>, token: &str) -> Res {
    let token = token.trim().to_string();
    if !valid_token(&token) {
        return Err("bad_token");
    }
    let st = st.clone();
    thread::spawn(move || {
        let fail = |err: &str| {
            let mut s = status(&st);
            s["error"] = json!(err);
            let ev = line(json!({"event": "telegram.changed", "params": s}));
            st.ui_txs.lock().unwrap().retain(|(_, tx)| tx.try_send(ev.clone()).is_ok());
        };
        let me = match api(&token, "getMe", &json!({}), 15) {
            Ok(me) => me,
            Err((401 | 404, _)) => return fail("token_rejected"),
            Err(_) => return fail("offline"),
        };
        let Ok(code) = new_code() else { return fail("io") };
        let _ = fs::remove_file(file());
        let b = spawn(&st, token, me["username"].as_str().unwrap_or_default().to_string(), None);
        *b.code.lock().unwrap() = Some((code, Instant::now(), 0));
        changed(&st);
    });
    Ok(json!({"state": "checking"}))
}

/// `telegram.unpair` (UI channel): file, token and threads gone at once (SEC-36)
pub fn unpair(st: &State) -> Value {
    if let Some(b) = st.tg.lock().unwrap().take() {
        b.stop.store(true, Ordering::Relaxed);
    }
    let _ = fs::remove_file(file());
    changed(st);
    status(st)
}

fn poll(st: Arc<State>, b: Arc<Bot>) {
    let _ = api(&b.token, "deleteWebhook", &json!({}), 15);
    if b.bot.lock().unwrap().is_empty()
        && let Ok(me) = api(&b.token, "getMe", &json!({}), 15)
    {
        *b.bot.lock().unwrap() = me["username"].as_str().unwrap_or_default().to_string();
        changed(&st);
    }
    let (mut offset, mut wait, mut conflicts) = (0i64, 2u64, 0);
    while !b.stop.load(Ordering::Relaxed) {
        let body = json!({"offset": offset, "timeout": 50, "allowed_updates": ["message", "callback_query"]});
        match api(&b.token, "getUpdates", &body, 60) {
            Ok(list) => {
                (wait, conflicts) = (2, 0);
                b.set(&st, if b.linked().is_some() { "on" } else { "pairing" }, "");
                for u in list.as_array().into_iter().flatten() {
                    offset = offset.max(u["update_id"].as_i64().unwrap_or_default() + 1);
                    if b.stop.load(Ordering::Relaxed) {
                        return;
                    }
                    handle(&st, &b, u);
                }
            }
            Err((401, _)) => return b.set(&st, "error", "token_rejected"),
            // 409 = another poller on the same token; the one being replaced ends within one long poll (50 s), so wait it out
            Err((409, _)) if conflicts < 12 => {
                conflicts += 1;
                thread::sleep(Duration::from_secs(5));
            }
            Err((409, _)) => return b.set(&st, "error", "token_busy"),
            Err(_) => {
                b.set(&st, "offline", "");
                thread::sleep(Duration::from_secs(wait));
                wait = (wait * 2).min(60);
            }
        }
    }
}

fn send_loop(st: Arc<State>, b: Arc<Bot>, rx: Receiver<Out>) {
    while !b.stop.load(Ordering::Relaxed) {
        let Ok(o) = rx.recv_timeout(Duration::from_secs(10)) else { continue };
        let (method, body, pane) = match o {
            Out::Api(m, body) => (m, body, None),
            Out::Notice(id, kind, at) => {
                thread::sleep(SETTLE.saturating_sub(at.elapsed()));
                match notice(&st, &b, id, kind) {
                    Some(body) => {
                        if debug() {
                            log(format_args!("out {}", body["text"]));
                        }
                        ("sendMessage", body, Some(id))
                    }
                    None => continue,
                }
            }
        };
        for _ in 0..3 {
            match api(&b.token, method, &body, 15) {
                Ok(r) => {
                    if let (Some(p), Some(mid)) = (pane, r["message_id"].as_i64()) {
                        b.remember(mid, p);
                        // the pane that just spoke becomes the active one: plain text answers it
                        let agent = st.panes.lock().unwrap().get(&p).is_some_and(|x| x.agent_name().is_some());
                        if agent {
                            *b.last.lock().unwrap() = Some(p);
                        }
                    }
                    break;
                }
                Err((429, Some(s))) => thread::sleep(Duration::from_secs(s.min(60))),
                Err((code, _)) => {
                    log(format_args!("{method} failed {code}"));
                    break;
                }
            }
        }
        thread::sleep(Duration::from_millis(1100));
    }
}

/// away = locked or idle (TECH 7e); `GEMBALA_TG_AWAY=1|0` forces it for tests
fn is_away(mins: u64) -> bool {
    match std::env::var("GEMBALA_TG_AWAY").as_deref() {
        Ok("1") => return true,
        Ok("0") => return false,
        _ => {}
    }
    let run = |args: &[&str]| Command::new("/usr/sbin/ioreg").args(args).output().map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default();
    locked(&run(&["-n", "Root", "-d", "1"])) || idle_secs(&run(&["-c", "IOHIDSystem", "-d", "4"])).is_some_and(|s| s >= mins * 60)
}

fn watch(st: Arc<State>, b: Arc<Bot>) {
    while !b.stop.load(Ordering::Relaxed) {
        if b.linked().is_some() {
            // SEC-36: telegram.json changed outside the daemon = stop until paired again from the app
            if mtime() != *b.stamp.lock().unwrap() {
                b.stop.store(true, Ordering::Relaxed);
                return b.set(&st, "error", "file_changed");
            }
            let mins = values(&st)["telegram"]["away_idle_mins"].as_u64().unwrap_or(5);
            let away = is_away(mins);
            if away && !b.away.swap(true, Ordering::Relaxed) {
                flush(&st, &b);
            }
            b.away.store(away, Ordering::Relaxed);
        }
        thread::sleep(Duration::from_secs(10));
    }
}

/// just went away: what is still waiting goes out now
fn flush(st: &State, b: &Bot) {
    let panes: Vec<(u64, Arc<Pane>)> = st.panes.lock().unwrap().iter().map(|(id, p)| (*id, p.clone())).collect();
    for (id, pane) in panes {
        let seq = pane.status_seq.load(Ordering::Relaxed);
        if *pane.status.lock().unwrap() == "blocked" && pane.agent_name().is_some() && b.first(id, seq) {
            let _ = b.out.try_send(Out::Notice(id, Kind::Blocked(seq), Instant::now()));
        } else if pane.asked_pw.load(Ordering::Relaxed) {
            let _ = b.out.try_send(Out::Notice(id, Kind::Password, Instant::now()));
        }
    }
}

fn live(st: &State) -> Option<Arc<Bot>> {
    st.tg.lock().unwrap().clone().filter(|b| b.linked().is_some() && !b.stop.load(Ordering::Relaxed))
}

/// `status_changed`: blocked/done of an agent pane while the Mac is away (FR-40 switches in `[notify]`). A prompt typed
/// from the phone is always answered, away or not, like Hermes: 👀 on the user's message while it works, 👍 + result when done.
pub fn on_status(st: &State, id: u64, pane: &Pane, status: &str) {
    let Some(b) = live(st) else { return };
    if pane.agent_name().is_none() {
        return;
    }
    let asked = *pane.tg_ask.lock().unwrap();
    if !b.away.load(Ordering::Relaxed) && asked.is_none() {
        return;
    }
    let seq = pane.status_seq.load(Ordering::Relaxed);
    let on = |k: &str| asked.is_some() || values(st)["notify"][k].as_bool().unwrap_or(true);
    let kind = match status {
        "blocked" if on("on_blocked") => Kind::Blocked(seq),
        "done" if on("on_done") => Kind::Done,
        _ => return,
    };
    if kind == Kind::Done
        && let Some(m) = pane.tg_ask.lock().unwrap().take()
    {
        b.react(m, "👍");
    }
    if b.first(id, seq) {
        let _ = b.out.try_send(Out::Notice(id, kind, Instant::now()));
    }
}

/// Agent TUIs draw an input box under the conversation: Claude = rule, `❯` line, rule (status line below); omp = a
/// `╭…╮`/`╰…╯` box. Rows above the box (blank rows and rules right above it dropped too).
fn above_input(rows: &[&str]) -> usize {
    let rule = |s: &str| {
        let t = s.trim();
        t.chars().count() >= 10 && t.chars().all(|c| matches!(c, '─' | '━' | '-'))
    };
    let starts = |r: &str, c: char| r.trim_start().starts_with(c);
    let claude = (1..rows.len()).rev().find(|&i| (starts(rows[i], '❯') || starts(rows[i], '>')) && rule(rows[i - 1])).map(|i| i - 1);
    let boxed = || (0..rows.len()).rev().find(|&i| starts(rows[i], '╭') && rows[i + 1..].iter().any(|r| starts(r, '╰')));
    let Some(mut j) = claude.or_else(boxed) else { return rows.len() };
    while j > 0 && (rule(rows[j - 1]) || rows[j - 1].trim().is_empty()) {
        j -= 1;
    }
    j
}

/// omp closes each turn with `YYYY-MM-DD hh:mm:ss  ⤵ …` (time, tokens)
fn stats_row(r: &str) -> bool {
    let b = r.trim_start().as_bytes();
    b.len() >= 19 && b[..19].iter().enumerate().all(|(i, c)| match i {
        4 | 7 => *c == b'-',
        10 => *c == b' ',
        13 | 16 => *c == b':',
        _ => c.is_ascii_digit(),
    })
}

/// Where the latest turn starts: the prompt typed from the phone (any agent), else Claude's `❯` echo, else after omp's
/// previous stats row. Banner, tips and older turns drop out.
fn turn_start(rows: &[&str], typed: Option<&str>) -> usize {
    let key: String = typed.and_then(|t| t.lines().next()).map(|l| l.trim().chars().take(30).collect()).unwrap_or_default();
    if let Some(i) = (!key.is_empty()).then(|| rows.iter().rposition(|r| r.contains(&key))).flatten() {
        return i;
    }
    if let Some(i) = rows.iter().rposition(|r| r.trim_start().starts_with('❯')) {
        return i;
    }
    let stats: Vec<usize> = rows.iter().enumerate().filter(|(_, r)| stats_row(r)).map(|(i, _)| i).collect();
    match stats[..] {
        [.., prev, _] => prev + 1,
        _ => 0,
    }
}

/// agent panes: the latest turn above the input box; shell panes: the screen as is (a `❯` zsh prompt isn't a turn)
fn screen_snip(pane: &Pane, lines: usize) -> String {
    let text = pane.view.lock().unwrap().0.text();
    if pane.agent_name().is_none() {
        return snippet(&text, lines);
    }
    let rows: Vec<&str> = text.lines().collect();
    let rows = &rows[..above_input(&rows)];
    let typed = pane.tg_text.lock().unwrap().clone();
    snippet(&rows[turn_start(rows, typed.as_deref())..].join("\n"), lines)
}

/// `screen_watch`, once a second per pane: password prompt (rising edge) and long shell commands that finished
pub fn tick(st: &State, id: u64, pane: &Pane) {
    let long = pane.long_done.lock().unwrap().take();
    let Some(b) = live(st) else {
        pane.asked_pw.store(false, Ordering::Relaxed);
        return;
    };
    let pw = pty::busy(&pane.master, pane.sid).is_some() && pty::wants_password(&pane.master);
    let was = pane.asked_pw.swap(pw, Ordering::Relaxed);
    if !b.away.load(Ordering::Relaxed) {
        return;
    }
    if pw && !was {
        let _ = b.out.try_send(Out::Notice(id, Kind::Password, Instant::now()));
    }
    let min = values(st)["notify"]["long_command_secs"].as_u64().unwrap_or(30);
    if let Some(secs) = long.filter(|s| *s >= min) {
        let _ = b.out.try_send(Out::Notice(id, Kind::Long(secs), Instant::now()));
    }
}

fn snippet_on(v: &Value, ws: &str) -> bool {
    v["telegram"]["snippet"].as_bool().unwrap_or(true) && !v["telegram"]["snippet_off"].as_array().into_iter().flatten().any(|w| w.as_str() == Some(ws))
}

/// first line only name · workspace + status (what the lock screen shows, SEC-37)
fn head(name: &str, ws: &str, status: &str) -> String {
    if ws.is_empty() { format!("<b>{}</b> {}", esc(name), esc(status)) } else { format!("<b>{} · {}</b> {}", esc(name), esc(ws), esc(status)) }
}

/// message body built when it is sent (after SETTLE), from the pane as it is now; `None` = no longer relevant
fn notice(st: &State, b: &Bot, id: u64, kind: Kind) -> Option<Value> {
    let pane = st.panes.lock().unwrap().get(&id).cloned()?;
    let (name, ws) = (pane.name.lock().unwrap().clone(), pane.workspace.lock().unwrap().clone());
    let agent = pane.agent_name().unwrap_or_default();
    let v = values(st);
    let id_l = b.id_lang;
    let status = match kind {
        Kind::Blocked(seq) => {
            if *pane.status.lock().unwrap() != "blocked" || pane.status_seq.load(Ordering::Relaxed) != seq {
                return None;
            }
            t(id_l, "blocked").to_string()
        }
        Kind::Done => t(id_l, "done").to_string(),
        Kind::Password => t(id_l, "password").to_string(),
        Kind::Long(secs) => format!("{} ({})", t(id_l, "long"), dur(secs)),
    };
    let mut text = head(&name, &ws, &status);
    if kind != Kind::Password && snippet_on(&v, &ws) {
        if let Some(l) = label(&pane.title.lock().unwrap(), &agent) {
            text += &format!("\n{}", esc(&l));
        }
        let lines = v["telegram"]["lines"].as_u64().unwrap_or(30).min(30) as usize;
        let snip = screen_snip(&pane, lines);
        if !snip.is_empty() {
            text += &format!("\n<pre>{}</pre>", esc(&snip));
        }
    }
    let mut body = json!({"chat_id": b.chat(), "text": text, "parse_mode": "HTML"});
    if let Kind::Blocked(seq) = kind {
        let row: Vec<Value> = keys(&agent).iter().map(|k| json!({"text": if *k == "esc" { "Esc" } else { k }, "callback_data": cb_data(id, seq, k)})).collect();
        body["reply_markup"] = json!({"inline_keyboard": [row]});
    }
    Some(body)
}

/// pane for a phone message: reply to a bot message, a name, or the active pane; no active pane and a single agent pane = that
/// one; several = `pick` (the caller offers buttons)
fn target(st: &State, b: &Bot, to: Option<To>) -> Result<(u64, Arc<Pane>), &'static str> {
    let id = match to {
        Some(To::Name(n)) => return st.by_name(&n).ok_or("no_pane"),
        Some(To::Reply(r, name)) => match b.sent.lock().unwrap().iter().find(|(m, _)| *m == r).map(|(_, p)| *p) {
            Some(id) => id,
            // not in RAM (daemon restarted): the pane named on the message's first line
            None => return st.by_name(&name).filter(|_| !name.is_empty()).ok_or("old_reply"),
        },
        None => {
            let panes = st.panes.lock().unwrap();
            match (*b.last.lock().unwrap()).filter(|id| panes.contains_key(id)) {
                Some(id) => id,
                None => {
                    let agents: Vec<u64> = panes.iter().filter(|(_, p)| p.agent_name().is_some()).map(|(id, _)| *id).collect();
                    match agents[..] {
                        [] => return Err("no_agent"),
                        [one] => one,
                        _ => return Err("pick"),
                    }
                }
            }
        }
    };
    st.panes.lock().unwrap().get(&id).map(|p| (id, p.clone())).ok_or("no_pane")
}

/// buttons to pick the active pane, agent panes only, two per row
fn picker(st: &State, b: &Bot) {
    let mut panes: Vec<(String, u64)> =
        st.panes.lock().unwrap().iter().filter(|(_, p)| p.agent_name().is_some()).map(|(id, p)| (p.name.lock().unwrap().clone(), *id)).collect();
    panes.sort();
    let rows: Vec<Vec<Value>> = panes.chunks(2).map(|c| c.iter().map(|(n, id)| json!({"text": n, "callback_data": format!("p|{id}")})).collect()).collect();
    let text = esc(t(b.id_lang, "pick"));
    let _ = b.out.try_send(Out::Api("sendMessage", json!({"chat_id": b.chat(), "text": text, "parse_mode": "HTML", "reply_markup": {"inline_keyboard": rows}})));
}

/// type into the pane (SEC-36 checks), then 👀 on the user's message until the pane is done (`on_status`)
fn deliver(st: &State, b: &Bot, id: u64, pane: &Pane, text: &str, msg: i64) {
    let status = *pane.status.lock().unwrap();
    let pw = pty::busy(&pane.master, pane.sid).is_some() && pty::wants_password(&pane.master);
    let why = match typed(pane.agent_name().is_some(), status, pw, text) {
        Ok(bytes) => match type_into(st, id, pane, &bytes) {
            Ok(()) => {
                audit(st, pane, text);
                *b.last.lock().unwrap() = Some(id);
                *pane.tg_ask.lock().unwrap() = Some(msg);
                *pane.tg_text.lock().unwrap() = Some(text.to_string());
                return b.react(msg, "👀");
            }
            Err(_) => "no_pane",
        },
        Err(Why::Shell) => "shell",
        Err(Why::Blocked) => "need_ok",
        Err(Why::Password) => "pw",
        Err(Why::TooLong) => "too_long",
        Err(Why::Bad) => "bad",
    };
    b.tell(t(b.id_lang, why));
}

fn handle(st: &Arc<State>, b: &Bot, u: &Value) {
    let id_l = b.id_lang;
    let input = parse(u, b.linked().as_ref(), now());
    if debug() {
        log(format_args!("in {input:?}"));
    }
    match input {
        In::Ignore => {}
        In::Start { code, chat, user, name } => {
            let mut c = b.code.lock().unwrap();
            let Some((want, at, tries)) = c.as_mut() else { return };
            if at.elapsed() > CODE_TTL || *tries >= CODE_TRIES {
                *c = None;
                return;
            }
            if !ct_eq(code.as_bytes(), want.as_bytes()) {
                *tries += 1;
                return;
            }
            *c = None;
            drop(c);
            let link = Link { token: b.token.clone(), chat, user, name };
            let body = json!({"token": link.token, "chat": chat, "user": user, "name": link.name}).to_string();
            if config::write_atomic(&file(), body.as_bytes()).is_err() {
                return b.set(st, "error", "io");
            }
            *b.stamp.lock().unwrap() = mtime();
            *b.link.lock().unwrap() = Some(link);
            let list: [(&str, &str); 3] = if id_l {
                [("status", "semua pane"), ("ke", "<pane> — ganti pane aktif"), ("lihat", "<pane> — layar terbaru")]
            } else {
                [("status", "every pane"), ("to", "<pane> — switch the active pane"), ("look", "<pane> — latest screen")]
            };
            let cmds: Vec<Value> = list.iter().map(|(c, d)| json!({"command": c, "description": d})).collect();
            let _ = b.out.try_send(Out::Api("setMyCommands", json!({"commands": cmds})));
            let host = Command::new("/usr/sbin/scutil").args(["--get", "ComputerName"]).output().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default();
            b.say(&format!("{} {}.\n\n{}", t(id_l, "linked"), esc(&host), esc(t(id_l, "help"))));
            b.set(st, "on", "");
        }
        In::Help => b.tell(t(id_l, "help")),
        In::Status => b.say(&status_text(st, id_l, *b.last.lock().unwrap())),
        In::Look(name) => match st.by_name(&name) {
            None => b.tell(t(id_l, "no_pane")),
            Some((_, pane)) => {
                let (n, ws) = (pane.name.lock().unwrap().clone(), pane.workspace.lock().unwrap().clone());
                let st_txt = state_word(&pane, id_l);
                let v = values(st);
                if !snippet_on(&v, &ws) {
                    return b.say(&format!("{}\n{}", head(&n, &ws, &st_txt), esc(t(id_l, "snip_off"))));
                }
                let lines = v["telegram"]["lines"].as_u64().unwrap_or(30).min(30) as usize;
                let snip = screen_snip(&pane, lines);
                b.say(&format!("{}\n<pre>{}</pre>", head(&n, &ws, &st_txt), esc(if snip.is_empty() { " " } else { &snip })));
            }
        },
        In::Type { to, text, msg } => match target(st, b, to) {
            Ok((id, pane)) => deliver(st, b, id, &pane, &text, msg),
            Err("pick") => {
                *b.pending.lock().unwrap() = Some((text, msg));
                picker(st, b);
            }
            Err(k) => b.tell(t(id_l, k)),
        },
        In::Use(name) => match st.by_name(&name) {
            Some((id, p)) if p.agent_name().is_some() => {
                *b.last.lock().unwrap() = Some(id);
                b.tell(&format!("{} {}", t(id_l, "now_to"), p.name.lock().unwrap()));
            }
            Some(_) => b.tell(t(id_l, "shell")),
            None => b.tell(t(id_l, "no_pane")),
        },
        In::Pick { pane: id, cb, msg } => {
            let pane = st.panes.lock().unwrap().get(&id).cloned().filter(|p| p.agent_name().is_some());
            let name = pane.as_ref().map(|p| p.name.lock().unwrap().clone());
            let word = name.as_ref().map_or_else(|| t(id_l, "no_pane").to_string(), |n| format!("→ {n}"));
            let _ = b.out.try_send(Out::Api("answerCallbackQuery", json!({"callback_query_id": cb, "text": word})));
            let _ = b.out.try_send(Out::Api("editMessageReplyMarkup", json!({"chat_id": b.chat(), "message_id": msg, "reply_markup": {"inline_keyboard": []}})));
            if let Some(p) = pane {
                *b.last.lock().unwrap() = Some(id);
                let waiting = b.pending.lock().unwrap().take();
                if let Some((text, m)) = waiting {
                    deliver(st, b, id, &p, &text, m);
                }
            }
        }
        In::Media { to, caption, file, ext, msg } => match target(st, b, to) {
            // ponytail: no pending photo; the user picks a pane, then sends it again
            Err("pick") => picker(st, b),
            Err(k) => b.tell(t(id_l, k)),
            // checked before the download: nothing lands on disk for a pane that can't take it
            Ok((_, pane)) if pane.agent_name().is_none() => b.tell(t(id_l, "shell")),
            Ok((id, pane)) => match download(st, b, &file, &ext) {
                None => b.tell(t(id_l, "dl_fail")),
                Some(path) => {
                    let p = path.to_string_lossy();
                    let text = if caption.is_empty() { p.into_owned() } else { format!("{caption} {p}") };
                    deliver(st, b, id, &pane, &text, msg);
                }
            },
        },
        In::Key { pane: id, seq, key, cb, msg } => {
            let pane = st.panes.lock().unwrap().get(&id).cloned();
            let ok = pane.as_ref().is_some_and(|p| {
                // SEC-28 pattern: the same dialog is still open (status and its sequence unchanged)
                *p.status.lock().unwrap() == "blocked" && p.status_seq.load(Ordering::Relaxed) == seq && keys(&p.agent_name().unwrap_or_default()).contains(&key.as_str())
            });
            let pane = pane.filter(|_| ok);
            let sent = pane.as_ref().is_some_and(|p| type_into(st, id, p, key_bytes(&key)).is_ok());
            let word = if sent { t(id_l, "sent") } else { t(id_l, "gone") };
            let _ = b.out.try_send(Out::Api("answerCallbackQuery", json!({"callback_query_id": cb, "text": word})));
            let _ = b.out.try_send(Out::Api("editMessageReplyMarkup", json!({"chat_id": b.chat(), "message_id": msg, "reply_markup": {"inline_keyboard": []}})));
            if let (true, Some(p)) = (sent, pane) {
                audit(st, &p, &format!("[{}: {}]", t(id_l, "answered"), key));
            }
        }
    }
}

fn audit(st: &State, pane: &Pane, body: &str) {
    if let Some(a) = &st.audit {
        let name = pane.name.lock().unwrap().clone();
        let m = bus::Msg { id: String::new(), from: 0, from_name: "telegram".into(), to: 0, to_name: name, body: body.into(), reply_to: None, hop: 0, at: SystemTime::now() };
        a.write("telegram", &m, None);
    }
}

fn state_word(pane: &Pane, id_l: bool) -> String {
    match *pane.status.lock().unwrap() {
        "" => t(id_l, "shell_st").to_string(),
        "blocked" => format!("{} ({})", t(id_l, "blocked"), dur(pane.status_at.lock().unwrap().elapsed().as_secs())),
        s @ ("idle" | "working" | "done") => t(id_l, s).to_string(),
        s => s.to_string(),
    }
}

/// `/status`: per workspace, panes needing permission first (FR-65 order), the active pane marked, no screen content
fn status_text(st: &State, id_l: bool, active: Option<u64>) -> String {
    let mut rows: Vec<(String, bool, String, String)> = st
        .panes
        .lock()
        .unwrap()
        .iter()
        .map(|(id, p)| {
            let blocked = *p.status.lock().unwrap() == "blocked";
            let mut word = state_word(p, id_l);
            if active == Some(*id) {
                word += if id_l { " ← aktif" } else { " ← active" };
            }
            (p.workspace.lock().unwrap().clone(), !blocked, p.name.lock().unwrap().clone(), word)
        })
        .collect();
    if rows.is_empty() {
        return t(id_l, "none").to_string();
    }
    rows.sort();
    let mut out = String::new();
    let mut ws_seen: Option<&str> = None;
    for (ws, _, name, word) in &rows {
        if ws_seen != Some(ws.as_str()) {
            out += &format!("{}<b>{}</b>\n", if ws_seen.is_some() { "\n" } else { "" }, esc(if ws.is_empty() { "-" } else { ws }));
            ws_seen = Some(ws);
        }
        out += &format!("{} — {}\n", esc(name), esc(word));
    }
    out.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link() -> Link {
        Link { token: "1:x".into(), chat: 42, user: 7, name: "R".into() }
    }

    fn msg(chat: i64, user: i64, text: &str, date: i64) -> Value {
        json!({"message": {"message_id": 5, "date": date, "chat": {"id": chat, "type": "private"}, "from": {"id": user, "first_name": "Raka", "username": "raka"}, "text": text}})
    }

    #[test]
    fn token_dan_quote() {
        assert!(valid_token("8123456789:AAHabcdefghijklmnopqrstuvwxyz0123"));
        assert!(!valid_token("8123456789:short"));
        assert!(!valid_token("abc:AAHabcdefghijklmnopqrstuvwxyz0123"));
        assert!(!valid_token("8123456789:AAHabcdefghijklmnopqrstuvwxyz01\"3"));
        assert_eq!(curl_quote(r#"{"a":"x\"y\\n"}"#), r#"{\"a\":\"x\\\"y\\\\n\"}"#);
        assert_eq!(esc("<b>&"), "&lt;b&gt;&amp;");
    }

    #[test]
    fn potongan_layar() {
        assert_eq!(snippet("a\nb  \nc\n\n  \n", 2), "b\nc");
        assert_eq!(snippet("", 30), "");
        let long = "x".repeat(5000);
        assert_eq!(snippet(&long, 30).chars().count(), MAX_SNIPPET);
        assert_eq!(label("\u{2810} Fix login", "claude").as_deref(), Some("Fix login"));
        assert_eq!(label("claude", "claude"), None);
    }

    #[test]
    fn pairing_hanya_start_dari_chat_pribadi() {
        let n = 1000;
        assert_eq!(
            parse(&msg(42, 7, "/start ABCD2345", n), None, n),
            In::Start { code: "ABCD2345".into(), chat: 42, user: 7, name: "Raka (@raka)".into() }
        );
        assert_eq!(parse(&msg(42, 7, "/status", n), None, n), In::Ignore);
        let mut g = msg(42, 7, "/start ABCD2345", n);
        g["message"]["chat"]["type"] = json!("group");
        assert_eq!(parse(&g, None, n), In::Ignore);
    }

    #[test]
    fn chat_lain_dan_pesan_basi_diabaikan() {
        let l = link();
        let n = 1000;
        assert_eq!(parse(&msg(99, 7, "/status", n), Some(&l), n), In::Ignore);
        assert_eq!(parse(&msg(42, 8, "/status", n), Some(&l), n), In::Ignore);
        assert_eq!(parse(&msg(42, 7, "/status", n - MAX_AGE - 1), Some(&l), n), In::Ignore);
        assert_eq!(parse(&msg(42, 7, "/status@NifNightBot", n), Some(&l), n), In::Status);
        assert_eq!(parse(&msg(42, 7, "/lihat shell-6", n), Some(&l), n), In::Look("shell-6".into()));
        assert_eq!(parse(&msg(42, 7, "/lihat", n), Some(&l), n), In::Help);
        assert_eq!(parse(&msg(42, 7, "/kirim omp-2 lanjut ya", n), Some(&l), n), In::Type { to: Some(To::Name("omp-2".into())), text: "lanjut ya".into(), msg: 5 });
        assert_eq!(parse(&msg(42, 7, "/kirim omp-2", n), Some(&l), n), In::Help);
        let mut r = msg(42, 7, "bikin changelog", n);
        r["message"]["reply_to_message"] = json!({"message_id": 77});
        assert_eq!(parse(&r, Some(&l), n), In::Type { to: Some(To::Reply(77, String::new())), text: "bikin changelog".into(), msg: 5 });
        // a reply after a daemon restart still finds the pane from the quoted message's first line
        r["message"]["reply_to_message"]["text"] = json!("claude-1 · gembala kelar\n✳ Permintaan jawaban singkat");
        assert_eq!(parse(&r, Some(&l), n), In::Type { to: Some(To::Reply(77, "claude-1".into())), text: "bikin changelog".into(), msg: 5 });
        r["message"]["reply_to_message"]["text"] = json!("Ke pane mana?");
        assert_eq!(parse(&r, Some(&l), n), In::Type { to: Some(To::Reply(77, String::new())), text: "bikin changelog".into(), msg: 5 });
        // plain text = active pane, no command needed
        assert_eq!(parse(&msg(42, 7, "lanjut bikin test", n), Some(&l), n), In::Type { to: None, text: "lanjut bikin test".into(), msg: 5 });
        assert_eq!(parse(&msg(42, 7, "/ke claude-2", n), Some(&l), n), In::Use("claude-2".into()));
        assert_eq!(parse(&msg(42, 7, "/to", n), Some(&l), n), In::Help);
    }

    #[test]
    fn tombol_cuma_dari_pengguna_terikat() {
        let l = link();
        let q = |from: i64, data: &str| json!({"callback_query": {"id": "c1", "from": {"id": from}, "data": data, "message": {"message_id": 9, "chat": {"id": 42}}}});
        assert_eq!(parse(&q(7, "k|3|11|2"), Some(&l), 0), In::Key { pane: 3, seq: 11, key: "2".into(), cb: "c1".into(), msg: 9 });
        assert_eq!(parse(&q(8, "k|3|11|2"), Some(&l), 0), In::Ignore);
        assert_eq!(parse(&q(7, "k|3|11|y"), Some(&l), 0), In::Ignore);
        assert_eq!(parse(&q(7, "k|3|11|2"), None, 0), In::Ignore);
        assert_eq!(parse(&q(7, "p|4"), Some(&l), 0), In::Pick { pane: 4, cb: "c1".into(), msg: 9 });
        assert_eq!(parse(&q(8, "p|4"), Some(&l), 0), In::Ignore);
        assert_eq!(cb_parse(&cb_data(12, 4471, "esc")), Some((12, 4471, "esc".into())));
        assert!(cb_data(u64::MAX, u64::MAX, "esc").len() <= 64);
        assert_eq!(keys("codex"), &["esc"]);
    }

    #[test]
    fn ketik_dari_hp_dibatasi() {
        assert_eq!(typed(false, "", false, "ls"), Err(Why::Shell));
        assert_eq!(typed(true, "idle", true, "rahasia"), Err(Why::Password));
        assert_eq!(typed(true, "blocked", false, "y"), Err(Why::Blocked));
        assert_eq!(typed(true, "idle", false, &"a".repeat(MAX_TEXT + 1)), Err(Why::TooLong));
        assert_eq!(typed(true, "idle", false, "\x1b\x07"), Err(Why::Bad));
        assert_eq!(typed(true, "working", false, "lanjut\x1b[201~ ya").unwrap(), b"\x1b[200~lanjut[201~ ya\x1b[201~\r".to_vec());
    }

    #[test]
    fn foto_dan_file_dari_hp() {
        let l = link();
        let n = 1000;
        let mut p = msg(42, 7, "", n);
        p["message"]["photo"] = json!([{"file_id": "kecil", "file_size": 10}, {"file_id": "besar", "file_size": 900}]);
        assert_eq!(parse(&p, Some(&l), n), In::Media { to: None, caption: String::new(), file: "besar".into(), ext: "jpg".into(), msg: 5 });
        p["message"]["caption"] = json!("/kirim claude-1 cek gambar ini");
        assert_eq!(parse(&p, Some(&l), n), In::Media { to: Some(To::Name("claude-1".into())), caption: "cek gambar ini".into(), file: "besar".into(), ext: "jpg".into(), msg: 5 });
        let mut d = msg(42, 7, "", n);
        d["message"]["document"] = json!({"file_id": "f", "file_name": "log../../x.T!XT", "file_size": 5});
        d["message"]["reply_to_message"] = json!({"message_id": 77});
        assert_eq!(parse(&d, Some(&l), n), In::Media { to: Some(To::Reply(77, String::new())), caption: String::new(), file: "f".into(), ext: "bin".into(), msg: 5 });
        d["message"]["document"]["file_size"] = json!(21 << 20);
        assert_eq!(parse(&d, Some(&l), n), In::Help);
        let mut pdf = msg(42, 7, "", n);
        pdf["message"]["document"] = json!({"file_id": "f", "file_name": "Laporan.PDF"});
        assert!(matches!(parse(&pdf, Some(&l), n), In::Media { ref ext, .. } if ext == "pdf"));
    }

    #[test]
    fn kotak_input_claude_dibuang() {
        let rows = ["❯ bikin file", "  Ran 1 shell command", "● Done.", "", "──────────────", "❯ ", "──────────────", "  ◆ Fable 5.1 ⚡high"];
        assert_eq!(above_input(&rows), 3);
        assert_eq!(above_input(&["$ ls", "a b"]), 2);
        let convo = [" ▐▛███▜▌ Claude Code v2.1", "❯ halo", "● Halo.", "❯ jawab pake 1 kata", "● Oke."];
        assert_eq!(turn_start(&convo, None), 3);
        assert_eq!(turn_start(&convo, Some("halo")), 1);
        assert_eq!(turn_start(&["tanpa prompt"], None), 0);
        // omp: banner, older prompt, the phone's prompt, answer, stats row, input box
        let omp = ["   v18.16.0", " Connected to MCP servers.", " Yang senin 1 doang?", " 2026-11-08 23:20:01  ⤵ 900", "",
            " jawab satu kata saja: halo", "", " halo", " 2026-11-08 23:25:26  ⤵ 928  ⤴ 3", "", "╭── ⬢ DeepSeek ──╮", "╰─        ─╯"];
        let cut = above_input(&omp);
        assert_eq!(cut, 9);
        assert_eq!(turn_start(&omp[..cut], Some("jawab satu kata saja: halo\nbaris kedua")), 5);
        assert_eq!(turn_start(&omp[..cut], None), 4);
        assert!(stats_row(" 2026-11-08 23:25:26  ⤵ 928"));
        assert!(!stats_row("2026-11-08 tanpa jam"));
        assert_eq!(label("π > Check tasks", "omp").as_deref(), Some("Check tasks"));
        assert_eq!(label("✳ Claude Code", "claude"), None);
    }

    #[test]
    fn idle_dan_kunci_dari_ioreg() {
        assert_eq!(idle_secs("    | |     \"HIDIdleTime\" = 74107037708\n"), Some(74));
        assert_eq!(idle_secs("nothing"), None);
        assert!(locked(r#""IOConsoleUsers" = ({"CGSSessionScreenIsLocked"=Yes,"kCGSSessionOnConsoleKey"=Yes})"#));
        assert!(!locked(r#""IOConsoleUsers" = ({"kCGSSessionOnConsoleKey"=Yes})"#));
    }

    #[test]
    fn baris_pertama_tanpa_isi_layar() {
        assert_eq!(head("shell-6", "gembala", "butuh izin"), "<b>shell-6 · gembala</b> butuh izin");
        assert_eq!(head("a<b", "", "kelar"), "<b>a&lt;b</b> kelar");
        let v = json!({"telegram": {"snippet": true, "snippet_off": ["E2E Phase 1"]}});
        assert!(snippet_on(&v, "gembala"));
        assert!(!snippet_on(&v, "E2E Phase 1"));
        assert!(!snippet_on(&json!({"telegram": {"snippet": false}}), "gembala"));
        assert_eq!(dur(130), "2m 10s");
    }
}
