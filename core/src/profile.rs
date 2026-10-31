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
const SECRET: &[&str] = &["sk-ant-", "sk-", "ghp_", "github_pat_", "AKIA", "PRIVATE KEY", "xoxb-", "xoxp-"];

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
                } else if !secret && (!ok_text(v, 1000) || SECRET.iter().any(|s| v.contains(s))) {
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

/// Teks yang dijalankan `$SHELL -l -c` (TECH bagian 6). `None` = shell login interaktif.
/// Agent jalan di subshell supaya env rahasia tidak tertinggal di shell pane setelah agent keluar;
/// lalu `exec $SHELL -l` membuat pane tetap hidup (FR-18). Exit 127 = pesan instal (FR-32).
pub fn launch(p: &Profile) -> Option<String> {
    if p.command.is_empty() {
        return None;
    }
    let mut s = String::from("( ");
    for (k, v) in &p.env {
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
    for a in &p.args {
        s += &format!(" {}", q(a));
    }
    let install = AGENTS.iter().find(|a| a.0 == p.agent).map(|a| a.3);
    let hint = match install {
        Some(i) => format!("{} tidak ditemukan di PATH login shell. Pasang dengan: {i}", p.name),
        None => format!("Perintah {} tidak ditemukan di PATH login shell.", p.command),
    };
    s += &format!(" ); [ $? -eq 127 ] && printf '\\n%s\\n' {}; exec \"$SHELL\" -l", q(&hint));
    Some(s)
}

pub fn to_json(p: &Profile) -> Value {
    json!({
        "id": p.id, "name": p.name, "agent": p.agent, "command": p.command, "args": p.args, "cwd": p.cwd,
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

fn run_with_timeout(mut c: Command, limit: Duration) -> Option<String> {
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
            "[[profile]]\nid = \"claude\"\n[profile.env]\nANTHROPIC_API_KEY = \"sk-ant-abc\"\nOK = \"biasa\"\n\
             [profile.secret_env]\nA = \"op://Private/Anthropic/credential\"\nB = \"keychain:anthropic/aufa@x.id\"\nC = \"op://x\"\nD = \"$(rm -rf /)\"\n",
        );
        let p = ps.iter().find(|p| p.id == "claude").unwrap();
        assert_eq!(p.env, [("OK".to_string(), "biasa".to_string())]);
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
        let s = launch(&p).unwrap();
        assert!(s.starts_with("( export X='a b'\\''c'; "), "{s}");
        assert!(s.contains("export K=\"$(security find-generic-password -w -s 'svc' -a 'me@x')\"; "), "{s}");
        assert!(s.contains("export O=\"$(op read 'op://V/I/f')\"; "), "{s}");
        assert!(s.contains("'my tool' 'it'\\''s' '$(id)' );"), "{s}");
        assert!(s.ends_with("exec \"$SHELL\" -l"), "{s}");
        assert!(s.contains("brew install --cask claude-code"), "{s}");
        assert_eq!(launch(&builtin()[0]), None);
    }

    #[test]
    fn launch_jalan_dan_pesan_127() {
        // perintah tidak ada: pesan instal tampil, lalu shell login (di sini `exit` lewat env SHELL palsu)
        let p = Profile { command: "gembala-tidak-ada-xyz".into(), ..builtin()[1].clone() };
        let out = Command::new("/bin/sh").env("SHELL", "/usr/bin/true").arg("-c").arg(launch(&p).unwrap()).output().unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(text.contains("Claude Code tidak ditemukan") && text.contains("brew install"), "{text}");
    }

    #[test]
    fn versi_dari_teks() {
        assert_eq!(version_of("2.1.309 (Claude Code)").as_deref(), Some("2.1.309"));
        assert_eq!(version_of("codex-cli 0.149.0").as_deref(), Some("0.149.0"));
        assert_eq!(version_of("omp/18.13.3").as_deref(), Some("18.13.3"));
        assert_eq!(version_of("tanpa versi"), None);
    }
}
