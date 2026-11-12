//! FR-75: model choices come from the destination CLI, scoped to its login.
use super::*;
use std::io::{BufRead, BufReader, Read};
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

const EVERY: Duration = Duration::from_secs(600);
const LIMIT: Duration = Duration::from_secs(10);
const BYTES: u64 = 2 * 1024 * 1024;

#[derive(Default)]
pub struct Store(HashMap<(String, String), Entry>);

struct Entry {
    value: Value,
    checked: Option<Instant>,
    pending: bool,
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs()
}

pub fn list(st: &Arc<State>, p: &Value) -> Result<Value, &'static str> {
    let agent = p["agent"].as_str().ok_or("bad_args")?;
    if !usage::AGENTS.contains(&agent) {
        return Err("bad_args");
    }
    let login = match &p["account"] {
        Value::Null => "utama",
        Value::String(s) if s.is_empty() => "utama",
        Value::String(s) => s,
        _ => return Err("bad_args"),
    };
    let account = st.accounts.lock().unwrap().iter().find(|a| a.agent == agent && a.id == login).cloned();
    if login != "utama" && account.is_none() {
        return Err("no_such_account");
    }
    if agent == "claude" {
        let models: Vec<_> = account::MODELS.iter().map(|id| json!({"id": id, "label": id})).collect();
        return Ok(json!({"models": models, "source": "alias", "at": now(), "pending": false}));
    }
    let key = (agent.to_owned(), login.to_owned());
    let mut cache = st.models.lock().unwrap();
    let entry = cache.0.entry(key.clone()).or_insert_with(|| Entry {
        value: json!({"models": [], "source": "none", "at": 0}), checked: None, pending: false,
    });
    if !entry.pending && entry.checked.is_none_or(|at| at.elapsed() >= EVERY) {
        entry.pending = true;
        let st = st.clone();
        let key = key.clone();
        thread::spawn(move || {
            let path = st.agents.lock().unwrap().as_array()
                .and_then(|agents| agents.iter().find(|a| a["agent"] == key.0 && a["installed"] == true))
                .and_then(|a| a["path"].as_str()).unwrap_or(&key.0).to_owned();
            let home = account.as_ref().map(|a| account::expand(&a.dir));
            let models = probe(&path, &key.0, home.as_deref(), LIMIT).unwrap_or_default();
            let source = if models.is_empty() { "none" } else { "cli" };
            st.models.lock().unwrap().0.insert(key.clone(), Entry {
                value: json!({"models": models, "source": source, "at": now()}), checked: Some(Instant::now()), pending: false,
            });
            let ev = line(json!({"event": "models.changed", "params": {"agent": key.0, "account": key.1}}));
            st.ui_txs.lock().unwrap().retain(|(_, tx)| tx.try_send(ev.clone()).is_ok());
        });
    }
    let mut out = entry.value.clone();
    out["pending"] = json!(entry.pending);
    Ok(out)
}

fn parse(agent: &str, text: &str) -> Option<Vec<Value>> {
    let mut out = Vec::new();
    let mut add = |id: &str, label: &str| {
        if out.len() < 200 && !id.is_empty() && validate_model_effort(id, "").is_ok() && !out.iter().any(|m: &Value| m["id"] == id) {
            let label: String = label.chars().filter(|c| !c.is_control()).take(200).collect();
            out.push(json!({"id": id, "label": if label.is_empty() { id } else { &label }}));
        }
    };
    if agent == "commandcode" {
        // v1.88 --list-models: id, two or more spaces, description; ignore headings and CLI examples.
        for row in text.lines().take_while(|s| !s.starts_with("Pass the full id")) {
            if let Some((id, _)) = row.trim().split_once("  ") {
                add(id, id);
            }
        }
    } else if agent == "opencode" {
        for id in text.lines().map(str::trim).filter(|s| s.contains('/')) {
            add(id, id);
        }
    } else {
        let v: Value = serde_json::from_str(text).ok()?;
        let rows = match agent {
            "codex" => v["result"]["data"].as_array()?,
            "omp" => v["models"].as_array()?,
            "gemini" => v.as_array()?,
            _ => return None,
        };
        for row in rows {
            if agent == "omp" && row["kind"] != "chat" {
                continue;
            }
            let id_key = if agent == "omp" { "selector" } else { "id" };
            let label_key = if agent == "omp" { "name" } else { "displayName" };
            if let Some(id) = row[id_key].as_str() {
                add(id, row[label_key].as_str().unwrap_or(id));
            }
        }
    }
    Some(out)
}

/// Bound both output and lifetime; killing the process group also closes pipes inherited by CLI helpers.
fn probe(path: &str, agent: &str, home: Option<&std::path::Path>, limit: Duration) -> Option<Vec<Value>> {
    let mut c = Command::new(path);
    match agent {
        "codex" => { c.args(["-c", "approval_policy=never", "-c", "features.plugins=false", "-s", "read-only", "-a", "never", "app-server"]); }
        "omp" => { c.args(["models", "--json"]); }
        "opencode" => { c.arg("models"); }
        "gemini" => { c.args(["models", "-o", "json"]); }
        "commandcode" => { c.arg("--list-models"); }
        _ => return None,
    }
    pty::scrub_env(&mut c);
    if let Some(home) = home {
        // Named non-Claude accounts currently only have a Codex reader (the account loader enables Claude only).
        if agent != "codex" { return None; }
        c.env("CODEX_HOME", home);
    }
    c.process_group(0).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
    let mut child = c.spawn().ok()?;
    let mut stdin = child.stdin.take()?;
    let stdout = child.stdout.take()?;
    let rpc = agent == "codex";
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        if rpc {
            // Same app-server handshake as usage.rs; no login files or credentials are read here.
            let init = json!({"jsonrpc":"2.0", "id":1, "method":"initialize", "params":{"clientInfo":{"name":"gembala", "version":env!("CARGO_PKG_VERSION")}}});
            if writeln!(stdin, "{init}").is_err() { return; }
            let mut sent = false;
            for line in BufReader::new(stdout.take(BYTES)).lines().map_while(Result::ok) {
                let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
                match v["id"].as_u64() {
                    Some(1) if !sent && v.get("result").is_some() => {
                        let ready = json!({"jsonrpc":"2.0", "method":"initialized", "params":{}});
                        let read = json!({"jsonrpc":"2.0", "id":2, "method":"model/list", "params":{"limit":200}});
                        if writeln!(stdin, "{ready}").and_then(|_| writeln!(stdin, "{read}")).is_err() { return; }
                        sent = true;
                    }
                    Some(2) => { let _ = tx.send(line); return; }
                    _ => {}
                }
            }
        } else {
            drop(stdin);
            let mut out = String::new();
            if stdout.take(BYTES + 1).read_to_string(&mut out).is_ok() && out.len() <= BYTES as usize {
                let _ = tx.send(out);
            }
        }
    });
    let start = Instant::now();
    let got = rx.recv_timeout(limit).ok();
    let success = if rpc { got.is_some() } else {
        loop {
            match child.try_wait() {
                Ok(Some(status)) => break status.success(),
                Ok(None) if got.is_some() && start.elapsed() < limit => thread::sleep(Duration::from_millis(10)),
                _ => break false,
            }
        }
    };
    unsafe { libc::kill(-(child.id() as i32), libc::SIGKILL); }
    let _ = child.wait();
    if !success { return None; }
    parse(agent, &got?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    // Captured from the installed CLIs on 2026-11-11, reduced to public model metadata.
    const CODEX: &str = include_str!("../../tests/fixtures/models/codex.json");
    const OMP: &str = include_str!("../../tests/fixtures/models/omp.json");
    const OPENCODE: &str = include_str!("../../tests/fixtures/models/opencode.txt");
    const COMMANDCODE: &str = include_str!("../../tests/fixtures/models/commandcode.txt");

    #[test]
    fn commandcode_models_ignore_headings_and_examples() {
        assert_eq!(parse("commandcode", COMMANDCODE).unwrap(), vec![
            json!({"id":"deepseek/deepseek-v4-pro", "label":"deepseek/deepseek-v4-pro"}),
            json!({"id":"gpt-6.1-sol", "label":"gpt-6.1-sol"}),
        ]);
        assert!(parse("commandcode", "--yolo  injected\ngpt-good  valid\ngpt-good  duplicate").unwrap().len() == 1);
    }
    const GEMINI: &str = include_str!("../../tests/fixtures/models/gemini.json");

    #[test]
    fn cli_fixtures() {
        assert_eq!(parse("codex", CODEX).unwrap()[0], json!({"id":"gpt-6.1-sol", "label":"GPT-6.1-Sol"}));
        assert_eq!(parse("omp", OMP).unwrap()[0], json!({"id":"apple/on-device", "label":"Apple AFM 3 Core Advanced"}));
        assert_eq!(parse("opencode", OPENCODE).unwrap().len(), 5);
        assert_eq!(parse("gemini", GEMINI).unwrap()[0], json!({"id":"auto", "label":"Auto"}));
    }

    #[test]
    fn untrusted_model_ids_and_labels() {
        for agent in ["codex", "omp", "gemini"] {
            let rows = json!([
                {"id":"--yolo", "selector":"--yolo", "kind":"chat"},
                {"id":"bad model", "selector":"bad model", "kind":"chat"},
                {"id":"ok", "selector":"ok", "kind":"embedding"},
                {"id":"ok", "selector":"provider/ok", "kind":"chat", "displayName":"<b>ok</b>\u{001b}\n", "name":"<b>ok</b>\u{001b}\n"}
            ]);
            let input = match agent {
                "codex" => json!({"result":{"data":rows}}),
                "omp" => json!({"models":rows}),
                _ => rows,
            };
            let models = parse(agent, &input.to_string()).unwrap();
            assert!(models.iter().all(|v| validate_model_effort(v["id"].as_str().unwrap(), "").is_ok()));
            assert!(models.iter().all(|v| !v["label"].as_str().unwrap().contains('\u{001b}')));
            assert_eq!(models.len(), 1);
            assert!(parse(agent, "broken").is_none());
            assert!(parse(agent, "{}").is_none());
        }
        assert_eq!(parse("opencode", "--yolo\n--yolo/model\nprovider/good\nprovider/good\nnoise\n").unwrap(), vec![json!({"id":"provider/good", "label":"provider/good"})]);
        let large = (0..250).map(|i| format!("provider/model-{i}\n")).collect::<String>();
        assert_eq!(parse("opencode", &large).unwrap().len(), 200);
        assert!(parse("gemini", &json!([{"id":"x".repeat(65)}]).to_string()).unwrap().is_empty());
    }

    fn script(tag: &str, body: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("gbl-models-{}-{tag}", std::process::id()));
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[test]
    fn fake_clis_follow_protocol_and_filter_ids() {
        for (agent, fixture, args) in [("codex", CODEX, ""), ("omp", OMP, "models --json"), ("opencode", OPENCODE, "models"), ("gemini", GEMINI, "models -o json"), ("commandcode", COMMANDCODE, "--list-models")] {
            let handshake = if agent == "codex" {
                "test \"$CODEX_HOME\" = /tmp/gbl-models-home || exit 2\nread -r init\ncase $init in *initialize*) ;; *) exit 3;; esac\nprintf '%s\\n' '{\"id\":1,\"result\":{}}'\nread -r ready\ncase $ready in *initialized*) ;; *) exit 4;; esac\nread -r request\ncase $request in *model/list*) ;; *) exit 5;; esac\n".to_owned()
            } else { format!("test \"$*\" = '{args}' || exit 6\n") };
            let output = if agent == "codex" { serde_json::from_str::<Value>(fixture).unwrap().to_string() } else { fixture.to_owned() };
            let path = script(agent, &format!("{handshake}printf '%s\\n' '{}'", output.replace('\'', "'\\''")));
            let home = (agent == "codex").then_some(std::path::Path::new("/tmp/gbl-models-home"));
            // the real limit: a busy machine starts the fake CLI's shell slower than 2 s (failed under parallel tests)
            assert_eq!(probe(path.to_str().unwrap(), agent, home, LIMIT), parse(agent, fixture), "{agent}");
            std::fs::remove_file(path).unwrap();
        }
        let path = script("hostile", "printf '%s' '[{\"id\":\"--yolo\"},{\"id\":\"gemini-safe\"}]'");
        assert_eq!(probe(path.to_str().unwrap(), "gemini", None, LIMIT).unwrap(), vec![json!({"id":"gemini-safe", "label":"gemini-safe"})]);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn fake_cli_failure_and_timeout() {
        for (tag, body) in [("broken", "printf broken"), ("failed", "printf '[{\"id\":\"valid\"}]'; exit 1"), ("timeout", "sleep 20"), ("closed-stdout", "exec 1>&-; sleep 20")] {
            let path = script(tag, body);
            let start = Instant::now();
            assert!(probe(path.to_str().unwrap(), "gemini", None, Duration::from_millis(80)).is_none(), "{tag}");
            assert!(start.elapsed() < Duration::from_secs(2));
            std::fs::remove_file(path).unwrap();
        }
        assert!(probe("/nonexistent/gembala-agent", "gemini", None, LIMIT).is_none());
    }
}
