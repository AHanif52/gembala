//! FR-74, SEC-27, SEC-30: harness recipes (TECH 12, plan H1). A recipe is a TOML file in `<config folder>/recipes/`,
//! never read from a repo or cwd. It runs only after the user approved the exact bytes: the SHA-256 of the approved
//! text is kept in `state.json`, and a file edited elsewhere falls back to "needs approval". Parsing and validation are
//! pure (no I/O) so the rules can be tested without a daemon; `Store` is the thin file layer on top.

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use toml_edit::{DocumentMut, Item, TableLike};

use crate::diag::Diag;

/// SEC-30 hard limits: a recipe may lower them, never raise them (values above are clamped with a warning)
pub const MAX_ROUNDS: i64 = 3;
pub const MAX_AGENTS: usize = 3;
pub const MAX_TOTAL_SECS: u64 = 2 * 3600;
const MAX_STEPS: usize = 12;
const MAX_FILE: u64 = 64 * 1024;
const MAX_TEXT: usize = 4000;
/// written by "New task" (FR-73) before the first step, so a step may list it as an input
pub const REQUEST_FILE: &str = "permintaan.md";

const FALLBACK_ON: &[&str] = &["exit", "timeout", "unknown", "limit"];
const EFFORTS: &[&str] = &["low", "medium", "high", "max"];
const PASSES: &[&str] = &["reply", "file", "first_line_pass", "exit0"];
const STEP_KEYS: &[&str] = &["id", "kind", "name", "need", "instruction", "agents", "model", "effort", "output", "inputs", "pass", "timeout", "gate", "human", "shell", "max_cost"];
const RECIPE_KEYS: &[&str] = &["name", "fallback_on", "token_saver", "max_rounds", "after_max", "on_limit", "total", "step"];

#[derive(Debug, Clone, PartialEq)]
pub struct Step {
    pub id: String,
    /// `work` | `check`
    pub kind: String,
    pub name: String,
    /// the user's own words (custom stages), kept so the stage can be rephrased later
    pub need: String,
    pub instruction: String,
    /// agent kinds (claude, codex, omp, opencode); order = fallback order
    pub agents: Vec<String>,
    pub model: String,
    pub effort: String,
    /// artifact file name inside the run folder
    pub output: String,
    /// artifact names an earlier step (or the request) must have produced
    pub inputs: Vec<String>,
    /// `reply` | `file` | `first_line_pass` | `exit0`
    pub pass: String,
    pub timeout_secs: u64,
    pub gate: bool,
    /// Ship: always the human's button, never an agent
    pub human: bool,
    /// no AI: the workspace test command runs in a shell
    pub shell: bool,
    pub max_cost: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Recipe {
    pub name: String,
    pub fallback_on: Vec<String>,
    pub token_saver: bool,
    pub max_rounds: u32,
    /// `stop` | `escalate`
    pub after_max: String,
    /// `fallback` | `wait`
    pub on_limit: String,
    pub total_secs: u64,
    pub steps: Vec<Step>,
}

#[derive(Default)]
pub struct Parsed {
    pub recipe: Option<Recipe>,
    /// not saveable or runnable while any is left
    pub errors: Vec<Diag>,
    /// loaded anyway (clamped, ignored, or not active yet)
    pub warnings: Vec<Diag>,
}

pub fn hash(text: &str) -> String {
    Sha256::digest(text.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

/// `15m`, `90s`, `2h` (whole numbers only)
pub fn duration(s: &str) -> Option<u64> {
    let unit = s.chars().last()?;
    let n: u64 = s[..s.len() - unit.len_utf8()].parse().ok().filter(|n| *n > 0)?;
    n.checked_mul(match unit {
        's' => 1,
        'm' => 60,
        'h' => 3600,
        _ => return None,
    })
}

fn file_name_ok(s: &str) -> bool {
    (1..=64).contains(&s.len()) && !s.starts_with('.') && s.bytes().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'-' | b'_'))
}

/// goes behind `--model=` and similar, so it can never start like an option or carry shell characters
fn model_ok(s: &str) -> bool {
    (1..=80).contains(&s.len()) && !s.starts_with('-') && s.bytes().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'-' | b'_' | b':' | b'/' | b'@'))
}

struct Ctx {
    errors: Vec<Diag>,
    warnings: Vec<Diag>,
}

impl Ctx {
    fn err(&mut self, code: &'static str, at: &str) {
        self.errors.push(Diag::new(code).at(at));
    }

    fn warn(&mut self, code: &'static str, at: &str) {
        self.warnings.push(Diag::new(code).at(at));
    }

    fn string(&mut self, t: &dyn TableLike, k: &str, at: &str) -> Option<String> {
        let item = t.get(k)?;
        match item.as_str() {
            Some(s) => Some(s.to_string()),
            None => {
                self.err("bad_type", &format!("{at}.{k}"));
                None
            }
        }
    }

    fn boolean(&mut self, t: &dyn TableLike, k: &str, at: &str) -> Option<bool> {
        let item = t.get(k)?;
        let b = item.as_bool();
        if b.is_none() {
            self.err("bad_type", &format!("{at}.{k}"));
        }
        b
    }

    fn strings(&mut self, t: &dyn TableLike, k: &str, at: &str) -> Option<Vec<String>> {
        let item = t.get(k)?;
        let list = item.as_array().and_then(|a| a.iter().map(|v| v.as_str().map(String::from)).collect::<Option<Vec<_>>>());
        if list.is_none() {
            self.err("bad_type", &format!("{at}.{k}"));
        }
        list
    }

    fn one_of(&mut self, v: Option<String>, opts: &[&str], default: &str, at: &str) -> String {
        match v {
            Some(s) if opts.contains(&s.as_str()) => s,
            Some(_) => {
                self.errors.push(Diag::new("one_of").at(at).arg("opts", opts.join(", ")));
                default.to_string()
            }
            None => default.to_string(),
        }
    }

    fn unknown_keys(&mut self, t: &dyn TableLike, known: &[&str], at: &str) {
        for (k, _) in t.iter() {
            if !known.contains(&k) {
                self.warn("unknown_key", &format!("{at}.{k}"));
            }
        }
    }

    /// free text sent to an agent later: same sanitizer as messages (SEC-7), bounded
    fn text(&mut self, t: &dyn TableLike, k: &str, at: &str) -> String {
        let Some(s) = self.string(t, k, at) else { return String::new() };
        if s.len() > MAX_TEXT || crate::msg::sanitize(&s).map_or(true, |c| c != s) {
            self.err("text_bad", &format!("{at}.{k}"));
            return String::new();
        }
        s
    }
}

/// `installed` = agent kinds found on this Mac; `None` skips the installed check (pure tests, preview before detection)
pub fn parse(text: &str, id: &str, installed: Option<&[String]>) -> Parsed {
    let mut cx = Ctx { errors: Vec::new(), warnings: Vec::new() };
    let recipe = build(text, id, installed, &mut cx);
    Parsed { recipe: recipe.filter(|_| cx.errors.is_empty()), errors: cx.errors, warnings: cx.warnings }
}

fn build(text: &str, id: &str, installed: Option<&[String]>, cx: &mut Ctx) -> Option<Recipe> {
    let doc = match text.parse::<DocumentMut>() {
        Ok(d) => d,
        Err(e) => {
            cx.errors.push(Diag::new("toml_syntax").arg("os", e.message()));
            return None;
        }
    };
    let root = doc.as_table() as &dyn TableLike;
    cx.unknown_keys(root, RECIPE_KEYS, "recipe");

    let name = cx.string(root, "name", "name").filter(|n| !n.is_empty() && n.len() <= 60 && !n.chars().any(char::is_control)).unwrap_or_else(|| id.to_string());
    let after = cx.string(root, "after_max", "after_max");
    let after_max = cx.one_of(after, &["stop", "escalate"], "stop", "after_max");
    let on_limit = cx.string(root, "on_limit", "on_limit");
    let on_limit = cx.one_of(on_limit, &["fallback", "wait"], "fallback", "on_limit");
    // decision 5 Nov 2026: limit detection is off in 3a until its hook patterns are proven on real agents
    let fallback_on = match cx.strings(root, "fallback_on", "fallback_on") {
        Some(v) => {
            if let Some(bad) = v.iter().find(|s| !FALLBACK_ON.contains(&s.as_str())) {
                cx.errors.push(Diag::new("one_of").at("fallback_on").arg("opts", FALLBACK_ON.join(", ")).arg("got", bad));
            }
            v
        }
        None => ["exit", "timeout", "unknown"].map(String::from).to_vec(),
    };
    if fallback_on.iter().any(|s| s == "limit") || on_limit == "wait" {
        cx.warn("limit_inactive", "fallback_on");
    }
    let token_saver = cx.boolean(root, "token_saver", "token_saver").unwrap_or(false);

    let rounds = match root.get("max_rounds") {
        None => 2,
        Some(i) => match i.as_integer() {
            Some(n) if n < 0 => {
                cx.err("int_range", "max_rounds");
                2
            }
            Some(n) if n > MAX_ROUNDS => {
                cx.warn("clamped", "max_rounds");
                MAX_ROUNDS
            }
            Some(n) => n,
            None => {
                cx.err("bad_type", "max_rounds");
                2
            }
        },
    } as u32;
    let total_secs = match cx.string(root, "total", "total") {
        None => MAX_TOTAL_SECS,
        Some(s) => match duration(&s) {
            Some(n) if n > MAX_TOTAL_SECS => {
                cx.warn("clamped", "total");
                MAX_TOTAL_SECS
            }
            Some(n) => n,
            None => {
                cx.err("duration", "total");
                MAX_TOTAL_SECS
            }
        },
    };

    let mut steps = Vec::new();
    match root.get("step") {
        Some(Item::ArrayOfTables(aot)) => {
            if aot.len() > MAX_STEPS {
                cx.err("too_many_steps", "step");
            }
            for (i, t) in aot.iter().enumerate().take(MAX_STEPS) {
                if let Some(s) = step(t, i, installed, cx) {
                    steps.push(s);
                }
            }
        }
        Some(_) => cx.err("bad_type", "step"),
        None => cx.err("no_steps", "step"),
    }
    if !steps.is_empty() {
        order(&mut steps, cx);
    }
    Some(Recipe { name, fallback_on, token_saver, max_rounds: rounds, after_max, on_limit, total_secs, steps })
}

fn step(t: &dyn TableLike, i: usize, installed: Option<&[String]>, cx: &mut Ctx) -> Option<Step> {
    let at = format!("step[{}]", i + 1);
    cx.unknown_keys(t, STEP_KEYS, &at);
    let id = cx.string(t, "id", &at).filter(|s| crate::addon::valid_key(s));
    let Some(id) = id else {
        cx.err("step_id", &at);
        return None;
    };
    let at = format!("step.{id}");
    let human = cx.boolean(t, "human", &at).unwrap_or(false);
    let shell = cx.boolean(t, "shell", &at).unwrap_or(false);
    let kind = cx.string(t, "kind", &at);
    let kind = cx.one_of(kind, &["work", "check"], if shell { "check" } else { "work" }, &format!("{at}.kind"));
    let agents = cx.strings(t, "agents", &at).unwrap_or_default();
    let model = cx.string(t, "model", &at).unwrap_or_default();
    let effort = cx.string(t, "effort", &at);
    let effort = if effort.is_none() { String::new() } else { cx.one_of(effort, EFFORTS, "", &format!("{at}.effort")) };
    let output = cx.string(t, "output", &at).unwrap_or_default();
    let inputs = cx.strings(t, "inputs", &at).unwrap_or_default();
    let gate = cx.boolean(t, "gate", &at).unwrap_or(false);

    if !model.is_empty() && !model_ok(&model) {
        cx.err("model_bad", &format!("{at}.model"));
    }
    if !output.is_empty() && !file_name_ok(&output) {
        cx.err("file_name", &format!("{at}.output"));
    }
    if let Some(bad) = inputs.iter().find(|n| !file_name_ok(n)) {
        cx.errors.push(Diag::new("file_name").at(format!("{at}.inputs")).arg("got", bad));
    }
    if agents.len() > MAX_AGENTS {
        cx.err("too_many_agents", &format!("{at}.agents"));
    }
    let known: Vec<&str> = crate::profile::agent_ids().collect();
    for (n, a) in agents.iter().enumerate() {
        if !known.contains(&a.as_str()) {
            cx.errors.push(Diag::new("one_of").at(format!("{at}.agents")).arg("opts", known.join(", ")).arg("got", a));
        } else if agents[..n].contains(a) {
            cx.err("agent_dup", &format!("{at}.agents"));
        }
    }
    let usable: Vec<&String> = installed.map_or_else(|| agents.iter().collect(), |inst| agents.iter().filter(|a| inst.contains(a)).collect());
    if human {
        if !agents.is_empty() || shell {
            cx.err("ship_no_agent", &at);
        }
    } else if shell {
        if kind != "check" || !agents.is_empty() {
            cx.err("shell_step", &at);
        }
    } else if agents.is_empty() {
        cx.err("no_agent", &at);
    } else if usable.is_empty() {
        cx.err("agent_not_installed", &at);
    } else if usable.len() < agents.len() {
        cx.warn("agent_missing", &at);
    }

    let default_pass = if shell { "exit0" } else if output.is_empty() { "reply" } else if kind == "check" { "first_line_pass" } else { "file" };
    let pass = cx.string(t, "pass", &at);
    let pass = cx.one_of(pass, PASSES, default_pass, &format!("{at}.pass"));
    if matches!(pass.as_str(), "file" | "first_line_pass") && output.is_empty() {
        cx.err("pass_needs_output", &at);
    }
    if shell && pass != "exit0" {
        cx.err("shell_step", &at);
    }
    let timeout_secs = match cx.string(t, "timeout", &at) {
        None => 15 * 60,
        Some(s) => match duration(&s) {
            Some(n) if n <= MAX_TOTAL_SECS => n,
            _ => {
                cx.err("duration", &format!("{at}.timeout"));
                15 * 60
            }
        },
    };
    let max_cost = match t.get("max_cost") {
        None => None,
        Some(i) => match i.as_float().or_else(|| i.as_integer().map(|n| n as f64)).filter(|c| (0.0..=1000.0).contains(c)) {
            Some(c) => Some(c),
            None => {
                cx.err("num_range", &format!("{at}.max_cost"));
                None
            }
        },
    };
    Some(Step {
        name: cx.string(t, "name", &at).filter(|n| !n.is_empty() && n.len() <= 60 && !n.chars().any(char::is_control)).unwrap_or_else(|| id.clone()),
        need: cx.text(t, "need", &at),
        instruction: cx.text(t, "instruction", &at),
        id,
        kind,
        agents,
        model,
        effort,
        output,
        inputs,
        pass,
        timeout_secs,
        gate,
        human,
        shell,
        max_cost,
    })
}

/// whole-recipe rules about order (BRD 8): checked after every step parsed, so one report lists all of them
fn order(steps: &mut Vec<Step>, cx: &mut Ctx) {
    for (i, s) in steps.iter().enumerate() {
        if steps[..i].iter().any(|p| p.id == s.id) {
            cx.err("step_dup", &format!("step.{}", s.id));
        }
    }
    // Ship is added when missing so a hand-written recipe can never commit on its own (SEC-30)
    if !steps.iter().any(|s| s.human) {
        steps.push(Step {
            id: "ship".into(),
            kind: "work".into(),
            name: "ship".into(),
            need: String::new(),
            instruction: String::new(),
            agents: Vec::new(),
            model: String::new(),
            effort: String::new(),
            output: String::new(),
            inputs: Vec::new(),
            pass: "reply".into(),
            timeout_secs: 0,
            gate: true,
            human: true,
            shell: false,
            max_cost: None,
        });
    }
    let ship = steps.iter().position(|s| s.human).unwrap_or(0);
    if steps.iter().filter(|s| s.human).count() > 1 {
        cx.err("ship_many", "step");
    }
    // only a report-like work step may follow Ship (Laporan)
    if let Some(s) = steps[ship + 1..].iter().find(|s| s.kind != "work" || s.shell || s.gate || s.human) {
        cx.err("ship_not_last", &format!("step.{}", s.id));
    }
    match steps.iter().position(|s| s.kind == "work" && !s.human) {
        None => cx.err("no_work", "step"),
        Some(first) => {
            if let Some(s) = steps[..first].iter().find(|s| !s.human) {
                cx.err("check_first", &format!("step.{}", s.id));
            }
        }
    }
    let mut made: Vec<&str> = vec![REQUEST_FILE];
    for s in steps.iter() {
        for n in &s.inputs {
            if !made.contains(&n.as_str()) {
                cx.errors.push(Diag::new("input_missing").at(format!("step.{}.inputs", s.id)).arg("got", n));
            }
        }
        if !s.output.is_empty() {
            if made.contains(&s.output.as_str()) {
                cx.err("output_dup", &format!("step.{}.output", s.id));
            }
            made.push(&s.output);
        }
    }
}

/// `{ui}` summary for list rows and the flow view: ids and names only, the full recipe stays in the file
pub fn summary(r: &Recipe) -> Value {
    json!({
        "name": r.name, "max_rounds": r.max_rounds, "after_max": r.after_max, "total_secs": r.total_secs,
        "steps": r.steps.iter().map(|s| json!({"id": s.id, "name": s.name, "kind": s.kind, "human": s.human, "shell": s.shell, "agents": s.agents})).collect::<Vec<_>>(),
    })
}

const FEATURE: &str = r#"name = "Fitur baru"
max_rounds = 2

[[step]]
id = "plan"
name = "Plan"
kind = "work"
agents = ["claude", "omp", "codex"]
model = "opus"
effort = "high"
output = "plan.md"
inputs = ["permintaan.md"]
timeout = "15m"
gate = true

[[step]]
id = "execute"
name = "Execute"
kind = "work"
agents = ["claude", "codex", "omp"]
model = "sonnet"
effort = "medium"
inputs = ["plan.md"]
timeout = "40m"

[[step]]
id = "test"
name = "Test"
kind = "check"
shell = true
timeout = "10m"

[[step]]
id = "scan-security"
name = "Scan Security"
kind = "check"
agents = ["codex", "omp", "claude"]
effort = "high"
output = "security.md"
timeout = "15m"

[[step]]
id = "ship"
name = "Ship"
human = true
"#;

const BUGFIX: &str = r#"name = "Perbaiki bug"
max_rounds = 2

[[step]]
id = "reproduce"
name = "Reproduksi"
kind = "work"
agents = ["claude", "codex"]
model = "sonnet"
effort = "medium"
inputs = ["permintaan.md"]
timeout = "15m"

[[step]]
id = "execute"
name = "Execute"
kind = "work"
agents = ["claude", "codex", "omp"]
model = "sonnet"
effort = "medium"
timeout = "40m"

[[step]]
id = "test"
name = "Test"
kind = "check"
shell = true
timeout = "10m"

[[step]]
id = "ship"
name = "Ship"
human = true
"#;

/// recipe files + approvals under the Gembala config folder (`recipes/`, `state.json`)
pub struct Store {
    dir: PathBuf,
    state: PathBuf,
}

impl Store {
    pub fn new(config_dir: &Path) -> Store {
        Store { dir: config_dir.join("recipes"), state: config_dir.join("state.json") }
    }

    /// first start only: the folder does not exist yet. A deleted default stays deleted.
    pub fn ensure_defaults(&self) -> io::Result<()> {
        if self.dir.exists() {
            return Ok(());
        }
        for (id, text) in [("fitur-baru", FEATURE), ("perbaiki-bug", BUGFIX)] {
            crate::config::write_atomic(&self.dir.join(format!("{id}.toml")), text.as_bytes())?;
        }
        Ok(())
    }

    fn path(&self, id: &str) -> Result<PathBuf, Diag> {
        if crate::addon::valid_key(id) { Ok(self.dir.join(format!("{id}.toml"))) } else { Err(Diag::new("recipe_id")) }
    }

    /// a regular file inside the recipes folder, no symlink out of it, bounded size
    fn read(&self, id: &str) -> Result<String, Diag> {
        let p = self.path(id)?;
        let m = fs::symlink_metadata(&p).map_err(|e| crate::config::io_err("io_read", e))?;
        if !m.is_file() || m.len() > MAX_FILE {
            return Err(Diag::new("recipe_file"));
        }
        fs::read_to_string(&p).map_err(|e| crate::config::io_err("io_read", e))
    }

    fn approvals(&self) -> Value {
        fs::read_to_string(&self.state).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()).filter(Value::is_object).unwrap_or_else(|| json!({}))
    }

    fn save_approvals(&self, v: &Value) -> Result<(), Diag> {
        crate::config::write_atomic(&self.state, format!("{v}\n").as_bytes()).map_err(|e| crate::config::io_err("io_write", e))
    }

    fn ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = fs::read_dir(&self.dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| e.file_name().to_str()?.strip_suffix(".toml").map(String::from))
            .filter(|id| crate::addon::valid_key(id))
            .collect();
        ids.sort();
        ids
    }

    /// `broken` (errors) | `needs_approval` (never approved) | `changed` (approved once, file differs now) | `approved`
    fn row(&self, id: &str, installed: Option<&[String]>) -> Value {
        let text = match self.read(id) {
            Ok(t) => t,
            Err(d) => return json!({"id": id, "status": "broken", "errors": [d.json()], "warnings": []}),
        };
        let p = parse(&text, id, installed);
        let h = hash(&text);
        let known = self.approvals()["recipes"][id].as_str().map(String::from);
        let status = match (&p.recipe, &known) {
            (None, _) => "broken",
            (_, Some(k)) if *k == h => "approved",
            (_, Some(_)) => "changed",
            (_, None) => "needs_approval",
        };
        json!({
            "id": id, "status": status, "hash": h,
            "recipe": p.recipe.as_ref().map(summary),
            "errors": p.errors.iter().map(Diag::json).collect::<Vec<_>>(),
            "warnings": p.warnings.iter().map(Diag::json).collect::<Vec<_>>(),
        })
    }

    pub fn list(&self, installed: Option<&[String]>) -> Value {
        Value::Array(self.ids().iter().map(|id| self.row(id, installed)).collect())
    }

    /// row + the exact text, for the review screen (what the user approves is this text, nothing else)
    pub fn get(&self, id: &str, installed: Option<&[String]>) -> Result<Value, Diag> {
        let text = self.read(id)?;
        let mut v = self.row(id, installed);
        v["text"] = json!(text);
        Ok(v)
    }

    /// approves only the bytes the user reviewed: `hash` is the one the screen showed, re-checked against the file now
    pub fn approve(&self, id: &str, hash_seen: &str, installed: Option<&[String]>) -> Result<(), Diag> {
        let text = self.read(id)?;
        if hash(&text) != hash_seen {
            return Err(Diag::new("recipe_changed"));
        }
        if parse(&text, id, installed).recipe.is_none() {
            return Err(Diag::new("recipe_invalid"));
        }
        let mut v = self.approvals();
        v["recipes"][id] = json!(hash_seen);
        self.save_approvals(&v)
    }

    #[allow(dead_code)] // the H3 runner calls this before every run
    /// run gate: the recipe, only when its current bytes are the approved ones (SEC-27)
    pub fn approved(&self, id: &str, installed: Option<&[String]>) -> Result<Recipe, Diag> {
        let text = self.read(id)?;
        if self.approvals()["recipes"][id].as_str() != Some(&hash(&text)) {
            return Err(Diag::new("recipe_not_approved"));
        }
        parse(&text, id, installed).recipe.ok_or_else(|| Diag::new("recipe_invalid"))
    }

    pub fn delete(&self, id: &str) -> Result<(), Diag> {
        let p = self.path(id)?;
        fs::remove_file(&p).map_err(|e| crate::config::io_err("io_write", e))?;
        let mut v = self.approvals();
        if let Some(m) = v["recipes"].as_object_mut() {
            m.remove(id);
        }
        self.save_approvals(&v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(text: &str) -> Recipe {
        let p = parse(text, "t", None);
        assert!(p.errors.is_empty(), "{:?}", p.errors);
        p.recipe.unwrap()
    }

    fn codes(text: &str) -> Vec<String> {
        parse(text, "t", None).errors.iter().map(|d| format!("{}@{}", d.code, d.args.iter().find(|a| a.0 == "at").map_or("", |a| a.1.as_str()))).collect()
    }

    const MIN: &str = "[[step]]\nid = \"a\"\nagents = [\"claude\"]\n";

    #[test]
    fn defaults_parse_clean() {
        for t in [FEATURE, BUGFIX] {
            let p = parse(t, "x", None);
            assert!(p.errors.is_empty() && p.warnings.is_empty(), "{:?} {:?}", p.errors, p.warnings);
        }
        let r = ok(FEATURE);
        assert_eq!(r.steps.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), ["plan", "execute", "test", "scan-security", "ship"]);
        let t = &r.steps[2];
        assert!(t.shell && t.pass == "exit0" && t.kind == "check");
        assert_eq!(r.steps[3].pass, "first_line_pass");
        assert_eq!(r.steps[0].pass, "file");
    }

    #[test]
    fn ship_added_when_missing() {
        let r = ok(MIN);
        assert_eq!(r.steps.len(), 2);
        let s = r.steps.last().unwrap();
        assert!(s.human && s.gate && s.agents.is_empty());
    }

    #[test]
    fn sec30_limits_clamp_and_warn() {
        let p = parse(&format!("max_rounds = 9\ntotal = \"5h\"\n{MIN}"), "t", None);
        let r = p.recipe.unwrap();
        assert_eq!((r.max_rounds, r.total_secs), (3, 7200));
        assert_eq!(p.warnings.iter().filter(|w| w.code == "clamped").count(), 2);
        assert!(codes(&format!("max_rounds = -1\n{MIN}")).iter().any(|c| c.starts_with("int_range")));
        assert!(codes("[[step]]\nid = \"a\"\nagents = [\"claude\", \"codex\", \"omp\", \"opencode\"]\n").iter().any(|c| c.starts_with("too_many_agents")));
    }

    #[test]
    fn order_rules() {
        let check_first = "[[step]]\nid = \"t\"\nkind = \"check\"\nshell = true\n[[step]]\nid = \"a\"\nagents = [\"claude\"]\n";
        assert!(codes(check_first).contains(&"check_first@step.t".into()));
        let no_input = "[[step]]\nid = \"a\"\nagents = [\"claude\"]\ninputs = [\"plan.md\"]\n";
        assert!(codes(no_input).contains(&"input_missing@step.a.inputs".into()));
        let from_request = "[[step]]\nid = \"a\"\nagents = [\"claude\"]\ninputs = [\"permintaan.md\"]\n";
        ok(from_request);
        let dup = "[[step]]\nid = \"a\"\nagents = [\"claude\"]\n[[step]]\nid = \"a\"\nagents = [\"claude\"]\n";
        assert!(codes(dup).contains(&"step_dup@step.a".into()));
        let after_ship = format!("{MIN}[[step]]\nid = \"s\"\nhuman = true\n[[step]]\nid = \"c\"\nkind = \"check\"\nshell = true\n");
        assert!(codes(&after_ship).contains(&"ship_not_last@step.c".into()));
        // a report after Ship is allowed
        ok(&format!("{MIN}[[step]]\nid = \"s\"\nhuman = true\n[[step]]\nid = \"laporan\"\nagents = [\"claude\"]\noutput = \"laporan.md\"\n"));
        let two_ship = format!("{MIN}[[step]]\nid = \"s\"\nhuman = true\n[[step]]\nid = \"s2\"\nhuman = true\n");
        assert!(codes(&two_ship).iter().any(|c| c.starts_with("ship_many")));
        let out_dup = "[[step]]\nid = \"a\"\nagents = [\"claude\"]\noutput = \"x.md\"\n[[step]]\nid = \"b\"\nagents = [\"claude\"]\noutput = \"x.md\"\n";
        assert!(codes(out_dup).contains(&"output_dup@step.b.output".into()));
    }

    #[test]
    fn step_shape_rules() {
        assert!(codes("[[step]]\nid = \"a\"\n").contains(&"no_agent@step.a".into()));
        assert!(codes("[[step]]\nid = \"a\"\nagents = [\"claude\"]\npass = \"file\"\n").contains(&"pass_needs_output@step.a".into()));
        assert!(codes("[[step]]\nid = \"a\"\nkind = \"check\"\nshell = true\nagents = [\"claude\"]\n").contains(&"shell_step@step.a".into()));
        assert!(codes("[[step]]\nid = \"a\"\nagents = [\"claude\"]\n[[step]]\nid = \"s\"\nhuman = true\nagents = [\"claude\"]\n").contains(&"ship_no_agent@step.s".into()));
        assert!(codes("[[step]]\nid = \"a\"\nagents = [\"claude\", \"claude\"]\n").contains(&"agent_dup@step.a.agents".into()));
        assert!(codes("[[step]]\nid = \"a\"\nagents = [\"nano\"]\n").iter().any(|c| c.starts_with("one_of")));
        assert!(codes("[[step]]\nid = \"a b\"\nagents = [\"claude\"]\n").iter().any(|c| c.starts_with("step_id")));
        assert!(codes("[[step]]\nid = \"a\"\nagents = [\"claude\"]\ntimeout = \"3h\"\n").contains(&"duration@step.a.timeout".into()));
        assert!(codes("[[step]]\nid = \"a\"\nagents = [\"claude\"]\neffort = \"ultra\"\n").iter().any(|c| c.starts_with("one_of@step.a.effort")));
    }

    #[test]
    fn injection_shaped_values_rejected() {
        for bad in ["--dangerously-skip-permissions", "opus; rm -rf /", "a b", "$(x)"] {
            assert!(codes(&format!("[[step]]\nid = \"a\"\nagents = [\"claude\"]\nmodel = \"{bad}\"\n")).contains(&"model_bad@step.a.model".into()), "{bad}");
        }
        for bad in ["../x.md", "a/b.md", ".hidden", "a b.md"] {
            assert!(codes(&format!("[[step]]\nid = \"a\"\nagents = [\"claude\"]\noutput = \"{bad}\"\n")).contains(&"file_name@step.a.output".into()), "{bad}");
        }
        // control characters in text bound for an agent
        assert!(codes("[[step]]\nid = \"a\"\nagents = [\"claude\"]\ninstruction = \"x\\u001b[2Jy\"\n").contains(&"text_bad@step.a.instruction".into()));
    }

    #[test]
    fn installed_agents() {
        let inst = ["codex".to_string()];
        let t = "[[step]]\nid = \"a\"\nagents = [\"claude\", \"codex\"]\n";
        let p = parse(t, "t", Some(&inst));
        assert!(p.errors.is_empty() && p.warnings.iter().any(|w| w.code == "agent_missing"));
        let p = parse("[[step]]\nid = \"a\"\nagents = [\"claude\"]\n", "t", Some(&inst));
        assert!(p.errors.iter().any(|e| e.code == "agent_not_installed"));
    }

    #[test]
    fn limit_off_in_3a() {
        let p = parse(&format!("fallback_on = [\"exit\", \"limit\"]\n{MIN}"), "t", None);
        assert!(p.recipe.is_some() && p.warnings.iter().any(|w| w.code == "limit_inactive"));
        assert_eq!(ok(MIN).fallback_on, ["exit", "timeout", "unknown"]);
        assert!(codes(&format!("fallback_on = [\"error\"]\n{MIN}")).iter().any(|c| c.starts_with("one_of@fallback_on")));
    }

    #[test]
    fn bad_toml_and_types() {
        assert!(codes("[[step").iter().any(|c| c.starts_with("toml_syntax")));
        assert!(codes("max_rounds = \"2\"\n[[step]]\nid = \"a\"\nagents = [\"claude\"]\n").contains(&"bad_type@max_rounds".into()));
        assert!(codes("name = \"x\"\n").contains(&"no_steps@step".into()));
        assert!(parse(&format!("oops = 1\n{MIN}"), "t", None).warnings.iter().any(|w| w.code == "unknown_key"));
    }

    #[test]
    fn duration_forms() {
        assert_eq!((duration("90s"), duration("15m"), duration("2h")), (Some(90), Some(900), Some(7200)));
        for bad in ["", "m", "15", "0m", "-5m", "1.5h", "5d", "９m", "m９"] {
            assert_eq!(duration(bad), None, "{bad}");
        }
    }

    fn store() -> (PathBuf, Store) {
        let d = std::env::temp_dir().join(format!("gembala-recipe-{}-{:?}", std::process::id(), std::thread::current().id()));
        let _ = fs::remove_dir_all(&d);
        let s = Store::new(&d);
        (d, s)
    }

    #[test]
    fn approval_follows_the_bytes() {
        let (d, s) = store();
        s.ensure_defaults().unwrap();
        let rows = s.list(None);
        assert_eq!(rows.as_array().unwrap().len(), 2);
        assert!(rows.as_array().unwrap().iter().all(|r| r["status"] == "needs_approval"));
        assert_eq!(s.approved("fitur-baru", None).unwrap_err().code, "recipe_not_approved");

        let got = s.get("fitur-baru", None).unwrap();
        let h = got["hash"].as_str().unwrap().to_string();
        // a stale hash (file edited after review) is refused
        assert_eq!(s.approve("fitur-baru", "deadbeef", None).unwrap_err().code, "recipe_changed");
        s.approve("fitur-baru", &h, None).unwrap();
        assert_eq!(s.get("fitur-baru", None).unwrap()["status"], "approved");
        assert!(s.approved("fitur-baru", None).is_ok());

        // edited outside the app: no longer approved, shows as changed
        let p = d.join("recipes/fitur-baru.toml");
        fs::write(&p, format!("{}\n# edited\n", got["text"].as_str().unwrap())).unwrap();
        assert_eq!(s.get("fitur-baru", None).unwrap()["status"], "changed");
        assert_eq!(s.approved("fitur-baru", None).unwrap_err().code, "recipe_not_approved");

        // a broken recipe cannot be approved even with the right hash
        fs::write(&p, "name = \"x\"\n").unwrap();
        let h2 = s.get("fitur-baru", None).unwrap()["hash"].as_str().unwrap().to_string();
        assert_eq!(s.get("fitur-baru", None).unwrap()["status"], "broken");
        assert_eq!(s.approve("fitur-baru", &h2, None).unwrap_err().code, "recipe_invalid");

        s.delete("fitur-baru").unwrap();
        assert!(!p.exists() && s.approvals()["recipes"].get("fitur-baru").is_none());
        // deleted defaults are not written back
        s.ensure_defaults().unwrap();
        assert!(!p.exists());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn files_outside_the_folder_are_not_read() {
        let (d, s) = store();
        s.ensure_defaults().unwrap();
        assert_eq!(s.get("../x", None).unwrap_err().code, "recipe_id");
        assert_eq!(s.delete("a/b").unwrap_err().code, "recipe_id");
        // a symlink inside the folder pointing out is refused
        let outside = d.join("outside.toml");
        fs::write(&outside, FEATURE).unwrap();
        std::os::unix::fs::symlink(&outside, d.join("recipes/link.toml")).unwrap();
        assert_eq!(s.get("link", None).unwrap_err().code, "recipe_file");
        // oversized file
        fs::write(d.join("recipes/big.toml"), "#".repeat(70_000)).unwrap();
        assert_eq!(s.get("big", None).unwrap_err().code, "recipe_file");
        let _ = fs::remove_dir_all(&d);
    }
}
