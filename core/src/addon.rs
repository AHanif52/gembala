//! FR-78: add-ons per launch. Claude Code plugins (`--plugin-dir`) and per-workspace MCP servers
//! (`--mcp-config`, `--strict-mcp-config`; Codex `-c mcp_servers.*`). Only folders and files the user put in
//! `config.toml` are used, never one found in the repo or cwd (SEC-27 pattern); agent config is not touched (SEC-15).

use serde_json::{Value, json};
use std::path::{Component, Path, PathBuf};

pub struct Plugin {
    pub id: String,
    /// as written in config.toml (`~/…` or absolute); `dir` is the expanded folder
    pub path: String,
    pub dir: PathBuf,
    /// `off` | `all` | `workspaces`
    pub scope: String,
    /// workspace ids (layout.json), used when scope = `workspaces`
    pub workspaces: Vec<String>,
}

pub struct Mcp {
    pub workspace: String,
    /// relative to the Gembala config folder, as written; `path` is the resolved file
    pub file: String,
    pub path: PathBuf,
    /// "Cuma ini": Claude skips the global MCP servers
    pub strict: bool,
    /// server names in the file, for the Settings page
    pub servers: Vec<String>,
}

#[derive(Default)]
pub struct Addons {
    pub plugins: Vec<Plugin>,
    pub mcp: Vec<Mcp>,
}

const SCOPES: &[&str] = &["off", "all", "workspaces"];

pub fn valid_key(s: &str) -> bool {
    (1..=64).contains(&s.len()) && s.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
}

/// `[addons.plugin.<id>]` and `[workspace.<id>.mcp]` (TECH, FR-78). `base` = Gembala config folder.
pub fn from_config(doc: Option<&toml_edit::DocumentMut>, base: &Path) -> (Addons, Vec<crate::diag::Diag>) {
    use crate::diag::Diag;
    let (mut out, mut errors) = (Addons::default(), Vec::new());
    fn table(item: Option<&toml_edit::Item>) -> Option<&dyn toml_edit::TableLike> {
        item.and_then(|t| t.as_table_like())
    }
    let plugins = table(doc.and_then(|d| d.get("addons")).and_then(|a| a.get("plugin")));
    for (id, t) in plugins.iter().flat_map(|t| t.iter()) {
        let at = |k: &str| format!("addons.plugin.{id}.{k}");
        let Some(t) = t.as_table_like().filter(|_| valid_key(id)) else {
            errors.push(Diag::new("plugin_id").at(format!("addons.plugin.{id}")).then("skip_plugin"));
            continue;
        };
        let s = |k: &str| t.get(k).and_then(|v| v.as_str()).map(str::to_string);
        // a relative folder would resolve against the pane's cwd, i.e. the repo
        let Some(path) = s("path").filter(|p| (p.starts_with('/') || p.starts_with("~/")) && !p.chars().any(char::is_control)) else {
            errors.push(Diag::new("abs_dir").at(at("path")).then("skip_plugin"));
            continue;
        };
        let dir = crate::account::expand(&path);
        if !dir.join(".claude-plugin/plugin.json").is_file() {
            errors.push(Diag::new("no_plugin_json").at(at("path")).then("skip_plugin"));
            continue;
        }
        let scope = match s("scope") {
            None => "all".to_string(),
            Some(v) if SCOPES.contains(&v.as_str()) => v,
            Some(_) => {
                errors.push(Diag::new("one_of").at(at("scope")).arg("opts", SCOPES.join(", ")).then("plugin_off"));
                "off".to_string()
            }
        };
        let workspaces = t.get("workspaces").and_then(|v| v.as_array()).map(|a| a.iter().filter_map(|v| v.as_str()).filter(|w| valid_key(w)).map(String::from).collect()).unwrap_or_default();
        out.plugins.push(Plugin { id: id.to_string(), path, dir, scope, workspaces });
    }
    let workspaces = table(doc.and_then(|d| d.get("workspace")));
    for (ws, t) in workspaces.iter().flat_map(|t| t.iter()) {
        let Some(m) = t.get("mcp").and_then(|m| m.as_table_like()) else { continue };
        let at = format!("workspace.{ws}.mcp.file");
        let file = m.get("file").and_then(|v| v.as_str()).unwrap_or_default();
        // decision 4 Nov 2026: only a file inside the Gembala config folder (no absolute path, no `..`)
        let inside = !file.is_empty() && !file.chars().any(char::is_control) && Path::new(file).components().all(|c| matches!(c, Component::Normal(_)));
        if !valid_key(ws) || !inside {
            errors.push(Diag::new("mcp_path").at(&at).then("skip_mcp"));
            continue;
        }
        let path = base.join(file);
        if !path.is_file() {
            errors.push(Diag::new("file_missing").at(&at).arg("file", file).then("skip_mcp"));
            continue;
        }
        let strict = m.get("strict").and_then(|v| v.as_bool()).unwrap_or(false);
        let servers = servers(&path).into_iter().map(|(n, _)| n).collect();
        out.mcp.push(Mcp { workspace: ws.to_string(), file: file.to_string(), path, strict, servers });
    }
    (out, errors)
}

impl Plugin {
    fn on(&self, ws: &str) -> bool {
        self.scope == "all" || (self.scope == "workspaces" && self.workspaces.iter().any(|w| w == ws))
    }
}

/// Launch flags for `agent` in workspace `ws`. Agents without an equivalent flag get none ("belum didukung").
pub fn args(a: &Addons, agent: &str, ws: &str) -> Vec<String> {
    let mcp = a.mcp.iter().find(|m| !ws.is_empty() && m.workspace == ws);
    let mut out = Vec::new();
    match agent {
        "claude" => {
            for p in a.plugins.iter().filter(|p| p.on(ws)) {
                out.extend(["--plugin-dir".to_string(), p.dir.to_string_lossy().into_owned()]);
            }
            if let Some(m) = mcp {
                out.extend(["--mcp-config".to_string(), m.path.to_string_lossy().into_owned()]);
                if m.strict {
                    out.push("--strict-mcp-config".into());
                }
            }
        }
        // checked with codex 0.166.0 on 4 Nov 2026: a server given by `-c` is spawned and its tools listed.
        // Codex has no strict flag, so its own servers stay loaded.
        "codex" => out.extend(mcp.map(|m| codex_mcp(&m.path)).unwrap_or_default()),
        _ => {}
    }
    out
}

/// `{"mcpServers": {name: {command, args} | {url}}}` (Claude's format) as Codex `-c` overrides.
/// JSON strings and string arrays are valid TOML values.
/// ponytail: `env` is not passed, its values would show in the process list; add Codex `env_vars` if a server needs it
fn codex_mcp(file: &Path) -> Vec<String> {
    let mut out = Vec::new();
    for (name, s) in servers(file) {
        let mut set = |k: &str, v: &Value| out.extend(["-c".to_string(), format!("mcp_servers.{name}.{k}={v}")]);
        if let Some(cmd) = s["command"].as_str() {
            set("command", &json!(cmd));
            if let Some(args) = s["args"].as_array().filter(|a| a.iter().all(Value::is_string)) {
                set("args", &Value::Array(args.clone()));
            }
        } else if let Some(url) = s["url"].as_str() {
            set("url", &json!(url));
        }
    }
    out
}

/// `mcpServers` entries with a usable name; unreadable or malformed file = none
fn servers(file: &Path) -> Vec<(String, Value)> {
    let v: Value = std::fs::read_to_string(file).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    v["mcpServers"].as_object().into_iter().flatten().filter(|(n, _)| valid_key(n)).map(|(n, s)| (n.clone(), s.clone())).collect()
}

/// For the Settings page (FR-78): what is configured, not the files' contents.
pub fn to_json(a: &Addons) -> Value {
    json!({
        "plugins": a.plugins.iter().map(|p| json!({"id": p.id, "path": p.path, "scope": p.scope, "workspaces": p.workspaces})).collect::<Vec<_>>(),
        "mcp": a.mcp.iter().map(|m| json!({"workspace": m.workspace, "file": m.file, "strict": m.strict, "servers": m.servers})).collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (PathBuf, toml_edit::DocumentMut) {
        let base = std::env::temp_dir().join(format!("gembala-addon-{}", std::process::id()));
        let plug = base.join("plugins/caveman");
        std::fs::create_dir_all(plug.join(".claude-plugin")).unwrap();
        std::fs::write(plug.join(".claude-plugin/plugin.json"), "{}").unwrap();
        std::fs::create_dir_all(base.join("plugins/empty")).unwrap();
        std::fs::create_dir_all(base.join("mcp")).unwrap();
        std::fs::write(base.join("mcp/ws1.json"), r#"{"mcpServers": {"probe": {"command": "node", "args": ["srv.js", "a \"b\""]}, "web": {"url": "https://x.test/mcp"}, "bad name": {"command": "x"}}}"#).unwrap();
        let p = |n: &str| base.join("plugins").join(n).to_string_lossy().into_owned();
        let doc = format!(
            "[addons.plugin.caveman]\npath = \"{}\"\n[addons.plugin.ponytail]\npath = \"{}\"\nscope = \"workspaces\"\nworkspaces = [\"ws2\"]\n\
             [addons.plugin.empty]\npath = \"{}\"\n[addons.plugin.rel]\npath = \"plugins/caveman\"\n\
             [workspace.ws1.mcp]\nfile = \"mcp/ws1.json\"\nstrict = true\n[workspace.ws2.mcp]\nfile = \"../x.json\"\n[workspace.ws3.mcp]\nfile = \"/etc/x.json\"\n",
            p("caveman"), p("caveman"), p("empty")
        );
        (base, doc.parse().unwrap())
    }

    #[test]
    fn plugins_and_mcp_per_workspace() {
        let (base, doc) = setup();
        let (a, errors) = from_config(Some(&doc), &base);
        assert_eq!(a.plugins.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(), ["caveman", "ponytail"]);
        // empty folder, relative folder, `..` and absolute MCP file are each reported and skipped
        assert_eq!(errors.len(), 4, "{errors:?}");
        let dir = base.join("plugins/caveman").to_string_lossy().into_owned();
        let mcp = base.join("mcp/ws1.json").to_string_lossy().into_owned();
        assert_eq!(args(&a, "claude", "ws1"), ["--plugin-dir", &dir, "--mcp-config", &mcp, "--strict-mcp-config"]);
        assert_eq!(args(&a, "claude", "ws2"), ["--plugin-dir", &dir, "--plugin-dir", &dir]);
        assert!(args(&a, "omp", "ws1").is_empty() && args(&a, "opencode", "ws1").is_empty());
        assert_eq!(
            args(&a, "codex", "ws1"),
            ["-c", r#"mcp_servers.probe.command="node""#, "-c", r#"mcp_servers.probe.args=["srv.js","a \"b\""]"#, "-c", r#"mcp_servers.web.url="https://x.test/mcp""#]
        );
        assert!(args(&a, "codex", "ws2").is_empty());
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn codex_override_parses_as_toml() {
        // the `-c` value Codex receives must be TOML: escaped JSON strings and arrays are
        let v: toml_edit::Value = r#"["srv.js","a \"b\"","tab\t"]"#.parse().unwrap();
        assert_eq!(v.as_array().unwrap().get(1).unwrap().as_str(), Some("a \"b\""));
    }
}
