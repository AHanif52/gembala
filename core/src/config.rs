//! `config.toml` (TECH section 5, FR-42): validation per key, defaults for broken values,
//! the file is never overwritten automatically. Settings writes go through `toml_edit` so comments survive.

use serde_json::{Map, Value, json};
use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use toml_edit::DocumentMut;

use crate::diag::Diag;

enum Kind {
    Int(i64, i64, i64), // min, max, default
    Float(f64, f64, f64),
    Bool(bool),
    Enum(&'static [&'static str], &'static str),
    Str(&'static str),
    /// List of ids from the fixed choices, order = display (default)
    List(&'static [&'static str], &'static [&'static str]),
    /// Free list of names (e.g. workspaces), default empty
    Names,
}

use Kind::*;

// ponytail: only the sections the app uses so far; notify/app/toolbar/workspace/profile get added in M4–M7
const UI: &[(&str, Kind)] = &[
    // BRD 11: system = first of id/en in the macOS language list, else English (TECH 20)
    ("language", Enum(&["system", "id", "en"], "system")),
    // FR-97: simple = everyday words, technical fields folded; full = everything; ask = not answered yet (asked once)
    ("mode", Enum(&["ask", "simple", "full"], "ask")),
    ("theme", Enum(&["system", "light", "dark"], "system")),
    ("font_family", Str("Hack")),
    ("font_size", Int(8, 32, 13)),
    ("line_height", Float(1.0, 2.0, 1.2)),
    ("cursor", Enum(&["bar", "block", "underline"], "bar")),
    ("cursor_blink", Bool(true)),
    ("option_as_meta", Enum(&["none", "left", "right", "both"], "left")),
    // measured 11 Nov 2026 (TECH 24): a full scrollback costs WebKit ~8 KB per line per live pane; 5,000 lines × 4 busy panes
    // ≈ +125 MB over 1,000. 2,000 keeps a long agent answer on screen; older output is in the daemon log (FR-62)
    ("scrollback_lines", Int(0, 100_000, 2_000)),
    ("screen_reader", Bool(false)),
    // FR-59: editor input at the zsh prompt; the shell prompt is hidden (applies to new panes)
    ("input_editor", Bool(true)),
    // FR-58: command block timeline in shell panes (line, dot, block head, Rerun/Edit/Copy buttons);
    // off = plain terminal, OSC 133 markers are still tracked for block jumps
    ("command_timeline", Bool(true)),
    // Codex footer shown like Claude's status (model, context, 5h/weekly usage); off = the user's own Codex footer
    ("codex_status", Bool(true)),
    // FR-20: Cmd+Q = app closes, agents keep running (keep) or every pane is stopped (stop)
    ("on_quit", Enum(&["keep", "stop"], "keep")),
    // FR-86: minutes a helper pane (FR-75) may sit quiet after its first reply before it's closed; 0 = never
    ("helper_idle_close", Int(0, 240, 15)),
];
const CHIPS: &[&str] = &["attach", "diff", "rich_input", "cwd", "branch", "settings"];
/// FR-55: content and order of the agent toolbar chips
const TOOLBAR: &[(&str, Kind)] = &[("left", List(CHIPS, &["attach", "diff", "rich_input"])), ("right", List(CHIPS, &["cwd", "branch", "settings"]))];
const LIMITS: &[(&str, Kind)] = &[("max_panes", Int(1, 256, 32))];
/// FR-40: banner macOS + badge Dock + Dock melompat. FR-87: one switch per event kind; `on_blocked` stays the parent of
/// permission/answer/password, and `sound_done` is the only sound that starts off (TECH 27.5)
const NOTIFY: &[(&str, Kind)] = &[
    ("on_blocked", Bool(true)),
    ("on_done", Bool(true)),
    ("long_command_secs", Int(5, 3600, 30)),
    ("sound", Bool(true)),
    ("on_permission", Bool(true)),
    ("on_answer", Bool(true)),
    ("on_password", Bool(true)),
    ("on_limit", Bool(true)),
    ("on_approve", Bool(true)),
    ("on_failed", Bool(true)),
    ("sound_done", Bool(false)),
];
/// FR-80: Telegram bot; token and bound chat live in telegram.json, not here (SEC-36)
const TELEGRAM: &[(&str, Kind)] = &[("away_idle_mins", Int(1, 120, 5)), ("snippet", Bool(true)), ("snippet_off", Names), ("lines", Int(1, 30, 30))];
/// FR-82: check GitHub for a newer release (the only network use besides the Telegram bot)
const UPDATE: &[(&str, Kind)] = &[("check", Bool(true))];
/// FR-74: Pengaturan › Harness. Defaults for recipes that leave a value out (SEC-30 caps still apply) and the harness
/// notifications (gate waiting, a stage asking for permission, run done or stopped)
const HARNESS: &[(&str, Kind)] = &[
    ("max_rounds", Int(0, 3, 2)),
    ("total_mins", Int(10, 120, 120)),
    ("fallback_on", List(&["exit", "timeout", "unknown", "limit"], &["exit", "timeout", "unknown"])),
    ("plan_gate", Bool(false)),
    ("token_saver", Bool(false)),
    ("notify_gate", Bool(true)),
    ("notify_blocked", Bool(true)),
    ("notify_done", Bool(true)),
];
const SECTIONS: &[(&str, &[(&str, Kind)])] = &[("ui", UI), ("limits", LIMITS), ("notify", NOTIFY), ("toolbar", TOOLBAR), ("telegram", TELEGRAM), ("update", UPDATE), ("harness", HARNESS)];

pub struct Config {
    /// Built-in profiles + `[[profile]]` entries that pass validation (FR-29). Secret references come along, values don't.
    pub profiles: Vec<crate::profile::Profile>,
    /// `{ui, limits, keys}` in effect (defaults for broken or missing values).
    pub values: Value,
    pub errors: Vec<Diag>,
    /// The file can't be parsed at all: the caller uses the last valid values.
    pub broken: bool,
    /// FR-61: `[[account]]` entries that pass validation
    pub accounts: Vec<crate::account::Account>,
    /// FR-78: plugin and per-workspace MCP add-ons that passed validation
    pub addons: crate::addon::Addons,
}

pub fn path() -> PathBuf {
    if let Some(p) = std::env::var_os("GEMBALA_CONFIG") {
        return p.into();
    }
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| "/".into());
    home.join("Library/Application Support/Gembala/config.toml")
}

/// Layout (FR-45) next to config: workspace structure, tabs, splits, profiles, folders. No screen content (SEC-13).
pub fn layout_path() -> PathBuf {
    path().with_file_name("layout.json")
}

fn default(kind: &Kind) -> Value {
    match kind {
        Int(_, _, d) => json!(d),
        Float(_, _, d) => json!(d),
        Bool(d) => json!(d),
        Enum(_, d) | Str(d) => json!(d),
        List(_, d) => json!(d),
        Names => json!([]),
    }
}

fn check(kind: &Kind, v: &Value) -> Result<Value, Diag> {
    match kind {
        Int(lo, hi, _) => v.as_i64().filter(|n| (lo..=hi).contains(&n)).map(|n| json!(n)).ok_or_else(|| Diag::new("int_range").arg("lo", lo).arg("hi", hi)),
        Float(lo, hi, _) => v.as_f64().filter(|n| (*lo..=*hi).contains(n)).map(|n| json!(n)).ok_or_else(|| Diag::new("num_range").arg("lo", lo).arg("hi", hi)),
        Bool(_) => v.as_bool().map(|b| json!(b)).ok_or("bool".into()),
        Enum(opts, _) => v.as_str().filter(|s| opts.contains(s)).map(|s| json!(s)).ok_or_else(|| Diag::new("one_of").arg("opts", opts.join(", "))),
        List(opts, _) => v
            .as_array()
            .filter(|a| a.len() <= opts.len() && a.iter().all(|x| x.as_str().is_some_and(|s| opts.contains(&s))))
            .map(|a| Value::Array(a.clone()))
            .ok_or_else(|| Diag::new("list_of").arg("opts", opts.join(", "))),
        Names => v
            .as_array()
            .filter(|a| a.len() <= 64 && a.iter().all(|x| x.as_str().is_some_and(|s| !s.is_empty() && s.len() <= 64 && !s.chars().any(char::is_control))))
            .map(|a| Value::Array(a.clone()))
            .ok_or("names".into()),
        Str(_) => v
            .as_str()
            .filter(|s| !s.is_empty() && s.len() <= 200 && !s.chars().any(char::is_control))
            .map(|s| json!(s))
            .ok_or("text_len".into()),
    }
}

fn to_json(item: &toml_edit::Item) -> Value {
    if let Some(n) = item.as_integer() {
        json!(n)
    } else if let Some(f) = item.as_float() {
        json!(f)
    } else if let Some(b) = item.as_bool() {
        json!(b)
    } else if let Some(s) = item.as_str() {
        json!(s)
    } else if let Some(a) = item.as_array() {
        Value::Array(a.iter().map(|v| v.as_str().map_or(Value::Null, |s| json!(s))).collect())
    } else {
        Value::Null
    }
}

/// `None` = file doesn't exist yet (all defaults, no errors).
pub fn load(text: Option<&str>) -> Config {
    let mut errors = Vec::new();
    let doc = match text.map(str::parse::<DocumentMut>) {
        Some(Err(e)) => {
            errors.push(Diag::new("toml_parse").arg("why", e.to_string().trim()));
            return Config { values: defaults(), errors, broken: true, profiles: crate::profile::builtin(), accounts: Vec::new(), addons: Default::default() };
        }
        Some(Ok(d)) => Some(d),
        None => None,
    };
    let mut values = Map::new();
    for (section, spec) in SECTIONS {
        let table = doc.as_ref().and_then(|d| d.get(section));
        let mut out = Map::new();
        for (key, kind) in *spec {
            let v = match table.and_then(|t| t.get(key)) {
                None => default(kind),
                Some(item) => check(kind, &to_json(item)).unwrap_or_else(|why| {
                    errors.push(why.at(format!("{section}.{key}")).then("default").arg("default", default(kind)));
                    default(kind)
                }),
            };
            out.insert(key.to_string(), v);
        }
        if let Some(t) = table.and_then(|t| t.as_table_like()) {
            for (k, _) in t.iter().filter(|(k, _)| !spec.iter().any(|(s, _)| s == k)) {
                errors.push(Diag::new("unknown_key").at(format!("{section}.{k}")).then("ignored"));
            }
        }
        values.insert(section.to_string(), Value::Object(out));
    }
    // [keys] passed through as is (text → text); the UI knows the action list and flags conflicts
    let mut keys = Map::new();
    if let Some(t) = doc.as_ref().and_then(|d| d.get("keys")).and_then(|t| t.as_table_like()) {
        for (k, v) in t.iter() {
            match v.as_str() {
                Some(a) => drop(keys.insert(k.to_string(), json!(a))),
                None => errors.push(Diag::new("key_not_action").at(format!("keys.\"{k}\"")).then("ignored")),
            }
        }
    }
    values.insert("keys".into(), Value::Object(keys));
    let item = doc.as_ref().and_then(|d| d.get("profile"));
    let arr = item.and_then(|v| v.as_array_of_tables());
    if item.is_some() && arr.is_none() {
        errors.push(Diag::new("not_array_table").at("profile").then("ignored"));
    }
    let tables: Vec<&dyn toml_edit::TableLike> = arr.map(|a| a.iter().map(|t| t as &dyn toml_edit::TableLike).collect()).unwrap_or_default();
    let (mut profiles, perr) = crate::profile::from_config(&tables);
    errors.extend(perr);
    // FR-61: accounts, then account references in profiles (unknown account or duplicate env = profile uses Utama)
    let item = doc.as_ref().and_then(|d| d.get("account"));
    let arr = item.and_then(|v| v.as_array_of_tables());
    if item.is_some() && arr.is_none() {
        errors.push(Diag::new("not_array_table").at("account").then("ignored"));
    }
    let tables: Vec<&dyn toml_edit::TableLike> = arr.map(|a| a.iter().map(|t| t as &dyn toml_edit::TableLike).collect()).unwrap_or_default();
    let (accounts, aerr) = crate::account::from_config(&tables);
    errors.extend(aerr);
    for p in profiles.iter_mut().filter(|p| !p.account.is_empty()) {
        let why = if !accounts.iter().any(|a| a.id == p.account && a.agent == p.agent) {
            Some("no_such_account")
        } else if crate::account::conflicts(&p.env) {
            Some("account_env_conflict")
        } else {
            None
        };
        if let Some(why) = why {
            errors.push(Diag::new(why).at(format!("profile {}.account", p.id)).then("main_account"));
            p.account.clear();
        }
    }
    values.insert("profiles".into(), profiles.iter().map(crate::profile::to_json).collect());
    values.insert("accounts".into(), accounts.iter().map(crate::account::to_json).collect());
    let (mut addons, aerr) = crate::addon::from_config(doc.as_ref(), path().parent().unwrap_or(Path::new("/")));
    errors.extend(aerr);
    addons.codex_status = values.get("ui").and_then(|u| u["codex_status"].as_bool()).unwrap_or(true);
    values.insert("addons".into(), crate::addon::to_json(&addons));
    Config { values: Value::Object(values), errors, broken: false, profiles, accounts, addons }
}

fn defaults() -> Value {
    let mut values = Map::new();
    for (section, spec) in SECTIONS {
        let out: Map<String, Value> = spec.iter().map(|(k, kind)| (k.to_string(), default(kind))).collect();
        values.insert(section.to_string(), Value::Object(out));
    }
    values.insert("keys".into(), json!({}));
    values.insert("profiles".into(), crate::profile::builtin().iter().map(crate::profile::to_json).collect());
    Value::Object(values)
}

/// Change one key from Settings. Comments and file order stay; a broken file isn't touched.
pub fn set(path: &Path, section: &str, key: &str, value: &Value) -> Result<(), Diag> {
    let kind = SECTIONS
        .iter()
        .find(|(s, _)| *s == section)
        .and_then(|(_, spec)| spec.iter().find(|(k, _)| *k == key))
        .map(|(_, kind)| kind)
        .ok_or_else(|| Diag::new("unknown_key").at(format!("{section}.{key}")))?;
    let v = check(kind, value).map_err(|why| why.at(format!("{section}.{key}")))?;
    let text = match fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(io_err("io_read", e)),
    };
    let mut doc: DocumentMut = text.parse().map_err(|_| Diag::new("config_broken"))?;
    if doc.get(section).is_none() {
        doc[section] = toml_edit::table();
    }
    doc[section][key] = match v {
        Value::Bool(b) => toml_edit::value(b),
        Value::Number(n) if n.is_i64() => toml_edit::value(n.as_i64().unwrap()),
        Value::Number(n) => toml_edit::value(n.as_f64().unwrap_or_default()),
        Value::String(s) => toml_edit::value(s),
        Value::Array(a) => toml_edit::value(a.iter().filter_map(Value::as_str).collect::<toml_edit::Array>()),
        _ => unreachable!("check hanya menghasilkan skalar atau daftar teks"),
    };
    write_atomic(path, doc.to_string().as_bytes()).map_err(|e| io_err("io_write", e))
}

/// OS error while reading/writing config: the OS text stays as is
pub fn io_err(code: &'static str, e: io::Error) -> Diag {
    Diag::new(code).arg("os", e)
}

fn read_doc(path: &Path) -> Result<(DocumentMut, Vec<Diag>), Diag> {
    let text = match fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(io_err("io_read", e)),
    };
    let doc: DocumentMut = text.parse().map_err(|_| Diag::new("config_broken"))?;
    Ok((doc, load(Some(&text)).errors))
}

/// Write only when it adds no new error; the first new error goes back to Settings, the file isn't touched.
fn commit(path: &Path, doc: &DocumentMut, before: &[Diag]) -> Result<(), Diag> {
    let text = doc.to_string();
    if let Some(fresh) = load(Some(&text)).errors.into_iter().find(|e| !before.contains(e)) {
        return Err(fresh);
    }
    write_atomic(path, text.as_bytes()).map_err(|e| io_err("io_write", e))
}

/// FR-29: save a profile from Settings to `[[profile]]` with the same id (or add a new one).
pub fn save_profile(path: &Path, p: &Value) -> Result<(), Diag> {
    let id = p["id"].as_str().ok_or_else(|| Diag::new("required").at("id"))?;
    let (mut doc, before) = read_doc(path)?;
    // merge with the old entry: keys that weren't sent (e.g. env whose values aren't shown) stay
    let old = doc
        .get("profile")
        .and_then(|v| v.as_array_of_tables())
        .and_then(|a| a.iter().find(|x| x.get("id").and_then(|v| v.as_str()) == Some(id)).cloned());
    let mut t = old.unwrap_or_default();
    t["id"] = toml_edit::value(id);
    for k in ["name", "command", "cwd", "policy", "permission", "account"] {
        if let Some(v) = p[k].as_str() {
            t[k] = toml_edit::value(v);
        }
    }
    if let Some(args) = p["args"].as_array() {
        t["args"] = toml_edit::value(args.iter().filter_map(Value::as_str).collect::<toml_edit::Array>());
    }
    for k in ["summarize", "notify"] {
        if let Some(b) = p[k].as_bool() {
            t[k] = toml_edit::value(b);
        }
    }
    // env merged per key (old values are never sent to the UI); secret_env replaced whole (its references show in the form)
    for (k, merge) in [("env", true), ("secret_env", false)] {
        if let Some(m) = p[k].as_object() {
            let mut sub = match (merge, t.get(k).and_then(|v| v.as_table())) {
                (true, Some(old)) => old.clone(),
                _ => toml_edit::Table::new(),
            };
            for (name, v) in m {
                sub[name.as_str()] = toml_edit::value(v.as_str().unwrap_or_default());
            }
            t[k] = toml_edit::Item::Table(sub);
        }
    }
    if doc.get("profile").is_none() {
        doc["profile"] = toml_edit::Item::ArrayOfTables(toml_edit::ArrayOfTables::new());
    }
    let arr = doc["profile"].as_array_of_tables_mut().ok_or_else(|| Diag::new("not_array_table").at("profile"))?;
    let found = arr.iter().position(|x| x.get("id").and_then(|v| v.as_str()) == Some(id));
    match found.and_then(|i| arr.get_mut(i)) {
        Some(existing) => *existing = t,
        None => arr.push(t),
    }
    commit(path, &doc, &before)
}

/// FR-39: shortcuts from Settings. The old combo for that action is dropped; empty `combo` = back to default.
/// Combo and action name are checked again (cmd required, conflicts) by the Rust app side that builds the menu.
pub fn set_key(path: &Path, action: &str, combo: &str) -> Result<(), Diag> {
    let ok = |s: &str, extra: &str| s.len() <= 64 && s.chars().all(|c| c.is_ascii_alphanumeric() || extra.contains(c));
    if action.is_empty() || !ok(action, "._") || !ok(combo, "+.,=-;'/[]\\`") {
        return Err("bad_combo".into());
    }
    let (mut doc, before) = read_doc(path)?;
    if doc.get("keys").is_none() {
        doc["keys"] = toml_edit::table();
    }
    let keys = doc["keys"].as_table_like_mut().ok_or_else(|| Diag::new("not_table").at("keys"))?;
    let old: Vec<String> = keys.iter().filter(|(_, v)| v.as_str() == Some(action)).map(|(k, _)| k.to_string()).collect();
    for k in old {
        keys.remove(&k);
    }
    if !combo.is_empty() {
        keys.insert(&combo.to_lowercase(), toml_edit::value(action));
    }
    commit(path, &doc, &before)
}

/// FR-61: add `[[account]]` (id unique per agent; full validation by `load` through `commit`).
pub fn save_account(path: &Path, a: &crate::account::Account) -> Result<(), Diag> {
    let (mut doc, before) = read_doc(path)?;
    if !doc.contains_key("account") {
        doc["account"] = toml_edit::Item::ArrayOfTables(toml_edit::ArrayOfTables::new());
    }
    let arr = doc["account"].as_array_of_tables_mut().ok_or_else(|| Diag::new("not_array_table").at("account"))?;
    if arr.iter().any(|x| x.get("id").and_then(|v| v.as_str()) == Some(a.id.as_str())) {
        return Err(Diag::new("account_exists").arg("id", &a.id));
    }
    let mut t = toml_edit::Table::new();
    t["id"] = toml_edit::value(a.id.as_str());
    t["agent"] = toml_edit::value(a.agent.as_str());
    t["name"] = toml_edit::value(a.name.as_str());
    t["dir"] = toml_edit::value(a.dir.as_str());
    t["share"] = toml_edit::value(a.share.iter().map(String::as_str).collect::<toml_edit::Array>());
    arr.push(t);
    commit(path, &doc, &before)
}

/// FR-61 (8): change settings of an existing account (share, model, effort, trust); full validation by `load` through `commit`.
pub fn update_account(path: &Path, a: &crate::account::Account) -> Result<(), Diag> {
    let (mut doc, before) = read_doc(path)?;
    let t = doc
        .get_mut("account")
        .and_then(|v| v.as_array_of_tables_mut())
        .and_then(|arr| arr.iter_mut().find(|x| x.get("id").and_then(|v| v.as_str()) == Some(a.id.as_str())))
        .ok_or_else(|| Diag::new("no_such_account").arg("id", &a.id))?;
    t["share"] = toml_edit::value(a.share.iter().map(String::as_str).collect::<toml_edit::Array>());
    for (k, v) in [("model", &a.model), ("effort", &a.effort)] {
        if v.is_empty() {
            t.remove(k);
        } else {
            t[k] = toml_edit::value(v.as_str());
        }
    }
    if a.trust {
        t.remove("trust");
    } else {
        t["trust"] = toml_edit::value(false);
    }
    commit(path, &doc, &before)
}

/// FR-61: remove `[[account]]`; folder, links, and Keychain aren't touched.
pub fn remove_account(path: &Path, id: &str) -> Result<(), Diag> {
    let (mut doc, before) = read_doc(path)?;
    if let Some(arr) = doc.get_mut("account").and_then(|v| v.as_array_of_tables_mut()) {
        arr.retain(|x| x.get("id").and_then(|v| v.as_str()) != Some(id));
    }
    // profiles referencing this account go back to Utama (without this commit rejects, since the reference becomes an error)
    if let Some(arr) = doc.get_mut("profile").and_then(|v| v.as_array_of_tables_mut()) {
        for t in arr.iter_mut().filter(|t| t.get("account").and_then(|v| v.as_str()) == Some(id)) {
            t.remove("account");
        }
    }
    commit(path, &doc, &before)
}

/// Implicit parent table (`[a.b]` without a bare `[a]` header), created when missing.
fn implicit<'a>(t: &'a mut toml_edit::Table, key: &str) -> Result<&'a mut toml_edit::Table, Diag> {
    if !t.contains_key(key) {
        let mut n = toml_edit::Table::new();
        n.set_implicit(true);
        t.insert(key, toml_edit::Item::Table(n));
    }
    t[key].as_table_mut().ok_or_else(|| Diag::new("not_table").at(key))
}

/// FR-78: add or change `[addons.plugin.<id>]` (only the given fields), or remove it; validated by `load` via `commit`.
pub fn set_plugin(path: &Path, id: &str, dir: Option<&str>, scope: Option<&str>, workspaces: Option<&[String]>, remove: bool) -> Result<(), Diag> {
    let (mut doc, before) = read_doc(path)?;
    let plugins = implicit(implicit(doc.as_table_mut(), "addons")?, "plugin")?;
    if remove {
        plugins.remove(id);
        return commit(path, &doc, &before);
    }
    if !plugins.contains_key(id) && dir.is_none() {
        return Err(Diag::new("no_such_plugin").arg("id", id));
    }
    let t = implicit(plugins, id)?;
    t.set_implicit(false);
    if let Some(d) = dir {
        t["path"] = toml_edit::value(d);
    }
    if let Some(s) = scope {
        t["scope"] = toml_edit::value(s);
    }
    if let Some(w) = workspaces {
        t["workspaces"] = toml_edit::value(w.iter().map(String::as_str).collect::<toml_edit::Array>());
    }
    commit(path, &doc, &before)
}

/// FR-78: `[workspace.<id>.mcp]` = file + strict, or removed (`None`).
pub fn set_mcp(path: &Path, ws: &str, mcp: Option<(&str, bool)>) -> Result<(), Diag> {
    let (mut doc, before) = read_doc(path)?;
    let w = implicit(implicit(doc.as_table_mut(), "workspace")?, ws)?;
    match mcp {
        Some((file, strict)) => {
            let t = implicit(w, "mcp")?;
            t.set_implicit(false);
            t["file"] = toml_edit::value(file);
            t["strict"] = toml_edit::value(strict);
        }
        None => drop(w.remove("mcp")),
    }
    commit(path, &doc, &before)
}

/// FR-29: reset a built-in profile / delete an extra profile = drop every `[[profile]]` with that id.
pub fn reset_profile(path: &Path, id: &str) -> Result<(), Diag> {
    let (mut doc, before) = read_doc(path)?;
    if let Some(arr) = doc.get_mut("profile").and_then(|v| v.as_array_of_tables_mut()) {
        arr.retain(|x| x.get("id").and_then(|v| v.as_str()) != Some(id));
    }
    commit(path, &doc, &before)
}

/// Write to a temp file in the same folder, then `rename` (TECH section 5). Folder `0700` (SEC-25).
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let dir = path.parent().ok_or_else(|| io::Error::other("path tanpa folder"))?;
    // the Gembala folder and its subfolders (hooks/) are created 0700; one level above is enough
    if let Some(up) = dir.parent().filter(|u| !u.exists()) {
        crate::rundir::ensure_private_dir(up)?;
    }
    crate::rundir::ensure_private_dir(dir)?;
    let name = path.file_name().map(|n| n.to_string_lossy()).unwrap_or_default();
    let tmp = dir.join(format!(".{name}.{}", std::process::id()));
    let mut f = fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    fs::rename(&tmp, path)
}

/// FR-77 target 2: brief for the agent pane. The agent writes the whole new config.toml to the draft file only.
pub fn ai_brief(ask: &str, section: &str, current: &str, draft: &Path) -> String {
    let keys: Vec<String> = SECTIONS.iter().map(|(s, spec)| format!("[{s}] {}", spec.iter().map(|(k, _)| *k).collect::<Vec<_>>().join(", "))).collect();
    let focus = if section.is_empty() { String::new() } else { format!("The user opened this from the `{section}` part of Settings.\n") };
    format!(
        "Gembala \"Atur pakai AI\": change Gembala's own settings (config.toml) as the user asks.\n\n\
Request from the user, as written:\n{ask}\n\n{focus}\
Current config.toml (keep everything the request doesn't touch, comments included):\n```toml\n{current}\n```\n\n\
Write the whole new config.toml to this file and nothing else: {}\n\
Do not edit the real config.toml or any other file: the user reviews the diff in Gembala and approves it there. \
When the file is written, say \"draft written\" in one line.\n\n\
Known tables and keys: {}. Also [[profile]] (id, agent, command, args, cwd, ...), [keys] (shortcuts), [[account]], [addon].\n\
Gembala refuses a draft that changes a profile's policy, permission, token or secret_env, any [[account]], or [telegram]: \
those stay as they are. Unknown keys and out-of-range values are reported as problems.",
        draft.display(),
        keys.join("; "),
    )
}

/// FR-77 target 2 (Atur pakai AI on config.toml): the parts an AI draft may not change (BRD: access rules, recipient
/// policy, credentials). Returns what changed: `profile.<id>` (policy, permission mode, token, secret references),
/// `account`, `telegram`. A new profile is fine only with safe values; removing one is fine.
pub fn guarded_changes(before: &str, after: &str) -> Vec<String> {
    let (a, b) = (load(Some(before)), load(Some(after)));
    let guard = |p: &crate::profile::Profile| (p.policy.clone(), p.permission.clone(), p.token, p.secret_env.clone());
    let mut out = Vec::new();
    for p in &b.profiles {
        let changed = match a.profiles.iter().find(|x| x.id == p.id) {
            Some(old) => guard(old) != guard(p),
            None => p.policy != "auto" || p.permission != "default" || p.token || !p.secret_env.is_empty(),
        };
        if changed {
            out.push(format!("profile.{}", p.id));
        }
    }
    if a.accounts != b.accounts {
        out.push("account".into());
    }
    if a.values["telegram"] != b.values["telegram"] {
        out.push("telegram".into());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ai_draft_guarded_keys() {
        let base = "[ui]\nfont_size = 13\n[[profile]]\nid = \"kerja\"\nagent = \"claude\"\n";
        // ordinary settings and a new safe profile pass
        assert!(guarded_changes(base, &format!("{base}[notify]\nsound = false\n[[profile]]\nid = \"baru\"\nagent = \"codex\"\n")).is_empty());
        assert!(guarded_changes(base, &base.replace("13", "14")).is_empty());
        // recipient policy, permission mode, token, a new loose profile, telegram: refused
        assert_eq!(guarded_changes(base, &format!("{base}policy = \"confirm\"\n")), ["profile.kerja"]);
        assert_eq!(guarded_changes(base, &format!("{base}permission = \"auto\"\n")), ["profile.kerja"]);
        assert_eq!(guarded_changes(base, &format!("{base}[[profile]]\nid = \"bebas\"\nagent = \"claude\"\ntoken = true\n")), ["profile.bebas"]);
        assert_eq!(guarded_changes(base, &format!("{base}[telegram]\nsnippet = false\n")), ["telegram"]);
    }

    #[test]
    fn tanpa_file_semua_bawaan() {
        let c = load(None);
        assert!(c.errors.is_empty() && !c.broken);
        assert_eq!(c.values["ui"]["font_size"], 13);
        assert_eq!(c.values["limits"]["max_panes"], 32);
    }

    #[test]
    fn kunci_rusak_pakai_bawaan_sisanya_tetap() {
        let c = load(Some("[ui]\nfont_size = 99\ncursor = \"block\"\ntheme = 3\nwarna = 1\n[keys]\n\"cmd+t\" = \"pane.split.right\"\n\"cmd+y\" = 5\n"));
        assert!(!c.broken);
        assert_eq!(c.values["ui"]["font_size"], 13);
        assert_eq!(c.values["ui"]["cursor"], "block");
        assert_eq!(c.values["ui"]["theme"], "system");
        assert_eq!(c.values["keys"]["cmd+t"], "pane.split.right");
        let all = c.errors.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n");
        for want in ["ui.font_size", "ui.theme", "ui.warna", "cmd+y"] {
            assert!(all.contains(want), "{want} tidak ditandai: {all}");
        }
    }

    #[test]
    fn notifikasi_per_jenis() {
        // FR-87: a file from before the per-kind switches still reads; new kinds default on, the done sound off
        let c = load(Some("[notify]\non_blocked = false\nsound = false\n"));
        assert!(c.errors.is_empty(), "{:?}", c.errors);
        assert_eq!(c.values["notify"]["on_blocked"], false);
        assert_eq!(c.values["notify"]["sound"], false);
        for k in ["on_permission", "on_answer", "on_password", "on_limit", "on_approve", "on_failed"] {
            assert_eq!(c.values["notify"][k], true, "{k}");
        }
        assert_eq!(c.values["notify"]["sound_done"], false);
        assert_eq!(load(Some("[notify]\non_failed = false\nsound_done = true\n")).values["notify"]["on_failed"], false);
    }

    #[test]
    fn mode_enum() {
        // FR-97: not answered = asked once; anything unknown falls back to asking, never to a mode the user didn't pick
        assert_eq!(load(None).values["ui"]["mode"], "ask");
        assert_eq!(load(Some("[ui]\nmode = \"simple\"\n")).values["ui"]["mode"], "simple");
        assert_eq!(load(Some("[ui]\nmode = \"expert\"\n")).values["ui"]["mode"], "ask");
    }

    #[test]
    fn language_enum() {
        assert_eq!(load(None).values["ui"]["language"], "system");
        assert_eq!(load(Some("[ui]\nlanguage = \"en\"\n")).values["ui"]["language"], "en");
        let c = load(Some("[ui]\nlanguage = \"fr\"\n"));
        assert_eq!(c.values["ui"]["language"], "system");
        assert!(c.errors.iter().map(ToString::to_string).collect::<Vec<_>>().join("").contains("ui.language"), "{:?}", c.errors);
    }

    #[test]
    fn linimasa_perintah_dari_config() {
        // FR-58: on by default; false from config sticks, a non-bool value falls back to the default and is flagged
        assert_eq!(load(None).values["ui"]["command_timeline"], true);
        let c = load(Some("[ui]\ncommand_timeline = false\n"));
        assert_eq!(c.values["ui"]["command_timeline"], false);
        // Codex footer: on by default, off leaves the user's own `tui.status_line` alone
        assert!(load(None).addons.codex_status && c.addons.codex_status);
        assert!(!load(Some("[ui]\ncodex_status = false\n")).addons.codex_status);
        assert!(c.errors.is_empty(), "{:?}", c.errors);
        let c = load(Some("[ui]\ncommand_timeline = \"mati\"\n"));
        assert_eq!(c.values["ui"]["command_timeline"], true);
        assert!(c.errors.iter().map(ToString::to_string).collect::<Vec<_>>().join("").contains("ui.command_timeline"), "{:?}", c.errors);
        // written through config.set, not just read
        let p = scratch("linimasa");
        set(&p, "ui", "command_timeline", &json!(false)).unwrap();
        assert_eq!(load(Some(&fs::read_to_string(&p).unwrap())).values["ui"]["command_timeline"], false);
    }

    #[test]
    fn helper_idle_close() {
        // FR-86: 15 minutes by default, 0 turns it off, out of range falls back and is flagged
        assert_eq!(load(None).values["ui"]["helper_idle_close"], 15);
        assert_eq!(load(Some("[ui]\nhelper_idle_close = 0\n")).values["ui"]["helper_idle_close"], 0);
        let c = load(Some("[ui]\nhelper_idle_close = 999\n"));
        assert_eq!(c.values["ui"]["helper_idle_close"], 15);
        assert!(c.errors.iter().map(ToString::to_string).collect::<Vec<_>>().join("").contains("ui.helper_idle_close"), "{:?}", c.errors);
    }

    #[test]
    fn toolbar_dari_config() {
        let c = load(Some("[toolbar]\nleft = [\"rich_input\", \"diff\"]\nright = [\"branch\", \"jam\"]\n"));
        assert_eq!(c.values["toolbar"]["left"], json!(["rich_input", "diff"]));
        assert_eq!(c.values["toolbar"]["right"], json!(["cwd", "branch", "settings"]), "rusak = bawaan");
        assert!(c.errors.iter().map(ToString::to_string).collect::<Vec<_>>().join("").contains("toolbar.right"));
        let p = scratch("toolbar");
        set(&p, "toolbar", "left", &json!(["diff"])).unwrap();
        assert_eq!(load(Some(&fs::read_to_string(&p).unwrap())).values["toolbar"]["left"], json!(["diff"]));
    }

    #[test]
    fn pintasan_dari_pengaturan() {
        let p = scratch("keys");
        set_key(&p, "pane.split.right", "Cmd+T").unwrap();
        set_key(&p, "pane.split.right", "cmd+e").unwrap(); // replaces, doesn't add
        let c = load(Some(&fs::read_to_string(&p).unwrap()));
        assert_eq!(c.values["keys"], json!({"cmd+e": "pane.split.right"}));
        set_key(&p, "pane.split.right", "").unwrap();
        assert_eq!(load(Some(&fs::read_to_string(&p).unwrap())).values["keys"], json!({}));
        assert!(set_key(&p, "x\ny", "cmd+a").is_err());
    }

    #[test]
    fn file_tidak_terurai_ditandai_broken() {
        let c = load(Some("[ui\nfont_size = "));
        assert!(c.broken);
        assert_eq!(c.values["ui"]["font_size"], 13);
    }

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("gembala-cfg-{}-{tag}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        d.join("config.toml")
    }

    #[test]
    fn set_menjaga_komentar_dan_menolak_nilai_salah() {
        let p = scratch("set");
        set(&p, "ui", "font_size", &json!(15)).unwrap(); // file and folder don't exist yet
        let mut text = fs::read_to_string(&p).unwrap();
        text = format!("# punya pengguna\n{text}[keys]\n\"cmd+t\" = \"pane.split.right\" # komentar\n");
        fs::write(&p, &text).unwrap();
        set(&p, "ui", "cursor", &json!("block")).unwrap();
        let after = fs::read_to_string(&p).unwrap();
        assert!(after.contains("# punya pengguna") && after.contains("# komentar"), "{after}");
        assert_eq!(load(Some(&after)).values["ui"]["cursor"], "block");
        assert!(set(&p, "ui", "font_size", &json!(99)).is_err());
        assert!(set(&p, "ui", "warna", &json!(1)).is_err());
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(fs::metadata(p.parent().unwrap()).unwrap().permissions().mode() & 0o777, 0o700);
    }

    #[test]
    fn profil_disimpan_dan_dikembalikan() {
        let p = scratch("profil");
        set(&p, "ui", "font_size", &json!(14)).unwrap();
        let pr = json!({"id": "claude", "args": ["--model", "opus"], "env": {"CLAUDE_CONFIG_DIR": "~/.claude"}, "summarize": false});
        save_profile(&p, &pr).unwrap();
        save_profile(&p, &json!({"id": "lazygit", "name": "Lazygit", "command": "lazygit"})).unwrap();
        let c = load(Some(&fs::read_to_string(&p).unwrap()));
        let claude = c.profiles.iter().find(|x| x.id == "claude").unwrap();
        assert_eq!((claude.args.len(), claude.summarize), (2, false));
        assert!(c.profiles.iter().any(|x| x.id == "lazygit"));
        assert_eq!(c.values["ui"]["font_size"], 14, "bagian lain tetap");
        // change without sending env: old env stays
        save_profile(&p, &json!({"id": "claude", "name": "Claude"})).unwrap();
        let c = load(Some(&fs::read_to_string(&p).unwrap()));
        assert_eq!(c.profiles.iter().find(|x| x.id == "claude").unwrap().env.len(), 1);
        // a secret in plain env is rejected, the file doesn't change
        let before = fs::read_to_string(&p).unwrap();
        let bad = save_profile(&p, &json!({"id": "claude", "env": {"KEY": "sk-ant-123"}})).unwrap_err();
        assert_eq!(bad.code, "env_secret", "{bad}");
        assert_eq!(fs::read_to_string(&p).unwrap(), before);
        reset_profile(&p, "claude").unwrap();
        let c = load(Some(&fs::read_to_string(&p).unwrap()));
        assert_eq!(c.profiles.iter().find(|x| x.id == "claude").unwrap().args.len(), 0);
    }

    #[test]
    fn set_tidak_menimpa_file_rusak() {
        let p = scratch("rusak");
        crate::rundir::ensure_private_dir(p.parent().unwrap()).unwrap();
        fs::write(&p, "[ui\n").unwrap();
        assert!(set(&p, "ui", "font_size", &json!(14)).is_err());
        assert_eq!(fs::read_to_string(&p).unwrap(), "[ui\n");
    }
}
