//! Agent profiles (FR-18, 29, 30, 32, 33; SEC-32) and detection of installed agents (FR-28, 31).
//! Launch commands are built here as shell text with every user-supplied part quoted.

use serde_json::{Value, json};
use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::diag::Diag;

#[derive(Clone, Debug, PartialEq)]
pub struct Profile {
    pub id: String,
    pub name: String,
    pub agent: String,
    /// Empty = interactive login shell.
    pub command: String,
    pub args: Vec<String>,
    /// Empty = follow the focused pane (FR-30). `~` becomes `$HOME`.
    pub cwd: String,
    pub env: Vec<(String, String)>,
    /// References only: `op://…` or `keychain:service/account` (SEC-32).
    pub secret_env: Vec<(String, String)>,
    /// FR-56: summarize output when the agent exits (on by default).
    pub summarize: bool,
    /// FR-40: notifications for panes from this profile (on by default).
    pub notify: bool,
    /// SEC-5: recipient policy `auto` | `confirm` | `deny`
    pub policy: String,
    /// FR-64: the agent's own permission mode, `default` | `auto`; Gembala only adds launch flags (see `permission_flags`)
    pub permission: String,
    /// SEC-3 fallback: hand out `GEMBALA_TOKEN` (e.g. an agent running inside tmux)
    pub token: bool,
    /// FR-61: the profile's default `[[account]]` id; empty = Utama
    pub account: String,
    /// FR-13: role and workspace brief for this pane (not from the profile config), go into the launch hint
    pub brief: String,
    /// FR-13: read-only role; Claude is locked through `--disallowedTools`, other agents only get a hint
    pub readonly: bool,
    /// FR-78: add-on flags for this pane's workspace (set at launch, not from the profile config)
    pub addons: Vec<String>,
}

/// (agent, name, command, install command shown when missing, tested major.minor version)
/// Install commands checked against how they were installed on the dev machine 1 Nov 2026; codex not yet (TECH section 7).
const AGENTS: &[(&str, &str, &str, &str, &str)] = &[
    ("claude", "Claude Code", "claude", "brew install --cask claude-code", "2.1"),
    ("codex", "Codex", "codex", "npm install -g @openai/codex", "0.167"),
    ("opencode", "OpenCode", "opencode", "brew install anomalyco/tap/opencode", "1.19"),
    ("omp", "Oh My Pi", "omp", "brew install can1357/tap/omp", "18.18"),
];

/// agent kinds Gembala knows (recipes name agents by these)
pub fn agent_ids() -> impl Iterator<Item = &'static str> {
    AGENTS.iter().map(|a| a.0)
}

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
        brief: String::new(),
        readonly: false,
        addons: vec![],
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

/// SEC-12/SEC-32: values that look like secret keys.
pub const SECRET: &[&str] = &["sk-ant-", "sk-", "ghp_", "github_pat_", "AKIA", "PRIVATE KEY", "xoxb-", "xoxp-"];

/// Position of `pat` not glued to the end of another word (`task-list` isn't `sk-`).
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

/// `[[profile]]` from config: same id as a built-in = overrides the filled parts; new id = extra profile.
pub fn from_config(tables: &[&dyn toml_edit::TableLike]) -> (Vec<Profile>, Vec<Diag>) {
    let mut out = builtin();
    let mut errors = Vec::new();
    for (i, t) in tables.iter().enumerate() {
        let at = |k: &str| format!("profile[{i}].{k}");
        let Some(id) = t.get("id").and_then(|v| v.as_str()) else {
            errors.push(Diag::new("required").at(at("id")).then("skip_profile"));
            continue;
        };
        if !(1..=32).contains(&id.len()) || !id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') {
            errors.push(Diag::new("profile_id").at(at("id")).then("skip_profile"));
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
                Some(_) => errors.push(Diag::new("text_bad").at(at(key)).then("default")),
            }
        }
        for (key, field) in [("summarize", &mut p.summarize), ("notify", &mut p.notify), ("token", &mut p.token)] {
            match t.get(key).map(|v| v.as_bool()) {
                None => {}
                Some(Some(b)) => *field = b,
                Some(None) => errors.push(Diag::new("bool").at(at(key))),
            }
        }
        match t.get("policy").map(|v| v.as_str()) {
            None => {}
            Some(Some(v)) if crate::msg::Policy::parse(v).is_some() => p.policy = v.into(),
            Some(_) => errors.push(Diag::new("one_of").at(at("policy")).arg("opts", "auto, confirm, deny")),
        }
        // FR-61: matched to [[account]] by config::load (accounts aren't read here yet)
        if let Some(a) = t.get("account").and_then(|v| v.as_str()) {
            p.account = a.to_string();
        }
        match t.get("permission").map(|v| v.as_str()) {
            None => {}
            Some(Some(v @ ("default" | "auto"))) => p.permission = v.into(),
            Some(_) => errors.push(Diag::new("one_of").at(at("permission")).arg("opts", "default, auto")),
        }
        if let Some(a) = t.get("args") {
            match a.as_array().map(|arr| arr.iter().map(|v| v.as_str().filter(|s| ok_text(s, 500)).map(String::from)).collect::<Option<Vec<_>>>()) {
                Some(Some(v)) => p.args = v,
                _ => errors.push(Diag::new("text_list").at(at("args")).then("default")),
            }
        }
        for (key, secret) in [("env", false), ("secret_env", true)] {
            let Some(tbl) = t.get(key).and_then(|v| v.as_table_like()) else { continue };
            let mut list = Vec::new();
            for (k, v) in tbl.iter() {
                let v = v.as_str().unwrap_or_default();
                if !ok_env_name(k) {
                    errors.push(Diag::new("env_name").at(format!("{}.{k}", at(key))));
                } else if secret && !ok_ref(v) {
                    errors.push(Diag::new("secret_ref").at(format!("{}.{k}", at(key))));
                } else if !secret && (!ok_text(v, 1000) || SECRET.iter().any(|s| word_start(v, s).is_some())) {
                    errors.push(Diag::new("env_secret").at(format!("{}.{k}", at(key))));
                } else {
                    list.push((k.to_string(), v.to_string()));
                }
            }
            if secret { p.secret_env = list } else { p.env = list }
        }
    }
    (out, errors)
}

/// Single quotes for POSIX shells.
fn q(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Location of the `gembala` binary and of Gembala's hook files folder (SEC-26: agent config isn't touched).
pub struct Hooks {
    pub exe: String,
    pub dir: std::path::PathBuf,
}

/// FR-4: agent hook event → pane status. `None` = the event doesn't change the status.
/// `payload` = stdin (Claude) or the last argument (Codex); only used to tell "needs permission" apart,
/// its content isn't passed to the daemon (SEC-33), except the last message of a Codex turn for the auto reply (FR-66).
/// Codex TUI names a new thread with a side turn on its own thread (`last-assistant-message` = `{"title":"…"}`),
/// reported through `notify` like a real turn, often before the real one ends. It isn't the pane's turn: not done,
/// not a reply (FR-66), not the session to resume (FR-68). Recognised by Codex's title prompt; `thread-id` can't
/// tell it apart because `/new` also starts a new thread.
/// ponytail: matches Codex 0.166's prompt text; if Codex rewords it, title turns count as turns again (old behaviour)
fn codex_title_turn(payload: &str) -> bool {
    serde_json::from_str::<Value>(payload).is_ok_and(|v| {
        v["input-messages"][0].as_str().is_some_and(|m| m.starts_with("Generate a concise, single-line task title"))
    })
}

pub fn hook_status(agent: &str, event: &str, payload: &str) -> Option<&'static str> {
    Some(match (agent, event) {
        ("claude", "prompt" | "tool") => "working",
        ("claude", "stop") => "done",
        // FR-61: SessionStart = session id + owning account, status unchanged
        ("claude", "start") => "start",
        // Claude Code Notification: tool permission = blocked; "waiting for input" = ready for a task
        // read from `notification_type`/`message` only: the payload always holds `permission_mode`, so waiting for input would read as "needs permission"
        ("claude", "notify") if claude_permission(payload) => "blocked",
        ("claude", "notify") => "idle",
        ("codex", "notify") if codex_title_turn(payload) => return None,
        ("codex", "notify") if payload.contains("agent-turn-complete") => "done",
        // FR-68: omp waiting for tool approval = needs permission; once decided it's working again
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

/// On-screen text meaning the agent waits for a human answer (lowercase).
// FR-68: dialog persetujuan Codex ("Would you like to run/make the following …")
// only phrases typical of dialogs: loose words like "approve" also show up in agent answers ("approve if it shows") = false needs-permission
const PROMPTS: &[&str] = &["(y/n)", "[y/n]", "(yes/no)", "[yes/no]", "do you want to proceed", "do you want to make", "do you want to create", "would you like to run", "would you like to make", "allow once", "allow tool:",
    // Codex dialog footer, always on the bottom line: a long "Reason:" pushes "Would you like to run" out of the 12-line tail
    "press enter to confirm or esc to cancel"];

/// FR-84, B2: the folder-trust dialog Claude Code ("Yes, I trust this folder") and Codex ("Trust this folder?") show when
/// launched in a folder they don't know yet. Read from the screen: Claude writes no session file during it, Codex has no
/// hook for it, and Gembala never reads or writes the agents' trust records (SEC-15).
/// The dialog's own footer must be the last line, so a conversation that quotes the dialog doesn't read as one.
pub fn trust_prompt(screen: &str) -> bool {
    let t = screen.to_lowercase();
    let last = t.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or_default();
    t.contains("trust this folder") && (last.contains("enter to confirm") || last.contains("enter continue"))
}

/// FR-4 fallback: status from the screen for agents without hooks. `tail` = the bottom few screen rows,
/// `quiet` = time without output, `prev` = previous status.
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

/// `gembala` command hint for profile agents, through the official `--append-system-prompt` flag (Claude, OMP).
/// Agent config isn't touched; agents typed by hand use a paragraph in the user's global instruction file.
const PETUNJUK_START: &str = "You run in a Gembala pane.";
pub const PETUNJUK: &str = "You run in a Gembala pane. Reach other agents in the same workspace with `gembala` (on PATH, \
JSON output): `gembala status` = list panes; `gembala send <pane> <message>` = send, wait for the reply with `gembala wait --reply <id>`; \
a message labeled `[Gembala · from <pane> · <id>]` is answered with `gembala reply <id> <text>` when done, in 1-2 sentences: what was executed and how (files, commit, \
test result), longer only if the sender asked; label `[Gembala · from user]` = the user, \
answer on screen. The recipient cannot see your conversation: write every send as a full handover (goal, context and user decisions, \
files/paths, what was tried or is forbidden, constraints, reply format), never a one-line summary; multi-line is fine. \
User says lempar/kirim/tanya ke another agent = `gembala status`, then `gembala send`. \
Need an agent that is not open (or more panes)? `gembala send <name> <task>` with a name from `can_open` in `gembala status` \
(e.g. `claude-utama`, `omp`) asks the user to open it: result `needs_open`, the task goes once they approve; max 4 per agent. \
A user prompt naming panes as `@<pane>` = send each of them that full handover yourself with `gembala send`; never the prompt verbatim. \
Pane names (`shell-N`, `@<pane>`, anything in `gembala status`) are Gembala panes, not sessions of your agent tool: reach them only with `gembala`, \
never with built-in session messaging (ListAgents, SendMessage, cross-session messages). \
Errors: `target_busy` = retry with `--queue`; `no_such_pane` = closed (old names of renamed panes still work), check `gembala status` and ask the user; \
`acl_denied` = other workspace or the pane refuses messages, tell the user. `gembala wait` gives up after 10 minutes unless `--timeout <seconds>` (max 3600). \
Do not look for processes via ps.";

/// FR-56: manual agent command for the summary block, without the flags Gembala inserts (hint, hooks) that the
/// zsh function adds: showing what the user typed is enough.
/// The hint is matched by its opening sentence, not the whole text: the hook file can come from another daemon build.
/// `--session-id` is added by Gembala's zsh function too; the session card already shows `claude --resume <id>`.
pub fn shown_args(args: &[String]) -> String {
    let ours = |a: &str| a.contains(PETUNJUK_START) || a.contains("omp-gembala.ts") || a.contains(" hook claude ") || a.contains("\"hook\",\"codex\"") || a.starts_with("mcp_servers.") || a.starts_with("tui.status_line=") || a == "--strict-mcp-config";
    let mut out: Vec<&str> = vec![];
    let mut skip = false;
    for a in args {
        if std::mem::take(&mut skip) {
            continue;
        }
        // FR-78: add-on flags come from the zsh function too
        if ["--session-id", "--plugin-dir", "--mcp-config"].contains(&a.as_str()) {
            skip = true;
        } else if ours(a) {
            // the flag that owns this value (`--settings`, `-c`, `--append-system-prompt`) is dropped too
            if !a.starts_with('-') && out.last().is_some_and(|f| f.starts_with('-') && !f.contains('=')) {
                out.pop();
            }
        } else {
            out.push(a);
        }
    }
    out.join(" ")
}

/// Hook flags/env per agent at launch (TECH section 7, tested 31 Oct 2026).
/// FR-13: Claude Code file-changing tools denied for the read-only role (Bash stays, for `gembala send`)
pub const READONLY_TOOLS: &[&str] = &["Edit", "Write", "MultiEdit", "NotebookEdit"];

/// FR-13: known roles: (id, read only, hint)
pub const ROLES: &[(&str, bool, &str)] = &[
    ("koordinator", true, "Your role: coordinator. Do not modify files. Read the code, split the work into small tasks, send them to the worker panes with `gembala send`, and check their results before reporting back."),
    ("planner", true, "Your role: planner. Do not modify files. Write a short step-by-step plan for the task and send it to the worker pane with `gembala send`."),
    ("pekerja", false, "Your role: worker. Do the task you receive, run the relevant tests, then reply with a short summary of what changed."),
    ("reviewer", true, "Your role: reviewer. Do not modify files. Review the changes you are asked to check and reply with concrete findings (file, line, problem)."),
];

/// FR-4: Claude Code status hooks, same for profiles and a typed `claude`.
/// PostToolUse: `blocked` goes back to `working` once permission is granted (PreToolUse runs before the permission dialog).
/// StopFailure: the turn stopped on an API/limit error; without this the pane stays "working".
/// FR-27: `rtk hook claude` on Bash only when the saver is on and `rtk` is on PATH (SEC-31; Gembala never reads the agent's
/// own config, so a user-installed RTK means the user turns this switch off).
fn claude_hooks(exe: &str, rtk: bool) -> Value {
    let one = |ev: &str| json!([{"hooks": [{"type": "command", "command": format!("{} hook claude {ev}", q(exe))}]}]);
    let mut pre = Vec::new();
    if rtk {
        pre.push(json!({"matcher": "Bash", "hooks": [{"type": "command", "command": "rtk hook claude"}]}));
    }
    pre.push(json!({"hooks": [{"type": "command", "command": format!("{} hook claude tool", q(exe))}]}));
    json!({"hooks": {
        "UserPromptSubmit": one("prompt"), "PreToolUse": pre, "PostToolUse": one("tool"), "Stop": one("stop"), "StopFailure": one("stop"),
        "Notification": one("notify"), "SessionStart": one("start"),
    }})
}

fn hook_flags(p: &Profile, h: &Hooks, saver: bool) -> (Vec<String>, Vec<(String, String)>) {
    let petunjuk = if p.brief.is_empty() { PETUNJUK.to_string() } else { format!("{PETUNJUK}\n\n{}", p.brief) };
    let petunjuk = petunjuk.as_str();
    let file = |name: &str| h.dir.join(name).to_string_lossy().into_owned();
    match p.agent.as_str() {
        "claude" => {
            let mut f = vec!["--settings".into(), claude_hooks(&h.exe, saver && rtk_version().is_some()).to_string(), "--append-system-prompt".into(), petunjuk.into()];
            if p.readonly {
                f.push("--disallowedTools".into());
                f.extend(READONLY_TOOLS.iter().map(|t| t.to_string()));
            }
            (f, vec![])
        }
        // ponytail: the user config's developer_instructions get overwritten; merge them if anyone uses it
        "codex" => (
            vec![
                "-c".into(),
                format!("notify={}", json!([h.exe, "hook", "codex", "notify"])),
                "-c".into(),
                format!("developer_instructions={}", json!(petunjuk)),
            ],
            vec![],
        ),
        "opencode" => (vec![], vec![("OPENCODE_CONFIG".into(), file("opencode.json"))]),
        "omp" => (vec![format!("--hook={}", file("omp-gembala.ts")), format!("--append-system-prompt={petunjuk} {OMP_TOOLS}")], vec![]),
        _ => (vec![], vec![]),
    }
}

/// FR-58: the hook folder serves as `ZDOTDIR` for zsh panes. Each file sources the user's same file as is
/// (with the user's `ZDOTDIR` while sourcing), `.zshrc` adds the OSC 133 block markers, `.zlogin` restores
/// `ZDOTDIR` so agents and subshells don't inherit it. User files aren't changed.
fn zsh_file(name: &str, extra: &str) -> String {
    format!(
        "# made by Gembala (FR-58); your {name} is read as is\n\
         GEMBALA_ZDOTDIR=$ZDOTDIR\nZDOTDIR=${{GEMBALA_USER_ZDOTDIR:-$HOME}}\n\
         [[ -f \"$ZDOTDIR/{name}\" ]] && builtin source \"$ZDOTDIR/{name}\"\n\
         GEMBALA_USER_ZDOTDIR=$ZDOTDIR\nZDOTDIR=$GEMBALA_ZDOTDIR\n{extra}"
    )
}

const ZSH_MARKS: &str = r#"__gembala_precmd() {
  local s=$?
  # A carries the prompt folder (no control characters) for the block head in the UI
  builtin print -rn -- $'\e]133;D;'"$s"$'\a\e]133;A;'"${PWD//[[:cntrl:]]/}"$'\a'
  if [[ -n $GEMBALA_EDITOR ]]; then
    # FR-59: Gembala's editor replaces the prompt; the empty line above the command holds the block head
    PS1=$'\n'"%{"$'\e]133;B\a'"%}" RPROMPT=
    (( ${#zle_highlight} )) || zle_highlight=(default:bold)  # bold command = block head (like Warp)
  else
    [[ $PS1 == *'133;B'* ]] || PS1="$PS1%{"$'\e]133;B\a'"%}"
  fi
}
__gembala_preexec() { builtin print -rn -- $'\e]133;C;'"$(builtin print -rn -- "$1" | /usr/bin/base64)"$'\a'; }
autoload -Uz add-zsh-hook && add-zsh-hook precmd __gembala_precmd && add-zsh-hook preexec __gembala_preexec
"#;

/// FR-9: agents typed by hand in a pane get the `gembala` hint too (same flags as profiles).
/// `function name` (not `name()`) so a user alias with the same name doesn't break the definition; the alias still works
/// through this function. Claude/OMP only when opening the TUI (no arguments or first argument is an option), not subcommands.
/// Claude without resume flags gets `--session-id` from the start (FR-68): the daemon reads it from the process arguments.
/// FR-78: Claude and Codex ask the daemon for the add-ons of the pane's current workspace (one argument per line).
/// ponytail: positional `claude "prompt"` gets no hint.
fn zsh_agents(dir: &std::path::Path, exe: &str) -> String {
    let f = q(&dir.join("gembala.md").to_string_lossy());
    // FR-68: typed Codex = same notify hook as the profile; typed OpenCode = Gembala config (hint + plugin)
    let notify = q(&format!("notify={}", json!([exe, "hook", "codex", "notify"])));
    let oc = q(&dir.join("opencode.json").to_string_lossy());
    // FR-67: manual omp gets hooks too (status + message tool) and the omp-specific hint
    let (fo, hook) = (q(&dir.join("gembala.md.omp").to_string_lossy()), q(&format!("--hook={}", dir.join("omp-gembala.ts").to_string_lossy())));
    // FR-4, FR-61: typed Claude = same hooks as the profile (status, session id + owning account); the daemon doesn't read its screen
    let start = q(&claude_hooks(exe, false).to_string());
    format!(
        "function claude {{ [[ -n $CLAUDE_CONFIG_DIR ]] && command gembala trust-sync \"$CLAUDE_CONFIG_DIR\" 2>/dev/null; if (( ! $# )) || [[ $1 == -* ]]; then local s=(); [[ \" $* \" == *\" -\"[rc]\" \"* || \" $* \" == *\" --\"(resume|continue|session-id|fork-session)* ]] || s=(--session-id \"$(uuidgen | tr A-Z a-z)\"); command claude --append-system-prompt \"$(<{f})\" --settings {start} ${{(f)\"$(command gembala addons claude 2>/dev/null)\"}} \"${{s[@]}}\" \"$@\"; else command claude \"$@\"; fi }}\n\
         function omp {{ if (( ! $# )) || [[ $1 == -* ]]; then command omp {hook} \"--append-system-prompt=$(<{fo})\" \"$@\"; else command omp \"$@\"; fi }}\n\
         function codex {{ GEMBALA_SPOOL=\"$(command gembala token 2>/dev/null)\" command codex -c {notify} -c \"$(<{f}.codex)\" ${{(f)\"$(command gembala addons codex 2>/dev/null)\"}} \"$@\" }}\n\
         function opencode {{ OPENCODE_CONFIG=${{OPENCODE_CONFIG:-{oc}}} command opencode \"$@\" }}\n\
         [[ -f {acc} ]] && builtin source {acc}\n",
        acc = q(&dir.join("accounts.zsh").to_string_lossy())
    )
}

/// FR-61: `claude-<account>` in a shell pane = `claude` (with the hooks above) using that account's folder.
/// Rewritten by the daemon each time config loads, so a new account works in panes opened after that.
pub fn zsh_accounts(accounts: &[crate::account::Account]) -> String {
    let mut s = String::from("# made by Gembala (FR-61): one command per agent account\n");
    // a Utama session resumed from a shell on another account (resume command `claude-utama --resume <id>`).
    // local + unset, not a subshell: in `( … )` the pane's foreground process is that zsh, so the daemon
    // doesn't see `claude` (no toolbar, no status)
    if accounts.iter().any(|a| a.agent == "claude") {
        s += "function claude-utama { local CLAUDE_CONFIG_DIR; unset CLAUDE_CONFIG_DIR; claude \"$@\" }\n";
    }
    for a in accounts.iter().filter(|a| a.agent == "claude") {
        let (k, v) = crate::account::env(a);
        let flags: String = crate::account::flags(a).iter().map(|f| format!(" {}", q(f))).collect();
        s += &format!("function claude-{} {{ {k}={} claude{flags} \"$@\" }}\n", a.id, home_q(&v));
    }
    s
}

/// `~/…` outside the quotes so it becomes the home folder
pub fn home_q(v: &str) -> String {
    v.strip_prefix("~/").map_or_else(|| q(v), |rest| format!("\"$HOME\"/{}", q(rest)))
}

const ZSH_RESTORE: &str = "ZDOTDIR=$GEMBALA_USER_ZDOTDIR\n[[ -f \"$ZDOTDIR/.zlogin\" ]] && builtin source \"$ZDOTDIR/.zlogin\"\n\
if [[ $ZDOTDIR == $HOME ]]; then unset ZDOTDIR; else export ZDOTDIR; fi\nunset GEMBALA_USER_ZDOTDIR GEMBALA_ZDOTDIR\n";

/// OpenCode and Oh My Pi hook files, plus the command block zsh files, written by the daemon at start to the Gembala config folder (`0700`/`0600`).
pub fn hook_files(h: &Hooks) -> Vec<(&'static str, String)> {
    let exe = json!(h.exe);
    let plugin = h.dir.join("opencode-gembala.js").to_string_lossy().into_owned();
    vec![
        (".zshenv", zsh_file(".zshenv", "")),
        (".zprofile", zsh_file(".zprofile", "")),
        (".zshrc", zsh_file(".zshrc", &format!("{ZSH_MARKS}{}", zsh_agents(&h.dir, &h.exe)))),
        (".zlogin", format!("# made by Gembala (FR-58); your .zlogin is read as is, then ZDOTDIR is restored\n{ZSH_RESTORE}")),
        (
            "opencode.json",
            json!({"$schema": "https://opencode.ai/config.json", "plugin": [format!("file://{plugin}")],
                "instructions": [h.dir.join("gembala.md").to_string_lossy()]})
            .to_string(),
        ),
        ("gembala.md", format!("{PETUNJUK}\n")),
        ("gembala.md.omp", format!("{PETUNJUK} {OMP_TOOLS}\n")),
        // Codex `-c` value for manual agents (TOML string = JSON string without special characters)
        // FR-66: in the sandbox, the turn's final answer goes out as the reply when `gembala reply` fails
        ("gembala.md.codex", format!("developer_instructions={}", json!(format!("{PETUNJUK} Your final message of a turn is also delivered as the reply to a waiting message.")))),
        (
            "opencode-gembala.js",
            OPENCODE_HOOK.replace("__EXE__", &exe.to_string()),
        ),
        ("omp-gembala.ts", OMP_HOOK.replace("__EXE__", &exe.to_string())),
    ]
}

/// OpenCode plugin: pane status (FR-4). Child sessions (`task` subagents) only pass on permissions/questions: their idle isn't the
/// end of the pane's turn. Any status other than `idle` (busy, retry) = working. Pattern from the herdr/Warp plugins (5 Nov 2026).
const OPENCODE_HOOK: &str = r#"// made by Gembala; pane status (FR-4)
import { execFile } from "node:child_process";
const hook = (s, id) => execFile(__EXE__, ["hook", "opencode", s, String(id ?? "")], () => {});
const child = new Set();
export const Gembala = async () => ({
  event: async ({ event }) => {
    const t = event.type, p = event.properties ?? {};
    if (p.info?.id && p.info.parentID) child.add(p.info.id);
    // FR-68: resume exactly this session; child sessions don't replace the pane session
    const sub = child.has(p.sessionID), id = sub ? "" : p.sessionID;
    if (t === "question.asked" || (t.startsWith("permission.") && t !== "permission.replied")) hook("permission", id);
    else if (t === "permission.replied" || t === "question.replied" || t === "question.rejected") hook("busy", id);
    else if (sub) return;
    else if (t === "session.status") hook(p.status?.type === "idle" ? "idle" : "busy", id);
    else if (t === "session.idle") hook("idle", id);
  },
});
"#;

/// omp hook: pane status (FR-4) and message tool (FR-67). Hook tools don't go through bash, so the user's bash patterns
/// (e.g. `*psql*` = prompt) don't hold a message that happens to mention that word.
const OMP_HOOK: &str = r#"// made by Gembala; pane status (FR-4) and message tools without bash (FR-67)
import { execFile } from "node:child_process";
const exe = __EXE__;
const hook = (s: string) => execFile(exe, ["hook", "omp", s], () => {});
const run = (args: string[], signal?: AbortSignal) =>
  new Promise<string>((ok) => execFile(exe, args, { signal, maxBuffer: 1 << 20 }, (e, out, err) => ok(String(out || err || (e ? e.message : "")).trim())));
export default function (pi: any) {
  pi.on("agent_start", async () => hook("start"));
  // omp retries automatically (willContinue) = turn not done yet; without this a false "done", then working again
  pi.on("agent_end", async (e: any) => { if (e?.willContinue !== true) hook("end"); });
  // `ask` tool = agent waiting for a human answer
  pi.on("tool_execution_start", async (e: any) => { if (e?.toolName === "ask") hook("approval"); });
  pi.on("tool_execution_end", async (e: any) => { if (e?.toolName === "ask") hook("resolved"); });
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
  tool("gembala_reply", "Answer a message labeled [Gembala · from <pane> · <id>].", z.object({ msg_id: z.string(), message: z.string() }), "write", (p) => ["reply", p.msg_id, p.message]);
  tool("gembala_wait", "Wait for the reply to a message you sent.", z.object({ reply_id: z.string(), timeout_s: z.number().optional() }), "read",
    (p) => ["wait", "--reply", p.reply_id, ...(p.timeout_s ? ["--timeout", String(p.timeout_s)] : [])]);
  tool("gembala_read", "Read the last lines of another pane in this workspace.", z.object({ target: z.string(), lines: z.number().optional() }), "read",
    (p) => ["read", p.target, ...(p.lines ? ["-n", String(p.lines)] : [])]);
}
"#;

/// FR-67: extra omp hint: use the hook tool, not bash
pub const OMP_TOOLS: &str = "In omp, use the gembala_status, gembala_send, gembala_reply, gembala_wait and gembala_read tools instead of running gembala in bash.";

/// FR-53: session id from Gembala (UUID) and whether the pane resumes the previous session.
pub struct Session {
    pub id: Option<String>,
    pub resume: bool,
}

/// FR-68: session ids allowed for that agent (written into a shell command): UUID for Claude/Codex,
/// letters, digits, `_`, `-` for OpenCode/omp.
pub fn valid_session(agent: &str, s: &str) -> bool {
    match agent {
        "claude" | "codex" => valid_uuid(s),
        _ => (1..=80).contains(&s.len()) && s.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-'),
    }
}

pub fn valid_uuid(s: &str) -> bool {
    s.len() == 36 && s.chars().enumerate().all(|(i, c)| if [8, 13, 18, 23].contains(&i) { c == '-' } else { c.is_ascii_hexdigit() })
}

/// Claude Code accepts an id from outside; other agents only "resume the last one" (TECH section 7a, `--help` 1 Nov 2026).
fn session_flags(agent: &str, s: &Session) -> Vec<String> {
    let id = s.id.as_deref().filter(|i| valid_session(agent, i));
    match (agent, s.resume, id) {
        ("claude", false, Some(id)) => vec!["--session-id".into(), id.into()],
        ("claude", true, Some(id)) => vec!["--resume".into(), id.into()],
        // FR-68: the exact id, not the last session in the folder
        ("codex", true, Some(id)) => vec!["resume".into(), id.into()],
        ("opencode", true, Some(id)) => vec!["--session".into(), id.into()],
        ("claude", true, None) | ("opencode" | "omp", true, _) => vec!["--continue".into()],
        ("codex", true, _) => vec!["resume".into(), "--last".into()],
        _ => vec![],
    }
}

/// FR-72: the agent's own session picker (`--help` 3 Nov 2026); OpenCode has no picker from the command line
pub fn picker_flags(agent: &str) -> &'static [&'static str] {
    match agent {
        "claude" | "omp" => &["--resume"],
        "codex" => &["resume"],
        _ => &[],
    }
}

/// FR-64: permission mode flags per agent (`--help` 2 Nov 2026). OpenCode has no flag for the TUI (`--auto` is only on `run`): empty.
/// Codex stays sandboxed `workspace-write`; omp `yolo` still honors the user's `bash.patterns` rules.
pub fn permission_flags(agent: &str, permission: &str) -> Vec<String> {
    let f: &[&str] = match (agent, permission) {
        ("claude", "auto") => &["--permission-mode", "auto"],
        ("omp", "auto") => &["--approval-mode=yolo"],
        ("codex", "auto") => &["--ask-for-approval", "never", "--sandbox", "workspace-write"],
        _ => &[],
    };
    f.iter().map(|s| s.to_string()).collect()
}

/// Text run by `$SHELL -l -c` (TECH section 6). `None` = interactive login shell.
/// The agent runs in a subshell so secret env doesn't linger in the pane shell after the agent exits;
/// then `exec $SHELL -l` keeps the pane alive (FR-18). Exit 127 = install message (FR-32).
pub fn launch(p: &Profile, h: Option<&Hooks>, session: Option<&Session>, saver: bool) -> Option<String> {
    if p.command.is_empty() {
        return None;
    }
    let (mut flags, hook_env) = h.map(|h| hook_flags(p, h, saver)).unwrap_or_default();
    flags.extend(permission_flags(&p.agent, &p.permission));
    // before the session flags: Codex `resume <id>` is a subcommand, `-c` must come first
    flags.extend(p.addons.iter().cloned());
    flags.extend(session.map(|s| session_flags(&p.agent, s)).unwrap_or_default());
    // FR-56: invisible markers for the daemon (OSC 777), around the agent session output
    let mut s = String::from(if p.summarize { "printf '\\033]777;gembala;agent-start\\007'; ( " } else { "( " });
    // interactive PATH (already taken by detect at daemon start), so agents only on the `.zshrc` PATH are found
    if let Some(path) = USER_PATH.get().cloned().flatten() {
        s += &format!("export PATH={}:\"$PATH\"; ", q(&path));
    }
    for (k, v) in p.env.iter().chain(&hook_env) {
        // `~/…` outside the quotes so it becomes the home folder (e.g. CLAUDE_CONFIG_DIR per account)
        s += &format!("export {k}={}; ", home_q(v));
    }
    for (k, r) in &p.secret_env {
        // secrets are resolved by the pane shell; their values never pass through the daemon (SEC-32)
        if let Some((svc, acct)) = r.strip_prefix("keychain:").and_then(|x| x.split_once('/')) {
            s += &format!("export {k}=\"$(security find-generic-password -w -s {} -a {})\"; ", q(svc), q(acct));
        } else {
            s += &format!("export {k}=\"$(op read {})\"; ", q(r));
        }
    }
    // FR-66: the Codex sandbox blocks the pane socket; token (name without TOKEN so Codex doesn't drop it) for the spool path
    if p.agent == "codex" {
        s += "export GEMBALA_SPOOL=\"$(gembala token 2>/dev/null)\"; ";
    }
    s += &q(&p.command);
    for a in flags.iter().chain(&p.args) {
        s += &format!(" {}", q(a));
    }
    let install = AGENTS.iter().find(|a| a.0 == p.agent).map(|a| a.3);
    let hint = match install {
        Some(i) => format!("{} isn't in the login shell PATH. Install it with: {i}", p.name),
        None => format!("Command {} isn't in the login shell PATH.", p.command),
    };
    s += " ); s=$?; ";
    if p.summarize {
        s += "printf '\\033]777;gembala;agent-exit;%d\\007' \"$s\"; ";
    }
    s += &format!("[ \"$s\" -eq 127 ] && printf '\\n%s\\n' {}; ", q(&hint));
    // FR-58/59: the hook `.zlogin` already restored the user's ZDOTDIR, so zsh after the agent exits gets the hook
    // folder again (same as a shell pane); without this there are no block markers and no input editor
    match h {
        Some(h) => s += &format!(
            "case \"$SHELL\" in */zsh) exec env ZDOTDIR={} GEMBALA_USER_ZDOTDIR=\"${{ZDOTDIR:-$HOME}}\" \"$SHELL\" -l ;; *) exec \"$SHELL\" -l ;; esac",
            q(&h.dir.to_string_lossy())
        ),
        None => s += "exec \"$SHELL\" -l",
    }
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

/// Interactive login shell PATH, like a pane. Many installers only add to PATH in `.zshrc`
/// (standalone Codex: `~/.local/bin`), which `-l -c` doesn't read. Taken once per daemon.
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

/// FR-28: find agents through the login shell (same PATH as a normal terminal), 10-second limit.
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

/// FR-78/FR-27: RTK installed (same PATH as the panes)? Gembala only shows it; switching it is Fase 3.
/// Taken once per daemon: a pane launch (`rtk` hook, FR-27) must not wait on a process every time.
pub fn rtk_version() -> Option<String> {
    static RTK: std::sync::LazyLock<Option<String>> = std::sync::LazyLock::new(|| {
        let path = [user_path(), std::env::var("PATH").ok()].into_iter().flatten().collect::<Vec<_>>().join(":");
        let mut c = Command::new("rtk");
        c.arg("--version").env("PATH", path).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
        run_with_timeout(c, Duration::from_secs(3)).and_then(|out| version_of(&out))
    });
    RTK.clone()
}

fn version_of(raw: &str) -> Option<String> {
    raw.split(|c: char| !(c.is_ascii_digit() || c == '.'))
        .find(|t| t.split('.').filter(|p| !p.is_empty()).count() >= 2)
        .map(|t| t.trim_matches('.').to_string())
}

pub fn run_with_timeout(mut c: Command, limit: Duration) -> Option<String> {
    let mut child = c.spawn().ok()?;
    // stdout is read while waiting: output above the pipe buffer (~64 KB, e.g. a big git diff) makes the process
    // wait to be read, so reading after it finishes always hangs until the time limit
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
        let (p, e) = from_config(&tables);
        (p, e.iter().map(ToString::to_string).collect())
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
        let s = launch(&p, None, None, false).unwrap();
        assert!(s.starts_with("printf '\\033]777;gembala;agent-start\\007'; ( export X='a b'\\''c'; export D=\"$HOME\"/'.claude-kerja'; "), "{s}");
        assert!(s.contains("printf '\\033]777;gembala;agent-exit;%d\\007' \"$s\""), "{s}");
        let quiet = Profile { summarize: false, ..p.clone() };
        assert!(!launch(&quiet, None, None, false).unwrap().contains("777"));
        assert!(s.contains("export K=\"$(security find-generic-password -w -s 'svc' -a 'me@x')\"; "), "{s}");
        assert!(s.contains("export O=\"$(op read 'op://V/I/f')\"; "), "{s}");
        assert!(s.contains("'my tool' 'it'\\''s' '$(id)' );"), "{s}");
        assert!(s.ends_with("exec \"$SHELL\" -l"), "{s}");
        assert!(s.contains("brew install --cask claude-code"), "{s}");
        assert_eq!(launch(&builtin()[0], None, None, false), None);
    }

    #[test]
    fn mode_izin_jadi_flag_per_agent() {
        let auto = |a: &str| permission_flags(a, "auto").join(" ");
        assert_eq!(auto("claude"), "--permission-mode auto");
        assert_eq!(auto("omp"), "--approval-mode=yolo");
        assert_eq!(auto("codex"), "--ask-for-approval never --sandbox workspace-write");
        assert!(permission_flags("opencode", "auto").is_empty() && permission_flags("claude", "default").is_empty());
        let p = Profile { permission: "auto".into(), ..builtin()[4].clone() };
        assert!(launch(&p, None, None, false).unwrap().contains("'omp' '--approval-mode=yolo' );"));
        let (ps, errs) = parse("[[profile]]\nid = \"omp\"\npermission = \"yolo\"\n");
        assert!(errs.iter().any(|e| e.contains("opts=default, auto")) && ps.iter().all(|x| x.permission == "default"));
    }

    #[test]
    fn launch_jalan_dan_pesan_127() {
        // command missing: the install message shows, then a login shell (here `exit` through a fake SHELL env)
        let p = Profile { command: "gembala-tidak-ada-xyz".into(), ..builtin()[1].clone() };
        let out = Command::new("/bin/sh").env("SHELL", "/usr/bin/true").arg("-c").arg(launch(&p, None, None, false).unwrap()).output().unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(text.contains("Claude Code isn't in the login shell PATH") && text.contains("brew install"), "{text}");
    }

    #[test]
    fn shown_args_tanpa_sisipan_gembala() {
        let h = Hooks { exe: "/Apps/Gem bala/gembala".into(), dir: "/cfg/hooks".into() };
        for (agent, user) in [("claude", vec!["--model", "opus"]), ("codex", vec!["resume", "--last"]), ("omp", vec!["--verbose"])] {
            let p = builtin().into_iter().find(|p| p.id == agent).unwrap();
            let mut args: Vec<String> = vec![agent.into()];
            args.extend(hook_flags(&p, &h, false).0);
            args.extend(user.iter().map(|s| s.to_string()));
            assert_eq!(shown_args(&args), format!("{agent} {}", user.join(" ")));
        }
        // hint from another daemon build + session id from the zsh function: only what the user typed is left
        assert!(PETUNJUK.starts_with(PETUNJUK_START));
        let typed = ["claude", "--append-system-prompt", "You run in a Gembala pane. Older text.", "--model", "opus", "--session-id", "9e38"];
        assert_eq!(shown_args(&typed.map(String::from)), "claude --model opus");
        let addons = ["claude", "--plugin-dir", "/p/caveman", "--mcp-config", "/cfg/mcp/w.json", "--strict-mcp-config", "-c"];
        assert_eq!(shown_args(&addons.map(String::from)), "claude -c");
        let codex = ["codex", "-c", "mcp_servers.probe.command=\"node\"", "resume"];
        assert_eq!(shown_args(&codex.map(String::from)), "codex resume");
    }

    #[test]
    fn hook_dipasang_per_agent() {
        let h = Hooks { exe: "/Apps/Gem bala/gembala".into(), dir: "/cfg/hooks".into() };
        let by = |id: &str| builtin().into_iter().find(|p| p.id == id).unwrap();
        let claude = launch(&by("claude"), Some(&h), None, false).unwrap();
        assert!(claude.contains("'claude' '--settings' '{\"hooks\":"), "{claude}");
        assert!(claude.contains("hook claude stop"), "{claude}");
        // zsh after the agent exits loads the hooks again (input editor + blocks), the user's ZDOTDIR carried along
        assert!(claude.ends_with("exec env ZDOTDIR='/cfg/hooks' GEMBALA_USER_ZDOTDIR=\"${ZDOTDIR:-$HOME}\" \"$SHELL\" -l ;; *) exec \"$SHELL\" -l ;; esac"), "{claude}");
        let tail = "case \"$SHELL\" in */zsh) exec env ZDOTDIR=/h GEMBALA_USER_ZDOTDIR=\"${ZDOTDIR:-$HOME}\" \"$SHELL\" -l ;; *) exec \"$SHELL\" -l ;; esac";
        let out = std::process::Command::new("/bin/sh").args(["-c", &tail.replace("\"$SHELL\" -l ;; *)", "sh -c 'echo $ZDOTDIR $GEMBALA_USER_ZDOTDIR' ;; *)")]).env("SHELL", "/bin/zsh").env("ZDOTDIR", "/u").output().unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "/h /u");
        // `gembala` hint through the official flag; a single quote inside would break the shell quoting
        assert!(!PETUNJUK.contains('\'') && claude.contains(&format!("'--append-system-prompt' '{PETUNJUK}'")), "{claude}");
        assert!(launch(&by("omp"), Some(&h), None, false).unwrap().contains(&format!("'--append-system-prompt={PETUNJUK} {OMP_TOOLS}'")));
        // FR-13: read-only role locked in Claude, brief goes into the hint
        let ro = Profile { readonly: true, brief: "Your role: reviewer.".into(), ..by("claude") };
        let l = launch(&ro, Some(&h), None, false).unwrap();
        assert!(l.contains("'--disallowedTools' 'Edit' 'Write'") && l.contains("Your role: reviewer."), "{l}");
        let c = launch(&Profile { readonly: true, brief: "B".into(), ..by("codex") }, Some(&h), None, false).unwrap();
        assert!(!c.contains("disallowedTools") && c.contains("\\n\\nB"), "{c}");
        let codex = launch(&by("codex"), Some(&h), None, false).unwrap();
        assert!(codex.contains("'-c' 'notify=[\"/Apps/Gem bala/gembala\",\"hook\",\"codex\",\"notify\"]'"), "{codex}");
        assert!(codex.contains(&format!("'-c' 'developer_instructions=\"{PETUNJUK}\"'")), "{codex}");
        let files = hook_files(&h);
        let oc: serde_json::Value = serde_json::from_str(&files.iter().find(|f| f.0 == "opencode.json").unwrap().1).unwrap();
        assert_eq!(oc["instructions"][0], "/cfg/hooks/gembala.md");
        assert!(files.iter().any(|f| f.0 == "gembala.md" && f.1.contains(PETUNJUK)));
        let rc = &files.iter().find(|f| f.0 == ".zshrc").unwrap().1;
        // the agent functions in .zshrc must be valid zsh syntax
        let tmp = std::env::temp_dir().join(format!("gembala-zshrc-{}", std::process::id()));
        std::fs::write(&tmp, rc).unwrap();
        let ok = std::process::Command::new("zsh").arg("-n").arg(&tmp).status().unwrap().success();
        let _ = std::fs::remove_file(&tmp);
        assert!(ok, "{rc}");
        assert!(rc.contains("--session-id") && rc.contains("hook\",\"codex\",\"notify") && rc.contains("function opencode {"), "{rc}");
        assert!(rc.contains("function claude {") && rc.contains("'/cfg/hooks/gembala.md'"), "{rc}");
        assert!(launch(&by("opencode"), Some(&h), None, false).unwrap().contains("export OPENCODE_CONFIG='/cfg/hooks/opencode.json'"));
        assert!(launch(&by("omp"), Some(&h), None, false).unwrap().contains("'--hook=/cfg/hooks/omp-gembala.ts'"));
        assert_eq!(launch(&by("shell"), Some(&h), None, false), None);
        // the Claude hook command runs in a shell: a path with spaces stays one word
        let settings: serde_json::Value = serde_json::from_str(hook_flags(&by("claude"), &h, false).0[1].as_str()).unwrap();
        assert_eq!(settings["hooks"]["Stop"][0]["hooks"][0]["command"], "'/Apps/Gem bala/gembala' hook claude stop");
        assert_eq!(settings["hooks"]["PostToolUse"][0]["hooks"][0]["command"], "'/Apps/Gem bala/gembala' hook claude tool");
        assert_eq!(settings["hooks"]["StopFailure"][0]["hooks"][0]["command"], "'/Apps/Gem bala/gembala' hook claude stop");
        // FR-27: no saver = no RTK entry; saver + rtk found = RTK first, then Gembala's own tool hook
        assert_eq!(settings["hooks"]["PreToolUse"].as_array().map(Vec::len), Some(1), "{settings}");
        let with = claude_hooks("/x/gembala", true);
        assert_eq!(with["hooks"]["PreToolUse"][0]["matcher"], "Bash", "{with}");
        assert_eq!(with["hooks"]["PreToolUse"][0]["hooks"][0]["command"], "rtk hook claude", "{with}");
        assert_eq!(with["hooks"]["PreToolUse"][1]["hooks"][0]["command"], "'/x/gembala' hook claude tool", "{with}");
        // FR-4: a typed claude carries the same hooks (status from hooks, not the screen)
        assert!(rc.contains(&q(&settings.to_string())), "{rc}");
    }

    #[test]
    fn sesi_per_agent() {
        let by = |id: &str| builtin().into_iter().find(|p| p.id == id).unwrap();
        let id = "0b6f2a1e-5c3d-4e7f-8a9b-1c2d3e4f5a6b";
        let new = Session { id: Some(id.into()), resume: false };
        let again = Session { id: Some(id.into()), resume: true };
        assert!(launch(&by("claude"), None, Some(&new), false).unwrap().contains(&format!("'claude' '--session-id' '{id}'")));
        assert!(launch(&by("claude"), None, Some(&again), false).unwrap().contains(&format!("'claude' '--resume' '{id}'")));
        // FR-68: exact id when there is one, "last session in folder" only without an id
        assert!(launch(&by("codex"), None, Some(&again), false).unwrap().contains(&format!("'codex' 'resume' '{id}'")));
        assert!(launch(&by("opencode"), None, Some(&Session { id: Some("ses_4Ab-9".into()), resume: true }), false).unwrap().contains("'opencode' '--session' 'ses_4Ab-9'"));
        // FR-72: the agent's own session picker = picker flag, no new session id
        let mut pick = by("claude");
        pick.args.extend(picker_flags("claude").iter().map(|s| s.to_string()));
        let l = launch(&pick, None, Some(&Session { id: None, resume: false }), false).unwrap();
        assert!(l.contains("'claude' '--resume'") && !l.contains("--session-id"), "{l}");
        assert!(picker_flags("opencode").is_empty());
        let none = Session { id: None, resume: true };
        assert!(launch(&by("codex"), None, Some(&none), false).unwrap().contains("'codex' 'resume' '--last'"));
        assert!(launch(&by("opencode"), None, Some(&none), false).unwrap().contains("'opencode' '--continue'"));
        assert!(!valid_session("opencode", "a b") && !valid_session("codex", "ses_1") && valid_session("omp", "01a19b03-x"));
        let bad = Session { id: Some("x; rm -rf /".into()), resume: false };
        assert!(!launch(&by("claude"), None, Some(&bad), false).unwrap().contains("rm"));
        assert!(valid_uuid(id) && !valid_uuid("0b6f2a1e"));
    }

    #[test]
    fn status_dari_event_hook() {
        assert_eq!(hook_status("claude", "prompt", ""), Some("working"));
        assert_eq!(hook_status("omp", "approval", ""), Some("blocked"));
        assert_eq!(hook_status("omp", "resolved", ""), Some("working"));
        assert_eq!(hook_status("claude", "notify", r#"{"message":"Claude needs your permission to use Bash"}"#), Some("blocked"));
        assert_eq!(hook_status("claude", "notify", r#"{"message":"Claude is waiting for your input"}"#), Some("idle"));
        // the real payload always carries permission_mode: waiting for input stays idle
        let idle = r#"{"permission_mode":"auto","message":"Claude is waiting for your input","notification_type":"idle_prompt"}"#;
        assert_eq!(hook_status("claude", "notify", idle), Some("idle"));
        let ask = r#"{"permission_mode":"auto","message":"Claude needs your permission to use Bash","notification_type":"permission_prompt"}"#;
        assert_eq!(hook_status("claude", "notify", ask), Some("blocked"));
        assert_eq!(hook_status("codex", "notify", r#"{"type":"agent-turn-complete"}"#), Some("done"));
        assert_eq!(hook_status("codex", "notify", r#"{"type":"lain"}"#), None);
        // payload seen from Codex 0.166 (5 Nov 2026): the thread-title side turn is ignored, a real turn isn't
        let title = r#"{"type":"agent-turn-complete","thread-id":"01a1aa30-5331","input-messages":["Generate a concise, single-line task title of at most 36 characters"],"last-assistant-message":"{\"title\":\"Reply hello\"}"}"#;
        assert_eq!(hook_status("codex", "notify", title), None);
        let turn = r#"{"type":"agent-turn-complete","thread-id":"01a1aa2f-ca06","input-messages":["Reply with one word: hello"],"last-assistant-message":"hello"}"#;
        assert_eq!(hook_status("codex", "notify", turn), Some("done"));
        assert_eq!(hook_status("opencode", "permission", ""), Some("blocked"));
        assert_eq!(hook_status("omp", "end", ""), Some("done"));
        assert_eq!(hook_status("x", "y", ""), None);
    }

    #[test]
    fn folder_trust_dialogs_are_recognised() {
        // screens as Claude Code 2.1.311 and Codex 0.167 draw them (10 Nov 2026)
        assert!(trust_prompt(" Quick safety check: Is this a project you created or one you trust?\n ❯ No, exit\n   Yes, I trust this folder\n Enter to confirm · Esc to cancel"));
        assert!(trust_prompt("  Folder access\n  Trust this folder? Codex can read, edit, and run files here\n› 1. Trust and continue\n  2. Back to Agent Command Center\n  enter continue · esc back\n"));
        assert!(!trust_prompt("❯ Try \"fix lint errors\"\n  ⏵⏵ auto mode on (shift+tab to cycle)"));
        // an agent quoting the dialog in its answer, with its own prompt and status line below
        assert!(!trust_prompt("⏺ Claude shows:\n   Yes, I trust this folder\n   Enter to confirm · Esc to cancel\n❯ \n  ⏵⏵ auto mode on (shift+tab to cycle)"));
        assert!(!trust_prompt("› Ask Codex to do anything\n  GPT-6-Luna low · Context 100% left"));
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
        // Codex 0.166 (5 Nov 2026): only the options and footer are left in the tail
        assert_eq!(s("› 1. Yes, proceed (y)\n  3. No, and tell Codex what to do differently (esc)\n\n  Press enter to confirm or esc to cancel", 4000, "working"), "blocked");
        // an agent answer mentioning approve or permission isn't a dialog
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
        // 1 MB > pipe buffer: used to hang until the time limit and come back empty (changes panel "Loading…" for long)
        let mut c = std::process::Command::new("/bin/sh");
        c.args(["-c", "head -c 1048576 /dev/zero | tr '\\0' x"]).stdout(std::process::Stdio::piped());
        let t0 = Instant::now();
        let out = run_with_timeout(c, Duration::from_secs(3)).unwrap();
        assert_eq!(out.len(), 1 << 20);
        assert!(t0.elapsed() < Duration::from_secs(1), "{:?}", t0.elapsed());
    }

    /// FR-78: typed `claude` gets the daemon's add-on lines as separate arguments (spaces kept); no daemon = none
    #[test]
    fn claude_diketik_dapat_addon() {
        let dir = std::env::temp_dir().join(format!("gembala-zsh-addon-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let bin = |name: &str, body: &str| {
            let f = dir.join(name);
            std::fs::write(&f, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(&f, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        };
        bin("claude", "for a in \"$@\"; do printf '<%s>' \"$a\"; done");
        bin("uuidgen", "echo ID");
        std::fs::write(dir.join("gembala.md"), "hint").unwrap();
        let run = |gembala: &str| {
            bin("gembala", gembala);
            let script = format!("{}claude --x", zsh_agents(&dir, "/x/gembala"));
            let out = std::process::Command::new("zsh").args(["-f", "-c", &script]).env("PATH", format!("{}:/usr/bin:/bin", dir.display())).output().unwrap();
            String::from_utf8_lossy(&out.stdout).into_owned()
        };
        let out = run("[ \"$1 $2\" = 'addons claude' ] && printf -- '--plugin-dir\\n/p a/x\\n'");
        assert!(out.ends_with("<--plugin-dir></p a/x><--session-id><id><--x>"), "{out}");
        assert!(run("exit 1").ends_with("}><--session-id><id><--x>"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn claude_per_akun_di_shell() {
        let a = |id: &str, agent: &str, dir: &str| crate::account::Account { id: id.into(), agent: agent.into(), name: id.into(), dir: dir.into(), share: vec![], model: String::new(), effort: String::new(), trust: true };
        let rc = zsh_accounts(&[a("kantor", "claude", "~/.claude-kantor"), a("x", "codex", "~/.codex-x")]);
        assert!(!rc.contains("claude-x"), "{rc}");
        // the `claude` function (hooks) gets the account folder through env that reaches child processes
        let script = format!("function claude {{ command env | grep '^CLAUDE_CONFIG_DIR=' }}\n{rc}claude-kantor --versi");
        let out = std::process::Command::new("zsh").args(["-f", "-c", &script]).env("HOME", "/rumah").output().unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "CLAUDE_CONFIG_DIR=/rumah/.claude-kantor", "{rc}");
        // FR-61 (8): account model/effort become flags before the user's arguments
        let mut k = a("kantor", "claude", "~/.claude-kantor");
        (k.model, k.effort) = ("opus".into(), "high".into());
        let script = format!("function claude {{ print -r -- \"$*\" }}\n{}claude-kantor --resume x", zsh_accounts(&[k]));
        let out = std::process::Command::new("zsh").args(["-f", "-c", &script]).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "--model opus --effort high --resume x");
        // Utama session from a Kantor account shell: `claude-utama` drops the account folder for that command only,
        // without a subshell (subshell depth 0) so `claude` is the pane's foreground process
        let script = format!("export CLAUDE_CONFIG_DIR=/k\nfunction claude {{ print -r -- \"${{CLAUDE_CONFIG_DIR:-utama}} $ZSH_SUBSHELL $*\" }}\n{}claude-utama --resume x; print -r -- $CLAUDE_CONFIG_DIR", zsh_accounts(&[a("kantor", "claude", "~/.claude-kantor")]));
        let out = std::process::Command::new("zsh").args(["-f", "-c", &script]).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "utama 0 --resume x\n/k");
        assert!(!zsh_accounts(&[]).contains("claude-utama"));
    }

    #[test]
    fn claude_diketik_membawa_session_start() {
        let rc = zsh_agents(std::path::Path::new("/cfg/hooks"), "/Apps/Gem bala/gembala");
        assert!(rc.contains("--settings '{\"hooks\":{") && rc.contains("\"SessionStart\"") && rc.contains("hook claude start") && rc.contains("hook claude stop"), "{rc}");
        assert_eq!(hook_status("claude", "start", "{}"), Some("start"));
    }
}
