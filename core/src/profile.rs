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
    /// FR-64: mode izin agent sendiri, `default` | `auto`; Gembala hanya menambah flag launch (lihat `permission_flags`)
    pub permission: String,
    /// SEC-3 cadangan: beri `GEMBALA_TOKEN` (mis. agent yang jalan di dalam tmux)
    pub token: bool,
    /// FR-61: id `[[account]]` bawaan profil; kosong = Utama
    pub account: String,
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
        permission: "default".into(),
        token: false,
        account: String::new(),
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
        // FR-61: dicocokkan ke [[account]] oleh config::load (akun belum terbaca di sini)
        if let Some(a) = t.get("account").and_then(|v| v.as_str()) {
            p.account = a.to_string();
        }
        match t.get("permission").map(|v| v.as_str()) {
            None => {}
            Some(Some(v @ ("default" | "auto"))) => p.permission = v.into(),
            Some(_) => errors.push(format!("{}: harus default atau auto", at("permission"))),
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
/// isinya tidak diteruskan ke daemon (SEC-33), kecuali pesan terakhir giliran Codex untuk balasan otomatis (FR-66).
pub fn hook_status(agent: &str, event: &str, payload: &str) -> Option<&'static str> {
    Some(match (agent, event) {
        ("claude", "prompt" | "tool") => "working",
        ("claude", "stop") => "done",
        // Notification Claude Code: izin alat = blocked; "menunggu input" = siap menerima tugas
        // dibaca dari `notification_type`/`message` saja: payload selalu memuat `permission_mode`, jadi menunggu input ikut "butuh izin"
        ("claude", "notify") if claude_permission(payload) => "blocked",
        ("claude", "notify") => "idle",
        ("codex", "notify") if payload.contains("agent-turn-complete") => "done",
        // FR-68: omp menunggu persetujuan tool = butuh izin; setelah diputus kembali bekerja
        ("omp", "approval") => "blocked",
        ("opencode", "busy") | ("omp", "start" | "resolved") => "working",
        ("opencode", "idle") | ("omp", "end") => "done",
        ("opencode", "permission") => "blocked",
        _ => return None,
    })
}

fn claude_permission(payload: &str) -> bool {
    let v: Value = serde_json::from_str(payload).unwrap_or_default();
    match v["notification_type"].as_str() {
        Some(t) => t == "permission_prompt",
        None => v["message"].as_str().is_some_and(|m| m.to_lowercase().contains("permission")),
    }
}

/// Teks di layar yang berarti agent menunggu jawaban manusia (huruf kecil).
// FR-68: dialog persetujuan Codex ("Would you like to run/make the following …")
// hanya kalimat khas dialog: kata lepas seperti "approve" juga muncul di jawaban agent ("approve kalau muncul") = butuh izin palsu
const PROMPTS: &[&str] = &["(y/n)", "[y/n]", "(yes/no)", "[yes/no]", "do you want to proceed", "do you want to make", "do you want to create", "would you like to run", "would you like to make", "allow once", "allow tool:"];

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
User says lempar/kirim/tanya ke another agent = `gembala status`, then `gembala send`. \
A user prompt naming panes as `@<pane>` = send each of them that full handover yourself with `gembala send`; never the prompt verbatim. Do not look for processes via ps.";

/// FR-56: perintah agent manual untuk blok ringkas, tanpa flag sisipan Gembala (petunjuk, hook) yang
/// ditambah fungsi zsh: yang tampil cukup yang diketik pengguna.
pub fn shown_args(args: &[String]) -> String {
    let ours = |a: &str| a.contains(PETUNJUK) || a.contains("omp-gembala.ts") || a.contains(" hook claude ") || a.contains("\"hook\",\"codex\"");
    let mut out: Vec<&str> = vec![];
    for a in args {
        if ours(a) {
            // flag pemilik nilai ini (`--settings`, `-c`, `--append-system-prompt`) ikut dibuang
            if !a.starts_with('-') && out.last().is_some_and(|f| f.starts_with('-') && !f.contains('=')) {
                out.pop();
            }
        } else {
            out.push(a);
        }
    }
    out.join(" ")
}

/// Flag/env hook per agent saat launch (TECH bagian 7, teruji 31 Okt 2026).
fn hook_flags(p: &Profile, h: &Hooks) -> (Vec<String>, Vec<(String, String)>) {
    let cmd = |ev: &str| format!("{} hook claude {ev}", q(&h.exe));
    let file = |name: &str| h.dir.join(name).to_string_lossy().into_owned();
    match p.agent.as_str() {
        "claude" => {
            let one = |ev: &str| json!([{"hooks": [{"type": "command", "command": cmd(ev)}]}]);
            let settings = json!({"hooks": {
                // PostToolUse: status `blocked` kembali `working` begitu izin disetujui (PreToolUse jalan sebelum dialog izin)
                "UserPromptSubmit": one("prompt"), "PreToolUse": one("tool"), "PostToolUse": one("tool"), "Stop": one("stop"), "Notification": one("notify"),
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
        "omp" => (vec![format!("--hook={}", file("omp-gembala.ts")), format!("--append-system-prompt={PETUNJUK} {OMP_TOOLS}")], vec![]),
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
/// Claude tanpa flag lanjut diberi `--session-id` sejak mulai (FR-68): daemon membacanya dari argumen proses.
/// ponytail: `claude "prompt"` positional tidak diberi petunjuk.
fn zsh_agents(dir: &std::path::Path, exe: &str) -> String {
    let f = q(&dir.join("gembala.md").to_string_lossy());
    // FR-68: Codex diketik = hook notify sama dengan profil; OpenCode diketik = config Gembala (petunjuk + plugin)
    let notify = q(&format!("notify={}", json!([exe, "hook", "codex", "notify"])));
    let oc = q(&dir.join("opencode.json").to_string_lossy());
    // FR-67: omp manual juga mendapat hook (status + tool pesan) dan petunjuk khusus omp
    let (fo, hook) = (q(&dir.join("gembala.md.omp").to_string_lossy()), q(&format!("--hook={}", dir.join("omp-gembala.ts").to_string_lossy())));
    format!(
        "function claude {{ [[ -n $CLAUDE_CONFIG_DIR ]] && command gembala trust-sync \"$CLAUDE_CONFIG_DIR\" 2>/dev/null; if (( ! $# )) || [[ $1 == -* ]]; then local s=(); [[ \" $* \" == *\" -\"[rc]\" \"* || \" $* \" == *\" --\"(resume|continue|session-id|fork-session)* ]] || s=(--session-id \"$(uuidgen | tr A-Z a-z)\"); command claude --append-system-prompt \"$(<{f})\" \"${{s[@]}}\" \"$@\"; else command claude \"$@\"; fi }}\n\
         function omp {{ if (( ! $# )) || [[ $1 == -* ]]; then command omp {hook} \"--append-system-prompt=$(<{fo})\" \"$@\"; else command omp \"$@\"; fi }}\n\
         function codex {{ GEMBALA_SPOOL=\"$(command gembala token 2>/dev/null)\" command codex -c {notify} -c \"$(<{f}.codex)\" \"$@\" }}\n\
         function opencode {{ OPENCODE_CONFIG=${{OPENCODE_CONFIG:-{oc}}} command opencode \"$@\" }}\n\
         [[ -f {acc} ]] && builtin source {acc}\n",
        acc = q(&dir.join("accounts.zsh").to_string_lossy())
    )
}

/// FR-61: `claude-<akun>` di pane shell = `claude` (dengan hook di atas) memakai folder akun itu.
/// Ditulis ulang daemon tiap config dimuat, jadi akun baru ada di pane yang dibuka sesudahnya.
pub fn zsh_accounts(accounts: &[crate::account::Account]) -> String {
    let mut s = String::from("# dibuat Gembala (FR-61): satu perintah per akun agent\n");
    for a in accounts.iter().filter(|a| a.agent == "claude") {
        let (k, v) = crate::account::env(a);
        let flags: String = crate::account::flags(a).iter().map(|f| format!(" {}", q(f))).collect();
        s += &format!("function claude-{} {{ {k}={} claude{flags} \"$@\" }}\n", a.id, home_q(&v));
    }
    s
}

/// `~/…` di luar kutip supaya jadi folder home
pub fn home_q(v: &str) -> String {
    v.strip_prefix("~/").map_or_else(|| q(v), |rest| format!("\"$HOME\"/{}", q(rest)))
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
        (".zshrc", zsh_file(".zshrc", &format!("{ZSH_MARKS}{}", zsh_agents(&h.dir, &h.exe)))),
        (".zlogin", format!("# dibuat Gembala (FR-58); .zlogin pengguna dibaca apa adanya, lalu ZDOTDIR dikembalikan\n{ZSH_RESTORE}")),
        (
            "opencode.json",
            json!({"$schema": "https://opencode.ai/config.json", "plugin": [format!("file://{plugin}")],
                "instructions": [h.dir.join("gembala.md").to_string_lossy()]})
            .to_string(),
        ),
        ("gembala.md", format!("{PETUNJUK}\n")),
        ("gembala.md.omp", format!("{PETUNJUK} {OMP_TOOLS}\n")),
        // nilai `-c` Codex untuk agent manual (string TOML = string JSON tanpa karakter khusus)
        // FR-66: di sandbox, jawaban akhir giliran ikut terkirim sebagai balasan bila `gembala reply` gagal
        ("gembala.md.codex", format!("developer_instructions={}", json!(format!("{PETUNJUK} Your final message of a turn is also delivered as the reply to a waiting message.")))),
        (
            "opencode-gembala.js",
            format!(
                "// dibuat Gembala; status pane (FR-4)\nimport {{ execFile }} from \"node:child_process\";\n\
                 const hook = (s, id) => execFile({exe}, [\"hook\", \"opencode\", s, String(id ?? \"\")], () => {{}});\n\
                 export const Gembala = async () => ({{\n  event: async ({{ event }}) => {{\n\
                 \x20   const id = event.properties?.sessionID; // FR-68: lanjutkan sesi ini persis\n\
                 \x20   if (event.type === \"session.status\") hook(event.properties?.status?.type === \"busy\" ? \"busy\" : \"idle\", id);\n\
                 \x20   else if (event.type === \"session.idle\") hook(\"idle\", id);\n\
                 \x20   else if (event.type.startsWith(\"permission.\")) hook(\"permission\", id);\n  }},\n}});\n"
            ),
        ),
        ("omp-gembala.ts", OMP_HOOK.replace("__EXE__", &exe.to_string())),
    ]
}

/// Hook omp: status pane (FR-4) dan tool pesan (FR-67). Tool dari hook tidak lewat bash, jadi pola bash pengguna
/// (mis. `*psql*` = prompt) tidak menahan pesan yang kebetulan menyebut kata itu.
const OMP_HOOK: &str = r#"// dibuat Gembala; status pane (FR-4) dan tool pesan tanpa bash (FR-67)
import { execFile } from "node:child_process";
const exe = __EXE__;
const hook = (s: string) => execFile(exe, ["hook", "omp", s], () => {});
const run = (args: string[], signal?: AbortSignal) =>
  new Promise<string>((ok) => execFile(exe, args, { signal, maxBuffer: 1 << 20 }, (e, out, err) => ok(String(out || err || (e ? e.message : "")).trim())));
export default function (pi: any) {
  pi.on("agent_start", async () => hook("start"));
  pi.on("agent_end", async () => hook("end"));
  pi.on("tool_approval_requested", async () => hook("approval")); // FR-68: butuh izin
  pi.on("tool_approval_resolved", async () => hook("resolved"));
  const z = pi.zod;
  const tool = (name: string, description: string, parameters: any, approval: string, args: (p: any) => string[]) =>
    pi.registerTool({ name, label: name, description, parameters, approval,
      async execute(_id: string, p: any, signal?: AbortSignal) { return { content: [{ type: "text", text: await run(args(p), signal) }], details: {} }; } });
  tool("gembala_status", "List Gembala panes in this workspace (name, agent, status).", z.object({}), "read", () => ["status"]);
  tool("gembala_send", "Send a message to another Gembala pane; returns its id. Write a full handover.",
    z.object({ to: z.string().describe("pane name, e.g. shell-4"), message: z.string(), queue: z.boolean().optional().describe("queue when the target is busy"), reply_to: z.string().optional() }),
    "write", (p) => ["send", p.to, p.message, ...(p.queue ? ["--queue"] : []), ...(p.reply_to ? ["--reply-to", p.reply_to] : [])]);
  tool("gembala_reply", "Answer a message labeled [Gembala · dari <pane> · <id>].", z.object({ msg_id: z.string(), message: z.string() }), "write", (p) => ["reply", p.msg_id, p.message]);
  tool("gembala_wait", "Wait for the reply to a message you sent.", z.object({ reply_id: z.string(), timeout_s: z.number().optional() }), "read",
    (p) => ["wait", "--reply", p.reply_id, ...(p.timeout_s ? ["--timeout", String(p.timeout_s)] : [])]);
  tool("gembala_read", "Read the last lines of another pane in this workspace.", z.object({ target: z.string(), lines: z.number().optional() }), "read",
    (p) => ["read", p.target, ...(p.lines ? ["-n", String(p.lines)] : [])]);
}
"#;

/// FR-67: petunjuk tambahan omp: pakai tool hook, bukan bash
pub const OMP_TOOLS: &str = "In omp, use the gembala_status, gembala_send, gembala_reply, gembala_wait and gembala_read tools instead of running gembala in bash.";

/// FR-53: id sesi dari Gembala (UUID) dan apakah pane melanjutkan sesi sebelumnya.
pub struct Session {
    pub id: Option<String>,
    pub resume: bool,
}

/// FR-68: id sesi yang boleh dipakai untuk agent itu (ditulis ke perintah shell): UUID untuk Claude/Codex,
/// huruf, angka, `_`, `-` untuk OpenCode/omp.
pub fn valid_session(agent: &str, s: &str) -> bool {
    match agent {
        "claude" | "codex" => valid_uuid(s),
        _ => (1..=80).contains(&s.len()) && s.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-'),
    }
}

pub fn valid_uuid(s: &str) -> bool {
    s.len() == 36 && s.chars().enumerate().all(|(i, c)| if [8, 13, 18, 23].contains(&i) { c == '-' } else { c.is_ascii_hexdigit() })
}

/// Claude Code menerima id dari luar; agent lain hanya "lanjutkan terakhir" (TECH bagian 7a, `--help` 1 Nov 2026).
fn session_flags(agent: &str, s: &Session) -> Vec<String> {
    let id = s.id.as_deref().filter(|i| valid_session(agent, i));
    match (agent, s.resume, id) {
        ("claude", false, Some(id)) => vec!["--session-id".into(), id.into()],
        ("claude", true, Some(id)) => vec!["--resume".into(), id.into()],
        // FR-68: id persis, bukan sesi terakhir di folder
        ("codex", true, Some(id)) => vec!["resume".into(), id.into()],
        ("opencode", true, Some(id)) => vec!["--session".into(), id.into()],
        ("claude", true, None) | ("opencode" | "omp", true, _) => vec!["--continue".into()],
        ("codex", true, _) => vec!["resume".into(), "--last".into()],
        _ => vec![],
    }
}

/// FR-64: flag mode izin per agent (`--help` 2 Nov 2026). OpenCode tidak punya flag untuk TUI (`--auto` hanya di `run`): kosong.
/// Codex tetap bersandbox `workspace-write`; omp `yolo` tetap menghormati rule `bash.patterns` milik pengguna.
pub fn permission_flags(agent: &str, permission: &str) -> Vec<String> {
    let f: &[&str] = match (agent, permission) {
        ("claude", "auto") => &["--permission-mode", "auto"],
        ("omp", "auto") => &["--approval-mode=yolo"],
        ("codex", "auto") => &["--ask-for-approval", "never", "--sandbox", "workspace-write"],
        _ => &[],
    };
    f.iter().map(|s| s.to_string()).collect()
}

/// Teks yang dijalankan `$SHELL -l -c` (TECH bagian 6). `None` = shell login interaktif.
/// Agent jalan di subshell supaya env rahasia tidak tertinggal di shell pane setelah agent keluar;
/// lalu `exec $SHELL -l` membuat pane tetap hidup (FR-18). Exit 127 = pesan instal (FR-32).
pub fn launch(p: &Profile, h: Option<&Hooks>, session: Option<&Session>) -> Option<String> {
    if p.command.is_empty() {
        return None;
    }
    let (mut flags, hook_env) = h.map(|h| hook_flags(p, h)).unwrap_or_default();
    flags.extend(permission_flags(&p.agent, &p.permission));
    flags.extend(session.map(|s| session_flags(&p.agent, s)).unwrap_or_default());
    // FR-56: penanda tak terlihat untuk daemon (OSC 777), mengapit output sesi agent
    let mut s = String::from(if p.summarize { "printf '\\033]777;gembala;agent-start\\007'; ( " } else { "( " });
    // PATH interaktif (sudah diambil detect saat daemon mulai), supaya agent yang hanya ada di PATH `.zshrc` ketemu
    if let Some(path) = USER_PATH.get().cloned().flatten() {
        s += &format!("export PATH={}:\"$PATH\"; ", q(&path));
    }
    for (k, v) in p.env.iter().chain(&hook_env) {
        // `~/…` di luar kutip supaya jadi folder home (mis. CLAUDE_CONFIG_DIR per akun)
        s += &format!("export {k}={}; ", home_q(v));
    }
    for (k, r) in &p.secret_env {
        // rahasia dibuka shell pane; nilainya tidak pernah lewat daemon (SEC-32)
        if let Some((svc, acct)) = r.strip_prefix("keychain:").and_then(|x| x.split_once('/')) {
            s += &format!("export {k}=\"$(security find-generic-password -w -s {} -a {})\"; ", q(svc), q(acct));
        } else {
            s += &format!("export {k}=\"$(op read {})\"; ", q(r));
        }
    }
    // FR-66: sandbox Codex menolak socket pane; token (nama tanpa TOKEN supaya tidak dibuang Codex) untuk jalur spool
    if p.agent == "codex" {
        s += "export GEMBALA_SPOOL=\"$(gembala token 2>/dev/null)\"; ";
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
        "id": p.id, "name": p.name, "agent": p.agent, "command": p.command, "args": p.args, "cwd": p.cwd, "summarize": p.summarize, "notify": p.notify, "policy": p.policy, "permission": p.permission, "token": p.token, "account": p.account,
        "env": p.env.iter().map(|(k, _)| k).collect::<Vec<_>>(),
        "secret_env": p.secret_env.iter().map(|(k, r)| json!({"name": k, "ref": r})).collect::<Vec<_>>(),
    })
}

static USER_PATH: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();

/// PATH shell login interaktif, seperti pane. Banyak pemasang hanya menambah PATH di `.zshrc`
/// (Codex standalone: `~/.local/bin`), yang tidak dibaca `-l -c`. Diambil sekali per daemon.
fn user_path() -> Option<String> {
    USER_PATH
        .get_or_init(|| {
            let mut c = Command::new(crate::pty::login_shell());
            c.args(["-l", "-i", "-c", "printf '\\n__GEMBALA_PATH__%s\\n' \"$PATH\""]).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
            crate::pty::scrub_env(&mut c);
            if let Some(home) = std::env::var_os("HOME") {
                c.current_dir(home);
            }
            let out = run_with_timeout(c, Duration::from_secs(10))?;
            out.lines().rev().find_map(|l| l.strip_prefix("__GEMBALA_PATH__")).filter(|p| !p.is_empty()).map(str::to_string)
        })
        .clone()
}

/// FR-28: cari agent lewat login shell (PATH sama dengan terminal biasa), batas 10 detik.
pub fn detect() -> Value {
    let names: Vec<&str> = AGENTS.iter().map(|a| a.0).collect();
    let script = format!(
        "{}for a in {}; do p=$(command -v \"$a\") && printf '%s\\t%s\\t%s\\n' \"$a\" \"$p\" \"$(\"$a\" --version 2>/dev/null | head -1)\"; done",
        user_path().map(|p| format!("export PATH={}:\"$PATH\"; ", q(&p))).unwrap_or_default(),
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
    // stdout dibaca sambil menunggu: output di atas buffer pipe (~64 KB, mis. git diff besar) membuat proses
    // menunggu dibaca, jadi baca-setelah-selesai selalu macet sampai batas waktu
    let mut stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut out = String::new();
        stdout.read_to_string(&mut out).ok().map(|_| out)
    });
    let t0 = Instant::now();
    while child.try_wait().ok()?.is_none() {
        if t0.elapsed() > limit {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    reader.join().ok()?
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
            env: vec![("X".into(), "a b'c".into()), ("D".into(), "~/.claude-kerja".into())],
            secret_env: vec![("K".into(), "keychain:svc/me@x".into()), ("O".into(), "op://V/I/f".into())],
            ..builtin()[1].clone()
        };
        let s = launch(&p, None, None).unwrap();
        assert!(s.starts_with("printf '\\033]777;gembala;agent-start\\007'; ( export X='a b'\\''c'; export D=\"$HOME\"/'.claude-kerja'; "), "{s}");
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
    fn mode_izin_jadi_flag_per_agent() {
        let auto = |a: &str| permission_flags(a, "auto").join(" ");
        assert_eq!(auto("claude"), "--permission-mode auto");
        assert_eq!(auto("omp"), "--approval-mode=yolo");
        assert_eq!(auto("codex"), "--ask-for-approval never --sandbox workspace-write");
        assert!(permission_flags("opencode", "auto").is_empty() && permission_flags("claude", "default").is_empty());
        let p = Profile { permission: "auto".into(), ..builtin()[4].clone() };
        assert!(launch(&p, None, None).unwrap().contains("'omp' '--approval-mode=yolo' );"));
        let (ps, errs) = parse("[[profile]]\nid = \"omp\"\npermission = \"yolo\"\n");
        assert!(errs.iter().any(|e| e.contains("default atau auto")) && ps.iter().all(|x| x.permission == "default"));
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
    fn shown_args_tanpa_sisipan_gembala() {
        let h = Hooks { exe: "/Apps/Gem bala/gembala".into(), dir: "/cfg/hooks".into() };
        for (agent, user) in [("claude", vec!["--model", "opus"]), ("codex", vec!["resume", "--last"]), ("omp", vec!["--verbose"])] {
            let p = builtin().into_iter().find(|p| p.id == agent).unwrap();
            let mut args: Vec<String> = vec![agent.into()];
            args.extend(hook_flags(&p, &h).0);
            args.extend(user.iter().map(|s| s.to_string()));
            assert_eq!(shown_args(&args), format!("{agent} {}", user.join(" ")));
        }
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
        assert!(launch(&by("omp"), Some(&h), None).unwrap().contains(&format!("'--append-system-prompt={PETUNJUK} {OMP_TOOLS}'")));
        let codex = launch(&by("codex"), Some(&h), None).unwrap();
        assert!(codex.contains("'-c' 'notify=[\"/Apps/Gem bala/gembala\",\"hook\",\"codex\",\"notify\"]'"), "{codex}");
        assert!(codex.contains(&format!("'-c' 'developer_instructions=\"{PETUNJUK}\"'")), "{codex}");
        let files = hook_files(&h);
        let oc: serde_json::Value = serde_json::from_str(&files.iter().find(|f| f.0 == "opencode.json").unwrap().1).unwrap();
        assert_eq!(oc["instructions"][0], "/cfg/hooks/gembala.md");
        assert!(files.iter().any(|f| f.0 == "gembala.md" && f.1.contains(PETUNJUK)));
        let rc = &files.iter().find(|f| f.0 == ".zshrc").unwrap().1;
        // fungsi agent di .zshrc harus sintaks zsh yang sah
        let tmp = std::env::temp_dir().join(format!("gembala-zshrc-{}", std::process::id()));
        std::fs::write(&tmp, rc).unwrap();
        let ok = std::process::Command::new("zsh").arg("-n").arg(&tmp).status().unwrap().success();
        let _ = std::fs::remove_file(&tmp);
        assert!(ok, "{rc}");
        assert!(rc.contains("--session-id") && rc.contains("hook\",\"codex\",\"notify") && rc.contains("function opencode {"), "{rc}");
        assert!(rc.contains("function claude {") && rc.contains("'/cfg/hooks/gembala.md'"), "{rc}");
        assert!(launch(&by("opencode"), Some(&h), None).unwrap().contains("export OPENCODE_CONFIG='/cfg/hooks/opencode.json'"));
        assert!(launch(&by("omp"), Some(&h), None).unwrap().contains("'--hook=/cfg/hooks/omp-gembala.ts'"));
        assert_eq!(launch(&by("shell"), Some(&h), None), None);
        // perintah hook Claude dijalankan shell: path berspasi tetap satu kata
        let settings: serde_json::Value = serde_json::from_str(hook_flags(&by("claude"), &h).0[1].as_str()).unwrap();
        assert_eq!(settings["hooks"]["Stop"][0]["hooks"][0]["command"], "'/Apps/Gem bala/gembala' hook claude stop");
        assert_eq!(settings["hooks"]["PostToolUse"][0]["hooks"][0]["command"], "'/Apps/Gem bala/gembala' hook claude tool");
    }

    #[test]
    fn sesi_per_agent() {
        let by = |id: &str| builtin().into_iter().find(|p| p.id == id).unwrap();
        let id = "0b6f2a1e-5c3d-4e7f-8a9b-1c2d3e4f5a6b";
        let new = Session { id: Some(id.into()), resume: false };
        let again = Session { id: Some(id.into()), resume: true };
        assert!(launch(&by("claude"), None, Some(&new)).unwrap().contains(&format!("'claude' '--session-id' '{id}'")));
        assert!(launch(&by("claude"), None, Some(&again)).unwrap().contains(&format!("'claude' '--resume' '{id}'")));
        // FR-68: id persis bila ada, "sesi terakhir di folder" hanya tanpa id
        assert!(launch(&by("codex"), None, Some(&again)).unwrap().contains(&format!("'codex' 'resume' '{id}'")));
        assert!(launch(&by("opencode"), None, Some(&Session { id: Some("ses_4Ab-9".into()), resume: true })).unwrap().contains("'opencode' '--session' 'ses_4Ab-9'"));
        let none = Session { id: None, resume: true };
        assert!(launch(&by("codex"), None, Some(&none)).unwrap().contains("'codex' 'resume' '--last'"));
        assert!(launch(&by("opencode"), None, Some(&none)).unwrap().contains("'opencode' '--continue'"));
        assert!(!valid_session("opencode", "a b") && !valid_session("codex", "ses_1") && valid_session("omp", "01a19b03-x"));
        let bad = Session { id: Some("x; rm -rf /".into()), resume: false };
        assert!(!launch(&by("claude"), None, Some(&bad)).unwrap().contains("rm"));
        assert!(valid_uuid(id) && !valid_uuid("0b6f2a1e"));
    }

    #[test]
    fn status_dari_event_hook() {
        assert_eq!(hook_status("claude", "prompt", ""), Some("working"));
        assert_eq!(hook_status("omp", "approval", ""), Some("blocked"));
        assert_eq!(hook_status("omp", "resolved", ""), Some("working"));
        assert_eq!(hook_status("claude", "notify", r#"{"message":"Claude needs your permission to use Bash"}"#), Some("blocked"));
        assert_eq!(hook_status("claude", "notify", r#"{"message":"Claude is waiting for your input"}"#), Some("idle"));
        // payload asli selalu membawa permission_mode: menunggu input tetap idle
        let idle = r#"{"permission_mode":"auto","message":"Claude is waiting for your input","notification_type":"idle_prompt"}"#;
        assert_eq!(hook_status("claude", "notify", idle), Some("idle"));
        let ask = r#"{"permission_mode":"auto","message":"Claude needs your permission to use Bash","notification_type":"permission_prompt"}"#;
        assert_eq!(hook_status("claude", "notify", ask), Some("blocked"));
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
        assert_eq!(s("Allow tool: bash\n❯ Approve\n  Deny", 100, "working"), "blocked");
        // jawaban agent yang menyebut approve/izin bukan dialog
        assert_eq!(s("psql bakal minta izin di shell-2, jadi approve kalau muncul", 4000, "working"), "done");
        assert_eq!(s("needs permission to write", 4000, "done"), "done");
    }

    #[test]
    fn versi_dari_teks() {
        assert_eq!(version_of("2.1.309 (Claude Code)").as_deref(), Some("2.1.309"));
        assert_eq!(version_of("codex-cli 0.149.0").as_deref(), Some("0.149.0"));
        assert_eq!(version_of("omp/18.13.3").as_deref(), Some("18.13.3"));
        assert_eq!(version_of("tanpa versi"), None);
    }

    #[test]
    fn output_besar_tidak_macet() {
        // 1 MB > buffer pipe: dulu macet sampai batas waktu lalu kosong (panel perubahan "Memuat…" lama)
        let mut c = std::process::Command::new("/bin/sh");
        c.args(["-c", "head -c 1048576 /dev/zero | tr '\\0' x"]).stdout(std::process::Stdio::piped());
        let t0 = Instant::now();
        let out = run_with_timeout(c, Duration::from_secs(3)).unwrap();
        assert_eq!(out.len(), 1 << 20);
        assert!(t0.elapsed() < Duration::from_secs(1), "{:?}", t0.elapsed());
    }

    #[test]
    fn claude_per_akun_di_shell() {
        let a = |id: &str, agent: &str, dir: &str| crate::account::Account { id: id.into(), agent: agent.into(), name: id.into(), dir: dir.into(), share: vec![], model: String::new(), effort: String::new(), trust: true };
        let rc = zsh_accounts(&[a("kantor", "claude", "~/.claude-kantor"), a("x", "codex", "~/.codex-x")]);
        assert!(!rc.contains("claude-x"), "{rc}");
        // fungsi `claude` (hook) menerima folder akun lewat env yang sampai ke proses anak
        let script = format!("function claude {{ command env | grep '^CLAUDE_CONFIG_DIR=' }}\n{rc}claude-kantor --versi");
        let out = std::process::Command::new("zsh").args(["-f", "-c", &script]).env("HOME", "/rumah").output().unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "CLAUDE_CONFIG_DIR=/rumah/.claude-kantor", "{rc}");
        // FR-61 (8): model/effort akun jadi flag sebelum argumen pengguna
        let mut k = a("kantor", "claude", "~/.claude-kantor");
        (k.model, k.effort) = ("opus".into(), "high".into());
        let script = format!("function claude {{ print -r -- \"$*\" }}\n{}claude-kantor --resume x", zsh_accounts(&[k]));
        let out = std::process::Command::new("zsh").args(["-f", "-c", &script]).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "--model opus --effort high --resume x");
    }
}
