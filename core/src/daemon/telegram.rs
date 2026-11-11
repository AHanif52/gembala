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
/// agent answer in a "done" message (Telegram's limit is 4096 characters after the HTML tags are read)
const MAX_ANSWER: usize = 3500;
const MAP_MAX: usize = 500;
/// a hook can say "blocked" a moment before the dialog is drawn: the screen is read this long after the event
const SETTLE: Duration = Duration::from_secs(1);
/// An agent's hook can land a moment after its status (Codex's screen watcher calls it done before the notify hook
/// arrives): a done card waits this long for the turn's last message instead of going out with no answer (11 Nov).
const ANSWER_WAIT: Duration = Duration::from_secs(3);

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
    /// when each pane's current turn started (status went to working)
    started: Mutex<HashMap<u64, Instant>>,
    /// last turn durations per agent kind (secs, max `TURNS_KEPT`): the estimate shown while a phone prompt runs
    turns: Mutex<HashMap<String, VecDeque<u64>>>,
    /// per pane: "⏳ working" message for a phone prompt (message id once sent, last edit), deleted when done
    progress: Mutex<HashMap<u64, (Option<i64>, Instant)>>,
    /// picker message already answered: a second tap (before its buttons are gone) must not edit it again (Telegram 400)
    picked: Mutex<Option<i64>>,
    /// open multiSelect polls by poll id (`multi_opts`); removed on the first vote
    polls: Mutex<HashMap<String, Poll>>,
}

/// what a multiSelect poll was sent for: the pane, its dialog generation, the option texts it offered, its message
struct Poll {
    pane: u64,
    seq: u64,
    labels: Vec<String>,
    msg: i64,
}

const TURNS_KEPT: usize = 10;
const PROGRESS_EVERY: Duration = Duration::from_secs(30);

enum Out {
    Api(&'static str, Value),
    Notice(u64, Kind, Instant),
    /// progress message; its id goes into `Bot.progress` for later edits
    Progress(u64, Value),
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Kind {
    Blocked(u64),
    /// how long the turn took, 0 = unknown
    Done(u64),
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
    /// `/ke` alone: buttons of agent panes
    Choose,
    /// pane picked from the buttons offered for plain text without an active pane
    Pick { pane: u64, cb: String, msg: i64 },
    /// `/buka <agent> [pane]`: start an agent from the fixed list in a shell pane idle at its prompt
    Open { agent: String, pane: Option<String> },
    /// workspace picked for `/buka <agent>` (many idle shells): its panes next
    OpenWs { ws: String, agent: String, cb: String, msg: i64 },
    /// shell pane picked for `/buka <agent>`
    OpenPick { pane: u64, agent: String, cb: String, msg: i64 },
    /// photo or file: saved like a pasted image (FR-54, SEC-34), its path typed into the pane after the caption
    Media { to: Option<To>, caption: String, file: String, ext: String, msg: i64 },
    Key { pane: u64, seq: u64, key: String, cb: String, msg: i64 },
    /// a vote on a multiSelect poll: the option indexes picked
    Vote { poll: String, picks: Vec<usize> },
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

/// SEC-38: patterns adapted in order from Habblaud shared/activity.ts (MIT; see THIRD_PARTY_NOTICES).
pub(super) fn mask_secrets(text: &str) -> String {
    use std::sync::OnceLock;
    static PATTERNS: OnceLock<Vec<(regex_lite::Regex, &'static str)>> = OnceLock::new();
    let patterns = PATTERNS.get_or_init(|| {
        [
            (r"-----BEGIN [A-Z ]*PRIVATE KEY-----[\s\S]*?(?:-----END [A-Z ]*PRIVATE KEY-----|$)", "[private key]"),
            (r#"(?i)\b(authorization|proxy-authorization|cookie|set-cookie|x-api-key|api-key|x-auth-token|private-token)(\s*:\s*)[^'"\n]+"#, "${1}${2}***"),
            (r"\b(Bearer|Basic)\s+[A-Za-z0-9._~+/=-]{6,}", "${1} ***"),
            (r#"(?i)(\b[a-z][a-z0-9+.-]*://[^\s:@/'"]+:)[^\s@/'"]+@"#, "${1}***@"),
            (r"(?i)(\bhttps?://hooks\.slack\.com/services/)T[A-Z0-9]+/B[A-Z0-9]+/[A-Za-z0-9]+", "${1}***"),
            (r"(?i)(\bhttps?://(?:discordapp|discord)\.com/api/webhooks/)\d+/[A-Za-z0-9_-]+", "${1}***"),
            (r"(?i)(\bhttps?://api\.telegram\.org/bot)\d+:[A-Za-z0-9_-]+", "${1}***"),
            (r#"((?:^|\s)(?:-u|--user)\s+['"]?[^\s:'"]+:)[^\s'"]+"#, "${1}***"),
            (r#"(?i)((?:^|\s)--?(?:token|password|passwd|pass|secret|api-key|apikey|access-token|auth-token)(?:\s+|=))(['"]?)[^\s'"]+['"]?"#, "${1}${2}***${2}"),
            (r#"(?i)\b([A-Za-z0-9_.-]*?(?:token|secret|passw(?:or)?d|pwd|api[_-]?key|access[_-]?key|private[_-]?key|_key|credentials?))(["']?\s*[=:]\s*)(["']?)([^\s"'&;|,}]+)["']?"#, "${1}${2}${3}***${3}"),
            (r"\bsk-(?:ant-|proj-)?[A-Za-z0-9_-]{8,}", "sk-***"),
            (r"\bgh[pousr]_[A-Za-z0-9]{16,}", "gh*_***"),
            (r"\bgithub_pat_[A-Za-z0-9_]{16,}", "github_pat_***"),
            (r"\bglpat-[A-Za-z0-9_-]{16,}", "glpat-***"),
            (r"\bxox[abposr]-[A-Za-z0-9-]{8,}", "xox*-***"),
            (r"\b(AKIA|ASIA)[0-9A-Z]{16}\b", "${1}***"),
            (r"\bAIza[0-9A-Za-z_-]{30,}", "AIza***"),
            (r"\b(?:sk|pk|rk)_(?:live|test)_[A-Za-z0-9]{10,}", "sk_***"),
            (r"\bnpm_[A-Za-z0-9]{30,}", "npm_***"),
            (r"\bhf_[A-Za-z0-9]{30,}", "hf_***"),
            (r"\beyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}", "eyJ***"),
        ].into_iter().map(|(pattern, replacement)| (regex_lite::Regex::new(pattern).unwrap(), replacement)).collect()
    });
    patterns.iter().fold(text.to_string(), |s, (pattern, replacement)| pattern.replace_all(&s, *replacement).into_owned())
}

/// a message whose HTML Telegram refused, as plain text (tags dropped, entities back to characters)
fn plain(mut body: Value) -> Value {
    let tags = regex_lite::Regex::new(r"<[^>]*>").unwrap();
    let text = tags.replace_all(body["text"].as_str().unwrap_or_default(), "").replace("&lt;", "<").replace("&gt;", ">").replace("&amp;", "&");
    body["text"] = json!(text);
    body.as_object_mut().map(|o| o.remove("parse_mode"));
    body
}

/// The agent's markdown answer as Telegram HTML (9 Nov: links inside `<pre>` couldn't be tapped, tables unreadable):
/// `[t](url)` = link, `**b**` = bold, `` `c` `` = code, fences = `<pre>`, `#` headings bold, `- ` = `• `, a table row = cells
/// joined by ` · ` (the `|---|` row dropped). Plain URLs Telegram links by itself. At most `max` characters of the answer.
fn md_html(md: &str, max: usize) -> String {
    let link = regex_lite::Regex::new(r"\[([^\]]+)\]\((https?://[^)\s]+)\)").unwrap();
    let bold = regex_lite::Regex::new(r"\*\*([^*]+)\*\*").unwrap();
    let inline = |s: &str| -> String {
        // odd pieces between backticks are code; links and bold only outside them
        s.split('`')
            .enumerate()
            .map(|(i, p)| if i % 2 == 1 { format!("<code>{}</code>", esc(p)) } else { bold.replace_all(&link.replace_all(&esc(p), "<a href=\"$2\">$1</a>"), "<b>$1</b>").into_owned() })
            .collect()
    };
    let n = md.chars().count();
    let md: String = if n > max { md.chars().take(max).collect::<String>() + " …" } else { md.to_string() };
    let (mut out, mut code) = (Vec::new(), None::<Vec<&str>>);
    for line in md.lines() {
        let t = line.trim();
        if t.starts_with("```") {
            match code.take() {
                Some(c) => out.push(format!("<pre>{}</pre>", esc(&c.join("\n")))),
                None => code = Some(Vec::new()),
            }
        } else if let Some(c) = code.as_mut() {
            c.push(line);
        } else if t.starts_with('|') {
            if !t.chars().all(|c| matches!(c, '|' | '-' | ':' | ' ')) {
                out.push(t.trim_matches('|').split('|').map(|c| inline(c.trim())).filter(|c| !c.is_empty()).collect::<Vec<_>>().join(" · "));
            }
        } else if let Some(h) = t.strip_prefix('#') {
            out.push(format!("<b>{}</b>", inline(h.trim_start_matches('#').trim())));
        } else if let Some(li) = t.strip_prefix("- ").or_else(|| t.strip_prefix("* ")) {
            out.push(format!("• {}", inline(li)));
        } else {
            out.push(inline(line));
        }
    }
    // an answer cut inside a fence still closes it
    if let Some(c) = code {
        out.push(format!("<pre>{}</pre>", esc(&c.join("\n"))));
    }
    out.join("\n")
}

/// last `lines` rows, trailing blank rows dropped, at most `MAX_SNIPPET` characters (cut from the top)
fn snippet(screen: &str, lines: usize) -> String {
    let masked = mask_secrets(screen);
    let rows = flat_tables(masked.lines().map(str::trim_end));
    let kept: Vec<String> = rows.into_iter().filter(|r| !chrome_row(r.trim())).collect();
    let end = kept.iter().rposition(|r| !r.is_empty()).map_or(0, |i| i + 1);
    let text = kept[end.saturating_sub(lines)..end].join("\n");
    let n = text.chars().count();
    text.chars().skip(n.saturating_sub(MAX_SNIPPET)).collect()
}

/// under a card: the agent's own answer from its stop hook (Claude, Codex, omp), else the screen for agents without one
/// (OpenCode). Codex's screen is its TUI transcript, never the answer (11 Nov: the done card held the process log, "Viewed
/// image", the 5h-limit warning and the input box).
fn turn_body(done: bool, agent: &str, answer: Option<&str>, screen: impl FnOnce() -> String) -> String {
    match answer {
        Some(a) => format!("\n\n{}", md_html(&mask_secrets(a), MAX_ANSWER)),
        None if done && agent == "codex" => String::new(),
        None => {
            let snip = screen();
            if snip.is_empty() { String::new() } else { format!("\n<pre>{}</pre>", esc(&snip)) }
        }
    }
}

/// agents whose stop hook carries the turn's own answer (`gembala hook` → `last`): their done card can wait for it
fn answers_from_hook(agent: &str) -> bool {
    matches!(agent, "claude" | "codex" | "omp")
}

/// A picker or a dialog on screen is the user's business on the Mac, never the answer: numbered options under a cursor
/// row (`❯ 1. Fable 5.1`) or the dialog's own footer. Read from the snippet that would have gone out, so scrollback above
/// the turn can't trip it.
fn picker_screen(snip: &str) -> bool {
    const FOOTERS: &[&str] = &["enter to set as default", "enter to confirm", "enter to select", "enter continue", "esc to cancel", "esc back"];
    let (mut numbered, mut cursor) = (false, false);
    for row in snip.lines() {
        let low = row.to_lowercase();
        if FOOTERS.iter().any(|f| low.contains(f)) {
            return true;
        }
        if choice(row).is_some() {
            numbered = true;
            let t = row.trim_start_matches(|c: char| c.is_whitespace() || c == '│');
            cursor |= t.starts_with('❯') || t.starts_with('›');
        }
    }
    numbered && cursor
}

/// agent TUI chrome that must not reach the phone: status/footer rows above the
/// input box (build status `Build · …`, model line, progress bars, quota and
/// key-hint footers). Dropped before the line budget, so chrome can't push the
/// real turn content out of the snippet.
fn chrome_row(t: &str) -> bool {
    if t.is_empty() {
        return false;
    }
    if t.contains("Build ·") || t.contains("ctrl+") || t.starts_with("Quota") || tool_call_row(t) {
        return true;
    }
    let bars = t.chars().filter(|c| matches!(c, '█' | '▓' | '▒' | '░' | '▄' | '▀' | '■' | '□' | '▪' | '▫')).count();
    bars >= 8 && bars * 2 >= t.chars().count()
}

/// Claude Code draws each tool call as `⏺ Bash(cd …)`, its result under `⎿`, plus the permission note and the cwd reset
/// (9 Nov: those rows buried the answer and the links inside them arrived as code). The `⏺` of a spoken answer stays:
/// only `⏺ ToolName(` is a call.
fn tool_call_row(t: &str) -> bool {
    let row = t.trim_start();
    let rest = row.strip_prefix('⏺').unwrap_or(row).trim_start();
    let call = rest.split_once('(').is_some_and(|(name, _)| !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'));
    row.starts_with('⎿') || (row.starts_with('⏺') && call) || t.contains("Allowed by auto mode classifier") || row.starts_with("Shell cwd was reset")
}

/// baris judul dialog konfirmasi OpenCode 1.19.5 (`△` + judul), bukan baris
/// opsi prompt utama: gerbang Enter kedua tombol Always. Baris opsi yang
/// tersorot (`❯ Always allow`) dan baris opsi utama tidak diawali `△`.
fn is_always_confirm_line(line: &str) -> bool {
    let t = line.trim();
    t.starts_with('△') && t.contains("Always allow")
}

fn always_confirm_on_screen(pane: &Pane) -> bool {
    pane.view.lock().unwrap().0.text().lines().any(is_always_confirm_line)
}

/// Box tables (Claude's `┌─┬─┐ │ … │`) read badly on a phone (9 Nov: "susah kebaca"): border rows drop, the lines a
/// row wraps over join cell by cell, one table row = one line with cells joined by ` · `
fn flat_tables<'a>(rows: impl Iterator<Item = &'a str>) -> Vec<String> {
    const BAR: [char; 2] = ['│', '┃'];
    let boxy = |c: char| ('\u{2500}'..='\u{257f}').contains(&c);
    let flush = |cells: &mut Vec<String>, out: &mut Vec<String>| {
        if !cells.is_empty() {
            out.push(cells.iter().filter(|c| !c.is_empty()).cloned().collect::<Vec<_>>().join(" · "));
            cells.clear();
        }
    };
    let (mut out, mut cells) = (Vec::new(), Vec::<String>::new());
    for r in rows {
        let t = r.trim();
        if !t.is_empty() && t.chars().all(|c| boxy(c) || c == ' ') && t.chars().any(|c| boxy(c) && !BAR.contains(&c)) {
            flush(&mut cells, &mut out);
        } else if t.starts_with(BAR) {
            let parts: Vec<&str> = t.trim_matches(BAR).split(BAR).collect();
            if cells.len() < parts.len() {
                cells.resize(parts.len(), String::new());
            }
            for (cell, p) in cells.iter_mut().zip(parts).filter(|(_, p)| !p.trim().is_empty()) {
                if !cell.is_empty() {
                    cell.push(' ');
                }
                cell.push_str(p.trim());
            }
        } else {
            flush(&mut cells, &mut out);
            out.push(r.to_string());
        }
    }
    flush(&mut cells, &mut out);
    out
}

/// FR-69 label without the spinner and generic titles (agent name)
fn label(title: &str, agent: &str) -> Option<String> {
    // spinner (braille), Claude's ✳, omp's `π >` prefix
    let t = title.trim_start_matches(|c: char| ('\u{2800}'..='\u{28ff}').contains(&c) || matches!(c, '✳' | '✻' | 'π' | '>') || c.is_whitespace()).trim();
    let generic = t.eq_ignore_ascii_case(agent) || t.eq_ignore_ascii_case("claude code");
    (!t.is_empty() && !generic).then(|| mask_secrets(t).chars().take(120).collect())
}

fn keys(agent: &str) -> &'static [&'static str] {
    // FR-80: buttons mirror each agent's own permission dialog (TECH 7e).
    // OpenCode 1.19.5, verified live 9 Nov 2026 (PTY + pyte, same bytes as the
    // buttons): initial selected is "once" so Enter approves at once; Right+Enter
    // moves to "always" and opens the "△ Always allow" confirmation stage whose
    // Enter confirms; Esc closes the dialog (no reason box in this version).
    // omp 18.18.3, verified live 10 Nov: bash dialog is Approve/Deny, the write
    // dialog adds "Approve write for this session" in the middle; Enter approves
    // once and Esc denies in both, so only those two are buttons (no Down+Enter).
    // Codex 0.167 numbered dialog (10 Nov screen): `› 1. Yes, proceed (y)`; a digit button sends the shortcut that row
    // shows in parentheses (codex_key), read again when pressed.
    // Claude numbered dialog: digits; Esc cancels.
    // FR-99 P2: an agent whose dialog was never recorded gets no button (Gemini, a new version): the prompt is answered on
    // the Mac. Sending Esc there could cancel something the user wanted, so the bar and the bot show no buttons at all.
    if agent == "claude" || agent == "codex" { &["1", "2", "3", "esc"] } else if agent == "opencode" { &["once", "always", "esc"] } else if agent == "omp" { &["once", "esc"] } else { &[] }
}

/// `❯ 2. No` / `  3. No, and tell Claude…` / Codex `› 1. Yes` -> the option number of a numbered dialog row
fn choice(row: &str) -> Option<u8> {
    let s = row.trim_start_matches(|c: char| c.is_whitespace() || c == '│' || c == '❯' || c == '›');
    let (d, rest) = s.split_once(". ")?;
    (d.len() == 1 && !rest.is_empty()).then(|| d.parse().ok()).flatten().filter(|n| (1..=9).contains(n))
}

/// FR-84: buttons under a "needs you" message. None for the folder-trust dialog: trusting lets the folder run code, and
/// Claude's first (unnumbered) option there is "No, exit", so it's answered on the Mac.
fn blocked_keys(agent: &str, screen: &str, reason: &str, id_l: bool) -> Vec<(&'static str, String)> {
    if reason == claude::TRUST {
        Vec::new()
    } else if agent == "claude" || agent == "codex" {
        claude_keys(agent, screen, id_l)
    } else if known_dialog(agent, screen) {
        keys(agent).iter().map(|k| (*k, button_label(agent, k, id_l))).collect()
    } else {
        Vec::new()
    }
}

/// static-key agents still need the recorded prompt on screen; an unrelated blocked screen gets no guessed buttons.
fn known_dialog(agent: &str, screen: &str) -> bool {
    let rows: Vec<&str> = screen.lines().map(unbox).collect();
    match agent {
        "omp" => rows.iter().any(|r| r.contains("Allow tool:"))
            && rows.iter().any(|r| r.trim_start_matches(['❯', '›']).trim() == "Approve")
            && rows.iter().any(|r| r.trim_start_matches(['❯', '›']).trim() == "Deny"),
        "opencode" => rows.contains(&"△ Permission required")
            && rows.iter().any(|r| r.contains("Allow once") && r.contains("Allow always") && r.contains("Reject")),
        _ => false,
    }
}

/// FR-80: omp and OpenCode buttons say what they do in the app's language (the screen words are English and "Allow once"
/// doesn't say it approves one run). FR-99 P2: OpenCode's own words "once/always" became "Izinkan sekali/Izinkan selalu"
/// (the mockup's bar and the bot show the same three buttons), and "don't ask again" reads "Izinkan terus".
fn button_label(agent: &str, key: &str, id_l: bool) -> String {
    let word = match (agent, key) {
        ("omp", "once") => if id_l { "Izinkan" } else { "Allow" },
        ("omp", "esc") => if id_l { "Tolak" } else { "Deny" },
        ("opencode", "once") => if id_l { "Izinkan sekali" } else { "Allow once" },
        ("opencode", "always") => if id_l { "Izinkan selalu" } else { "Allow always" },
        ("opencode", "esc") => if id_l { "Tolak" } else { "Reject" },
        (_, "esc") => if id_l { "Tolak" } else { "Deny" },
        _ => key,
    };
    word.to_string()
}

/// FR-99 P2: one button per option of a numbered dialog, in the words the user acts with. The dialog's own text stays the
/// fallback (BRD: the agent's question isn't translated), but the shapes every agent has get Gembala's words so the bar,
/// the bot and the strip say the same thing. None = no button: a mode switch ("switch to auto mode") and the deny option,
/// which is the same cancel as the one "Tolak" button already under every dialog (Claude "4. No", Codex "3. No … (esc)").
fn option_label(i: usize, raw: &str, id_l: bool) -> Option<String> {
    if switches_mode(raw) {
        return None;
    }
    let l = raw.to_lowercase();
    if ["no", "deny", "reject", "cancel"].iter().any(|w| l.trim_start().starts_with(w)) {
        return None;
    }
    let again = ["don't ask", "dont ask", "always", "from now on", "this session"].iter().any(|w| l.contains(w));
    let yes = ["yes", "proceed", "allow", "approve"].iter().any(|w| l.contains(w));
    let word = if again && yes {
        if id_l { "Izinkan terus" } else { "Allow always" }
    } else if yes {
        if id_l { "Izinkan" } else { "Allow" }
    } else {
        return Some(format!("{}. {}", i + 1, short(&raw.split_whitespace().collect::<Vec<_>>().join(" "), 60)));
    };
    Some(format!("{}. {}", i + 1, word))
}

const DIGITS: [&str; 9] = ["1", "2", "3", "4", "5", "6", "7", "8", "9"];

/// a screen row without box edges and padding
fn unbox(r: &str) -> &str {
    r.trim().trim_matches(|c: char| matches!(c, '│' | '┃')).trim()
}

/// a horizontal rule row (`──────`, `╌╌╌╌╌╌`): never dialog text
fn rule_row(r: &str, ch: char) -> bool {
    let t = r.trim();
    t.chars().count() >= 10 && t.chars().all(|c| c == ch)
}

/// Claude's detail block for a write dialog is file content (`1 hi`), not the command/file the dialog is about
fn file_body(r: &str) -> bool {
    r.split_once(' ').is_some_and(|(n, _)| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
}

fn join_ask(first: Option<String>, second: Option<String>) -> Option<String> {
    match (first, second) {
        (Some(a), Some(b)) => Some(format!("{a} · {b}")),
        (Some(a), None) => Some(a),
        (None, b) => b,
    }
}

/// FR-80: what a permission dialog asks for, one line (tool + command/file), read from the agent's own dialog; empty
/// when the screen doesn't name it. Recorded screens: `core/tests/dialogs/`.
fn ask_of(agent: &str, screen: &str) -> String {
    let rows: Vec<&str> = screen.lines().map(unbox).filter(|r| !r.is_empty()).collect();
    let ask = match agent {
        // omp: `╭─ Allow tool: bash ─╮` with `Command:` / `Path:` inside the box
        "omp" => {
            let tool = rows.iter().rev().find_map(|r| {
                let (_, t) = r.split_once("Allow tool:")?;
                let t = t.trim_matches(|c: char| c.is_whitespace() || matches!(c, '─' | '╭' | '╮'));
                (!t.is_empty()).then(|| t.to_string())
            });
            let what = rows.iter().rev().find_map(|r| {
                r.strip_prefix("Command: ").or_else(|| r.strip_prefix("Path: ")).map(|v| v.split(" · ").next().unwrap_or(v).to_string())
            });
            join_ask(tool, what)
        }
        // Codex: `$ <command>` inside the dialog, or `Description:` + `Destination:` for edits
        "codex" => {
            let from = rows.iter().rposition(|r| r.starts_with("Would you like to")).map_or(0, |i| i + 1);
            let dialog = &rows[from..];
            dialog.iter().find_map(|r| r.strip_prefix("$ ").map(str::to_string)).or_else(|| {
                let desc = dialog.iter().find_map(|r| r.strip_prefix("Description: ").map(str::to_string));
                let dest = dialog.iter().find_map(|r| r.strip_prefix("Destination: ").map(str::to_string));
                join_ask(desc, dest)
            })
        }
        // Claude: the block under the dialog's top rule, titled by its own first row (`Bash command`, `Create file`); the
        // detail between the two thin rules is the command/url unless it's just file content
        "claude" => rows.iter().rposition(|r| r.starts_with("Do you want")).and_then(|end| {
            let top = (0..end).rev().find(|&i| rule_row(rows[i], '─'))?;
            let block = &rows[top + 1..end];
            let useful: Vec<&str> = block.iter().copied().filter(|r| !rule_row(r, '╌') && !r.starts_with("Tip:")).collect();
            let title = *useful.first()?;
            let thin = block.iter().position(|r| rule_row(r, '╌'))?;
            let detail = block.get(thin + 1).copied().filter(|d| !file_body(d));
            join_ask(Some(title.to_string()), detail.or_else(|| useful.get(1).copied()).map(str::to_string))
        }),
        // OpenCode: `△ Permission required` + `← Access …`, or the `△ Always allow` confirmation stage
        "opencode" => rows.iter().rev().find_map(|r| r.strip_prefix('←').map(|a| a.trim().to_string())).or_else(|| {
            let i = rows.iter().position(|r| r.starts_with("△ "))?;
            let title = rows[i].trim_start_matches("△ ").to_string();
            let next = rows[i + 1..].iter().copied().find(|r| !r.starts_with('△'));
            join_ask(Some(title), next.map(str::to_string))
        }),
        _ => None,
    };
    ask.unwrap_or_default()
}

/// the dialog's own `Reason:` line (Codex, omp), empty when it has none; the last one wins (the dialog sits at the bottom)
fn why_of(screen: &str) -> String {
    screen.lines().map(unbox).filter_map(|r| r.strip_prefix("Reason: ")).next_back().unwrap_or_default().to_string()
}

/// FR-99 P2: the question above a numbered dialog's options (Claude's "Which database…"), so the answer bar can show it
/// with the options under it. Empty when nothing readable sits above the options.
fn question_of(screen: &str) -> String {
    let rows: Vec<&str> = screen.lines().collect();
    let Some(first) = rows.iter().rposition(|r| choice(r) == Some(1) && lead(r) <= 4) else { return String::new() };
    rows[..first]
        .iter()
        .rev()
        .map(|r| unbox(r))
        .find(|r| !r.is_empty() && !rule_row(r, '─') && !rule_row(r, '╌') && !r.to_lowercase().starts_with("esc"))
        .unwrap_or_default()
        .to_string()
}

/// FR-80: the permission card: title line, what the dialog asks for, its reason when it names one. The TUI's chrome
/// (stats row, box borders, key hints) never reaches the phone.
fn blocked_text(name: &str, ws: &str, status: &str, agent: &str, screen: &str, id_l: bool) -> String {
    let mut text = head(name, ws, status);
    let ask = short(&mask_secrets(&ask_of(agent, screen)), 120);
    // a question (no tool to name) shows its own words: the card used to say only "waiting for an answer" (11 Nov)
    let ask = if ask.is_empty() { short(&mask_secrets(&question_of(screen)), 200) } else { ask };
    if !ask.is_empty() {
        text += &format!("\n{}", esc(&ask));
    }
    let why = short(&mask_secrets(&why_of(screen)), 160);
    if !why.is_empty() {
        text += &format!("\n{}: {}", t(id_l, "why"), esc(&why));
    }
    text
}

/// FR-99 P2: what the permission bar above the pane shows. One reader with the bot (FR-80): `ask`/`why` from the same
/// functions, buttons from `blocked_keys` (so the bar and the Telegram keyboard carry the same words). A folder-trust
/// dialog or a dialog this build doesn't recognize has no buttons: it's answered on the Mac.
pub fn dialog_view(agent: &str, screen: &str, reason: &str, id_l: bool) -> Value {
    let trust = reason == claude::TRUST;
    let options = blocked_keys(agent, screen, reason, id_l);
    json!({
        "ask": short(&mask_secrets(&ask_of(agent, screen)), 200),
        "why": short(&mask_secrets(&why_of(screen)), 160),
        "question": short(&mask_secrets(&question_of(screen)), 200),
        "trust": trust,
        "options": options.iter().map(|(k, l)| json!({"key": k, "label": l})).collect::<Vec<_>>(),
    })
}

/// characters in front of a dialog row's text: spaces, the box edge, the cursor
fn lead(row: &str) -> usize {
    row.chars().take_while(|c| c.is_whitespace() || matches!(c, '│' | '❯' | '›')).count()
}

/// Claude / Codex numbered dialog: the option labels from the last `1.` down, wrapped rows (drawn deeper) joined. None when
/// the numbers don't run 1, 2, 3… (a list in the answer above, a dialog half drawn): nothing is offered or sent then.
/// Recorded screens: `core/tests/dialogs/` (TECH 22, agent dialogs of 10 Nov 2026).
fn dialog_opts(screen: &str) -> Option<Vec<String>> {
    let rows: Vec<&str> = screen.lines().collect();
    let first = rows.iter().rposition(|r| choice(r) == Some(1) && lead(r) <= 4)?;
    let at = lead(rows[first]);
    let mut opts: Vec<String> = Vec::new();
    for r in &rows[first..] {
        if r.trim().is_empty() {
            break;
        }
        match (lead(r), choice(r)) {
            (l, Some(n)) if l == at => {
                if n as usize != opts.len() + 1 {
                    return None;
                }
                opts.push(r.trim_start_matches(|c: char| c.is_whitespace() || matches!(c, '│' | '❯' | '›')).split_once(". ")?.1.trim().to_string());
            }
            (l, _) if l > at && !opts.is_empty() => {
                let last = opts.last_mut()?;
                *last = format!("{last} {}", r.trim().trim_end_matches('│').trim());
            }
            _ => break,
        }
    }
    (!opts.is_empty()).then_some(opts)
}

/// Claude's multiSelect question (AskUserQuestion `multiSelect: true`, 11 Nov): every option a checkbox (`[ ] Apel`,
/// `[✔] Apel`) under a tab row naming `✔ Submit`. (label, digit, ticked) per option, first row only (the description sits
/// deeper); Claude's own "Type something" is typed on the Mac, so it isn't one. Numbers must run 1, 2, 3… like
/// `dialog_opts`. Recorded screen: `core/tests/dialogs/claude-2.1.311-multiselect.txt`.
fn multi_opts(screen: &str) -> Option<Vec<(String, usize, bool)>> {
    let rows: Vec<&str> = screen.lines().collect();
    let first = rows.iter().rposition(|r| choice(r) == Some(1) && lead(r) <= 4)?;
    if !rows[..first].iter().any(|r| r.contains("✔ Submit")) {
        return None;
    }
    let at = lead(rows[first]);
    let (mut out, mut n) = (Vec::new(), 0);
    for r in &rows[first..] {
        if r.trim().is_empty() || lead(r) < at {
            break;
        }
        if lead(r) > at {
            continue;
        }
        n += 1;
        if choice(r)? as usize != n {
            return None;
        }
        let text = r.trim_start_matches(|c: char| c.is_whitespace() || matches!(c, '│' | '❯' | '›')).split_once(". ")?.1;
        let (mark, label) = text.strip_prefix('[')?.split_once(']')?;
        let label = label.trim();
        if label != "Type something" {
            out.push((label.to_string(), n, !mark.trim().is_empty()));
        }
    }
    (!out.is_empty()).then_some(out)
}

/// options that change how the agent asks from then on: Claude 2.1.311 "Yes, and switch to auto mode" (3 of 4 in a Bash
/// dialog, so a "3" button used to turn on auto mode), "switch to accept edits"; never a button, answered on the Mac
fn switches_mode(label: &str) -> bool {
    let l = label.to_lowercase();
    ["auto mode", "accept edits", "switch to", "bypass", "yolo", "full access", "dangerously"].iter().any(|w| l.contains(w))
}

/// FR-80, FR-99 P2: one button per option read on screen, labelled with what it does, except mode switches; plus Deny.
/// Nothing read = no button at all (rule 9 BRD 11: an unknown dialog is answered on the Mac, never by guessing a number).
fn claude_keys(agent: &str, screen: &str, id_l: bool) -> Vec<(&'static str, String)> {
    // FR-99 P2: a numbered dialog that doesn't read cleanly gets no button at all (not even Deny): guessing a number or
    // sending Esc could answer something else. The bar offers "Buka buat jawab" and the bot drops its keyboard.
    let Some(opts) = dialog_opts(screen) else { return Vec::new() };
    // a multiSelect digit only ticks a box and nothing submits it: the bot sends a poll instead (`notice`), Deny stays
    if multi_opts(screen).is_some() {
        return vec![("esc", button_label(agent, "esc", id_l))];
    }
    let mut v: Vec<(&'static str, String)> = opts
        .iter()
        .enumerate()
        .filter(|(i, _)| *i < DIGITS.len())
        .filter_map(|(i, l)| option_label(i, l, id_l).map(|label| (DIGITS[i], label)))
        .collect();
    v.push(("esc", button_label(agent, "esc", id_l)));
    v
}

/// pressing digit `n` now: still a numbered option on the current screen, and not a mode switch
fn digit_ok(screen: &str, n: usize) -> bool {
    dialog_opts(screen).and_then(|o| o.get(n.wrapping_sub(1)).cloned()).is_some_and(|l| !switches_mode(&l))
}

fn short(s: &str, n: usize) -> String {
    if s.chars().count() <= n { s.to_string() } else { format!("{}…", s.chars().take(n - 1).collect::<String>()) }
}

/// Codex option `n` (may wrap over several rows) -> the shortcut it shows last in parentheses: `(y)` -> y, `(esc)` -> Esc;
/// none = nothing is sent, also when the options from the last `1.` down aren't numbered 1, 2, 3… (misread dialog)
fn codex_key(screen: &str, n: u8) -> Option<Vec<u8>> {
    let opt = dialog_opts(screen)?.get((n as usize).checked_sub(1)?)?.clone();
    if switches_mode(&opt) {
        return None;
    }
    let s = opt.strip_suffix(')')?;
    match &s[s.rfind('(')? + 1..] {
        "esc" => Some(b"\x1b".to_vec()),
        k if k.len() == 1 && k.bytes().all(|c| c.is_ascii_lowercase()) => Some(k.as_bytes().to_vec()),
        _ => None,
    }
}

fn key_bytes(key: &str) -> &'static [u8] {
    match key {
        "1" => b"1",
        "2" => b"2",
        "3" => b"3",
        "4" => b"4",
        "5" => b"5",
        "6" => b"6",
        "7" => b"7",
        "8" => b"8",
        "9" => b"9",
        "once" => b"\r",
        // "always" (OpenCode) needs the confirmation gate and is written by the
        // In::Key arm, not here; falling through to Esc would be a lie, so it
        // has no byte mapping on purpose (the arm handles it before this).
        _ => b"\x1b",
    }
}

/// FR-80, FR-99 P2: one button press on the agent's own dialog. The single path for the bot and the permission bar (the
/// bar never has its own parser or bytes): OpenCode "Always" walks the "△ Always allow" confirmation stage and only
/// confirms it once that title is on screen; Codex and Claude re-read the dialog and refuse a number that is gone or that
/// switches the permission mode. `false` = nothing was sent.
pub fn send_answer(st: &State, id: u64, p: &Pane, key: &str, seq: u64) -> bool {
    match key {
        // OpenCode "Always": Right+Enter moves once→always and submits it, which opens the "△ Always allow" confirmation
        // stage (no reply is sent yet). The confirming Enter goes out only after that title is actually observed on screen;
        // a dialog that resolved meanwhile, a closed pane, or a timeout sends nothing (retryable: nothing was approved).
        "always" => {
            if !known_dialog("opencode", &p.view.lock().unwrap().0.text()) {
                return false;
            }
            if type_into_blocked(st, id, p, seq, b"\x1b[C\r").is_err() {
                return false;
            }
            let mut confirmed = false;
            let mut resolved = false;
            for _ in 0..20 {
                thread::sleep(Duration::from_millis(100));
                match st.panes.lock().unwrap().get(&id).cloned() {
                    None => break,
                    Some(q) => {
                        if *q.status.lock().unwrap() != "blocked" {
                            resolved = true;
                            break;
                        }
                        if always_confirm_on_screen(&q) {
                            confirmed = true;
                            break;
                        }
                    }
                }
            }
            if resolved {
                true
            } else if confirmed {
                match st.panes.lock().unwrap().get(&id).cloned() {
                    Some(q) => type_into_blocked(st, id, &q, seq, b"\r").is_ok(),
                    _ => false,
                }
            } else {
                false
            }
        }
        k if k != "esc" && p.agent_name().as_deref() == Some("codex") => {
            let screen = p.view.lock().unwrap().0.text();
            k.parse().ok().and_then(|n| codex_key(&screen, n)).is_some_and(|bytes| type_into_blocked(st, id, p, seq, &bytes).is_ok())
        }
        // Claude digit: re-read the dialog now; the option must still be there and not switch the permission mode
        k if k != "esc" && p.agent_name().as_deref() == Some("claude") => {
            let screen = p.view.lock().unwrap().0.text();
            k.parse().is_ok_and(|n| digit_ok(&screen, n)) && type_into_blocked(st, id, p, seq, key_bytes(k)).is_ok()
        }
        // every other key needs a dialog this build recognizes; an unknown agent has no key at all (keys() is empty)
        k => {
            let agent = p.agent_name().unwrap_or_default();
            let screen = p.view.lock().unwrap().0.text();
            keys(&agent).contains(&k) && known_dialog(&agent, &screen) && type_into_blocked(st, id, p, seq, key_bytes(k)).is_ok()
        }
    }
}

/// a multiSelect poll vote (11 Nov, recorded live on Claude 2.1.311): digits tick exactly what was picked, → opens the
/// review page, and its "1. Submit answers" sends it. Every step re-reads the screen and stops, with nothing more typed,
/// when the dialog changed (other options, another generation, review not reached). Another question tab after → goes
/// out as a fresh card.
fn vote(st: &State, b: &Bot, poll: &Poll, picks: &[usize]) -> bool {
    let r = vote_steps(st, b, poll, picks);
    if let Err(why) = r {
        log(format_args!("vote stopped: {why}"));
    }
    r.is_ok()
}

fn vote_steps(st: &State, b: &Bot, poll: &Poll, picks: &[usize]) -> Result<(), &'static str> {
    let id = poll.pane;
    let Some(p) = st.panes.lock().unwrap().get(&id).cloned() else { return Err("pane closed") };
    let screen = || p.view.lock().unwrap().0.text();
    let same = |s: &str| multi_opts(s).filter(|o| o.iter().map(|(l, ..)| short(&mask_secrets(l), 100)).eq(poll.labels.iter().cloned()));
    let wait = |ok: &dyn Fn(&str) -> bool| {
        (0..20).any(|_| {
            thread::sleep(Duration::from_millis(100));
            *p.status.lock().unwrap() == "blocked" && p.status_seq.load(Ordering::Relaxed) == poll.seq && ok(&screen())
        })
    };
    let Some(opts) = same(&screen()) else { return Err("options changed") };
    // one digit at a time, each waited on: Claude drops digits that arrive together ("23" ticked nothing, 11 Nov)
    for (i, (_, n, on)) in opts.iter().enumerate() {
        let want = picks.contains(&i);
        if want != *on {
            type_into_blocked(st, id, &p, poll.seq, key_bytes(DIGITS[n - 1]))?;
            if !wait(&|s: &str| same(s).is_some_and(|o| o[i].2 == want)) {
                return Err("box not ticked");
            }
        }
    }
    let ticked = |s: &str| same(s).is_some_and(|o| o.iter().enumerate().all(|(i, (.., on))| picks.contains(&i) == *on));
    type_into_blocked(st, id, &p, poll.seq, b"\x1b[C")?;
    let review = |s: &str| s.contains("Ready to submit your answers?") && dialog_opts(s).is_some_and(|o| o.first().is_some_and(|f| f == "Submit answers"));
    // → moved on: the review page, or the next tab (the same question with its boxes no longer shown)
    if !wait(&|s: &str| !ticked(s)) {
        return Err("→ did nothing");
    }
    if wait(&review) {
        return type_into_blocked(st, id, &p, poll.seq, b"1");
    }
    // the next question tab: its own card (poll or buttons)
    let _ = b.out.try_send(Out::Notice(id, Kind::Blocked(poll.seq), Instant::now()));
    Ok(())
}

fn cb_data(pane: u64, seq: u64, key: &str) -> String {
    format!("k|{pane}|{seq}|{key}")
}

/// Telegram's own limit is 8 buttons per row; the mockup keeps 4 so the labels stay readable on a phone (user decision
/// 11 Nov 2026). A longer dialog wraps onto another row instead of being squeezed: every option used to sit in one row,
/// which cut the last ones off on screen (the complaint that some choices were missing).
const KEYS_PER_ROW: usize = 4;

/// one dialog's buttons as `inline_keyboard` rows (`[]` = no keyboard at all, which is also the "answered" state)
fn key_rows(pane: u64, seq: u64, ks: &[(&'static str, String)]) -> Vec<Vec<Value>> {
    ks.chunks(KEYS_PER_ROW)
        .map(|row| row.iter().map(|(k, label)| json!({"text": label, "callback_data": cb_data(pane, seq, k)})).collect())
        .collect()
}

fn cb_parse(d: &str) -> Option<(u64, u64, String)> {
    let mut it = d.split('|');
    if it.next()? != "k" {
        return None;
    }
    let (pane, seq, key) = (it.next()?.parse().ok()?, it.next()?.parse().ok()?, it.next()?);
    (it.next().is_none() && (DIGITS.contains(&key) || ["once", "always", "esc"].contains(&key))).then(|| (pane, seq, key.to_string()))
}

/// Update -> what to do. Only the bound private chat counts (SEC-36); while pairing only `/start <code>`.
fn parse(u: &Value, link: Option<&Link>, now: i64) -> In {
    if let Some(a) = u.get("poll_answer") {
        let Some(l) = link else { return In::Ignore };
        if a["user"]["id"].as_i64() != Some(l.user) {
            return In::Ignore;
        }
        let picks = a["option_ids"].as_array().into_iter().flatten().filter_map(|o| o.as_u64().map(|o| o as usize)).collect();
        return In::Vote { poll: a["poll_id"].as_str().unwrap_or_default().to_string(), picks };
    }
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
        if let Some((pane, agent)) = data.strip_prefix("o|").and_then(|r| r.split_once('|')).and_then(|(p, a)| Some((p.parse().ok()?, a))) {
            return In::OpenPick { pane, agent: agent.to_string(), cb, msg };
        }
        if let Some((ws, agent)) = data.strip_prefix("w|").and_then(|r| r.split_once('|')) {
            return In::OpenWs { ws: ws.to_string(), agent: agent.to_string(), cb, msg };
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
    let name = first.split(" · ").next().filter(|n| *n != first).unwrap_or_default().trim_start_matches("⏳").trim().to_string();
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
        // 9 Nov: a photo captioned `/to shell-25 …` went to the active pane and Claude read `/to` as its own command
        let prefix = ["/kirim ", "/send ", "/ke ", "/to "].iter().find_map(|p| caption.strip_prefix(p));
        let (to, caption) = match prefix {
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
        // `/to <pane> <text>` = send there (the pane becomes the active one); `/to <pane>` = switch; `/to` = buttons
        "/ke" | "/to" => match rest.split_once(char::is_whitespace) {
            Some((name, body)) => In::Type { to: Some(To::Name(name.to_string())), text: body.trim().to_string(), msg },
            None if rest.is_empty() => In::Choose,
            None => In::Use(rest.to_string()),
        },
        "/buka" | "/open" if !rest.is_empty() => {
            let mut w = rest.split_whitespace();
            In::Open { agent: w.next().unwrap_or_default().to_string(), pane: w.next().map(String::from) }
        }
        "/kirim" | "/send" => match rest.split_once(char::is_whitespace) {
            Some((name, body)) if !body.trim().is_empty() => In::Type { to: Some(To::Name(name.to_string())), text: body.trim().to_string(), msg },
            _ => In::Help,
        },
        "/start" | "/help" | "/bantuan" | "/buka" | "/open" | "/lihat" | "/look" => In::Help,
        // the agent's own commands (`/exit`, `/compact`, `/model`) go to the pane like typed text (9 Nov: `/exit` got the help)
        _ => In::Type { to: reply, text: text.to_string(), msg },
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
        s if s < 3600 && s % 60 == 0 => format!("{}m", s / 60),
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

/// FR-87: kind word of a blocked pane. Only "waiting for an answer" (FR-84) is its own kind; every other ask is
/// "needs permission", followed by the agent's own reason when it says more (masked, it may hold a command)
fn blocked_word(reason: &str, id_l: bool) -> String {
    let (permission, answer) = (t(id_l, "blocked"), t(id_l, "answer"));
    let plain = |w: &str| ["minta izin", "needs permission", "nunggu jawaban", "waiting for an answer"].contains(&w);
    let word = if reason == "nunggu jawaban" || reason == answer { answer } else { permission };
    if reason.is_empty() || plain(reason) { word.to_string() } else { format!("{word} · {}", mask_secrets(reason)) }
}

fn t(id: bool, k: &str) -> &'static str {
    let (i, e) = match k {
        // FR-87 (TECH 27.1): the same words as the macOS banner, the bell and the "Butuh lu" strip
        "blocked" => ("butuh izin", "needs permission"),
        "answer" => ("nunggu jawaban", "waiting for an answer"),
        "why" => ("Alasan", "Reason"),
        "done" => ("selesai", "done"),
        "working" => ("lagi kerja", "working"),
        "idle" => ("nunggu perintah", "idle"),
        "password" => ("minta password", "asks for a password"),
        "password_hint" => ("Jawab di Mac; password nggak dikirim lewat HP.", "Answer it on the Mac; passwords never go through the phone."),
        "long" => ("selesai", "done"),
        "shell_st" => ("shell", "shell"),
        "linked" => ("Nyambung ke Gembala di", "Connected to Gembala on"),
        "gone" => ("udah lewat", "already answered"),
        "sent" => ("terkirim", "sent"),
        "answered" => ("dijawab", "answered"),
        "no_pane" => ("Pane itu nggak ada.", "No such pane."),
        "exited" => ("agent-nya udah keluar, pane balik jadi shell. Nyalain lagi: /buka <agent>", "the agent exited, the pane is a shell now. Start one again: /open <agent>"),
        "shell" => (
            "Pane shell nggak nerima ketikan dari HP. Nyalain agent di situ: /buka claude <pane>.",
            "Shell panes don't take typing from the phone. Start an agent there: /open claude <pane>.",
        ),
        "open_which" => ("Yang bisa dibuka:", "You can start:"),
        "open_pick" => ("Pane mana buat", "Which pane for"),
        "open_ws" => ("Workspace mana buat", "Which workspace for"),
        "opening" => ("Nyalain", "Starting"),
        "opened_hint" => ("Ketik aja prompt-nya begitu siap.", "Type your prompt once it's up."),
        "already_agent" => ("Pane itu udah jalanin agent, ketik aja.", "That pane already runs an agent, just type."),
        "busy_shell" => ("Pane itu lagi jalanin sesuatu. /buka cuma ke shell yang diam di prompt.", "That pane is busy. /open only uses a shell idle at its prompt."),
        "no_idle" => ("Nggak ada pane shell yang lagi kosong.", "No shell pane is idle."),
        "pw" => ("Lagi nunggu password, jawab di Mac.", "It's waiting for a password, answer on the Mac."),
        "too_long" => ("Kepanjangan, maks 4.000 karakter.", "Too long, 4,000 characters max."),
        "bad" => ("Teksnya kosong setelah dibersihin.", "Nothing left after cleaning the text."),
        "old_reply" => ("Pane pesan itu udah nggak ada. Ketik biasa buat ke pane aktif, atau /ke <pane>.", "That message's pane is gone. Type plainly for the active pane, or /to <pane>."),
        "snip_off" => ("Potongan layar dimatiin buat workspace ini.", "Screen snippets are off for this workspace."),
        "unread" => (
            "jawaban belum kebaca — layarnya lagi nampilin pilihan/dialog. Lihat: /lihat {pane}",
            "the answer wasn't read — the screen is showing a picker or dialog. Look: /look {pane}",
        ),
        "pick" => ("Ke pane mana?", "Which pane?"),
        "p_now" => ("lagi ngerjain", "is working"),
        "p_ran" => ("udah jalan", "running"),
        "p_eta" => ("biasanya kelar", "usually done in"),
        "p_late" => ("lebih lama dari biasanya", "longer than usual"),
        "p_none" => ("belum ada perkiraan, ini giliran pertamanya", "no estimate yet, first turn"),
        "p_queued" => ("Pesan lu antre di belakang giliran yang lagi jalan.", "Your message is queued behind the turn in progress."),
        "no_agent" => ("Belum ada pane agent.", "No agent pane yet."),
        "now_to" => ("Sekarang ngetik ke", "Now typing into"),
        "dl_fail" => ("Gagal ngunduh filenya (maks 20 MB).", "Couldn't download the file (20 MB max)."),
        "none" => ("Belum ada pane.", "No panes yet."),
        _ => (
            "Ketik biasa = masuk ke pane aktif (yang terakhir dipakai atau terakhir ngabarin).\nBales pesan bot = ke pane pesan itu.\nFoto/file = path-nya diketik ke pane aktif (caption /ke <pane> buat pane lain).\n/ke <pane> [teks] — ganti pane aktif (sekalian kirim teksnya); /ke doang = pilih dari tombol\nPerintah / yang bukan milik bot (/exit, /compact, /clear, /model) diteruskan ke pane aktif\n/buka <agent> [pane] — nyalain claude/codex/omp/opencode di shell yang kosong\n/status — semua pane\n/lihat <pane> — layar terbaru",
            "Plain text = goes to the active pane (last used or last to send a notice).\nReply to a bot message = that message's pane.\nPhoto/file = its path is typed into the active pane (caption /to <pane> for another).\n/to <pane> [text] — switch the active pane (and send the text); /to alone = pick from buttons\nA / command that isn't the bot's (/exit, /compact, /clear, /model) goes to the active pane\n/open <agent> [pane] — start claude/codex/omp/opencode in an idle shell\n/status — every pane\n/look <pane> — latest screen",
        ),
    };
    if id { i } else { e }
}

/// curl with its options (URL with the token included) on stdin, so nothing secret shows in argv
fn curl(cfg: &str, secs: u64) -> io::Result<std::process::Output> {
    let mut c = Command::new(crate::os::curl())
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
        // Telegram's own reason (e.g. "message is not modified"); never the token, it's only in the URL
        log(format_args!("{method} failed {}: {}", v["error_code"], v["description"].as_str().unwrap_or_default()));
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
        started: Mutex::default(),
        turns: Mutex::default(),
        progress: Mutex::default(),
        picked: Mutex::new(None),
        polls: Mutex::default(),
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
        let Ok(code) = new_code() else { return fail("tg_io") };
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

/// the chat's command menu; sent at pairing and on every start of a paired bot, so commands added or changed in a newer
/// version reach a chat paired before (10 Nov: the menu still showed the 8 Nov list)
fn menu(b: &Bot) {
    let list: [(&str, &str); 4] = if b.id_lang {
        [("status", "semua pane"), ("ke", "[pane] [teks] — pilih pane aktif, atau kirim teks ke pane itu"), ("buka", "<agent> [pane] — nyalain agent di shell"), ("lihat", "<pane> — layar terbaru")]
    } else {
        [("status", "every pane"), ("to", "[pane] [text] — pick the active pane, or send text to it"), ("open", "<agent> [pane] — start an agent in a shell"), ("look", "<pane> — latest screen")]
    };
    let cmds: Vec<Value> = list.iter().map(|(c, d)| json!({"command": c, "description": d})).collect();
    let _ = b.out.try_send(Out::Api("setMyCommands", json!({"commands": cmds})));
}

fn poll(st: Arc<State>, b: Arc<Bot>) {
    let _ = api(&b.token, "deleteWebhook", &json!({}), 15);
    if b.linked().is_some() {
        menu(&b);
    }
    if b.bot.lock().unwrap().is_empty()
        && let Ok(me) = api(&b.token, "getMe", &json!({}), 15)
    {
        *b.bot.lock().unwrap() = me["username"].as_str().unwrap_or_default().to_string();
        changed(&st);
    }
    let (mut offset, mut wait, mut conflicts) = (0i64, 2u64, 0);
    while !b.stop.load(Ordering::Relaxed) {
        let body = json!({"offset": offset, "timeout": 50, "allowed_updates": ["message", "callback_query", "poll_answer"]});
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
        let (mut progress, mut seq) = (None, 0);
        let (method, mut body, pane) = match o {
            Out::Api(m, body) => (m, body, None),
            Out::Progress(id, body) => {
                progress = Some(id);
                ("sendMessage", body, None)
            }
            Out::Notice(id, kind, at) => {
                thread::sleep(SETTLE.saturating_sub(at.elapsed()));
                match notice(&st, &b, id, kind) {
                    Some((method, body)) => {
                        if debug() {
                            log(format_args!("out {}", if method == "sendPoll" { &body["question"] } else { &body["text"] }));
                        }
                        if let Kind::Blocked(s) = kind {
                            seq = s;
                        }
                        (method, body, Some(id))
                    }
                    None => continue,
                }
            }
        };
        for _ in 0..3 {
            match api(&b.token, method, &body, 15) {
                Ok(r) => {
                    if let (Some(p), Some(mid)) = (progress, r["message_id"].as_i64()) {
                        // pane finished before this was sent: the progress message has nothing left to say
                        // a reply to "⏳ …" goes to that pane, like a reply to any of its messages
                        b.remember(mid, p);
                        match b.progress.lock().unwrap().get_mut(&p) {
                            Some(slot) => slot.0 = Some(mid),
                            None => {
                                let _ = b.out.try_send(Out::Api("deleteMessage", json!({"chat_id": b.chat(), "message_id": mid})));
                            }
                        }
                    }
                    if let (Some(p), Some(mid), Some(poll)) = (pane, r["message_id"].as_i64(), r["poll"]["id"].as_str()) {
                        let labels = body["options"].as_array().into_iter().flatten().filter_map(|o| o["text"].as_str().map(String::from)).collect();
                        let mut polls = b.polls.lock().unwrap();
                        // ponytail: unanswered polls pile up; past MAP_MAX they're all forgotten (a late vote says "gone")
                        if polls.len() > MAP_MAX {
                            polls.clear();
                        }
                        polls.insert(poll.to_string(), Poll { pane: p, seq, labels, msg: mid });
                    }
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
                // HTML Telegram can't parse (an agent answer converted from markdown): the same text without tags, not lost
                Err((400, _)) if body["parse_mode"] == "HTML" => body = plain(body),
                Err(_) => break,
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
            tick_progress(&st, &b);
        }
        thread::sleep(Duration::from_secs(10));
    }
}

/// every `PROGRESS_EVERY`: running time and estimate in each "⏳" message; a closed pane's message is deleted
fn tick_progress(st: &State, b: &Bot) {
    let due: Vec<(u64, i64)> =
        b.progress.lock().unwrap().iter().filter(|(_, (m, at))| m.is_some() && at.elapsed() >= PROGRESS_EVERY).map(|(id, (m, _))| (*id, m.unwrap_or_default())).collect();
    for (id, m) in due {
        let pane = st.panes.lock().unwrap().get(&id).cloned();
        let Some(pane) = pane else {
            b.progress.lock().unwrap().remove(&id);
            let _ = b.out.try_send(Out::Api("deleteMessage", json!({"chat_id": b.chat(), "message_id": m})));
            continue;
        };
        if let Some(slot) = b.progress.lock().unwrap().get_mut(&id) {
            slot.1 = Instant::now();
        }
        let text = progress_of(b, id, &pane, false);
        let _ = b.out.try_send(Out::Api("editMessageText", json!({"chat_id": b.chat(), "message_id": m, "text": text, "parse_mode": "HTML"})));
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
    if status == "done" && pane.claude.lock().unwrap().shell {
        return;
    }
    let Some(b) = live(st) else { return };
    let Some(agent) = pane.agent_name() else { return };
    // turn timing for the estimate: working = start, done/idle = end
    let took = match status {
        "working" => {
            b.started.lock().unwrap().entry(id).or_insert_with(Instant::now);
            0
        }
        "done" | "idle" => b.started.lock().unwrap().remove(&id).map_or(0, |t0| {
            let secs = t0.elapsed().as_secs();
            let mut turns = b.turns.lock().unwrap();
            let q = turns.entry(agent.clone()).or_default();
            q.push_back(secs);
            if q.len() > TURNS_KEPT {
                q.pop_front();
            }
            secs
        }),
        _ => 0,
    };
    let asked = !pane.tg_ask.lock().unwrap().is_empty();
    if !b.away.load(Ordering::Relaxed) && !asked {
        return;
    }
    let seq = pane.status_seq.load(Ordering::Relaxed);
    let on = |k: &str| asked || values(st)["notify"][k].as_bool().unwrap_or(true);
    let kind = match status {
        "blocked" if on("on_blocked") => Kind::Blocked(seq),
        "done" if on("on_done") => Kind::Done(took),
        _ => return,
    };
    if let Kind::Done(_) = kind {
        // every message typed while the agent worked (a photo, then a follow-up) gets its 👍, not only the last
        for m in pane.tg_ask.lock().unwrap().drain(..) {
            b.react(m, "👍");
        }
        // the result says how long it took; the "⏳ working" message goes away
        if let Some((Some(m), _)) = b.progress.lock().unwrap().remove(&id) {
            let _ = b.out.try_send(Out::Api("deleteMessage", json!({"chat_id": b.chat(), "message_id": m})));
        }
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
    // a box with a dialog cursor (`❯ 1.`, `› 1.`) in or below it is a permission dialog (10 Nov: Claude's Edit diff box
    // was cut as the input box, the phone got only the turn above it), not an input box; a numbered list typed in omp's
    // box has no cursor and stays the input box
    let cursor = |r: &str| matches!(r.trim_start_matches(|c: char| c.is_whitespace() || c == '│').chars().next(), Some('❯' | '›')) && choice(r).is_some();
    let boxed = || (0..rows.len()).rev().find(|&i| starts(rows[i], '╭') && rows[i + 1..].iter().any(|r| starts(r, '╰')) && !rows[i..].iter().any(|r| cursor(r)));
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
    // `❯ 1. Yes` is the cursor of a numbered dialog, not a prompt: taking it as the turn start dropped the question
    // (9 Nov: phone got "❯ 1. Yes / 2. No" with no command)
    if let Some(i) = rows.iter().rposition(|r| r.trim_start().starts_with('❯') && choice(r).is_none()) {
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
    let agent = pane.agent_name();
    if agent.is_none() {
        return snippet(&text, lines);
    }
    let rows: Vec<&str> = text.lines().collect();
    let end = if agent.as_deref() == Some("codex") {
        codex_above_input(&rows)
    } else {
        above_input(&rows)
    };
    let rows = &rows[..end];
    let typed = pane.tg_text.lock().unwrap().clone();
    let rows = &rows[turn_start(rows, typed.as_deref())..];
    let rows = rows.iter().copied().filter(|r| !codex_chrome_row(r, agent.as_deref())).collect::<Vec<_>>();
    snippet(&rows.join("\n"), lines)
}

fn codex_above_input(rows: &[&str]) -> usize {
    rows.iter().rposition(|r| r.trim_start().starts_with("› Ask Codex to do anything")).unwrap_or(rows.len())
}

fn codex_chrome_row(row: &str, agent: Option<&str>) -> bool {
    agent == Some("codex") && {
        let row = row.trim();
        row.starts_with("Worked for ") || row.starts_with("⚠ Heads up, you have less than 25% of your 5h limit")
            || row.starts_with("⚠ 5h limit:") || (row.contains("Context ") && row.contains(" window"))
            || row.contains("← for agents · ? for shortcuts") || row.contains("warnings · f2 to view") || row == "+ Show details"
    }
}

/// `screen_watch`, once a second per pane: password prompt (rising edge) and long shell commands that finished
pub fn tick(st: &State, id: u64, pane: &Pane) {
    let long = pane.long_done.lock().unwrap().take();
    let Some(b) = live(st) else {
        pane.asked_pw.store(false, Ordering::Relaxed);
        return;
    };
    // a message from the phone went to an agent that has exited since (`/exit`, a crash): no "done" will ever come
    // (agent_done clears the status without a status event), so say so instead of leaving "⏳ working" up (10 Nov)
    let gone = pane.agent_name().is_none() || pane.status.lock().unwrap().is_empty();
    if gone && !pane.tg_ask.lock().unwrap().is_empty() {
        for m in pane.tg_ask.lock().unwrap().drain(..) {
            b.react(m, "👍");
        }
        b.started.lock().unwrap().remove(&id);
        if let Some((Some(m), _)) = b.progress.lock().unwrap().remove(&id) {
            let _ = b.out.try_send(Out::Api("deleteMessage", json!({"chat_id": b.chat(), "message_id": m})));
        }
        b.tell(&format!("{}: {}", pane.name.lock().unwrap(), t(b.id_lang, "exited")));
    }
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

/// `snippet_off` holds workspace names (Settings writes names); the id is checked too for hand-edited config
fn snippet_on(v: &Value, ws: &[&str]) -> bool {
    v["telegram"]["snippet"].as_bool().unwrap_or(true) && !v["telegram"]["snippet_off"].as_array().into_iter().flatten().any(|w| w.as_str().is_some_and(|w| ws.contains(&w)))
}

/// workspace id → name from the layout the UI saved (`pane.workspace` holds the id; real app 9 Nov: /status showed `ap2mf4uo`)
// ponytail: layout.json read per message; cache it if /status gets slow
fn ws_names() -> HashMap<String, String> {
    let v: Value = fs::read_to_string(config::layout_path()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    v["workspaces"].as_array().into_iter().flatten().filter_map(|w| Some((w["id"].as_str()?.to_string(), w["name"].as_str()?.to_string()))).collect()
}

/// pane id → title of the tab holding it, as the sidebar shows it ("Isi DWM"); tabs without a title are left out
fn tab_titles() -> HashMap<u64, String> {
    fn walk(n: &Value, title: &str, out: &mut HashMap<u64, String>) {
        match n["pane"].as_u64() {
            Some(id) => drop(out.insert(id, title.to_string())),
            None => ["a", "b"].iter().for_each(|k| walk(&n[k], title, out)),
        }
    }
    let v: Value = fs::read_to_string(config::layout_path()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    let mut out = HashMap::new();
    for t in v["workspaces"].as_array().into_iter().flatten().flat_map(|w| w["tabs"].as_array().into_iter().flatten()) {
        if let Some(title) = t["title"].as_str().map(str::trim).filter(|s| !s.is_empty()) {
            walk(&t["tree"], &title.chars().take(40).collect::<String>(), &mut out);
        }
    }
    out
}

fn ws_name(id: &str) -> String {
    ws_names().remove(id).unwrap_or_else(|| id.to_string())
}

/// first line only name · workspace + status (what the lock screen shows, SEC-37)
fn head(name: &str, ws: &str, status: &str) -> String {
    if ws.is_empty() { format!("<b>{}</b> {}", esc(name), esc(status)) } else { format!("<b>{} · {}</b> {}", esc(name), esc(ws), esc(status)) }
}

/// message body built when it is sent (after SETTLE), from the pane as it is now; `None` = no longer relevant
fn notice(st: &State, b: &Bot, id: u64, kind: Kind) -> Option<(&'static str, Value)> {
    let pane = st.panes.lock().unwrap().get(&id).cloned()?;
    if matches!(kind, Kind::Done(_)) && (pane.claude.lock().unwrap().shell || *pane.status.lock().unwrap() != "done") {
        return None;
    }
    let (name, ws_id) = (pane.name.lock().unwrap().clone(), pane.workspace.lock().unwrap().clone());
    let ws = ws_name(&ws_id);
    let agent = pane.agent_name().unwrap_or_default();
    let v = values(st);
    let id_l = b.id_lang;
    let status = match kind {
        Kind::Blocked(seq) => {
            if *pane.status.lock().unwrap() != "blocked" || pane.status_seq.load(Ordering::Relaxed) != seq {
                return None;
            }
            let reason = pane.claude.lock().unwrap().reason.clone();
            blocked_word(&reason, id_l)
        }
        Kind::Done(0) => t(id_l, "done").to_string(),
        Kind::Done(secs) => format!("{} ({})", t(id_l, "done"), dur(secs)),
        Kind::Password => t(id_l, "password").to_string(),
        Kind::Long(secs) => format!("{} ({})", t(id_l, "long"), dur(secs)),
    };
    let mut text = head(&name, &ws, &status);
    let blocked = matches!(kind, Kind::Blocked(_));
    let screen = if blocked { pane.view.lock().unwrap().0.text() } else { String::new() };
    if kind == Kind::Password {
        text += &format!("\n{}", esc(t(id_l, "password_hint")));
    } else if blocked {
        // FR-80: what the dialog asks for and why, one line each; the TUI's chrome stays on the Mac (11 Nov the card was
        // a screen dump: stats row, box borders, key hints, and the buttons sat below the fold). SEC-33: a workspace with
        // screen snippets off gets the buttons only.
        let shown = if snippet_on(&v, &[&ws, &ws_id]) { &screen } else { "" };
        text = blocked_text(&name, &ws, &status, &agent, shown, id_l);
    } else if snippet_on(&v, &[&ws, &ws_id]) {
        if let Some(l) = label(&pane.title.lock().unwrap(), &agent) {
            text += &format!("\n{}", esc(&l));
        }
        let done = matches!(kind, Kind::Done(_));
        // The done status can come a moment before the hook that carries the turn's answer, so the card waits for it;
        // a turn that started meanwhile drops it. Only agents whose answer comes from their own hook can be waited on.
        if done && answers_from_hook(&agent) && pane.last_msg.lock().unwrap().is_none() {
            let end = Instant::now() + ANSWER_WAIT;
            while Instant::now() < end && *pane.status.lock().unwrap() == "done" && pane.last_msg.lock().unwrap().is_none() {
                thread::sleep(Duration::from_millis(100));
            }
            if *pane.status.lock().unwrap() != "done" {
                return None;
            }
        }
        let answer = done.then(|| pane.last_msg.lock().unwrap().take()).flatten();
        let lines = v["telegram"]["lines"].as_u64().unwrap_or(30).min(30) as usize;
        let snip = screen_snip(&pane, lines);
        // A picker or dialog is not an answer (9 Nov: the done card held Claude's `/model` list). Say the answer wasn't
        // read instead of sending the dialog's numbered options.
        text += &if done && answer.is_none() && picker_screen(&snip) {
            format!("\n{}", esc(&t(id_l, "unread").replace("{pane}", &name)))
        } else {
            turn_body(done, &agent, answer.as_deref(), || snip)
        };
    }
    let mut body = json!({"chat_id": b.chat(), "text": text, "parse_mode": "HTML"});
    if let Kind::Blocked(seq) = kind {
        let reason = pane.claude.lock().unwrap().reason.clone();
        let ks = blocked_keys(&agent, &screen, &reason, id_l);
        let rows = key_rows(id, seq, &ks);
        // multiSelect (11 Nov "pake vote"): the card is a poll with several answers, its Vote is the Submit (`vote`)
        if let Some(opts) = multi_opts(&screen).filter(|o| agent == "claude" && (2..=10).contains(&o.len())) {
            let q = if snippet_on(&v, &[&ws, &ws_id]) { short(&mask_secrets(&question_of(&screen)), 200) } else { String::new() };
            let first = if ws.is_empty() { format!("{name} {status}") } else { format!("{name} · {ws} {status}") };
            let options: Vec<Value> = opts.iter().map(|(l, ..)| json!({"text": short(&mask_secrets(l), 100)})).collect();
            let poll = json!({"chat_id": b.chat(), "question": short(format!("{first}\n{q}").trim(), 300), "options": options,
                              "is_anonymous": false, "allows_multiple_answers": true, "reply_markup": {"inline_keyboard": rows}});
            return Some(("sendPoll", poll));
        }
        if !rows.is_empty() {
            body["reply_markup"] = json!({"inline_keyboard": rows});
        }
    }
    Some(("sendMessage", body))
}

/// `last` only stands while an agent still runs in that pane. A Claude typed into a shell pane can exit, and `last` stayed
/// on it, so a reply to a card an agent posted through the bot itself (no pane name on the first line) was typed into that
/// shell and answered "shell panes don't take typing" (11 Nov). A shell pane is no pick at all: the single-agent / `pick` /
/// `no_agent` rules decide instead. `panes`: (id, an agent runs there).
fn last_agent(last: Option<u64>, panes: &[(u64, bool)]) -> Option<u64> {
    last.filter(|id| panes.iter().any(|(p, agent)| p == id && *agent))
}

/// pane for a phone message: reply to a bot message, a name, or the active pane; no active pane and a single agent pane = that
/// one; several = `pick` (the caller offers buttons)
fn target(st: &State, b: &Bot, to: Option<To>) -> Result<(u64, Arc<Pane>), &'static str> {
    let id = match to {
        Some(To::Name(n)) => return st.by_name(&n).ok_or("no_pane"),
        Some(To::Reply(r, name)) => {
            let known = b.sent.lock().unwrap().iter().find(|(m, _)| *m == r).map(|(_, p)| *p);
            match known {
                Some(id) => id,
                // not in RAM (daemon restarted): the pane named on the message's first line
                None if !name.is_empty() => return st.by_name(&name).ok_or("old_reply"),
                // reply to a message that isn't a pane's (the user's own photo, help, picker): like plain text
                None => return target(st, b, None),
            }
        }
        None => {
            let panes = st.panes.lock().unwrap();
            let listed: Vec<(u64, bool)> = panes.iter().map(|(id, p)| (*id, p.agent_name().is_some())).collect();
            match last_agent(*b.last.lock().unwrap(), &listed) {
                Some(id) => id,
                None => {
                    let agents: Vec<u64> = listed.iter().filter(|(_, agent)| *agent).map(|(id, _)| *id).collect();
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

/// What `/buka` may type: the four agents plus the Claude account commands Gembala writes to accounts.zsh (FR-61,
/// `claude-utama` + `claude-<id>`). The command is typed as is, never text from the phone (SEC-36).
fn openable(claude_accounts: &[String]) -> Vec<String> {
    let mut v: Vec<String> = ["claude", "codex", "omp", "opencode"].map(String::from).to_vec();
    if !claude_accounts.is_empty() {
        v.push("claude-utama".into());
    }
    v.extend(claude_accounts.iter().map(|id| format!("claude-{id}")));
    v
}

fn openable_now(st: &State) -> Vec<String> {
    openable(&st.accounts.lock().unwrap().iter().filter(|a| a.agent == "claude").map(|a| a.id.clone()).collect::<Vec<_>>())
}

/// a shell pane sitting at its prompt with nothing in front: the only kind `/buka` types into
fn idle_shell(p: &Pane) -> bool {
    p.agent_name().is_none() && p.at_prompt.load(Ordering::Relaxed) && pty::busy(&p.master, p.sid).is_none()
}

/// `/buka`: types `<agent>` + Enter like the user at the keyboard; the pane becomes an agent pane (screen_watch) and active
fn open_in(st: &State, b: &Bot, id: u64, p: &Pane, agent: &str) {
    let id_l = b.id_lang;
    if p.agent_name().is_some() {
        return b.tell(t(id_l, "already_agent"));
    }
    if !idle_shell(p) {
        return b.tell(t(id_l, "busy_shell"));
    }
    match type_into(st, id, p, format!("{agent}\r").as_bytes()) {
        Ok(()) => {
            audit(st, p, &format!("[/buka {agent}]"));
            *b.last.lock().unwrap() = Some(id);
            b.tell(&format!("{} {agent} → {}. {}", t(id_l, "opening"), p.name.lock().unwrap(), t(id_l, "opened_hint")));
        }
        Err(_) => b.tell(t(id_l, "no_pane")),
    }
}

/// `/buka <agent>` without a pane: idle shell panes, one per row with their folder (`shell-13 · app`). Many panes over
/// several workspaces = pick the workspace first (9 Nov: 24 `shell-N · workspace` buttons, "susah taunya yang mana")
fn open_picker(st: &State, b: &Bot, agent: &str, only_ws: Option<&str>) {
    let names = ws_names();
    let ws_of = |w: &str| names.get(w).cloned().unwrap_or_else(|| w.to_string());
    let mut panes: Vec<(String, String, u64, i32)> = st
        .panes
        .lock()
        .unwrap()
        .iter()
        .filter(|(_, p)| idle_shell(p))
        .map(|(id, p)| (p.workspace.lock().unwrap().clone(), p.name.lock().unwrap().clone(), *id, p.sid))
        .filter(|(w, ..)| only_ws.is_none_or(|o| o == w))
        .collect();
    if panes.is_empty() {
        return b.tell(t(b.id_lang, "no_idle"));
    }
    // workspace name, then shell-2 before shell-11
    panes.sort_by(|a, b| (ws_of(&a.0), a.1.len(), &a.1).cmp(&(ws_of(&b.0), b.1.len(), &b.1)));
    let mut spaces: Vec<&str> = panes.iter().map(|x| x.0.as_str()).collect();
    spaces.dedup();
    // callback data is 64 bytes max: a long workspace id can't go in a button
    let by_ws = only_ws.is_none() && spaces.len() > 1 && panes.len() > 6 && spaces.iter().all(|w| w.len() + agent.len() <= 60);
    let (key, rows): (&str, Vec<Vec<Value>>) = if by_ws {
        let rows = spaces.iter().map(|w| {
            let n = panes.iter().filter(|x| x.0 == *w).count();
            vec![json!({"text": format!("{} · {n} shell", ws_of(w)), "callback_data": format!("w|{w}|{agent}")})]
        });
        ("open_ws", rows.collect())
    } else {
        let home = std::env::var("HOME").unwrap_or_default();
        let tabs = tab_titles();
        let rows = panes.iter().map(|(w, name, id, sid)| {
            let cwd = pty::cwd_of(*sid).map(|c| c.to_string_lossy().into_owned()).unwrap_or_default();
            let dir = if cwd == home { "~".to_string() } else { cwd.rsplit('/').next().unwrap_or_default().to_string() };
            // the tab title the user gave ("Isi DWM") says more than the folder
            let what = tabs.get(id).cloned().unwrap_or(dir);
            let text = if spaces.len() > 1 { format!("{what} · {name} · {}", ws_of(w)) } else { format!("{what} · {name}") };
            vec![json!({"text": text, "callback_data": format!("o|{id}|{agent}")})]
        });
        ("open_pick", rows.collect())
    };
    let text = esc(&format!("{} {agent}?", t(b.id_lang, key)));
    let _ = b.out.try_send(Out::Api("sendMessage", json!({"chat_id": b.chat(), "text": text, "parse_mode": "HTML", "reply_markup": {"inline_keyboard": rows}})));
}

/// buttons to pick the active pane, agent panes only, one per row with its tab title (`Isi DWM · claude-3`)
fn picker(st: &State, b: &Bot) {
    let tabs = tab_titles();
    let mut panes: Vec<(String, u64)> = st
        .panes
        .lock()
        .unwrap()
        .iter()
        .filter(|(_, p)| p.agent_name().is_some())
        .map(|(id, p)| {
            let name = p.name.lock().unwrap().clone();
            (tabs.get(id).map_or_else(|| name.clone(), |t| format!("{t} · {name}")), *id)
        })
        .collect();
    panes.sort();
    let rows: Vec<Vec<Value>> = panes.iter().map(|(n, id)| vec![json!({"text": n, "callback_data": format!("p|{id}")})]).collect();
    let text = esc(t(b.id_lang, "pick"));
    let _ = b.out.try_send(Out::Api("sendMessage", json!({"chat_id": b.chat(), "text": text, "parse_mode": "HTML", "reply_markup": {"inline_keyboard": rows}})));
}

/// median of the last turns (secs), the estimate for the next one
fn median(q: Option<&VecDeque<u64>>) -> Option<u64> {
    let mut v: Vec<u64> = q?.iter().copied().collect();
    v.sort_unstable();
    v.get(v.len() / 2).copied()
}

/// "⏳ claude-1 · running 1m 30s · usually ±1m 40s": an estimate from past turns of that agent kind, never a promise
fn progress_text(id_l: bool, name: &str, ran: u64, eta: Option<u64>, queued: bool) -> String {
    let mut s = format!("⏳ {}", esc(name));
    s += &if ran < 5 { format!(" {}", t(id_l, "p_now")) } else { format!(" · {} {}", t(id_l, "p_ran"), dur(ran)) };
    s += &match eta {
        Some(e) if ran > e + 30 => format!(" · {} (±{})", t(id_l, "p_late"), dur(e)),
        Some(e) => format!(" · {} ±{}", t(id_l, "p_eta"), dur(e)),
        None => format!(" · {}", t(id_l, "p_none")),
    };
    if queued {
        s += &format!("\n{}", t(id_l, "p_queued"));
    }
    s
}

fn progress_of(b: &Bot, id: u64, pane: &Pane, queued: bool) -> String {
    let ran = b.started.lock().unwrap().get(&id).map_or(0, |t0| t0.elapsed().as_secs());
    let eta = median(b.turns.lock().unwrap().get(&pane.agent_name().unwrap_or_default()));
    progress_text(b.id_lang, &pane.name.lock().unwrap(), ran, eta, queued)
}

/// type into the pane (SEC-36 checks), then 👀 + a "⏳ working, usually ±…" message until the pane is done (`on_status`)
/// `file`: a photo/file path is in the text. Claude turns a pasted image path into `[Image #N]` while the Enter of the same
/// paste arrives, so the Enter was lost and the image waited for the next message (9 Nov); Enter goes ~1.5 s later
fn deliver(st: &State, b: &Bot, id: u64, pane: &Pane, text: &str, msg: i64, file: bool) {
    let status = *pane.status.lock().unwrap();
    let pw = pty::busy(&pane.master, pane.sid).is_some() && pty::wants_password(&pane.master);
    let why = match typed(pane.agent_name().is_some(), status, pw, text) {
        Ok(bytes) => match type_into(st, id, pane, &bytes[..bytes.len() - usize::from(file)]) {
            Ok(()) => {
                // ponytail: sleeps in the poll thread (next update waits 1.5 s); a timer thread if that shows
                if file {
                    thread::sleep(Duration::from_millis(1500));
                    // never an Enter into a dialog that came up meanwhile (it would pick option 1)
                    if *pane.status.lock().unwrap() != "blocked" {
                        let _ = type_into(st, id, pane, b"\r");
                    }
                }
                audit(st, pane, text);
                *b.last.lock().unwrap() = Some(id);
                pane.tg_ask.lock().unwrap().push(msg);
                *pane.tg_text.lock().unwrap() = Some(text.to_string());
                b.react(msg, "👀");
                // one progress message per pane; a follow-up while it works keeps the same one
                let fresh = {
                    let mut p = b.progress.lock().unwrap();
                    !p.contains_key(&id) && p.insert(id, (None, Instant::now())).is_none()
                };
                if fresh {
                    let body = progress_of(b, id, pane, status == "working");
                    let _ = b.out.try_send(Out::Progress(id, json!({"chat_id": b.chat(), "text": body, "parse_mode": "HTML", "reply_parameters": {"message_id": msg}})));
                }
                return;
            }
            Err(_) => "no_pane",
        },
        // which pane it landed on: the phone shows only the pane names, and a stray message needs to be traceable (11 Nov)
        Err(Why::Shell) => return b.tell(&format!("{}: {}", pane.name.lock().unwrap(), t(b.id_lang, "shell"))),
        // the buttons may never have gone out (Mac not away, prompt not from the phone): send them now (9 Nov: "use the
        // buttons" with no buttons in the chat)
        Err(Why::Blocked) => {
            let seq = pane.status_seq.load(Ordering::Relaxed);
            let _ = b.out.try_send(Out::Notice(id, Kind::Blocked(seq), Instant::now()));
            return;
        }
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
                return b.set(st, "error", "tg_io"); // not "io": that one means a pane folder in the UI
            }
            *b.stamp.lock().unwrap() = mtime();
            *b.link.lock().unwrap() = Some(link);
            menu(b);
            let host = Command::new("/usr/sbin/scutil").args(["--get", "ComputerName"]).output().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default();
            b.say(&format!("{} {}.\n\n{}", t(id_l, "linked"), esc(&host), esc(t(id_l, "help"))));
            b.set(st, "on", "");
        }
        In::Help => b.tell(t(id_l, "help")),
        In::Choose => picker(st, b),
        In::Status => b.say(&status_text(st, id_l, *b.last.lock().unwrap())),
        In::Look(name) => match st.by_name(&name) {
            None => b.tell(t(id_l, "no_pane")),
            Some((_, pane)) => {
                let (n, ws_id) = (pane.name.lock().unwrap().clone(), pane.workspace.lock().unwrap().clone());
                let ws = ws_name(&ws_id);
                let st_txt = state_word(&pane, id_l);
                let v = values(st);
                if !snippet_on(&v, &[&ws, &ws_id]) {
                    return b.say(&format!("{}\n{}", head(&n, &ws, &st_txt), esc(t(id_l, "snip_off"))));
                }
                let lines = v["telegram"]["lines"].as_u64().unwrap_or(30).min(30) as usize;
                let snip = screen_snip(&pane, lines);
                b.say(&format!("{}\n<pre>{}</pre>", head(&n, &ws, &st_txt), esc(if snip.is_empty() { " " } else { &snip })));
            }
        },
        In::Type { to, text, msg } => match target(st, b, to) {
            Ok((id, pane)) => deliver(st, b, id, &pane, &text, msg, false),
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
            // double tap (8 Nov log: two callbacks for one picker): answered above, nothing else
            if b.picked.lock().unwrap().replace(msg) == Some(msg) {
                return;
            }
            let _ = b.out.try_send(Out::Api("editMessageReplyMarkup", json!({"chat_id": b.chat(), "message_id": msg, "reply_markup": {"inline_keyboard": []}})));
            if let Some(p) = pane {
                *b.last.lock().unwrap() = Some(id);
                let waiting = b.pending.lock().unwrap().take();
                if let Some((text, m)) = waiting {
                    deliver(st, b, id, &p, &text, m, false);
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
                    deliver(st, b, id, &pane, &text, msg, true);
                }
            },
        },
        In::Open { agent, pane } => {
            let ok = openable_now(st);
            if !ok.contains(&agent) {
                return b.tell(&format!("{} {}", t(id_l, "open_which"), ok.join(", ")));
            }
            match pane {
                None => open_picker(st, b, &agent, None),
                Some(n) => match st.by_name(&n) {
                    Some((id, p)) => open_in(st, b, id, &p, &agent),
                    None => b.tell(t(id_l, "no_pane")),
                },
            }
        }
        In::OpenWs { ws, agent, cb, msg } => {
            let _ = b.out.try_send(Out::Api("answerCallbackQuery", json!({"callback_query_id": cb})));
            if b.picked.lock().unwrap().replace(msg) == Some(msg) {
                return;
            }
            let _ = b.out.try_send(Out::Api("editMessageReplyMarkup", json!({"chat_id": b.chat(), "message_id": msg, "reply_markup": {"inline_keyboard": []}})));
            // checked again like OpenPick: callback data comes back from Telegram
            if openable_now(st).contains(&agent) {
                open_picker(st, b, &agent, Some(&ws));
            } else {
                b.tell(t(id_l, "no_pane"));
            }
        }
        In::OpenPick { pane: id, agent, cb, msg } => {
            let _ = b.out.try_send(Out::Api("answerCallbackQuery", json!({"callback_query_id": cb})));
            if b.picked.lock().unwrap().replace(msg) == Some(msg) {
                return;
            }
            let _ = b.out.try_send(Out::Api("editMessageReplyMarkup", json!({"chat_id": b.chat(), "message_id": msg, "reply_markup": {"inline_keyboard": []}})));
            // the button's agent is checked again: callback data comes back from Telegram, not from us
            let pane = st.panes.lock().unwrap().get(&id).cloned();
            match pane.filter(|_| openable_now(st).contains(&agent)) {
                Some(p) => open_in(st, b, id, &p, &agent),
                None => b.tell(t(id_l, "no_pane")),
            }
        }
        In::Key { pane: id, seq, key, cb, msg } => {
            let pane = st.panes.lock().unwrap().get(&id).cloned();
            let ok = pane.as_ref().is_some_and(|p| {
                // SEC-28 pattern: the same dialog is still open (status and its sequence unchanged)
                *p.status.lock().unwrap() == "blocked" && p.status_seq.load(Ordering::Relaxed) == seq && keys(&p.agent_name().unwrap_or_default()).contains(&key.as_str())
            });
            let pane = pane.filter(|_| ok);
            let sent = match pane.as_ref() {
                Some(p) => send_answer(st, id, p, &key, seq),
                None => false,
            };
            let word = if sent { t(id_l, "sent") } else { t(id_l, "gone") };
            let _ = b.out.try_send(Out::Api("answerCallbackQuery", json!({"callback_query_id": cb, "text": word})));
            let _ = b.out.try_send(Out::Api("editMessageReplyMarkup", json!({"chat_id": b.chat(), "message_id": msg, "reply_markup": {"inline_keyboard": []}})));
            if let (true, Some(p)) = (sent, pane) {
                audit(st, &p, &format!("[{}: {}]", t(id_l, "answered"), key));
            }
        }
        // a vote taken back (no option) waits for the next one
        In::Vote { picks, .. } if picks.is_empty() => {}
        In::Vote { poll, picks } => {
            let Some(v) = b.polls.lock().unwrap().remove(&poll) else { return b.tell(t(id_l, "gone")) };
            let sent = vote(st, b, &v, &picks);
            let _ = b.out.try_send(Out::Api("stopPoll", json!({"chat_id": b.chat(), "message_id": v.msg, "reply_markup": {"inline_keyboard": []}})));
            let pane = st.panes.lock().unwrap().get(&v.pane).cloned();
            match (sent, pane) {
                (true, Some(p)) => {
                    let picked: Vec<&str> = picks.iter().filter_map(|i| v.labels.get(*i).map(String::as_str)).collect();
                    audit(st, &p, &format!("[{}: {}]", t(id_l, "answered"), picked.join(", ")));
                }
                _ => b.tell(t(id_l, "gone")),
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
/// (workspace name, agent pane rows: needs permission first, plain shell panes counted). Real app 9 Nov: 25 panes,
/// mostly idle shells, made /status a wall of "shell".
type WsRows = (String, Vec<AgentRow>, usize);
/// (not blocked, pane name, status word)
type AgentRow = (bool, String, String);

fn status_text(st: &State, id_l: bool, active: Option<u64>) -> String {
    let names = ws_names();
    let mut by_ws: HashMap<String, (Vec<AgentRow>, usize)> = HashMap::new();
    for (id, p) in st.panes.lock().unwrap().iter() {
        let ws_id = p.workspace.lock().unwrap().clone();
        let e = by_ws.entry(names.get(&ws_id).cloned().unwrap_or(ws_id)).or_default();
        if p.agent_name().is_none() {
            e.1 += 1;
            continue;
        }
        let mut word = state_word(p, id_l);
        if active == Some(*id) {
            word += if id_l { " ← aktif" } else { " ← active" };
        }
        e.0.push((*p.status.lock().unwrap() != "blocked", p.name.lock().unwrap().clone(), word));
    }
    let rows: Vec<WsRows> = by_ws.into_iter().map(|(ws, (agents, shells))| (ws, agents, shells)).collect();
    let body = status_body(rows, id_l);
    let jatah = usage_block(st, id_l);
    if jatah.is_empty() { body } else { format!("{body}\n\n{jatah}") }
}

/// FR-102: the allowance left per login, numbers and reset times only (SEC-33/36). No work content, no screen text.
fn usage_block(st: &State, id_l: bool) -> String {
    let rows = super::usage::telegram_lines(st, id_l);
    if rows.is_empty() {
        return String::new();
    }
    let mut out = format!("<b>{}</b>\n", if id_l { "Jatah" } else { "Allowance" });
    for (agent, name, parts) in rows {
        out += &format!("{} · {} — {}\n", esc(&agent), esc(&name), esc(&parts.join(" · ")));
    }
    out.trim_end().to_string()
}

fn status_body(mut rows: Vec<WsRows>, id_l: bool) -> String {
    if rows.is_empty() {
        return t(id_l, "none").to_string();
    }
    // workspaces with agents first, then by name
    rows.sort_by(|a, b| a.1.is_empty().cmp(&b.1.is_empty()).then_with(|| a.0.cmp(&b.0)));
    let mut out = String::new();
    for (ws, mut agents, shells) in rows {
        agents.sort();
        out += &format!("<b>{}</b>\n", esc(if ws.is_empty() { "-" } else { &ws }));
        for (_, name, word) in agents {
            out += &format!("{} — {}\n", esc(&name), esc(&word));
        }
        if shells > 0 {
            out += &format!("{shells} shell\n");
        }
        out += "\n";
    }
    out.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ks(v: Vec<(&'static str, String)>) -> Vec<&'static str> {
        v.into_iter().map(|(k, _)| k).collect()
    }

    /// FR-87: a blocked pane's card says "butuh izin" or "nunggu jawaban" (the banner's words), then the agent's own reason
    #[test]
    fn blocked_kind_words() {
        assert_eq!(blocked_word("", true), "butuh izin");
        assert_eq!(blocked_word("needs permission", false), "needs permission");
        assert_eq!(blocked_word("nunggu jawaban", true), "nunggu jawaban");
        assert_eq!(blocked_word("waiting for an answer", false), "waiting for an answer");
        assert_eq!(blocked_word("minta percaya folder", true), "butuh izin · minta percaya folder");
        assert_eq!(blocked_word("nunggu jawaban", false), "waiting for an answer"); // either language's plain word maps to the one in use
    }

    /// dialogs recorded from the real agents on 10 Nov 2026 (research/agent-dialogs, home path replaced): the buttons come
    /// from the options on screen, a mode switch never gets one, and No is always reachable
    #[test]
    fn recorded_dialogs() {
        let dlg = |f: &str| std::fs::read_to_string(format!("{}/tests/dialogs/{f}.txt", env!("CARGO_MANIFEST_DIR"))).unwrap();
        // Claude 2.1.311 Bash: 1 Yes, 2 always allow folder, 3 switch to auto mode, 4 No. "3" used to turn on auto mode.
        let bash = dlg("claude-2.1.311-bash-k3");
        let b = claude_keys("claude", &bash, true);
        // FR-99 P2: the mode switch and the deny option get no button of their own (Tolak = the same cancel)
        assert_eq!(ks(b.clone()), ["1", "2", "esc"]);
        assert_eq!((b[0].1.as_str(), b[1].1.as_str(), b[2].1.as_str()), ("1. Izinkan", "2. Izinkan terus", "Tolak"));
        assert!(!digit_ok(&bash, 3) && digit_ok(&bash, 4) && digit_ok(&bash, 1));
        // Write: option 2 switches to accept edits; outside the folder it wraps over two rows
        assert_eq!(ks(claude_keys("claude", &dlg("claude-2.1.311-write-k1"), true)), ["1", "esc"]);
        assert_eq!(ks(claude_keys("claude", &dlg("claude-2.1.311-outside-k1"), true)), ["1", "esc"]);
        assert_eq!(ks(claude_keys("claude", &dlg("claude-2.1.311-fetch-esc"), true)), ["1", "2", "esc"]);
        // Codex 0.167: the shortcut letters per dialog (p for a command, a for edits)
        let net = dlg("codex-0.167.0-net-y");
        assert_eq!(ks(claude_keys("codex", &net, true)), ["1", "2", "esc"]);
        assert_eq!((codex_key(&net, 1), codex_key(&net, 2), codex_key(&net, 3)), (Some(b"y".to_vec()), Some(b"p".to_vec()), Some(b"\x1b".to_vec())));
        assert_eq!(codex_key(&dlg("codex-0.167.0-outside-a"), 2), Some(b"a".to_vec()));
        // a digit pressed after the dialog is gone sends nothing
        assert!(!digit_ok("● Done.", 1));
        assert_eq!(cb_parse(&cb_data(3, 9, "4")), Some((3, 9, "4".into())));
    }

    fn link() -> Link {
        Link { token: "1:x".into(), chat: 42, user: 7, name: "R".into() }
    }

    fn msg(chat: i64, user: i64, text: &str, date: i64) -> Value {
        json!({"message": {"message_id": 5, "date": date, "chat": {"id": chat, "type": "private"}, "from": {"id": user, "first_name": "Raka", "username": "raka"}, "text": text}})
    }

    #[test]
    fn secret_patterns_and_near_misses() {
        let cases = [
            ("-----BEGIN RSA PRIVATE KEY-----\nsecret\n-----END RSA PRIVATE KEY-----", "[private key]", "-----BEGIN PUBLIC KEY-----abc"),
            ("Authorization: abcdef", "Authorization: ***", "Authorization pending"),
            ("Bearer abcdef123", "Bearer ***", "Bearer abc"),
            ("https://user:password@example.com", "https://user:***@example.com", "https://example.com/a"),
            ("https://hooks.slack.com/services/T123/B456/secret", "https://hooks.slack.com/services/***", "https://hooks.slack.com/services/help"),
            ("https://discord.com/api/webhooks/123/secret", "https://discord.com/api/webhooks/***", "https://discord.com/api/webhooks/help"),
            ("https://api.telegram.org/bot123:secret", "https://api.telegram.org/bot***", "https://api.telegram.org/help"),
            ("curl -u user:secret", "curl -u user:***", "curl -u user"),
            ("--token 'secret'", "--token '***'", "--tokens help"),
            ("GITHUB_TOKEN='secret'", "GITHUB_TOKEN='***'", "GITHUB_TOKEN pending"),
            ("sk-ant-abcdefghijk", "sk-***", "sk-short"),
            ("ghp_abcdefghijklmnop", "gh*_***", "ghp_short"),
            ("github_pat_abcdefghijklmnop", "github_pat_***", "github_pat_short"),
            ("glpat-abcdefghijklmnop", "glpat-***", "glpat-short"),
            ("xoxb-abcdefghijk", "xox*-***", "xoxb-short"),
            ("AKIA1234567890ABCDEF", "AKIA***", "xAKIA1234567890ABCDEF"),
            ("AIza123456789012345678901234567890", "AIza***", "AIza123"),
            ("sk_live_abcdefghij", "sk_***", "sk_live_short"),
            ("npm_123456789012345678901234567890", "npm_***", "npm test"),
            ("hf_123456789012345678901234567890", "hf_***", "hf"),
            ("eyJabcdefgh.abcdefgh.abcdefgh", "eyJ***", "eyJabc.a.b"),
        ];
        for (raw, masked, safe) in cases {
            assert_eq!(mask_secrets(raw), masked, "{raw}");
            assert_eq!(mask_secrets(safe), safe);
        }
        for (raw, expected) in [
            ("X-Api-Key: secret", "X-Api-Key: ***"),
            ("Private-Token: secret", "Private-Token: ***"),
            ("ASIA1234567890ABCDEF", "ASIA***"),
            ("gho_abcdefghijklmnop", "gh*_***"),
            ("NAME_SECRET=secret", "NAME_SECRET=***"),
            ("NAME_KEY=secret", "NAME_KEY=***"),
        ] {
            assert_eq!(mask_secrets(raw), expected);
        }
        assert_eq!(mask_secrets("Basic abcdef"), "Basic ***");
        assert_eq!(mask_secrets("--password=secret"), "--password=***");
        assert_eq!(mask_secrets(r#""password": "secret""#), r#""password": "***""#);
        assert_eq!(mask_secrets("Cookie: session=secret"), "Cookie: ***");
        assert_eq!(mask_secrets("-----BEGIN PRIVATE KEY-----\nsecret"), "[private key]");
    }

    #[test]
    fn snippets_mask_before_cutting_or_html() {
        let raw = "Bearer abcdef123\nhttps://user:secret@example.com\nGITHUB_TOKEN=secret";
        assert_eq!(mask_secrets(&snippet(raw, 30)), "Bearer ***\nhttps://user:***@example.com\nGITHUB_TOKEN=***");
        let long = format!("Bearer {}", "a".repeat(5000));
        assert_eq!(snippet(&long, 30), "Bearer ***");
        assert_eq!(label(&long, "claude").unwrap(), "Bearer ***");
        assert_eq!(md_html(&mask_secrets("Bearer abcdef123"), MAX_ANSWER), "Bearer ***");
    }

    #[test]
    fn jawaban_agent_jadi_html() {
        let md = "## Hasil\n**7 task** siap, lihat [task 152347](https://collab.x/t?a=1&b=2) dan `uji/laporan.md`.\n\n| Task | MR |\n|---|---|\n| 152347 | !716 |\n- <b>bukan tag</b>\n```\nlet x = 1 < 2;\n```";
        assert_eq!(
            md_html(md, 4000),
            "<b>Hasil</b>\n<b>7 task</b> siap, lihat <a href=\"https://collab.x/t?a=1&amp;b=2\">task 152347</a> dan <code>uji/laporan.md</code>.\n\nTask · MR\n152347 · !716\n• &lt;b&gt;bukan tag&lt;/b&gt;\n<pre>let x = 1 &lt; 2;</pre>"
        );
        // cut inside a fence: the <pre> still closes; a link without http stays text
        assert_eq!(md_html("```\nabcdef", 7), "<pre>abc …</pre>");
        assert_eq!(md_html("[x](javascript:alert(1))", 99), "[x](javascript:alert(1))");
        let p = plain(json!({"text": "<b>a</b> &lt;3 &amp; <a href=\"u\">b</a>", "parse_mode": "HTML"}));
        assert_eq!(p, json!({"text": "a <3 & b"}));
    }

    #[test]
    fn tabel_kotak_jadi_baris() {
        let screen = "Ada 2 task:\n  ┌──────────┬─────────────┐\n  │ Task     │ Judul       │\n  ├──────────┼─────────────┤\n  │ 152347   │ [BE] Kolom  │\n  │ (#83)    │ Result      │\n  ├──────────┼─────────────┤\n  │ 152349   │ │\n  └──────────┴─────────────┘\nSelesai.";
        assert_eq!(snippet(screen, 30), "Ada 2 task:\nTask · Judul\n152347 (#83) · [BE] Kolom Result\n152349\nSelesai.");
        // plain text and code stay as they are
        assert_eq!(snippet("a  b\n    if x {\n────\n", 30), "a  b\n    if x {");
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
    fn chrome_tui_dibuang_dari_potongan() {
        let screen = "Ringkasan kerja\n▣ Build · Muse Spark 1.3 · 7m 49s\nBuild · Muse Spark OpenCode Go · high\n██████████████████████████████\n/Users/x/gembala 209.9K (20%) · $0.06 ctrl+p commands\nQuota unavailable";
        assert_eq!(snippet(screen, 30), "Ringkasan kerja");
        // chrome rows can't eat the line budget: real content above them survives
        assert_eq!(snippet("ok\n████████", 30), "ok");
        // ordinary text with these words nowhere near TUI chrome stays
        assert_eq!(snippet("build ok\nctrl is a key", 30), "build ok\nctrl is a key");
        // bare quota footers read as TUI chrome and drop (rare prose tradeoff)
        assert_eq!(snippet("Quota unavailable", 30), "");
        // Claude's tool calls and their output are not the turn (9 Nov: answer buried under "Bash(cd …)")
        let turn = "● 7 task siap diuji.\n⏺ Bash(cd /x && python3 lapor.py laporan.md)\n  ⎿ Allowed by auto mode classifier\n  ⎿ Shell cwd was reset to /Users/x\n⏺ Read(laporan.md)\nSelesai.";
        assert_eq!(snippet(turn, 30), "● 7 task siap diuji.\nSelesai.");
        // a spoken answer keeps its `⏺`: no tool name before the parenthesis
        assert_eq!(snippet("⏺ Hasilnya (7 task) siap.", 30), "⏺ Hasilnya (7 task) siap.");
    }

    /// 9 Nov: the done card held Claude's `/model` picker instead of the answer. A picker or a dialog is never sent.
    #[test]
    fn layar_pemilih_bukan_jawaban() {
        let model = "❯ /model\n  1. Default (recommended)\n❯ 2. Fable 5.1\n  3. Sonnet 5.5\nEnter to set as default · Esc to cancel";
        assert!(picker_screen(&snippet(model, 30)));
        assert!(!picker_screen("● 7 task siap diuji.\n152347 · [BE] Kolom Result"));
        // an ordinary numbered list from an answer (no cursor row) stays
        assert!(!picker_screen("Hasil:\n1. perbaiki A\n2. perbaiki B"));
        // the dialog footer alone is enough (the options may be above the snippet's line budget)
        assert!(picker_screen("  2. Opus\nEnter to confirm"));
    }

    #[test]
    fn penahanan_jawaban_per_agent() {
        assert!(answers_from_hook("claude") && answers_from_hook("codex") && answers_from_hook("omp"));
        // OpenCode's plugin has no answer text: no reason to make its card wait
        assert!(!answers_from_hook("opencode") && !answers_from_hook("shell"));
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
        r["message"]["reply_to_message"]["text"] = json!("⏳ omp-1 · udah jalan 1m 30s · biasanya kelar ±2m");
        assert_eq!(parse(&r, Some(&l), n), In::Type { to: Some(To::Reply(77, "omp-1".into())), text: "bikin changelog".into(), msg: 5 });
        r["message"]["reply_to_message"]["text"] = json!("Ke pane mana?");
        assert_eq!(parse(&r, Some(&l), n), In::Type { to: Some(To::Reply(77, String::new())), text: "bikin changelog".into(), msg: 5 });
        // plain text = active pane, no command needed
        assert_eq!(parse(&msg(42, 7, "lanjut bikin test", n), Some(&l), n), In::Type { to: None, text: "lanjut bikin test".into(), msg: 5 });
        assert_eq!(parse(&msg(42, 7, "/ke claude-2", n), Some(&l), n), In::Use("claude-2".into()));
        assert_eq!(parse(&msg(42, 7, "/to", n), Some(&l), n), In::Choose);
        assert_eq!(parse(&msg(42, 7, "/to shell-25 cek ini", n), Some(&l), n), In::Type { to: Some(To::Name("shell-25".into())), text: "cek ini".into(), msg: 5 });
        // the agent's own commands go to the active pane; the bot's own without arguments = help
        assert_eq!(parse(&msg(42, 7, "/exit", n), Some(&l), n), In::Type { to: None, text: "/exit".into(), msg: 5 });
        assert_eq!(parse(&msg(42, 7, "/model", n), Some(&l), n), In::Type { to: None, text: "/model".into(), msg: 5 });
        assert_eq!(parse(&msg(42, 7, "/compact", n), Some(&l), n), In::Type { to: None, text: "/compact".into(), msg: 5 });
        assert_eq!(parse(&msg(42, 7, "/clear", n), Some(&l), n), In::Type { to: None, text: "/clear".into(), msg: 5 });
        // ... and as a reply: the command goes to the pane whose card was answered
        let mut ex = msg(42, 7, "/exit", n);
        ex["message"]["reply_to_message"] = json!({"message_id": 77, "text": "claude-1 · gembala kelar\n● Oke."});
        assert_eq!(parse(&ex, Some(&l), n), In::Type { to: Some(To::Reply(77, "claude-1".into())), text: "/exit".into(), msg: 5 });
        assert_eq!(parse(&msg(42, 7, "/help", n), Some(&l), n), In::Help);
        assert_eq!(parse(&msg(42, 7, "/buka", n), Some(&l), n), In::Help);
    }

    /// FR-99 P2 (mockup "Susunan tombol", user decision 11 Nov 2026): at most 4 buttons per row; a dialog with more wraps
    /// onto the next row instead of squeezing, so every choice can be answered from the phone. 0 = no keyboard at all.
    #[test]
    fn tombol_izin_maks_empat_per_baris() {
        let b = |k: &'static str, l: &str| (k, l.to_string());
        let three = [b("1", "1. Izinkan"), b("2", "2. Izinkan terus"), b("esc", "Tolak")];
        let r3 = key_rows(7, 9, &three);
        assert_eq!(r3.len(), 1);
        assert_eq!(r3[0].len(), 3);
        assert_eq!(r3[0][2]["callback_data"], "k|7|9|esc");
        // Claude's four options + Tolak: 4 + 1, the last one is still there (the missing-choice complaint)
        let five = [b("1", "1. Izinkan"), b("2", "2. Izinkan terus"), b("3", "3. Lagi"), b("4", "4. Opsi"), b("esc", "Tolak")];
        let r5 = key_rows(7, 9, &five);
        assert_eq!(r5.iter().map(Vec::len).collect::<Vec<_>>(), [4, 1]);
        assert_eq!(r5[1][0], json!({"text": "Tolak", "callback_data": "k|7|9|esc"}));
        // 8 = two rows of four (Telegram's own per-row limit), a ninth button starts a third row
        let eight: Vec<_> = DIGITS.iter().take(8).map(|k| (*k, format!("{k}."))).collect();
        assert_eq!(key_rows(1, 2, &eight).iter().map(Vec::len).collect::<Vec<_>>(), [4, 4]);
        let nine: Vec<_> = DIGITS.iter().map(|k| (*k, format!("{k}."))).collect();
        assert_eq!(key_rows(1, 2, &nine).iter().map(Vec::len).collect::<Vec<_>>(), [4, 4, 1]);
        // nothing to answer = no keyboard, which is also what "answered" sends
        assert!(key_rows(7, 9, &[]).is_empty());
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
        assert_eq!(cb_parse(&cb_data(12, 4471, "once")), Some((12, 4471, "once".into())));
        assert_eq!(cb_parse(&cb_data(12, 4471, "always")), Some((12, 4471, "always".into())));
        assert!(cb_data(u64::MAX, u64::MAX, "always").len() <= 64);
        assert_eq!(keys("opencode"), &["once", "always", "esc"]);
        assert_eq!(keys("omp"), &["once", "esc"]);
        assert_eq!(keys("codex"), &["1", "2", "3", "esc"]);
        // Codex 0.167 dialog as drawn 10 Nov: option 2 wraps, the shortcut is on its last row
        let codex = "  $ bash jam-bukti.sh out\n\n› 1. Yes, proceed (y)\n  2. Yes, and don't ask again for commands that start with `bash\n     jam-bukti.sh out` (p)\n  3. No, and tell Codex what to do differently (esc)\n\n  Press enter to confirm or esc to cancel";
        assert_eq!(ks(claude_keys("codex", codex, true)), ["1", "2", "esc"]);
        assert_eq!(codex_key(codex, 1).as_deref(), Some(&b"y"[..]));
        assert_eq!(codex_key(codex, 2).as_deref(), Some(&b"p"[..]));
        assert_eq!(codex_key(codex, 3).as_deref(), Some(&b"\x1b"[..]));
        assert_eq!(codex_key("› 1. Yes, proceed", 1), None);
        // option 2 wraps onto a row that reads like option 1: button 1 must stay `y`, never option 2's `p`
        let wrap = "› 1. Yes, proceed (y)\n  2. Yes, and don't ask again for commands that start with `bash\n     1. arg` (p)\n  3. No, and tell Codex what to do differently (esc)";
        assert_eq!(codex_key(wrap, 1).as_deref(), Some(&b"y"[..]));
        assert_eq!(codex_key(wrap, 2).as_deref(), Some(&b"p"[..]));
        // numbers out of order = not a dialog read right: nothing is sent
        assert_eq!(codex_key("› 1. Yes (y)\n  3. No (esc)", 3), None);
        assert_eq!(keys("claude"), &["1", "2", "3", "esc"]);
        // FR-99 P2: a plain shell (and any agent this build doesn't know) has no dialog buttons
        assert!(keys("shell").is_empty());
        assert_eq!(key_bytes("once"), b"\r");
        assert_eq!(key_bytes("esc"), b"\x1b");
    }

    /// every recorded screen: a dialog reads as blocked with the same 12-row tail the screen watcher reads, an answered
    /// screen doesn't. 11 Nov: the WebFetch dialog ("Do you want to allow …") read as working, then done, so no card came.
    #[test]
    fn rekaman_dialog_terbaca_blocked() {
        let dir = format!("{}/tests/dialogs", env!("CARGO_MANIFEST_DIR"));
        let mut files = 0;
        for e in std::fs::read_dir(&dir).unwrap() {
            let path = e.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            // a question isn't read from the screen ("Enter to select" is every picker's footer): Claude's session file says
            // "input needed" (claude.rs), the multiselect fixture is there for `multiselect_jadi_poll`
            if !name.ends_with(".txt") || name.contains("multiselect") {
                continue;
            }
            let screen = std::fs::read_to_string(&path).unwrap();
            let rows: Vec<&str> = screen.lines().filter(|l| !l.trim().is_empty()).collect();
            let tail = rows[rows.len().saturating_sub(12)..].join("\n");
            let blocked = profile::screen_status(&tail, Duration::from_secs(5), "working") == "blocked";
            // every fixture without `.after` is a dialog (or the "△ Always allow" stage); after = answered
            assert_eq!(blocked, !name.contains(".after"), "{name}");
            files += 1;
        }
        assert!(files >= 16, "dialog fixtures missing: {files}");
    }

    /// 11 Nov: a Claude multiSelect question goes out as a poll (Vote = Submit); its card keeps only Deny and shows the question
    #[test]
    fn multiselect_jadi_poll() {
        let screen = std::fs::read_to_string(format!("{}/tests/dialogs/claude-2.1.311-multiselect.txt", env!("CARGO_MANIFEST_DIR"))).unwrap();
        let opts = |s: &str| multi_opts(s).unwrap().into_iter().map(|(l, n, on)| format!("{n}:{l}:{on}")).collect::<Vec<_>>();
        // "Type something" is typed on the Mac, "Chat about this" sits under the rule, descriptions are not labels
        assert_eq!(opts(&screen), ["1:Apel:false", "2:Mangga:false", "3:Jeruk:false"]);
        assert_eq!(opts(&screen.replace("[ ] Mangga", "[✔] Mangga")), ["1:Apel:false", "2:Mangga:true", "3:Jeruk:false"]);
        assert_eq!(blocked_keys("claude", &screen, "", true), vec![("esc", "Tolak".to_string())]);
        assert!(blocked_text("shell-5", "", "nunggu jawaban", "claude", &screen, true).contains("SIMULASI multiSelect: pilih buah"));
        // a single-select question (no checkboxes) and the review page keep their numbered buttons
        assert!(multi_opts(&screen.replace("[ ] ", "")).is_none());
        let review = "←  ☒ Buah  ✔ Submit  →\nReview your answers\n ● Pilih buah\n   → Apel, Jeruk\nReady to submit your answers?\n❯ 1. Submit answers\n  2. Cancel\n";
        assert!(multi_opts(review).is_none());
        assert_eq!(dialog_opts(review).unwrap()[0], "Submit answers");
        // a vote from the linked user: option indexes; anyone else is ignored
        let l = Link { token: "t".into(), chat: 42, user: 7, name: "u".into() };
        let v = |from: i64| json!({"poll_answer": {"poll_id": "P1", "user": {"id": from}, "option_ids": [0, 2]}});
        assert_eq!(parse(&v(7), Some(&l), 0), In::Vote { poll: "P1".into(), picks: vec![0, 2] });
        assert_eq!(parse(&v(8), Some(&l), 0), In::Ignore);
        assert_eq!(parse(&v(7), None, 0), In::Ignore);
    }

    /// FR-80: the permission card per agent: one line saying what is asked, the dialog's own reason, and buttons in the
    /// bot's language. Screens recorded from the real agents (home path replaced with /Users/demo).
    #[test]
    fn kartu_izin_per_agent() {
        let dlg = |f: &str| std::fs::read_to_string(format!("{}/tests/dialogs/{f}.txt", env!("CARGO_MANIFEST_DIR"))).unwrap();
        let btns = |agent: &str, f: &str, id_l: bool| -> Vec<(&'static str, String)> { blocked_keys(agent, &dlg(f), "", id_l) };

        // omp bash: tool + command, buttons say what they do (Enter approves, Esc denies)
        let omp = dlg("omp-18.18.3-bash-once.dialog");
        assert_eq!(ask_of("omp", &omp), "bash · touch ran.txt");
        assert_eq!(
            blocked_text("omp-1", "gembala", "minta izin", "omp", &omp, true),
            "<b>omp-1 · gembala</b> minta izin\nbash · touch ran.txt"
        );
        assert_eq!(btns("omp", "omp-18.18.3-bash-once.dialog", true), [("once", "Izinkan".into()), ("esc", "Tolak".into())]);
        assert_eq!(btns("omp", "omp-18.18.3-bash-once.dialog", false), [("once", "Allow".into()), ("esc", "Deny".into())]);
        // omp write: the path, not the file body; no chrome row reaches the card
        assert_eq!(ask_of("omp", &dlg("omp-18.18.3-write-esc.dialog")), "write · note.txt");
        assert!(!ask_of("omp", &omp).contains('╭') && !ask_of("omp", &omp).contains('│'));
        // the reason row a pattern-matched command draws (screenshot 11 Nov) stays one line and is masked in the card
        let pattern = "╭─ Allow tool: bash ─╮\n│ Reason: Prompt required by bash pattern: *psql* Command: cd x && PGPASSWORD=hunter2 psql │\n│ ❯ Approve │\n│   Deny │\n╰────╯";
        assert_eq!(ask_of("omp", pattern), "bash");
        assert_eq!(
            blocked_text("omp-1", "", "minta izin", "omp", pattern, true),
            "<b>omp-1</b> minta izin\nbash\nAlasan: Prompt required by bash pattern: *psql* Command: cd x &amp;&amp; PGPASSWORD=*** psql"
        );

        // Codex: the command it wants to run and the reason it wrote; digits carry its own shortcut letters
        let codex = dlg("codex-0.167.0-net-y");
        assert_eq!(ask_of("codex", &codex), "curl -sI https://example.com -o head.txt");
        assert!(why_of(&codex).starts_with("Allow this exact curl command outside sandbox?"));
        let card = blocked_text("codex-2", "gembala", "needs permission", "codex", &codex, false);
        assert!(card.starts_with("<b>codex-2 · gembala</b> needs permission\ncurl -sI https://example.com -o head.txt\nReason: Allow this exact"), "{card}");
        assert!(!card.contains("OpenAI Codex") && !card.contains("Press enter") && !card.contains('╭'));
        // title + ask + reason: the buttons stay under the card instead of below a screen dump (11 Nov)
        assert!(card.lines().count() <= 3, "{card}");
        let cb = btns("codex", "codex-0.167.0-net-y", false);
        assert_eq!(cb.iter().map(|(k, _)| *k).collect::<Vec<_>>(), ["1", "2", "esc"]);
        assert!(cb[0].1 == "1. Allow" && cb[1].1 == "2. Allow always" && cb[2].1 == "Deny");
        assert_eq!(ask_of("codex", &dlg("codex-0.167.0-outside-a")), "Apply proposed file edits · /Users/demo/.gk-dialog-test/codex-a.txt");

        // Claude: its own dialog title + the command/url; write keeps the file name (the body `1 hi` is not the ask)
        assert_eq!(ask_of("claude", &dlg("claude-2.1.311-bash-k3")), "Bash command · touch ran.txt");
        assert_eq!(ask_of("claude", &dlg("claude-2.1.311-fetch-esc")), "Fetch · from the web-fetch agent · url: https://example.com/");
        assert_eq!(ask_of("claude", &dlg("claude-2.1.311-write-k1")), "Create file · note.txt");
        assert_eq!(ask_of("claude", &dlg("claude-2.1.311-outside-k1")), "Create file · ../../../../../Users/demo/.gk-dialog-test/claude-outside.txt");
        // the Bash dialog's 4 options: 3 switches to auto mode and 4 is the same cancel as Tolak, so neither gets a button
        let cl = btns("claude", "claude-2.1.311-bash-k3", true);
        assert_eq!(cl.iter().map(|(k, _)| *k).collect::<Vec<_>>(), ["1", "2", "esc"]);
        assert!(cl[0].1 == "1. Izinkan" && cl[1].1 == "2. Izinkan terus" && cl[2].1 == "Tolak");

        // OpenCode: the folder it wants, and the "△ Always allow" confirmation stage
        assert_eq!(ask_of("opencode", &dlg("opencode-1.19.5-outside-once.dialog")), "Access external directory ~/.gk-dialog-test");
        assert_eq!(
            btns("opencode", "opencode-1.19.5-outside-once.dialog", true),
            [("once", "Izinkan sekali".into()), ("always", "Izinkan selalu".into()), ("esc", "Tolak".into())]
        );
        assert_eq!(
            ask_of("opencode", &dlg("opencode-1.19.5-outside-always.confirm")),
            "Always allow · This will allow the following patterns until OpenCode is restarted"
        );
        // an agent whose dialog isn't read: no ask line, no button at all (answered on the Mac), never a guessed option
        assert_eq!(ask_of("gemini", "ada apa ya"), "");
        assert!(blocked_keys("gemini", "ada apa ya", "", true).is_empty());
        assert!(keys("gemini").is_empty());
    }

    /// FR-66: a done card carries the agent's own answer; Codex's screen is its TUI transcript, so a card that comes
    /// before its notify hook stays short instead of dumping the process log (11 Nov).
    #[test]
    fn kartu_done_codex_tanpa_transkrip() {
        let transcript = "• Viewed image b51d474b.jpg\n  + Show details\n⚠ 5h limit: 18% left\n› Ask Codex to do anything";
        assert_eq!(turn_body(true, "codex", Some("File sudah dibuat."), || transcript.into()), "\n\nFile sudah dibuat.");
        assert_eq!(turn_body(true, "codex", None, || transcript.into()), "");
        // omp/OpenCode have no stop hook: the screen is all there is
        assert_eq!(turn_body(true, "omp", None, || "halo".into()), "\n<pre>halo</pre>");
        // other cards (a long command finished) keep the screen
        assert_eq!(turn_body(false, "codex", None, || "ok".into()), "\n<pre>ok</pre>");
        // an empty screen adds nothing
        assert_eq!(turn_body(true, "claude", None, String::new), "");
    }

    #[test]
    fn judul_konfirmasi_always_hanya_garis_judul() {
        assert!(is_always_confirm_line("△ Always allow"));
        assert!(is_always_confirm_line("  △ Always allow  "));
        assert!(!is_always_confirm_line("△ Permission required"));
        assert!(!is_always_confirm_line("△ Reject permission"));
        assert!(!is_always_confirm_line("❯ Always allow"));
        assert!(!is_always_confirm_line("Allow once  Always allow  Reject"));
        assert!(!is_always_confirm_line("Always allow"));
        assert!(!is_always_confirm_line(""));
    }

    /// FR-80: plain phone text follows `last` only while an agent still runs in that pane. After a Claude exits in a shell
    /// pane, a reply to a card the agent posted through the bot itself (no pane name) was typed into that shell.
    #[test]
    fn pane_terakhir_dipakai_hanya_kalau_masih_agent() {
        let panes = [(3, false), (5, true)];
        assert_eq!(last_agent(Some(5), &panes), Some(5));
        // left on a shell pane: back to the single-agent / pick / no_agent rules
        assert_eq!(last_agent(Some(3), &panes), None);
        // pane closed meanwhile
        assert_eq!(last_agent(Some(9), &panes), None);
        assert_eq!(last_agent(None, &panes), None);
        assert_eq!(last_agent(Some(3), &[(3, false)]), None);
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
        p["message"]["caption"] = json!("/to shell-25 ko malah ke 24?");
        assert_eq!(parse(&p, Some(&l), n), In::Media { to: Some(To::Name("shell-25".into())), caption: "ko malah ke 24?".into(), file: "besar".into(), ext: "jpg".into(), msg: 5 });
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
        // boxed permission dialog (Edit diff in a box, options inside or below it) stays: it's the question
        let edit = ["● Update(a.rs)", "1 new message (click) ↓", "╭──────────╮", "│ Edit file │", "│ + x │", "╰──────────╯", "Do you want to make this edit to a.rs?", "❯ 1. Yes", "  2. No"];
        assert_eq!(above_input(&edit), edit.len());
        let boxed = ["╭──────────╮", "│ Edit file │", "│ ❯ 1. Yes │", "│   2. No │", "╰──────────╯"];
        assert_eq!(above_input(&boxed), boxed.len());
        // a numbered list typed in omp's input box is still the input box: the draft stays off the phone
        let draft = ["● Oke.", "", "╭── ⬢ DeepSeek ──╮", "│ 1. perbaiki A │", "│ 2. perbaiki B │", "╰─        ─╯"];
        assert_eq!(above_input(&draft), 1);
        let convo = [" ▐▛███▜▌ Claude Code v2.1", "❯ halo", "● Halo.", "❯ jawab pake 1 kata", "● Oke."];
        assert_eq!(turn_start(&convo, None), 3);
        assert_eq!(turn_start(&convo, Some("halo")), 1);
        assert_eq!(turn_start(&["tanpa prompt"], None), 0);
        // permission dialog: the question above `❯ 1. Yes` stays in the snippet
        let ask = ["❯ cek monitoring", "● Bash(curl -s https://x)", "Bash command", "  curl -s https://x", "Do you want to proceed?",
            "❯ 1. Yes", "  2. No", "", "Esc to cancel · Tab to amend"];
        assert_eq!(turn_start(&ask, None), 0);
        assert_eq!(ks(claude_keys("claude", &ask.join("\n"), true)), ["1", "esc"]);
        assert_eq!(ks(claude_keys("claude", "│ ❯ 1. Yes\n│   2. Yes, and don't ask again\n│   3. No, and tell Claude (esc)", true)), ["1", "2", "esc"]);
        // nothing read = no button: never guess a number or send Esc into a dialog this build doesn't know (rule 9 BRD 11)
        assert!(claude_keys("claude", "no dialog read", true).is_empty());
        assert_eq!(ks(claude_keys("claude", "1. a\n2. b\n3. c\n4. d\n❯ 1. Yes\n  2. No\nEsc to cancel", true)), ["1", "esc"]);
        // FR-84: a folder-trust dialog gets no buttons (answered on the Mac); a permission dialog keeps them
        let trust = " Quick safety check: Is this a project you created or one you trust?\n ❯ No, exit\n   Yes, I trust this folder\n Enter to confirm · Esc to cancel";
        assert!(blocked_keys("claude", trust, claude::TRUST, true).is_empty());
        assert!(blocked_keys("codex", "› 1. Trust and continue\n  2. Back\n  enter continue · esc back", claude::TRUST, true).is_empty());
        assert_eq!(ks(blocked_keys("claude", &ask.join("\n"), "minta izin", true)), ["1", "esc"]);
        assert_eq!(choice("❯ halo"), None);
        assert_eq!(choice("2026. tahun"), None);
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
    fn codex_screen_ke_hp_tanpa_footer() {
        let screen = [
            "› Jujur gw gatau lu butuh apa",
            "• Viewed image b51d474b.jpg",
            "  + Show details",
            "• Lu nggak perlu ngapa-ngapain.",
            "  Worked for 12s • 12:34",
            "⚠ 5h limit: 18% left · resets at 16:20 · /status",
            "› Ask Codex to do anything",
            "GPT-6-Sol high · Context 81% left · 258K window · weekly 53% left",
            "  ← for agents · ? for shortcuts",
            "⚠ 3 warnings · f2 to view",
        ];
        let end = codex_above_input(&screen);
        assert_eq!(end, 6);
        let kept = screen[..end].iter().copied().filter(|r| !codex_chrome_row(r, Some("codex"))).collect::<Vec<_>>().join("\n");
        assert_eq!(snippet(&kept, 30), "› Jujur gw gatau lu butuh apa\n• Viewed image b51d474b.jpg\n• Lu nggak perlu ngapa-ngapain.");
        assert!(codex_chrome_row("⚠ Heads up, you have less than 25% of your 5h limit left. Run /status for a breakdown.", Some("codex")));
        assert!(!codex_chrome_row("  Worked for 12s • 12:34", Some("claude")));
    }

    #[test]
    fn buka_agent_dari_daftar_tetap() {
        let l = link();
        let n = 1000;
        assert_eq!(parse(&msg(42, 7, "/buka claude shell-11", n), Some(&l), n), In::Open { agent: "claude".into(), pane: Some("shell-11".into()) });
        assert_eq!(parse(&msg(42, 7, "/open omp", n), Some(&l), n), In::Open { agent: "omp".into(), pane: None });
        assert_eq!(parse(&msg(42, 7, "/buka", n), Some(&l), n), In::Help);
        let q = json!({"callback_query": {"id": "c1", "from": {"id": 7}, "data": "o|11|claude-kantor", "message": {"message_id": 9, "chat": {"id": 42}}}});
        assert_eq!(parse(&q, Some(&l), 0), In::OpenPick { pane: 11, agent: "claude-kantor".into(), cb: "c1".into(), msg: 9 });
        let q = json!({"callback_query": {"id": "c1", "from": {"id": 7}, "data": "w|ap2mf4uo|claude", "message": {"message_id": 9, "chat": {"id": 42}}}});
        assert_eq!(parse(&q, Some(&l), 0), In::OpenWs { ws: "ap2mf4uo".into(), agent: "claude".into(), cb: "c1".into(), msg: 9 });
        assert_eq!(openable(&[]), ["claude", "codex", "omp", "opencode"]);
        assert_eq!(openable(&["kantor".into()]), ["claude", "codex", "omp", "opencode", "claude-utama", "claude-kantor"]);
        // free text is never an agent: "rm -rf ~" isn't in the list, so nothing is typed
        assert!(!openable(&[]).contains(&"rm".to_string()));
    }

    #[test]
    fn status_ringkas_nama_workspace() {
        let rows = vec![
            ("Issue Prod".to_string(), vec![], 3),
            ("gembala".to_string(), vec![(true, "shell-25".to_string(), "idle ← active".to_string()), (false, "omp-1".to_string(), "needs permission (1m 5s)".to_string())], 4),
        ];
        assert_eq!(status_body(rows, false), "<b>gembala</b>\nomp-1 — needs permission (1m 5s)\nshell-25 — idle ← active\n4 shell\n\n<b>Issue Prod</b>\n3 shell");
        assert_eq!(status_body(vec![], true), "Belum ada pane.");
    }

    /// FR-99 P2: the bar above the pane reads the same dialog as the bot: same ask line, same buttons and words. Folder
    /// trust and an agent whose dialog was never recorded offer no button, so the app shows "Buka buat jawab" instead.
    #[test]
    fn dialog_view_untuk_bar_izin() {
        let dlg = |f: &str| std::fs::read_to_string(format!("{}/tests/dialogs/{f}.txt", env!("CARGO_MANIFEST_DIR"))).unwrap();
        let bash = dlg("claude-2.1.311-bash-k3");
        let v = dialog_view("claude", &bash, "minta izin", true);
        assert_eq!(v["ask"], "Bash command · touch ran.txt");
        assert_eq!(v["trust"], false);
        let keys: Vec<&str> = v["options"].as_array().unwrap().iter().map(|o| o["key"].as_str().unwrap()).collect();
        let labels: Vec<&str> = v["options"].as_array().unwrap().iter().map(|o| o["label"].as_str().unwrap()).collect();
        assert_eq!(keys, ["1", "2", "esc"]);
        assert_eq!(labels, ["1. Izinkan", "2. Izinkan terus", "Tolak"]);
        // the same screen in English: the buttons follow the app's language, the ask line stays the agent's own words
        let en = dialog_view("claude", &bash, "", false);
        assert_eq!(en["options"][0]["label"], "1. Allow");
        assert_eq!(en["ask"], v["ask"]);
        // static-key dialogs also require the recorded screen shape; another blocked screen cannot inherit their buttons
        let omp = dlg("omp-18.18.3-bash-once.dialog");
        assert_eq!(dialog_view("omp", &omp, "", true)["options"].as_array().unwrap().len(), 2);
        assert_eq!(dialog_view("omp", "waiting for input", "", true)["options"].as_array().unwrap().len(), 0);
        let opencode = dlg("opencode-1.19.5-outside-once.dialog");
        assert_eq!(dialog_view("opencode", &opencode, "", true)["options"].as_array().unwrap().len(), 3);
        assert_eq!(dialog_view("opencode", "waiting for input", "", true)["options"].as_array().unwrap().len(), 0);
        // trust: no buttons even with a numbered list on screen
        let trust = " Quick safety check: Is this a project you trust?\n ❯ 1. No, exit\n  2. Yes, I trust this folder\n Esc to cancel";
        assert_eq!(dialog_view("claude", trust, claude::TRUST, true)["options"].as_array().unwrap().len(), 0);
        assert_eq!(dialog_view("claude", trust, claude::TRUST, true)["trust"], true);
        // Gemini: the dialog was never recorded, so nothing is offered
        assert_eq!(dialog_view("gemini", "Allow execution of: 'pytest -q'?\n ● Yes, allow once", "", true)["options"].as_array().unwrap().len(), 0);
        // an answer dialog: the question sits above the numbered options
        let q = " Which database should store login sessions?\n\n ❯ 1. Postgres   already used in staging\n   2. SQLite     simpler for local\n   3. Type something";
        let a = dialog_view("claude", q, "nunggu jawaban", true);
        assert_eq!(a["question"], "Which database should store login sessions?");
        // the options are answers, not permission grants: their own words stay (only the spacing is cleaned up)
        assert_eq!(a["options"][1]["label"], "2. SQLite simpler for local");
    }

    /// FR-99 P2: the answer box writes what the user typed, as if typed at the keyboard (bracketed paste + Enter).
    #[test]
    fn jawaban_bebas_dikirim_seperti_diketik() {
        let bytes = msg::paste("pakai SQLite");
        assert_eq!(bytes, b"\x1b[200~pakai SQLite\x1b[201~\r");
    }

    #[test]
    fn perkiraan_waktu_dari_giliran_sebelumnya() {
        let q: VecDeque<u64> = [40, 100, 70].into();
        assert_eq!(median(Some(&q)), Some(70));
        assert_eq!(median(None), None);
        assert_eq!(median(Some(&VecDeque::new())), None);
        assert_eq!(progress_text(true, "omp-1", 0, Some(70), false), "⏳ omp-1 lagi ngerjain · biasanya kelar ±1m 10s");
        assert_eq!(progress_text(true, "omp-1", 90, Some(70), false), "⏳ omp-1 · udah jalan 1m 30s · biasanya kelar ±1m 10s");
        assert_eq!(progress_text(true, "omp-1", 120, Some(70), false), "⏳ omp-1 · udah jalan 2m · lebih lama dari biasanya (±1m 10s)");
        assert_eq!(progress_text(false, "a<b", 0, None, true), "⏳ a&lt;b is working · no estimate yet, first turn\nYour message is queued behind the turn in progress.");
    }

    #[test]
    fn idle_dan_kunci_dari_ioreg() {
        assert_eq!(idle_secs("    | |     \"HIDIdleTime\" = 74107037708\n"), Some(74));
        assert_eq!(idle_secs("nothing"), None);
        assert!(locked(r#""IOConsoleUsers" = ({"CGSSessionScreenIsLocked"=Yes,"kCGSSessionOnConsoleKey"=Yes})"#));
        assert!(!locked(r#""IOConsoleUsers" = ({"kCGSSessionOnConsoleKey"=Yes})"#));
    }

    #[test]
    fn password_hint_says_to_answer_on_mac_in_both_languages() {
        assert_eq!(t(true, "password_hint"), "Jawab di Mac; password nggak dikirim lewat HP.");
        assert_eq!(t(false, "password_hint"), "Answer it on the Mac; passwords never go through the phone.");
        for id_l in [true, false] {
            assert!(!t(id_l, "password").contains('\n'));
        }
        assert!(matches!(typed(false, "working", true, "secret"), Err(Why::Shell)));
        assert!(matches!(typed(true, "working", true, "secret"), Err(Why::Password)));
    }

    #[test]
    fn baris_pertama_tanpa_isi_layar() {
        assert_eq!(head("shell-6", "gembala", "butuh izin"), "<b>shell-6 · gembala</b> butuh izin");
        assert_eq!(head("a<b", "", "kelar"), "<b>a&lt;b</b> kelar");
        let v = json!({"telegram": {"snippet": true, "snippet_off": ["E2E Phase 1"]}});
        assert!(snippet_on(&v, &["gembala", "ap2mf4uo"]));
        assert!(!snippet_on(&v, &["E2E Phase 1", "q4xovss2"]));
        assert!(!snippet_on(&json!({"telegram": {"snippet_off": ["q4xovss2"]}}), &["E2E Phase 1", "q4xovss2"]));
        assert!(!snippet_on(&json!({"telegram": {"snippet": false}}), &["gembala"]));
        assert_eq!(dur(130), "2m 10s");
    }
}
