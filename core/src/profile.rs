//! Profil agent (FR-18, 29, 30, 32, 33; SEC-32) dan deteksi agent terpasang (FR-28, 31).
//! Perintah launch disusun di sini sebagai teks shell yang semua bagian pengguna di-quote.

use serde_json::{Value, json};
use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[derive(Clone, Debug, PartialEq)]
pub struct Profile {
    pub id: String,
    pub name: String,
    pub agent: String,
    /// Kosong = shell login interaktif.
    pub command: String,
    pub args: Vec<String>,
    /// Kosong = ikut pane yang fokus (FR-30). `~` diganti `$HOME`.
    pub cwd: String,
    pub env: Vec<(String, String)>,
    /// Rujukan saja: `op://…` atau `keychain:service/akun` (SEC-32).
    pub secret_env: Vec<(String, String)>,
    /// FR-56: ringkas output saat agent keluar (bawaan nyala).
    pub summarize: bool,
    /// FR-40: notifikasi untuk pane dari profil ini (bawaan nyala).
    pub notify: bool,
    /// SEC-5: kebijakan penerima `auto` | `confirm` | `deny`
    pub policy: String,
    /// SEC-3 cadangan: beri `GEMBALA_TOKEN` (mis. agent yang jalan di dalam tmux)
    pub token: bool,
}

/// (agent, nama, perintah, perintah instal yang ditampilkan bila tidak ada, versi teruji major.minor)
/// Perintah instal dicek dari cara pasang di mesin pengembang 1 Nov 2026; codex belum (TECH bagian 7).
const AGENTS: &[(&str, &str, &str, &str, &str)] = &[
    ("claude", "Claude Code", "claude", "brew install --cask claude-code", "2.1"),
    ("codex", "Codex", "codex", "npm install -g @openai/codex", "0.149"),
    ("opencode", "OpenCode", "opencode", "brew install anomalyco/tap/opencode", "1.18"),
    ("omp", "Oh My Pi", "omp", "brew install can1357/tap/omp", "18.13"),
];

pub fn builtin() -> Vec<Profile> {
    let shell = Profile {
        id: "shell".into(),
        name: "Shell".into(),
        agent: "shell".into(),
        command: String::new(),
        args: vec![],
        cwd: String::new(),
        env: vec![],
        secret_env: vec![],
        summarize: true,
        notify: true,
        policy: "auto".into(),
        token: false,
    };
    let agents = AGENTS.iter().map(|(agent, name, cmd, _, _)| Profile {
        id: agent.to_string(),
        name: name.to_string(),
        agent: agent.to_string(),
        command: cmd.to_string(),
        ..shell.clone()
    });
    std::iter::once(shell.clone()).chain(agents).collect()
}

/// SEC-12/SEC-32: nilai yang tampak seperti kunci rahasia.
pub const SECRET: &[&str] = &["sk-ant-", "sk-", "ghp_", "github_pat_", "AKIA", "PRIVATE KEY", "xoxb-", "xoxp-"];

/// Posisi `pat` yang tidak menempel di akhir kata lain (`task-list` bukan `sk-`).
pub fn word_start(s: &str, pat: &str) -> Option<usize> {
    s.match_indices(pat).map(|(i, _)| i).find(|&i| !s[..i].chars().next_back().is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '-'))
}

fn ok_text(s: &str, max: usize) -> bool {
    s.len() <= max && !s.chars().any(char::is_control)
}

fn ok_env_name(s: &str) -> bool {
    !s.is_empty() && s.len() <= 64 && s.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_') && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn ok_ref(s: &str) -> bool {
    let chars = |t: &str, extra: &str| !t.is_empty() && t.chars().all(|c| c.is_ascii_alphanumeric() || " ._-".contains(c) || extra.contains(c));
    if let Some(r) = s.strip_prefix("op://") {
        return chars(r, "/") && r.split('/').count() >= 3;
    }
    match s.strip_prefix("keychain:").and_then(|r| r.split_once('/')) {
        Some((svc, acct)) => chars(svc, "") && chars(acct, "@"),
        None => false,
    }
}

/// `[[profile]]` dari config: id sama dengan bawaan = menimpa bagian yang diisi; id baru = profil tambahan.
pub fn from_config(tables: &[&dyn toml_edit::TableLike]) -> (Vec<Profile>, Vec<String>) {
    let mut out = builtin();
    let mut errors = Vec::new();
    for (i, t) in tables.iter().enumerate() {
        let at = |k: &str| format!("profile[{i}].{k}");
        let Some(id) = t.get("id").and_then(|v| v.as_str()) else {
            errors.push(format!("{}: wajib diisi, profil dilewati", at("id")));
            continue;
        };
        if !(1..=32).contains(&id.len()) || !id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') {
            errors.push(format!("{}: hanya a-z, 0-9, '-', maks 32; profil dilewati", at("id")));
            continue;
        }
        let pos = out.iter().position(|p| p.id == id).unwrap_or_else(|| {
            out.push(Profile { id: id.into(), name: id.into(), agent: "custom".into(), ..builtin()[0].clone() });
            out.len() - 1
        });
        let p = &mut out[pos];
        for (key, field) in [("name", &mut p.name), ("command", &mut p.command), ("cwd", &mut p.cwd)] {
            match t.get(key).map(|v| v.as_str()) {
                None => {}
                Some(Some(s)) if ok_text(s, 200) && (key != "cwd" || s.is_empty() || s.starts_with('/') || s.starts_with('~')) => *field = s.into(),
                Some(_) => errors.push(format!("{}: teks tidak sah, memakai bawaan", at(key))),
            }
        }
        for (key, field) in [("summarize", &mut p.summarize), ("notify", &mut p.notify), ("token", &mut p.token)] {
            match t.get(key).map(|v| v.as_bool()) {
                None => {}
                Some(Some(b)) => *field = b,
                Some(None) => errors.push(format!("{}: harus true atau false", at(key))),
            }
        }
        match t.get("policy").map(|v| v.as_str()) {
            None => {}
            Some(Some(v)) if crate::msg::Policy::parse(v).is_some() => p.policy = v.into(),
            Some(_) => errors.push(format!("{}: harus auto, confirm, atau deny", at("policy"))),
        }
        if let Some(a) = t.get("args") {
            match a.as_array().map(|arr| arr.iter().map(|v| v.as_str().filter(|s| ok_text(s, 500)).map(String::from)).collect::<Option<Vec<_>>>()) {
                Some(Some(v)) => p.args = v,
                _ => errors.push(format!("{}: harus daftar teks, memakai bawaan", at("args"))),
            }
        }
        for (key, secret) in [("env", false), ("secret_env", true)] {
            let Some(tbl) = t.get(key).and_then(|v| v.as_table_like()) else { continue };
            let mut list = Vec::new();
            for (k, v) in tbl.iter() {
                let v = v.as_str().unwrap_or_default();
                if !ok_env_name(k) {
                    errors.push(format!("{}.{k}: nama env tidak sah", at(key)));
                } else if secret && !ok_ref(v) {
                    errors.push(format!("{}.{k}: harus rujukan op://vault/item/field atau keychain:service/akun", at(key)));
                } else if !secret && (!ok_text(v, 1000) || SECRET.iter().any(|s| word_start(v, s).is_some())) {
                    errors.push(format!("{}.{k}: nilai tampak seperti rahasia; simpan rujukan di secret_env", at(key)));
                } else {
                    list.push((k.to_string(), v.to_string()));
                }
            }
            if secret { p.secret_env = list } else { p.env = list }
        }
    }
    (out, errors)
}

/// Kutip tunggal untuk shell POSIX.
fn q(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Lokasi binary `gembala` dan folder file hook milik Gembala (SEC-26: config agent tidak disentuh).
pub struct Hooks {
    pub exe: String,
    pub dir: std::path::PathBuf,
}

/// FR-4: event hook agent → status pane. `None` = event tidak mengubah status.
/// `payload` = stdin (Claude) atau argumen terakhir (Codex); hanya dipakai untuk membedakan "butuh izin",
/// isinya tidak pernah diteruskan ke daemon (SEC-33).
pub fn hook_status(agent: &str, event: &str, payload: &str) -> Option<&'static str> {
    Some(match (agent, event) {
        ("claude", "prompt" | "tool") => "working",
        ("claude", "stop") => "done",
        // Notification Claude Code: izin alat = blocked; "menunggu input" = siap menerima tugas
        ("claude", "notify") if payload.to_lowercase().contains("permission") => "blocked",
        ("claude", "notify") => "idle",
        ("codex", "notify") if payload.contains("agent-turn-complete") => "done",
        ("opencode", "busy") | ("omp", "start") => "working",
        ("opencode", "idle") | ("omp", "end") => "done",
        ("opencode", "permission") => "blocked",
        _ => return None,
    })
}

/// Teks di layar yang berarti agent menunggu jawaban manusia (huruf kecil).
const PROMPTS: &[&str] = &["(y/n)", "[y/n]", "(yes/no)", "[yes/no]", "do you want to", "approve", "allow once", "permission to"];

/// FR-4 cadangan: status dari layar untuk agent tanpa hook. `tail` = beberapa baris terbawah layar,
/// `quiet` = lama tanpa output, `prev` = status sebelumnya.
pub fn screen_status(tail: &str, quiet: std::time::Duration, prev: &str) -> &'static str {
    let t = tail.to_lowercase();
    if PROMPTS.iter().any(|p| t.contains(p)) {
        "blocked"
    } else if quiet < std::time::Duration::from_millis(1500) {
        "working"
    } else if matches!(prev, "working" | "done" | "blocked") {
        "done"
    } else {
        "idle"
    }
}

/// Petunjuk perintah `gembala` untuk agent dari profil, lewat flag resmi `--append-system-prompt` (Claude, OMP).
/// Config agent tidak disentuh; agent yang diketik manual memakai paragraf di file instruksi global pengguna.
pub const PETUNJUK: &str = "You run in a Gembala pane. Reach other agents in the same workspace with `gembala` (on PATH, \
JSON output): `gembala status` = list panes; `gembala send <pane> <message>` = send, wait for the reply with `gembala wait --reply <id>`; \
a message labeled `[Gembala · dari <pane> · <id>]` is answered with `gembala reply <id> <text>` when done, in 1-2 sentences: what was executed and how (files, commit, \
test result), longer only if the sender asked; label `[Gembala · dari pengguna]` = the user, \
answer on screen. The recipient cannot see your conversation: write every send as a full handover (goal, context and user decisions, \
files/paths, what was tried or is forbidden, constraints, reply format), never a one-line summary; multi-line is fine. \
User says lempar/kirim/tanya ke another agent = `gembala status`, then `gembala send`. Do not look for processes via ps.";

/// Flag/env hook per agent saat launch (TECH bagian 7, teruji 31 Okt 2026).
fn hook_flags(p: &Profile, h: &Hooks) -> (Vec<String>, Vec<(String, String)>) {
    let cmd = |ev: &str| format!("{} hook claude {ev}", q(&h.exe));
    let file = |name: &str| h.dir.join(name).to_string_lossy().into_owned();
    match p.agent.as_str() {
        "claude" => {
            let one = |ev: &str| json!([{"hooks": [{"type": "command", "command": cmd(ev)}]}]);
            let settings = json!({"hooks": {
                "UserPromptSubmit": one("prompt"), "PreToolUse": one("tool"), "Stop": one("stop"), "Notification": one("notify"),
            }});
            (vec!["--settings".into(), settings.to_string(), "--append-system-prompt".into(), PETUNJUK.into()], vec![])
        }
        // ponytail: developer_instructions milik config pengguna tertimpa; gabungkan kalau ada yang memakainya
        "codex" => (
            vec![
                "-c".into(),
                format!("notify={}", json!([h.exe, "hook", "codex", "notify"])),
                "-c".into(),
                format!("developer_instructions={}", json!(PETUNJUK)),
            ],
            vec![],
        ),
        "opencode" => (vec![], vec![("OPENCODE_CONFIG".into(), file("opencode.json"))]),
        "omp" => (vec![format!("--hook={}", file("omp-gembala.ts")), format!("--append-system-prompt={PETUNJUK}")], vec![]),
        _ => (vec![], vec![]),
    }
}

/// FR-58: folder hook dipakai sebagai `ZDOTDIR` pane zsh. Tiap file membaca file pengguna yang sama apa adanya
/// (dengan `ZDOTDIR` pengguna selama dibaca), `.zshrc` menambah penanda blok OSC 133, `.zlogin` mengembalikan
/// `ZDOTDIR` supaya agent dan subshell tidak ikut. File pengguna tidak diubah.
fn zsh_file(name: &str, extra: &str) -> String {
    format!(
        "# dibuat Gembala (FR-58); {name} pengguna dibaca apa adanya\n\
         GEMBALA_ZDOTDIR=$ZDOTDIR\nZDOTDIR=${{GEMBALA_USER_ZDOTDIR:-$HOME}}\n\
         [[ -f \"$ZDOTDIR/{name}\" ]] && builtin source \"$ZDOTDIR/{name}\"\n\
         GEMBALA_USER_ZDOTDIR=$ZDOTDIR\nZDOTDIR=$GEMBALA_ZDOTDIR\n{extra}"
    )
}

const ZSH_MARKS: &str = r#"__gembala_precmd() {
  local s=$?
  builtin print -n "\e]133;D;$s\a\e]133;A\a"
  if [[ -n $GEMBALA_EDITOR ]]; then
    PS1="%{"$'\e]133;B\a'"%}" RPROMPT=  # FR-59: editor Gembala menggantikan prompt
    (( ${#zle_highlight} )) || zle_highlight=(default:bold)  # perintah tebal = kepala blok (ala Warp)
  else
    [[ $PS1 == *'133;B'* ]] || PS1="$PS1%{"$'\e]133;B\a'"%}"
  fi
}
__gembala_preexec() { builtin print -rn -- $'\e]133;C;'"$(builtin print -rn -- "$1" | /usr/bin/base64)"$'\a'; }
autoload -Uz add-zsh-hook && add-zsh-hook precmd __gembala_precmd && add-zsh-hook preexec __gembala_preexec
"#;

/// FR-9: agent yang diketik manual di pane juga menerima petunjuk `gembala` (flag yang sama dengan profil).
/// `function name` (bukan `name()`) supaya alias pengguna bernama sama tidak merusak definisi; alias tetap jalan
/// lewat fungsi ini. Claude/OMP hanya saat membuka TUI (tanpa argumen atau argumen pertama opsi), bukan subperintah.
/// ponytail: `claude "prompt"` positional tidak diberi petunjuk; OpenCode manual belum.
fn zsh_agents(petunjuk: &str) -> String {
    let f = q(petunjuk);
    format!(
        "function claude {{ if (( ! $# )) || [[ $1 == -* ]]; then command claude --append-system-prompt \"$(<{f})\" \"$@\"; else command claude \"$@\"; fi }}\n\
         function omp {{ if (( ! $# )) || [[ $1 == -* ]]; then command omp \"--append-system-prompt=$(<{f})\" \"$@\"; else command omp \"$@\"; fi }}\n\
         function codex {{ command codex -c \"$(<{f}.codex)\" \"$@\" }}\n"
    )
}

const ZSH_RESTORE: &str = "ZDOTDIR=$GEMBALA_USER_ZDOTDIR\n[[ -f \"$ZDOTDIR/.zlogin\" ]] && builtin source \"$ZDOTDIR/.zlogin\"\n\
if [[ $ZDOTDIR == $HOME ]]; then unset ZDOTDIR; else export ZDOTDIR; fi\nunset GEMBALA_USER_ZDOTDIR GEMBALA_ZDOTDIR\n";

/// File hook OpenCode dan Oh My Pi, plus file zsh blok perintah, ditulis daemon saat start ke folder config Gembala (`0700`/`0600`).
pub fn hook_files(h: &Hooks) -> Vec<(&'static str, String)> {
    let exe = json!(h.exe);
    let plugin = h.dir.join("opencode-gembala.js").to_string_lossy().into_owned();
    vec![
        (".zshenv", zsh_file(".zshenv", "")),
        (".zprofile", zsh_file(".zprofile", "")),
        (".zshrc", zsh_file(".zshrc", &format!("{ZSH_MARKS}{}", zsh_agents(&h.dir.join("gembala.md").to_string_lossy())))),
        (".zlogin", format!("# dibuat Gembala (FR-58); .zlogin pengguna dibaca apa adanya, lalu ZDOTDIR dikembalikan\n{ZSH_RESTORE}")),
        (
            "opencode.json",
            json!({"$schema": "https://opencode.ai/config.json", "plugin": [format!("file://{plugin}")],
                "instructions": [h.dir.join("gembala.md").to_string_lossy()]})
            .to_string(),
        ),
        ("gembala.md", format!("{PETUNJUK}\n")),
        // nilai `-c` Codex untuk agent manual (string TOML = string JSON tanpa karakter khusus)
        ("gembala.md.codex", format!("developer_instructions={}", json!(PETUNJUK))),
        (
            "opencode-gembala.js",
            format!(
                "// dibuat Gembala; status pane (FR-4)\nimport {{ execFile }} from \"node:child_process\";\n\
                 const hook = (s) => execFile({exe}, [\"hook\", \"opencode\", s], () => {{}});\n\
                 export const Gembala = async () => ({{\n  event: async ({{ event }}) => {{\n\
                 \x20   if (event.type === \"session.status\") hook(event.properties?.status?.type === \"busy\" ? \"busy\" : \"idle\");\n\
                 \x20   else if (event.type === \"session.idle\") hook(\"idle\");\n\
                 \x20   else if (event.type.startsWith(\"permission.\")) hook(\"permission\");\n  }},\n}});\n"
            ),
        ),
        (
            "omp-gembala.ts",
            format!(
                "// dibuat Gembala; status pane (FR-4)\nimport {{ execFile }} from \"node:child_process\";\n\
                 const hook = (s: string) => execFile({exe}, [\"hook\", \"omp\", s], () => {{}});\n\
                 export default function (pi: any) {{\n  pi.on(\"agent_start\", async () => hook(\"start\"));\n\
                 \x20 pi.on(\"agent_end\", async () => hook(\"end\"));\n}}\n"
            ),
        ),
    ]
}

/// FR-53: id sesi dari Gembala (UUID) dan apakah pane melanjutkan sesi sebelumnya.
pub struct Session {
    pub id: Option<String>,
    pub resume: bool,
}

pub fn valid_uuid(s: &str) -> bool {
    s.len() == 36 && s.chars().enumerate().all(|(i, c)| if [8, 13, 18, 23].contains(&i) { c == '-' } else { c.is_ascii_hexdigit() })
}

/// Claude Code menerima id dari luar; agent lain hanya "lanjutkan terakhir" (TECH bagian 7a, `--help` 1 Nov 2026).
fn session_flags(agent: &str, s: &Session) -> Vec<String> {
    let id = s.id.as_deref().filter(|i| valid_uuid(i));
    match (agent, s.resume, id) {
        ("claude", false, Some(id)) => vec!["--session-id".into(), id.into()],
        ("claude", true, Some(id)) => vec!["--resume".into(), id.into()],
        ("claude", true, None) | ("opencode" | "omp", true, _) => vec!["--continue".into()],
        ("codex", true, _) => vec!["resume".into(), "--last".into()],
        _ => vec![],
    }
}

/// Teks yang dijalankan `$SHELL -l -c` (TECH bagian 6). `None` = shell login interaktif.
/// Agent jalan di subshell supaya env rahasia tidak tertinggal di shell pane setelah agent keluar;
/// lalu `exec $SHELL -l` membuat pane tetap hidup (FR-18). Exit 127 = pesan instal (FR-32).
pub fn launch(p: &Profile, h: Option<&Hooks>, session: Option<&Session>) -> Option<String> {
    if p.command.is_empty() {
        return None;
    }
    let (mut flags, hook_env) = h.map(|h| hook_flags(p, h)).unwrap_or_default();
    flags.extend(session.map(|s| session_flags(&p.agent, s)).unwrap_or_default());
    // FR-56: penanda tak terlihat untuk daemon (OSC 777), mengapit output sesi agent
    let mut s = String::from(if p.summarize { "printf '\\033]777;gembala;agent-start\\007'; ( " } else { "( " });
    for (k, v) in p.env.iter().chain(&hook_env) {
        s += &format!("export {k}={}; ", q(v));
    }
    for (k, r) in &p.secret_env {
        // rahasia dibuka shell pane; nilainya tidak pernah lewat daemon (SEC-32)
        if let Some((svc, acct)) = r.strip_prefix("keychain:").and_then(|x| x.split_once('/')) {
            s += &format!("export {k}=\"$(security find-generic-password -w -s {} -a {})\"; ", q(svc), q(acct));
        } else {
            s += &format!("export {k}=\"$(op read {})\"; ", q(r));
        }
    }
    s += &q(&p.command);
    for a in flags.iter().chain(&p.args) {
        s += &format!(" {}", q(a));
    }
    let install = AGENTS.iter().find(|a| a.0 == p.agent).map(|a| a.3);
    let hint = match install {
        Some(i) => format!("{} tidak ditemukan di PATH login shell. Pasang dengan: {i}", p.name),
        None => format!("Perintah {} tidak ditemukan di PATH login shell.", p.command),
    };
    s += " ); s=$?; ";
    if p.summarize {
        s += "printf '\\033]777;gembala;agent-exit;%d\\007' \"$s\"; ";
    }
    s += &format!("[ \"$s\" -eq 127 ] && printf '\\n%s\\n' {}; exec \"$SHELL\" -l", q(&hint));
    Some(s)
}

pub fn to_json(p: &Profile) -> Value {
    json!({
        "id": p.id, "name": p.name, "agent": p.agent, "command": p.command, "args": p.args, "cwd": p.cwd, "summarize": p.summarize, "notify": p.notify, "policy": p.policy, "token": p.token,
        "env": p.env.iter().map(|(k, _)| k).collect::<Vec<_>>(),
        "secret_env": p.secret_env.iter().map(|(k, r)| json!({"name": k, "ref": r})).collect::<Vec<_>>(),
    })
}

/// FR-28: cari agent lewat login shell (PATH sama dengan terminal biasa), batas 10 detik.
pub fn detect() -> Value {
    let names: Vec<&str> = AGENTS.iter().map(|a| a.0).collect();
    let script = format!(
        "for a in {}; do p=$(command -v \"$a\") && printf '%s\\t%s\\t%s\\n' \"$a\" \"$p\" \"$(\"$a\" --version 2>/dev/null | head -1)\"; done",
        names.join(" ")
    );
    let mut c = Command::new(crate::pty::login_shell());
    c.args(["-l", "-c", &script]).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
    crate::pty::scrub_env(&mut c);
    if let Some(home) = std::env::var_os("HOME") {
        c.current_dir(home);
    }
    let found = run_with_timeout(c, Duration::from_secs(10)).unwrap_or_default();
    let rows = AGENTS
        .iter()
        .map(|(agent, name, _, install, tested)| {
            let line = found.lines().find(|l| l.split('\t').next() == Some(agent));
            let mut f = line.map(|l| l.split('\t')).into_iter().flatten().skip(1);
            let (path, raw) = (f.next(), f.next().unwrap_or_default());
            let version = version_of(raw);
            json!({
                "agent": agent, "name": name, "installed": path.is_some(), "path": path, "version": version,
                "tested": version.as_deref().is_some_and(|v| v == *tested || v.starts_with(&format!("{tested}."))),
                "install": install,
            })
        })
        .collect::<Vec<_>>();
    Value::Array(rows)
}

fn version_of(raw: &str) -> Option<String> {
    raw.split(|c: char| !(c.is_ascii_digit() || c == '.'))
        .find(|t| t.split('.').filter(|p| !p.is_empty()).count() >= 2)
        .map(|t| t.trim_matches('.').to_string())
}

pub fn run_with_timeout(mut c: Command, limit: Duration) -> Option<String> {
    let mut child = c.spawn().ok()?;
    let t0 = Instant::now();
    while child.try_wait().ok()?.is_none() {
        if t0.elapsed() > limit {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let mut out = String::new();
    child.stdout.take()?.read_to_string(&mut out).ok()?;
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> (Vec<Profile>, Vec<String>) {
        let doc: toml_edit::DocumentMut = text.parse().unwrap();
        let arr = doc.get("profile").and_then(|v| v.as_array_of_tables());
        let tables: Vec<&dyn toml_edit::TableLike> = arr.map(|a| a.iter().map(|t| t as &dyn toml_edit::TableLike).collect()).unwrap_or_default();
        from_config(&tables)
    }

    #[test]
    fn bawaan_dan_timpa() {
        let (ps, errs) = parse(
            "[[profile]]\nid = \"claude\"\nargs = [\"--model\", \"opus\"]\n[profile.env]\nCLAUDE_CONFIG_DIR = \"~/.claude\"\n\
             [[profile]]\nid = \"lazygit\"\ncommand = \"lazygit\"\n",
        );
        assert!(errs.is_empty(), "{errs:?}");
        let claude = ps.iter().find(|p| p.id == "claude").unwrap();
        assert_eq!(claude.command, "claude");
        assert_eq!(claude.args, ["--model", "opus"]);
        assert!(ps.iter().any(|p| p.id == "lazygit" && p.agent == "custom"));
        assert_eq!(ps.iter().filter(|p| p.id == "shell").count(), 1);
    }

    #[test]
    fn sec32_rahasia_ditolak_rujukan_diterima() {
        let (ps, errs) = parse(
            "[[profile]]\nid = \"claude\"\n[profile.env]\nANTHROPIC_API_KEY = \"sk-ant-abc\"\nOK = \"biasa --disk-cache\"\n\
             [profile.secret_env]\nA = \"op://Private/Anthropic/credential\"\nB = \"keychain:anthropic/aufa@x.id\"\nC = \"op://x\"\nD = \"$(rm -rf /)\"\n",
        );
        let p = ps.iter().find(|p| p.id == "claude").unwrap();
        assert_eq!(p.env, [("OK".to_string(), "biasa --disk-cache".to_string())]);
        assert_eq!(p.secret_env.len(), 2);
        let all = errs.join("\n");
        for want in ["ANTHROPIC_API_KEY", "secret_env.C", "secret_env.D"] {
            assert!(all.contains(want), "{want}: {all}");
        }
    }

    #[test]
    fn launch_mengutip_semua_bagian() {
        let p = Profile {
            command: "my tool".into(),
            args: vec!["it's".into(), "$(id)".into()],
            env: vec![("X".into(), "a b'c".into())],
            secret_env: vec![("K".into(), "keychain:svc/me@x".into()), ("O".into(), "op://V/I/f".into())],
            ..builtin()[1].clone()
        };
        let s = launch(&p, None, None).unwrap();
        assert!(s.starts_with("printf '\\033]777;gembala;agent-start\\007'; ( export X='a b'\\''c'; "), "{s}");
        assert!(s.contains("printf '\\033]777;gembala;agent-exit;%d\\007' \"$s\""), "{s}");
        let quiet = Profile { summarize: false, ..p.clone() };
        assert!(!launch(&quiet, None, None).unwrap().contains("777"));
        assert!(s.contains("export K=\"$(security find-generic-password -w -s 'svc' -a 'me@x')\"; "), "{s}");
        assert!(s.contains("export O=\"$(op read 'op://V/I/f')\"; "), "{s}");
        assert!(s.contains("'my tool' 'it'\\''s' '$(id)' );"), "{s}");
        assert!(s.ends_with("exec \"$SHELL\" -l"), "{s}");
        assert!(s.contains("brew install --cask claude-code"), "{s}");
        assert_eq!(launch(&builtin()[0], None, None), None);
    }

    #[test]
    fn launch_jalan_dan_pesan_127() {
        // perintah tidak ada: pesan instal tampil, lalu shell login (di sini `exit` lewat env SHELL palsu)
        let p = Profile { command: "gembala-tidak-ada-xyz".into(), ..builtin()[1].clone() };
        let out = Command::new("/bin/sh").env("SHELL", "/usr/bin/true").arg("-c").arg(launch(&p, None, None).unwrap()).output().unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(text.contains("Claude Code tidak ditemukan") && text.contains("brew install"), "{text}");
    }

    #[test]
    fn hook_dipasang_per_agent() {
        let h = Hooks { exe: "/Apps/Gem bala/gembala".into(), dir: "/cfg/hooks".into() };
        let by = |id: &str| builtin().into_iter().find(|p| p.id == id).unwrap();
        let claude = launch(&by("claude"), Some(&h), None).unwrap();
        assert!(claude.contains("'claude' '--settings' '{\"hooks\":"), "{claude}");
        assert!(claude.contains("hook claude stop"), "{claude}");
        // petunjuk `gembala` lewat flag resmi; tanda kutip tunggal di dalamnya akan merusak kutipan shell
        assert!(!PETUNJUK.contains('\'') && claude.contains(&format!("'--append-system-prompt' '{PETUNJUK}'")), "{claude}");
        assert!(launch(&by("omp"), Some(&h), None).unwrap().contains(&format!("'--append-system-prompt={PETUNJUK}'")));
        let codex = launch(&by("codex"), Some(&h), None).unwrap();
        assert!(codex.contains("'-c' 'notify=[\"/Apps/Gem bala/gembala\",\"hook\",\"codex\",\"notify\"]'"), "{codex}");
        assert!(codex.contains(&format!("'-c' 'developer_instructions=\"{PETUNJUK}\"'")), "{codex}");
        let files = hook_files(&h);
        let oc: serde_json::Value = serde_json::from_str(&files.iter().find(|f| f.0 == "opencode.json").unwrap().1).unwrap();
        assert_eq!(oc["instructions"][0], "/cfg/hooks/gembala.md");
        assert!(files.iter().any(|f| f.0 == "gembala.md" && f.1.contains(PETUNJUK)));
        let rc = &files.iter().find(|f| f.0 == ".zshrc").unwrap().1;
        assert!(rc.contains("function claude {") && rc.contains("'/cfg/hooks/gembala.md'"), "{rc}");
        assert!(launch(&by("opencode"), Some(&h), None).unwrap().contains("export OPENCODE_CONFIG='/cfg/hooks/opencode.json'"));
        assert!(launch(&by("omp"), Some(&h), None).unwrap().contains("'--hook=/cfg/hooks/omp-gembala.ts'"));
        assert_eq!(launch(&by("shell"), Some(&h), None), None);
        // perintah hook Claude dijalankan shell: path berspasi tetap satu kata
        let settings: serde_json::Value = serde_json::from_str(hook_flags(&by("claude"), &h).0[1].as_str()).unwrap();
        assert_eq!(settings["hooks"]["Stop"][0]["hooks"][0]["command"], "'/Apps/Gem bala/gembala' hook claude stop");
    }

    #[test]
    fn sesi_per_agent() {
        let by = |id: &str| builtin().into_iter().find(|p| p.id == id).unwrap();
        let id = "0b6f2a1e-5c3d-4e7f-8a9b-1c2d3e4f5a6b";
        let new = Session { id: Some(id.into()), resume: false };
        let again = Session { id: Some(id.into()), resume: true };
        assert!(launch(&by("claude"), None, Some(&new)).unwrap().contains(&format!("'claude' '--session-id' '{id}'")));
        assert!(launch(&by("claude"), None, Some(&again)).unwrap().contains(&format!("'claude' '--resume' '{id}'")));
        assert!(launch(&by("codex"), None, Some(&again)).unwrap().contains("'codex' 'resume' '--last'"));
        assert!(launch(&by("opencode"), None, Some(&again)).unwrap().contains("'opencode' '--continue'"));
        let bad = Session { id: Some("x; rm -rf /".into()), resume: false };
        assert!(!launch(&by("claude"), None, Some(&bad)).unwrap().contains("rm"));
        assert!(valid_uuid(id) && !valid_uuid("0b6f2a1e"));
    }

    #[test]
    fn status_dari_event_hook() {
        assert_eq!(hook_status("claude", "prompt", ""), Some("working"));
        assert_eq!(hook_status("claude", "notify", r#"{"message":"Claude needs your permission to use Bash"}"#), Some("blocked"));
        assert_eq!(hook_status("claude", "notify", r#"{"message":"Claude is waiting for your input"}"#), Some("idle"));
        assert_eq!(hook_status("codex", "notify", r#"{"type":"agent-turn-complete"}"#), Some("done"));
        assert_eq!(hook_status("codex", "notify", r#"{"type":"lain"}"#), None);
        assert_eq!(hook_status("opencode", "permission", ""), Some("blocked"));
        assert_eq!(hook_status("omp", "end", ""), Some("done"));
        assert_eq!(hook_status("x", "y", ""), None);
    }

    #[test]
    fn status_dari_layar() {
        use std::time::Duration;
        let s = |t: &str, q: u64, p: &str| screen_status(t, Duration::from_millis(q), p);
        assert_eq!(s("Run rm -rf build? (y/N)", 5000, "working"), "blocked");
        assert_eq!(s("menulis file…", 200, "idle"), "working");
        assert_eq!(s("selesai", 4000, "working"), "done");
        assert_eq!(s("selesai", 4000, "idle"), "idle");
        assert_eq!(s("Do you want to proceed?", 100, "working"), "blocked");
    }

    #[test]
    fn versi_dari_teks() {
        assert_eq!(version_of("2.1.309 (Claude Code)").as_deref(), Some("2.1.309"));
        assert_eq!(version_of("codex-cli 0.149.0").as_deref(), Some("0.149.0"));
        assert_eq!(version_of("omp/18.13.3").as_deref(), Some("18.13.3"));
        assert_eq!(version_of("tanpa versi"), None);
    }
}
