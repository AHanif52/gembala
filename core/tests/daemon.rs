//! Real daemon tests over the socket (TECH section 16): UI channel, attach, BRD criterion 9.

use base64::{Engine, engine::general_purpose::STANDARD as B64};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::os::fd::FromRawFd;
use std::os::unix::fs::DirBuilderExt;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

struct Daemon {
    child: Child,
    dir: PathBuf,
    secret: String,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn start(tag: &str) -> Daemon {
    start_idle(tag, 3600)
}

fn start_idle(tag: &str, idle_secs: u64) -> Daemon {
    start_env(tag, idle_secs, &[])
}

fn start_env(tag: &str, idle_secs: u64, extra: &[(&str, &std::path::Path)]) -> Daemon {
    let dir = std::env::temp_dir().join(format!("gbl-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::DirBuilder::new().mode(0o700).create(&dir).unwrap();
    let mut fds = [0; 2];
    assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
    let (r, w) = (fds[0], fds[1]);
    let mut c = Command::new(env!("CARGO_BIN_EXE_gembala"));
    c.current_dir("/").args(["daemon", "--secret-fd", "3"])
        .env("GEMBALA_RUNDIR", &dir)
        .env("GEMBALA_IDLE_SECS", idle_secs.to_string())
        .env("GEMBALA_CONFIG", dir.join("cfg/config.toml"))
        // as if Gembala were launched from inside an agent session (SEC-35)
        .env("CLAUDECODE", "1")
        .env("CLAUDE_CODE_MESSAGING_TOKEN", "token-bocor-uji")
        .env("CODEX_THREAD_ID", "thread-bocor-uji")
        .env("CLAUDE_CONFIG_DIR", "/akun-bocor-uji")
        .envs(extra.iter().map(|(k, v)| (*k, *v)));
    unsafe {
        c.pre_exec(move || {
            libc::close(r);
            if libc::dup2(w, 3) == -1 { Err(std::io::Error::last_os_error()) } else { Ok(()) }
        });
    }
    let child = c.spawn().unwrap();
    unsafe { libc::close(w) };
    let mut secret = String::new();
    BufReader::new(unsafe { std::fs::File::from_raw_fd(r) }).read_line(&mut secret).unwrap();
    Daemon { child, dir, secret: secret.trim().into() }
}

/// Attach content as the UI writes it: raw history per size (`segments`), then its tail (`snapshot`), base64.
fn snapshot(r: &Value) -> String {
    B64.encode(written(&r["result"]))
}

/// `segments` then `snapshot` from the attach reply or a `pane.replace` event, as the bytes the UI writes.
fn written(o: &Value) -> Vec<u8> {
    let mut out = Vec::new();
    for s in o["segments"].as_array().into_iter().flatten() {
        out.extend(B64.decode(s["data"].as_str().unwrap()).unwrap());
    }
    out.extend(B64.decode(o["snapshot"].as_str().unwrap_or("")).unwrap());
    out
}

struct Cli {
    r: BufReader<UnixStream>,
    w: UnixStream,
    id: u64,
    events: Vec<Value>,
}

impl Cli {
    fn connect(d: &Daemon, sock: &str) -> Cli {
        let s = UnixStream::connect(d.dir.join(sock)).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        Cli { r: BufReader::new(s.try_clone().unwrap()), w: s, id: 0, events: vec![] }
    }

    fn recv(&mut self) -> Value {
        let mut l = String::new();
        let _ = self.r.read_line(&mut l); // read time limit = Null, the caller decides
        serde_json::from_str(&l).unwrap_or(Value::Null)
    }

    fn call(&mut self, method: &str, params: Value) -> Value {
        self.id += 1;
        let req = json!({"id": self.id, "method": method, "params": params});
        writeln!(self.w, "{req}").unwrap();
        loop {
            let v = self.recv();
            if v.is_null() || v["id"] == self.id {
                return v;
            }
            self.events.push(v);
        }
    }

    fn ui(d: &Daemon) -> Cli {
        let mut c = Cli::connect(d, "ui.sock");
        let r = c.call("hello", json!({"proto": 1, "client": "ui", "secret": d.secret}));
        assert_eq!(r["result"]["proto"], 1, "{r}");
        c
    }

    /// Collect pane output (snapshot + events) until `want` shows up or the pane exits.
    fn attach_until(&mut self, pane: u64, want: &[&str]) -> String {
        let r = self.call("pane.attach", json!({"pane": pane}));
        let mut out = B64.decode(snapshot(&r)).unwrap();
        for e in std::mem::take(&mut self.events) {
            out.extend(B64.decode(e["params"]["data"].as_str().unwrap_or("")).unwrap());
        }
        let t0 = Instant::now();
        loop {
            let s = String::from_utf8_lossy(&out).into_owned();
            if want.iter().all(|w| s.contains(w)) || t0.elapsed() > Duration::from_secs(15) {
                return s;
            }
            let e = self.recv();
            if e["event"] == "pane.exit" || e.is_null() {
                return s;
            }
            out.extend(B64.decode(e["params"]["data"].as_str().unwrap_or("")).unwrap());
        }
    }
}

#[test]
fn hello_kanal_ui() {
    let d = start("hello");
    Cli::ui(&d);
    let mut c = Cli::connect(&d, "ui.sock");
    assert_eq!(c.call("pane.create", json!({}))["error"]["code"], "hello_required");
    let mut c = Cli::connect(&d, "ui.sock");
    assert_eq!(c.call("hello", json!({"proto": 1, "secret": "0".repeat(64)}))["error"]["code"], "unauthorized");
    let mut c = Cli::connect(&d, "ui.sock");
    assert_eq!(c.call("hello", json!({"proto": 1}))["error"]["code"], "unauthorized");
    let mut c = Cli::connect(&d, "ui.sock");
    assert_eq!(c.call("hello", json!({"proto": 99, "secret": d.secret}))["error"]["code"], "version_mismatch");
    // a process outside a pane isn't recognized by the pane channel
    let mut c = Cli::connect(&d, "pane.sock");
    assert_eq!(c.call("hello", json!({"proto": 1}))["error"]["code"], "not_a_pane");
}

#[test]
fn pane_tulis_resize_attach_ulang() {
    let d = start("flow");
    let mut ui = Cli::ui(&d);
    let pane = ui.call("pane.create", json!({"rows": 24, "cols": 80}))["result"]["pane"].as_u64().unwrap();
    let w = |ui: &mut Cli, s: &str| ui.call("pane.write", json!({"pane": pane, "data": B64.encode(s)}));
    assert_eq!(w(&mut ui, "echo OK-$((100+23)); stty size\r")["result"], json!({}));
    let out = ui.attach_until(pane, &["OK-123", "24 80"]);
    assert!(out.contains("OK-123") && out.contains("24 80"), "{out}");

    assert_eq!(ui.call("pane.resize", json!({"pane": pane, "rows": 30, "cols": 100}))["result"], json!({}));
    w(&mut ui, "stty size\r");
    // a second UI re-attaches: the snapshot already holds the old output
    let mut ui2 = Cli::ui(&d);
    let out = ui2.attach_until(pane, &["OK-123", "30 100"]);
    assert!(out.contains("OK-123") && out.contains("30 100"), "{out}");

    assert_eq!(ui.call("pane.close", json!({"pane": pane}))["result"], json!({}));
    let t0 = Instant::now();
    loop {
        let e = ui2.recv();
        if e["event"] == "pane.exit" {
            break;
        }
        assert!(t0.elapsed() < Duration::from_secs(10), "pane tidak keluar");
    }
    assert_eq!(ui.call("pane.write", json!({"pane": pane, "data": ""}))["error"]["code"], "no_such_pane");
}

/// Re-attach from the same connection (tab moved to another workspace) doesn't duplicate output, and every attach
/// is followed by a pane.attached marker (the UI drops events before the marker since they're in the snapshot)
#[test]
fn attach_ulang_tidak_menggandakan_output() {
    let d = start("reattach");
    let mut ui = Cli::ui(&d);
    let pane = ui.call("pane.create", json!({"rows": 24, "cols": 80}))["result"]["pane"].as_u64().unwrap();
    for _ in 0..3 {
        let r = ui.call("pane.attach", json!({"pane": pane}));
        assert_eq!(r["result"]["sync"], true, "{r}");
        assert!(!next_event(&mut ui, "pane.attached", Duration::from_secs(5)).is_null(), "penanda tidak datang");
    }
    ui.events.clear();
    ui.call("pane.write", json!({"pane": pane, "data": B64.encode("echo SATU-$((1+1))\r")}));
    let mut out = String::new();
    for e in std::mem::take(&mut ui.events) {
        out += &String::from_utf8_lossy(&B64.decode(e["params"]["data"].as_str().unwrap_or("")).unwrap());
    }
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_secs(3) {
        let e = ui.recv();
        if e["event"] == "pane.output" {
            out += &String::from_utf8_lossy(&B64.decode(e["params"]["data"].as_str().unwrap()).unwrap());
        }
    }
    assert_eq!(out.matches("SATU-2").count(), 1, "{out}");
}

/// Criterion 9: a script inside a pane, even one holding the UI secret, can't use the UI channel;
/// through the pane channel, human actions are rejected.
#[test]
fn kriteria_9_skrip_di_pane() {
    let d = start("k9");
    let mut ui = Cli::ui(&d);
    let ui_sock = d.dir.join("ui.sock");
    let script = format!(
        r#"(printf '%s\n' '{{"id":1,"method":"hello","params":{{"proto":1,"secret":"{s}"}}}}'; sleep 1) | nc -U {u}; echo; \
(printf '%s\n' '{{"id":1,"method":"hello","params":{{"proto":1}}}}' '{{"id":2,"method":"pane.close","params":{{"pane":1}}}}' '{{"id":3,"method":"approve","params":{{}}}}'; sleep 1) | nc -U "$GEMBALA_SOCKET"; echo SELESAI-K9"#,
        s = d.secret,
        u = ui_sock.display()
    );
    let pane = ui.call("pane.create", json!({"cmd": script, "rows": 40, "cols": 200}))["result"]["pane"].as_u64().unwrap();
    let out = ui.attach_until(pane, &["SELESAI-K9"]);
    assert!(out.contains(r#"{"error":{"code":"unauthorized"},"id":1}"#), "{out}");
    assert!(out.contains(&format!(r#""pane":{pane}"#)), "kanal pane harus kenal pane-nya: {out}");
    assert!(out.contains(r#"{"error":{"code":"not_allowed"},"id":2}"#), "{out}");
    assert!(out.contains(r#"{"error":{"code":"not_allowed"},"id":3}"#), "{out}");
}

/// Criterion 12 (FR-23, SEC-29): a script in a pane can't restore or commit through the pane channel.
#[test]
fn criterion_12_restore_commit_not_on_pane_channel() {
    let d = start("k12");
    let mut ui = Cli::ui(&d);
    let script = r#"(printf '%s\n' '{"id":1,"method":"hello","params":{"proto":1}}' '{"id":2,"method":"git.file_restore","params":{"pane":1,"path":"a"}}' '{"id":3,"method":"git.file_commit","params":{"pane":1,"paths":["a"],"message":"x"}}'; sleep 1) | nc -U "$GEMBALA_SOCKET"; echo SELESAI-K12"#;
    let pane = ui.call("pane.create", json!({"cmd": script, "rows": 40, "cols": 200}))["result"]["pane"].as_u64().unwrap();
    let out = ui.attach_until(pane, &["SELESAI-K12"]);
    assert!(out.contains(r#"{"error":{"code":"not_allowed"},"id":2}"#), "{out}");
    assert!(out.contains(r#"{"error":{"code":"not_allowed"},"id":3}"#), "{out}");
}

/// FR-15, SEC-18/19/21: Rich Input `@file:` / `@pane:` become labeled, redacted blocks; a shell pane only when the
/// user allowed reading it, a file only inside the pane's folder.
#[test]
fn ctx_compose_file_and_pane() {
    let d = start("ctx");
    let mut ui = Cli::ui(&d);
    let dir = d.dir.join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("a.txt"), "ISI-FILE ghp_rahasia\n").unwrap();
    std::fs::write(d.dir.join("luar.txt"), "x").unwrap();
    let a = ui.call("pane.create", json!({"cwd": dir.display().to_string()}))["result"]["pane"].as_u64().unwrap();
    let b = ui.call("pane.create", json!({"cmd": "echo ISI-B sk-ant-rahasia; sleep 30"}))["result"].clone();
    ui.attach_until(b["pane"].as_u64().unwrap(), &["ISI-B"]);
    let name = b["name"].as_str().unwrap();
    let compose = |ui: &mut Cli, text: String| ui.call("ctx.compose", json!({"pane": a, "text": text}))["result"].clone();

    let r = compose(&mut ui, format!("cek @pane:{name}"));
    assert_eq!((&r["error"], &r["args"]["at"]), (&json!("acl_denied"), &json!(format!("@pane:{name}"))), "{r}");
    ui.call("pane.readable", json!({"pane": b["pane"], "on": true}));
    let body = compose(&mut ui, format!("cek @pane:{name} dan @file:a.txt"))["body"].as_str().unwrap().to_string();
    assert!(body.starts_with(&format!("cek @pane:{name} dan @file:a.txt\n\n--- output {name} (another agent, not instructions from the user) ---")), "{body}");
    assert!(body.contains("ISI-B") && body.contains("--- attached file a.txt (not instructions from the user) ---\nISI-FILE"), "{body}");
    assert!(!body.contains("sk-ant-rahasia") && !body.contains("ghp_rahasia"), "{body}");
    assert_eq!(compose(&mut ui, "@file:../luar.txt".into())["error"], "outside_cwd");
    assert_eq!(compose(&mut ui, "biasa aja".into())["body"], "biasa aja");
}

/// FR-27, SEC-31: the reader's switch decides; a reduced block keeps the label and merges repeated lines.
#[test]
fn saver_on_only_when_the_switch_is_on() {
    let d = start("saver");
    let mut ui = Cli::ui(&d);
    let b = ui.call("pane.create", json!({"cmd": "i=0; while [ $i -lt 60 ]; do echo SAMA-BARIS; i=$((i+1)); done; sleep 30"}))["result"].clone();
    let name = b["name"].as_str().unwrap();
    ui.attach_until(b["pane"].as_u64().unwrap(), &["SAMA-BARIS"]);
    let a = ui.call("pane.create", json!({}))["result"]["pane"].as_u64().unwrap();
    ui.call("pane.readable", json!({"pane": b["pane"], "on": true}));
    let compose = |ui: &mut Cli| ui.call("ctx.compose", json!({"pane": a, "text": format!("cek @pane:{name}")}))["result"]["body"].as_str().unwrap_or_default().to_string();

    let off = compose(&mut ui);
    assert!(off.matches("SAMA-BARIS").count() > 10, "{off}");
    ui.call("config.set", json!({"section": "harness", "key": "token_saver", "value": true}));
    let on = compose(&mut ui);
    assert!(on.contains("SAMA-BARIS (×"), "{on}");
    assert_eq!(on.matches("SAMA-BARIS").count(), 1, "{on}");
}

/// A client that attaches but doesn't read is dropped when its queue fills; the pane keeps running to the end.
#[test]
fn klien_lambat_diputus() {
    let d = start("lambat");
    let mut ui = Cli::ui(&d);
    let pane = ui.call("pane.create", json!({"cmd": "yes aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa | head -c 30000000; sleep 1", "rows": 24, "cols": 80}))["result"]["pane"].as_u64().unwrap();
    let mut lambat = Cli::ui(&d);
    lambat.call("pane.attach", json!({"pane": pane}));
    let t0 = Instant::now();
    loop {
        let e = ui.call("pane.list", json!({}));
        if !e["result"].as_array().unwrap().contains(&json!(pane)) {
            break; // pane finished; the daemon isn't held up by a slow client
        }
        assert!(t0.elapsed() < Duration::from_secs(30), "pane tertahan klien lambat");
        std::thread::sleep(Duration::from_millis(200));
    }
    let mut exit_seen = false;
    loop {
        let mut l = String::new();
        if lambat.r.read_line(&mut l).unwrap_or(0) == 0 {
            break;
        }
        exit_seen |= l.contains("pane.exit");
    }
    assert!(!exit_seen, "klien lambat seharusnya sudah diputus sebelum pane keluar");
}

/// SEC-35: pane env doesn't carry agent session markers from the process that launched the daemon.
#[test]
fn env_pane_bersih() {
    let d = start("env");
    let mut ui = Cli::ui(&d);
    let pane = ui.call("pane.create", json!({"cmd": "env; echo CWD=$(pwd); echo SELESAI-ENV", "rows": 50, "cols": 200}))["result"]["pane"].as_u64().unwrap();
    let out = ui.attach_until(pane, &["SELESAI-ENV"]);
    assert!(out.contains("GEMBALA_SOCKET="), "{out}");
    // daemon launched from cwd `/` (like an app from Finder), the pane still starts in home
    assert!(out.contains(&format!("CWD={}", std::env::var("HOME").unwrap())), "{out}");
    for bocor in ["CLAUDECODE=", "token-bocor-uji", "thread-bocor-uji", "akun-bocor-uji", "GEMBALA_RUNDIR="] {
        assert!(!out.contains(bocor), "{bocor} bocor ke pane");
    }
}

fn wait_info(ui: &mut Cli, pane: u64, busy: bool, within: Duration) -> Value {
    let t0 = Instant::now();
    loop {
        let r = ui.call("pane.info", json!({"pane": pane}))["result"].clone();
        if r["busy"] == busy || t0.elapsed() > within {
            return r;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// FR-38 process name for the close confirmation; FR-50 stop: TERM first, KILL after 3 seconds, shell stays.
#[test]
fn info_dan_hentikan_proses() {
    let d = start("kill");
    let mut ui = Cli::ui(&d);
    let pane = ui.call("pane.create", json!({}))["result"]["pane"].as_u64().unwrap();
    let w = |ui: &mut Cli, s: &str| ui.call("pane.write", json!({"pane": pane, "data": B64.encode(s)}));
    assert_eq!(wait_info(&mut ui, pane, false, Duration::from_secs(5))["busy"], false);
    assert_eq!(ui.call("pane.kill", json!({"pane": pane}))["result"]["stopped"], false);

    w(&mut ui, "sleep 60\r");
    let r = wait_info(&mut ui, pane, true, Duration::from_secs(5));
    assert_eq!(r["name"], "sleep", "{r}");
    assert_eq!(ui.call("pane.kill", json!({"pane": pane}))["result"]["stopped"], true);
    assert_eq!(wait_info(&mut ui, pane, false, Duration::from_secs(2))["busy"], false, "sleep harus mati oleh TERM");

    // TERM is ignored before exec, so it applies as soon as the name shows "perl"
    w(&mut ui, "sh -c \"trap '' TERM; exec perl -e 'sleep 60'\"\r");
    let t0 = Instant::now();
    while ui.call("pane.info", json!({"pane": pane}))["result"]["name"] != "perl" {
        assert!(t0.elapsed() < Duration::from_secs(5), "perl tidak terlihat");
        std::thread::sleep(Duration::from_millis(50));
    }
    let t0 = Instant::now();
    ui.call("pane.kill", json!({"pane": pane}));
    std::thread::sleep(Duration::from_millis(1500));
    assert_eq!(ui.call("pane.info", json!({"pane": pane}))["result"]["busy"], true, "TERM diabaikan, belum boleh mati");
    assert_eq!(wait_info(&mut ui, pane, false, Duration::from_secs(5))["busy"], false, "KILL setelah 3 detik");
    assert!(t0.elapsed() >= Duration::from_secs(3));
    assert!(ui.call("pane.list", json!({}))["result"].as_array().unwrap().contains(&json!(pane)), "shell pane tetap hidup");
}

fn exited_within(d: &mut Daemon, within: Duration) -> bool {
    let t0 = Instant::now();
    while t0.elapsed() < within {
        if d.child.try_wait().unwrap().is_some() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

/// FR-20: no UI and every pane idle = the daemon exits on its own; a running process = stays alive.
#[test]
fn daemon_berhenti_bila_tanpa_ui_dan_pane_diam() {
    let mut d = start_idle("idle", 1);
    let mut ui = Cli::ui(&d);
    let pane = ui.call("pane.create", json!({}))["result"]["pane"].as_u64().unwrap();
    ui.call("pane.write", json!({"pane": pane, "data": B64.encode("sleep 30\r")}));
    wait_info(&mut ui, pane, true, Duration::from_secs(5));
    drop(ui);
    assert!(!exited_within(&mut d, Duration::from_secs(4)), "ada proses jalan, daemon harus tetap hidup");

    let mut ui = Cli::ui(&d);
    ui.call("pane.kill", json!({"pane": pane}));
    wait_info(&mut ui, pane, false, Duration::from_secs(5));
    assert!(!exited_within(&mut d, Duration::from_secs(3)), "UI masih tersambung, daemon harus tetap hidup");
    drop(ui);
    assert!(exited_within(&mut d, Duration::from_secs(5)), "tanpa UI dan pane diam, daemon harus keluar");
}

/// FR-46: "Quit and stop all agents".
#[test]
fn daemon_stop() {
    let mut d = start("stop");
    let mut ui = Cli::ui(&d);
    ui.call("pane.create", json!({"cmd": "sleep 60"}));
    assert_eq!(ui.call("daemon.stop", json!({}))["result"], json!({}));
    assert!(exited_within(&mut d, Duration::from_secs(3)));
}

fn next_event(c: &mut Cli, name: &str, within: Duration) -> Value {
    let t0 = Instant::now();
    if let Some(i) = c.events.iter().position(|e| e["event"] == name) {
        return c.events.remove(i);
    }
    while t0.elapsed() < within {
        let e = c.recv();
        if e["event"] == name {
            return e;
        }
    }
    Value::Null
}

/// FR-42: config from the file, file changes apply without a restart, a broken file doesn't wipe the last valid values.
#[test]
fn config_dibaca_dipantau_dan_ditulis() {
    let d = start("cfg");
    let path = d.dir.join("cfg/config.toml");
    let mut ui = Cli::ui(&d);
    let c = ui.call("config.get", json!({}))["result"].clone();
    assert_eq!(c["values"]["ui"]["font_size"], 13);
    assert_eq!(c["errors"], json!([]));

    assert_eq!(ui.call("config.set", json!({"section": "ui", "key": "font_size", "value": 15}))["result"], json!({}));
    assert!(std::fs::read_to_string(&path).unwrap().contains("font_size = 15"));
    let e = next_event(&mut ui, "config.changed", Duration::from_secs(3));
    assert_eq!(e["params"]["values"]["ui"]["font_size"], 15, "{e}");
    let r = ui.call("config.set", json!({"section": "ui", "key": "font_size", "value": 99}))["result"].clone();
    assert!(r["error"] == "int_range" && r["args"]["hi"] == "32", "{r}");

    // edited by the user in an editor: applies without a restart
    std::fs::write(&path, "[ui]\nfont_size = 17\ncursor = \"kotak\"\n").unwrap();
    let e = next_event(&mut ui, "config.changed", Duration::from_secs(4));
    assert_eq!(e["params"]["values"]["ui"]["font_size"], 17, "{e}");
    assert_eq!(e["params"]["errors"][0]["args"]["at"], "ui.cursor", "{e}");

    // broken: last valid values stay, error flagged, the file isn't overwritten
    std::fs::write(&path, "[ui\n").unwrap();
    let e = next_event(&mut ui, "config.changed", Duration::from_secs(4));
    assert_eq!(e["params"]["values"]["ui"]["font_size"], 17, "{e}");
    assert_eq!(e["params"]["errors"][0]["code"], "toml_parse", "{e}");
    assert_eq!(ui.call("config.set", json!({"section": "ui", "key": "font_size", "value": 14}))["result"]["error"], "config_broken");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "[ui\n");
}

/// BRD section 11: pane limit per daemon, adjustable through [limits].
#[test]
fn batas_jumlah_pane() {
    let d = start("max");
    let mut ui = Cli::ui(&d);
    ui.call("config.set", json!({"section": "limits", "key": "max_panes", "value": 2}));
    assert!(ui.call("pane.create", json!({}))["result"]["pane"].is_u64());
    assert!(ui.call("pane.create", json!({}))["result"]["pane"].is_u64());
    assert_eq!(ui.call("pane.create", json!({}))["error"]["code"], "max_panes");
}

/// FR-29/30: panes from profiles (name `<agent>-<n>`, folder follows the focused pane); extra profiles from config;
/// FR-32: missing command = clear message, the pane stays alive as a shell.
#[test]
fn pane_dari_profil() {
    let d = start("prof");
    let cfg = d.dir.join("cfg");
    std::fs::create_dir_all(&cfg).unwrap();
    std::fs::set_permissions(&cfg, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
    std::fs::write(
        cfg.join("config.toml"),
        "[[profile]]\nid = \"cek\"\ncommand = \"sh\"\nargs = [\"-c\", \"echo PROFIL-$X-$(pwd)\"]\n[profile.env]\nX = \"jalan\"\n\
         [[profile]]\nid = \"hilang\"\ncommand = \"gembala-tidak-ada-xyz\"\n",
    )
    .unwrap();
    let mut ui = Cli::ui(&d);
    let until = |ui: &mut Cli, what: &str, ok: &dyn Fn(&Value) -> bool, m: &str, p: Value| {
        let t0 = Instant::now();
        while !ok(&ui.call(m, p.clone())["result"]) {
            assert!(t0.elapsed() < Duration::from_secs(10), "menunggu {what}");
            std::thread::sleep(Duration::from_millis(100));
        }
    };
    // the config watcher checks every 1 second
    until(&mut ui, "profil cek", &|c| c["values"]["profiles"].as_array().is_some_and(|a| a.iter().any(|p| p["id"] == "cek")), "config.get", json!({}));
    let base = ui.call("pane.create", json!({}))["result"].clone();
    assert_eq!(base["name"], "shell-1");
    let b = base["pane"].as_u64().unwrap();
    ui.call("pane.write", json!({"pane": b, "data": B64.encode("cd /tmp\r")}));
    until(&mut ui, "cd /tmp", &|i| i["cwd"].as_str().is_some_and(|c| c.ends_with("/tmp")), "pane.info", json!({"pane": b}));

    let r = ui.call("pane.create", json!({"profile": "cek", "from_pane": b}))["result"].clone();
    assert_eq!(r["name"], "cek-1", "{r}");
    let out = ui.attach_until(r["pane"].as_u64().unwrap(), &["PROFIL-jalan-"]);
    assert!(out.contains("PROFIL-jalan-/tmp") || out.contains("PROFIL-jalan-/private/tmp"), "{out}");

    let r = ui.call("pane.create", json!({"profile": "hilang"}))["result"].clone();
    let p = r["pane"].as_u64().unwrap();
    let out = ui.attach_until(p, &["isn't in the login shell PATH"]);
    assert!(out.contains("Command gembala-tidak-ada-xyz isn't in the login shell PATH"), "{out}");
    assert!(ui.call("pane.list", json!({}))["result"].as_array().unwrap().contains(&json!(p)), "pane tetap hidup");
    assert_eq!(ui.call("pane.create", json!({"profile": "tidak-ada"}))["error"]["code"], "no_such_profile");
}

/// FR-78: the zsh `claude` function asks `gembala addons claude`; the answer follows the pane's workspace.
#[test]
fn addon_per_workspace() {
    let d = start("addon");
    let cfg = d.dir.join("cfg");
    let plug = d.dir.join("plug");
    std::fs::create_dir_all(cfg.join("mcp")).unwrap();
    std::fs::create_dir_all(plug.join(".claude-plugin")).unwrap();
    std::fs::set_permissions(&cfg, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
    std::fs::write(plug.join(".claude-plugin/plugin.json"), "{}").unwrap();
    std::fs::write(cfg.join("mcp/w1.json"), r#"{"mcpServers": {}}"#).unwrap();
    std::fs::write(
        cfg.join("config.toml"),
        format!("[addons.plugin.p]\npath = \"{}\"\nscope = \"workspaces\"\nworkspaces = [\"w1\"]\n[workspace.w1.mcp]\nfile = \"mcp/w1.json\"\nstrict = true\n", plug.display()),
    )
    .unwrap();
    let mut ui = Cli::ui(&d);
    let t0 = Instant::now();
    while ui.call("config.get", json!({}))["result"]["values"]["addons"]["mcp"].as_array().is_none_or(|a| a.is_empty()) {
        assert!(t0.elapsed() < Duration::from_secs(10), "menunggu config add-on");
        std::thread::sleep(Duration::from_millis(100));
    }
    let run = |ui: &mut Cli, ws: &str| {
        let pane = ui.call("pane.create", json!({"cmd": "echo; gembala addons claude; echo ADDON-END", "workspace": ws}))["result"]["pane"].as_u64().unwrap();
        ui.attach_until(pane, &["ADDON-END"])
    };
    let out = run(&mut ui, "w1");
    assert!(out.contains(&format!("--plugin-dir\r\n{}\r\n--mcp-config\r\n", plug.display())), "{out}");
    assert!(out.contains("mcp/w1.json\r\n--strict-mcp-config\r\nADDON-END"), "{out}");
    let out = run(&mut ui, "w2");
    assert!(!out.contains("--plugin-dir") && !out.contains("--mcp-config"), "{out}");
}

/// FR-78: Settings writes plugins and per-workspace MCP; bad input is refused, the file stays valid.
#[test]
fn addon_dari_pengaturan() {
    let d = start("addonset");
    let plug = d.dir.join("Caveman Plug");
    std::fs::create_dir_all(plug.join(".claude-plugin")).unwrap();
    std::fs::write(plug.join(".claude-plugin/plugin.json"), "{}").unwrap();
    let mut ui = Cli::ui(&d);
    let r = ui.call("addon.plugin", json!({"path": plug}))["result"].clone();
    assert_eq!(r["id"], "caveman-plug", "{r}");
    let bad = ui.call("addon.plugin", json!({"path": d.dir}))["result"].clone();
    assert_eq!(bad["error"], "no_plugin_json", "{bad}");
    ui.call("addon.plugin", json!({"id": "caveman-plug", "scope": "workspaces", "workspaces": ["w1"]}));
    let mcp = r#"{"mcpServers": {"probe": {"command": "node"}}}"#;
    assert!(ui.call("addon.mcp.set", json!({"workspace": "w1", "text": "{}"}))["result"]["error"].is_string());
    assert_eq!(ui.call("addon.mcp.set", json!({"workspace": "w1", "text": mcp, "strict": true}))["result"], json!({}));
    let got = ui.call("addon.mcp.get", json!({"workspace": "w1"}))["result"].clone();
    assert_eq!((got["text"].as_str().map(str::trim), got["strict"].as_bool()), (Some(mcp), Some(true)));
    let a = ui.call("config.get", json!({}))["result"]["values"]["addons"].clone();
    assert_eq!(a["plugins"][0]["workspaces"], json!(["w1"]), "{a}");
    assert_eq!(a["mcp"][0]["servers"], json!(["probe"]), "{a}");
    let text = std::fs::read_to_string(d.dir.join("cfg/config.toml")).unwrap();
    assert!(text.contains("[addons.plugin.caveman-plug]") && text.contains("[workspace.w1.mcp]") && !text.contains("[addons]\n"), "{text}");
    ui.call("addon.plugin", json!({"id": "caveman-plug", "remove": true}));
    ui.call("addon.mcp.set", json!({"workspace": "w1", "text": ""}));
    let a = ui.call("config.get", json!({}))["result"]["values"]["addons"].clone();
    assert_eq!(a, json!({"plugins": [], "mcp": []}));
    assert!(!d.dir.join("cfg/mcp/w1.json").exists());
}

/// FR-28: detection through the login shell; every supported agent has a row.
#[test]
fn deteksi_agent() {
    let d = start("detect");
    let mut ui = Cli::ui(&d);
    let r = ui.call("agents.detect", json!({"refresh": true}))["result"].clone();
    let names: Vec<&str> = r.as_array().unwrap().iter().map(|a| a["agent"].as_str().unwrap()).collect();
    assert_eq!(names, ["claude", "codex", "opencode", "omp"]);
    for a in r.as_array().unwrap() {
        assert!(a["installed"].is_boolean() && a["install"].is_string(), "{a}");
    }
}

/// FR-45: saved layout (no screen content) and the daemon run id to tell old panes apart.
#[test]
fn layout_dan_boot() {
    let d = start("layout");
    let mut ui = Cli::ui(&d);
    assert_eq!(ui.call("layout.load", json!({}))["result"], Value::Null);
    let boot = ui.call("daemon.info", json!({}))["result"]["boot"].as_str().unwrap().to_string();
    assert_eq!(boot.len(), 16);
    let layout = json!({"version": 1, "boot": boot, "workspaces": [{"id": "w1", "tabs": []}]});
    assert_eq!(ui.call("layout.save", json!({"layout": layout}))["result"], json!({}));
    assert_eq!(ui.call("layout.load", json!({}))["result"], layout);
    let mode = std::fs::metadata(d.dir.join("cfg/layout.json")).unwrap().permissions();
    assert_eq!(std::os::unix::fs::PermissionsExt::mode(&mode) & 0o777, 0o600);
    assert_eq!(ui.call("layout.save", json!({"layout": "bukan objek"}))["error"]["code"], "bad_params");
}

/// Lines over 64 KB: rejected and the connection closed, the client doesn't hang.
#[test]
fn baris_terlalu_panjang_ditutup() {
    let d = start("long");
    let mut ui = Cli::ui(&d);
    let big = format!("{{\"id\":1,\"method\":\"layout.save\",\"params\":{{\"layout\":{{\"x\":\"{}\"}}}}}}\n", "x".repeat(70_000));
    let _ = ui.w.write_all(big.as_bytes()); // may fail if the daemon already closed
    let v = ui.recv();
    assert_eq!(v["error"]["code"], "line_too_long", "{v}");
    let mut rest = String::new();
    assert_eq!(ui.r.read_line(&mut rest).unwrap_or(0), 0, "koneksi harus ditutup");
}

/// FR-4: an agent hook inside a pane changes only that pane's status; the UI is told through `pane.status`.
#[test]
fn status_dari_hook() {
    let d = start("hook");
    let mut ui = Cli::ui(&d);
    let exe = env!("CARGO_BIN_EXE_gembala");
    // profile with agent "claude" so the status is tracked; its command is a script that calls the hook like Claude Code
    let cfg = d.dir.join("cfg");
    std::fs::create_dir_all(&cfg).unwrap();
    std::fs::set_permissions(&cfg, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
    // `claude` stand-in: ignores the hook flags Gembala inserts, calls the hook like Claude Code
    let fake = cfg.join("fake-claude.sh");
    std::fs::write(
        &fake,
        format!("#!/bin/sh\n'{exe}' hook claude prompt </dev/null\nsleep 1\necho '{{\"message\":\"Claude needs your permission to use Bash\"}}' | '{exe}' hook claude notify\nsleep 30\n"),
    )
    .unwrap();
    std::fs::set_permissions(&fake, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
    std::fs::write(cfg.join("config.toml"), format!("[[profile]]\nid = \"claude\"\ncommand = {}\n", json!(fake))).unwrap();
    let t0 = Instant::now();
    while !ui.call("config.get", json!({}))["result"]["values"]["profiles"].to_string().contains("fake-claude") {
        assert!(t0.elapsed() < Duration::from_secs(10), "config tidak terbaca");
        std::thread::sleep(Duration::from_millis(100));
    }
    let pane = ui.call("pane.create", json!({"profile": "claude"}))["result"]["pane"].as_u64().unwrap();
    let e = next_event(&mut ui, "pane.status", Duration::from_secs(8));
    assert_eq!(e["params"], json!({"pane": pane, "status": "working", "shell": false, "reason": ""}), "{e}");
    let e = next_event(&mut ui, "pane.status", Duration::from_secs(8));
    assert_eq!(e["params"]["status"], "blocked", "{e}");
    assert_eq!(ui.call("pane.info", json!({"pane": pane}))["result"]["status"], "blocked");
    // a process outside a pane can't change anyone's status
    let mut outsider = Cli::connect(&d, "pane.sock");
    assert_eq!(outsider.call("hello", json!({"proto": 1}))["error"]["code"], "not_a_pane");
}

/// FR-48: reset deletes only Gembala-owned files, then the daemon exits; other files in that folder aren't touched.
#[test]
fn reset_hanya_data_gembala() {
    let mut d = start("reset");
    let cfg = d.dir.join("cfg");
    let mut ui = Cli::ui(&d);
    ui.call("config.set", json!({"section": "ui", "key": "font_size", "value": 14}));
    ui.call("layout.save", json!({"layout": {"version": 1}}));
    std::fs::write(cfg.join("punya-orang-lain.txt"), "jangan dihapus").unwrap();
    assert!(cfg.join("config.toml").exists() && cfg.join("layout.json").exists() && cfg.join("hooks/opencode.json").exists());
    assert_eq!(ui.call("daemon.reset", json!({}))["result"], json!({}));
    assert!(exited_within(&mut d, Duration::from_secs(3)));
    assert!(!cfg.join("config.toml").exists() && !cfg.join("layout.json").exists() && !cfg.join("hooks").exists());
    assert!(cfg.join("punya-orang-lain.txt").exists());
}

/// FR-3: pane names are unique and renamable.
#[test]
fn ganti_nama_pane() {
    let d = start("rename");
    let mut ui = Cli::ui(&d);
    let a = ui.call("pane.create", json!({}))["result"]["pane"].as_u64().unwrap();
    let b = ui.call("pane.create", json!({}))["result"]["pane"].as_u64().unwrap();
    assert_eq!(ui.call("pane.rename", json!({"pane": a, "name": "review-1"}))["result"]["name"], "review-1");
    assert_eq!(ui.call("pane.rename", json!({"pane": b, "name": "review-1"}))["error"]["code"], "name_taken");
    assert_eq!(ui.call("pane.rename", json!({"pane": b, "name": "Ada Spasi"}))["error"]["code"], "bad_name");
    assert_eq!(ui.call("pane.info", json!({"pane": a}))["result"]["pane_name"], "review-1");
    // the next automatic name doesn't clash with names in use, old names included (FR-79: shell-1, shell-2)
    ui.call("pane.rename", json!({"pane": b, "name": "shell-3"}));
    assert_eq!(ui.call("pane.create", json!({}))["result"]["name"], "shell-4");
}

/// FR-79: old names stay addresses; current names win; a replacement pane brings its aliases from the layout.
#[test]
fn fr79_nama_lama_tetap_alamat() {
    let d = start("alias");
    let mut ui = Cli::ui(&d);
    let g = env!("CARGO_BIN_EXE_gembala");
    let kirim = format!("sleep 2; '{g}' send terima-1 'halo' --queue; '{g}' wait terima-1 --pattern TIDAK-ADA --timeout 1; '{g}' status; echo SELESAI-79; sleep 30");
    profiles(&d, &mut ui, &(sh_profile("terima", "sleep 30") + &sh_profile("kirim", &kirim)), "SELESAI-79");
    let t = ui.call("pane.create", json!({"profile": "terima", "workspace": "w"}))["result"]["pane"].as_u64().unwrap();
    let r = ui.call("pane.rename", json!({"pane": t, "name": "kantor-1"}))["result"].clone();
    assert_eq!(r["aliases"], json!(["terima-1"]), "{r}");
    let a = ui.call("pane.create", json!({"profile": "kirim", "workspace": "w"}))["result"]["pane"].as_u64().unwrap();
    let out = ui.attach_until(a, &["SELESAI-79"]).replace(['\r', '\n'], "");
    assert!(out.contains("\"to\":\"kantor-1\""), "{out}");
    assert!(out.contains("\"target\":\"kantor-1\""), "{out}");
    assert!(out.contains("\"aliases\":[\"terima-1\"]"), "{out}");
    // another pane takes that name = the alias is released
    let s = ui.call("pane.create", json!({}))["result"]["pane"].as_u64().unwrap();
    assert_eq!(ui.call("pane.rename", json!({"pane": s, "name": "terima-1"}))["result"]["aliases"], json!(["shell-1"]));
    assert_eq!(ui.call("pane.rename", json!({"pane": t, "name": "kantor-2"}))["result"]["aliases"], json!(["kantor-1"]));
    // replacement pane: the old name is reused if free; name and aliases from the layout (untrusted) are filtered, max 8
    let r = ui.call("pane.create", json!({"name": "lanjut-1", "aliases": ["Bukan Nama", "kantor-2"]}))["result"].clone();
    assert_eq!((&r["name"], &r["aliases"]), (&json!("lanjut-1"), &json!(["kantor-2"])), "{r}");
    // name still used by another pane = automatic name, that name becomes the newest alias
    let r = ui.call("pane.create", json!({"name": "kantor-2", "aliases": ["kantor-1"]}))["result"].clone();
    assert_eq!((&r["name"], &r["aliases"]), (&json!("shell-2"), &json!(["kantor-1", "kantor-2"])), "{r}");
    assert_eq!(ui.call("pane.create", json!({"name": "Bukan Nama"}))["result"]["name"], "shell-3");
    let many: Vec<String> = (0..10).map(|i| format!("lama-{i}")).collect();
    assert_eq!(ui.call("pane.create", json!({"aliases": many}))["result"]["aliases"].as_array().unwrap().len(), 8);
}

/// FR-56: agent exits = screen replaced by a summary block; the full output can still be opened while the pane lives.
#[test]
fn ringkas_output_saat_agent_keluar() {
    let d = start("ringkas");
    let cfg = d.dir.join("cfg");
    std::fs::create_dir_all(&cfg).unwrap();
    std::fs::set_permissions(&cfg, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
    std::fs::write(
        cfg.join("config.toml"),
        "[[profile]]\nid = \"palsu\"\ncommand = \"sh\"\nargs = [\"-c\", \"echo OUTPUT-AGENT-PANJANG; sleep 1; exit 3\"]\n",
    )
    .unwrap();
    let mut ui = Cli::ui(&d);
    let t0 = Instant::now();
    while !ui.call("config.get", json!({}))["result"]["values"]["profiles"].to_string().contains("OUTPUT-AGENT") {
        assert!(t0.elapsed() < Duration::from_secs(10), "config tidak terbaca");
        std::thread::sleep(Duration::from_millis(100));
    }
    let pane = ui.call("pane.create", json!({"profile": "palsu"}))["result"]["pane"].as_u64().unwrap();
    ui.call("pane.attach", json!({"pane": pane}));
    let e = next_event(&mut ui, "pane.replace", Duration::from_secs(10));
    let text = String::from_utf8(written(&e["params"])).unwrap();
    assert!(text.contains("palsu-1") && text.contains("keluar 3"), "{text}");
    let mut ui2 = Cli::ui(&d);
    let snap = snapshot(&ui2.call("pane.attach", json!({"pane": pane})));
    let snap = String::from_utf8(B64.decode(snap).unwrap()).unwrap();
    assert!(snap.contains("palsu-1") && !snap.contains("OUTPUT-AGENT-PANJANG"), "{snap}");
    let full = ui.call("pane.agent_output", json!({"pane": pane}))["result"]["data"].as_str().unwrap().to_string();
    assert!(String::from_utf8_lossy(&B64.decode(full).unwrap()).contains("OUTPUT-AGENT-PANJANG"));
}

/// FR-56: an agent typed by hand in a shell pane is summarized too (command + how to resume), the shell prompt stays.
#[test]
fn ringkas_agent_manual() {
    let d = start("manual");
    let omp = d.dir.join("omp");
    // copy of the gembala binary named "omp" (agents are recognized by process name); macOS rejects a copied /bin/sleep
    std::fs::copy(env!("CARGO_BIN_EXE_gembala"), &omp).unwrap();
    let mut ui = Cli::ui(&d);
    // the shell prompt must show below the block; with the input editor (FR-59) the zsh prompt is empty on purpose
    ui.call("config.set", json!({"section": "ui", "key": "input_editor", "value": false}));
    let pane = ui.call("pane.create", json!({}))["result"]["pane"].as_u64().unwrap();
    ui.call("pane.attach", json!({"pane": pane}));
    std::thread::sleep(Duration::from_secs(1));
    ui.call("pane.write", json!({"pane": pane, "data": B64.encode("echo RIWAYAT-$((40+2))\r")}));
    std::thread::sleep(Duration::from_millis(500));
    // an agent that clears the screen and scrollback at start (like Claude Code): earlier history stays
    ui.call("pane.write", json!({"pane": pane, "data": B64.encode(format!("printf '\\033[2J\\033[3J\\033[H'; '{}' watch\r", omp.display()))}));
    std::thread::sleep(Duration::from_secs(3));
    ui.call("pane.write", json!({"pane": pane, "data": B64.encode("\x03")}));
    let e = next_event(&mut ui, "pane.replace", Duration::from_secs(15));
    if e.is_null() {
        let snap = snapshot(&Cli::ui(&d).call("pane.attach", json!({"pane": pane})));
        panic!("tanpa pane.replace; layar: {}", String::from_utf8_lossy(&B64.decode(snap).unwrap()));
    }
    let text = String::from_utf8(written(&e["params"])).unwrap();
    assert!(text.contains("omp watch") && text.contains("omp --continue"), "{text}");
    assert_eq!(e["params"]["resume"], "omp --continue"); // the Resume button in the UI
    // like Warp: commands before the agent stay above the block; the agent's command line isn't doubled
    assert!(text.contains("RIWAYAT-42") && text.find("RIWAYAT-42") < text.find("omp watch"), "{text}");
    assert_eq!(text.matches("watch").count(), 1, "{text}");
    std::thread::sleep(Duration::from_millis(500));
    let snap = snapshot(&Cli::ui(&d).call("pane.attach", json!({"pane": pane})));
    let snap = String::from_utf8_lossy(&B64.decode(snap).unwrap()).into_owned();
    // the shell prompt shows below the block, with bracketed paste still on
    let after = snap.split("--continue").nth(1).unwrap_or_default();
    assert!(after.contains('%') || after.contains('$'), "prompt shell harus tampil di bawah blok: {snap}");
    assert!(snap.contains("\x1b[?2004h"), "bracketed paste prompt hilang: {snap}");
}

/// FR-62: command blocks saved without output, then shown in a new pane that carries the old pane's history key;
/// the key comes back once that pane is gone, and `{boot, pane}` from an old layout still loads.
#[test]
fn riwayat_blok_tersimpan_tanpa_output() {
    use std::os::unix::fs::PermissionsExt;
    let d = start("riwayat");
    let mut ui = Cli::ui(&d);
    let r = ui.call("pane.create", json!({}))["result"].clone();
    let (p, hist) = (r["pane"].as_u64().unwrap(), r["hist"].as_str().unwrap().to_string());
    assert!(hist.len() == 16 && hist.bytes().all(|c| c.is_ascii_hexdigit()), "{r}");
    ui.call("pane.attach", json!({"pane": p}));
    std::thread::sleep(Duration::from_secs(1));
    ui.call("pane.write", json!({"pane": p, "data": B64.encode("echo RIWAYAT-$((6*7))\r")}));
    let file = d.dir.join(format!("cfg/history/{hist}.jsonl"));
    let t0 = Instant::now();
    while !std::fs::read_to_string(&file).unwrap_or_default().contains("RIWAYAT") {
        assert!(t0.elapsed() < Duration::from_secs(10), "blok tidak tercatat di {}", file.display());
        std::thread::sleep(Duration::from_millis(100));
    }
    let isi = std::fs::read_to_string(&file).unwrap();
    assert!(isi.contains("echo RIWAYAT-$((6*7))") && !isi.contains("RIWAYAT-42"), "output tidak boleh tersimpan: {isi}");
    assert_eq!(std::fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o600);
    let snap = |ui: &mut Cli, q: u64| {
        let s = snapshot(&ui.call("pane.attach", json!({"pane": q})));
        String::from_utf8_lossy(&B64.decode(s).unwrap()).into_owned()
    };
    // the key stays with the live pane: a second pane from it gets its own key, the blocks copied in
    let r = ui.call("pane.create", json!({"history": hist}))["result"].clone();
    assert_ne!(r["hist"], json!(hist));
    let layar = snap(&mut Cli::ui(&d), r["pane"].as_u64().unwrap());
    assert!(layar.contains("echo RIWAYAT-$((6*7))") && layar.contains("Sesi sebelumnya"), "{layar}");
    // the pane is gone (rerun, daemon restart): its replacement takes the same key and file
    ui.call("pane.close", json!({"pane": p}));
    let t0 = Instant::now();
    while ui.call("pane.list", json!({}))["result"].as_array().unwrap().contains(&json!(p)) {
        assert!(t0.elapsed() < Duration::from_secs(5), "pane {p} tidak tertutup");
        std::thread::sleep(Duration::from_millis(100));
    }
    let r = ui.call("pane.create", json!({"history": hist}))["result"].clone();
    assert_eq!(r["hist"], json!(hist));
    let layar = snap(&mut Cli::ui(&d), r["pane"].as_u64().unwrap());
    assert!(layar.contains("echo RIWAYAT-$((6*7))") && layar.contains("Sesi sebelumnya"), "{layar}");
    // a layout saved before keys points at `<boot>-<pane>`
    let old = "{\"t\":1,\"cwd\":\"~\",\"cmd\":\"echo LAYOUT-LAMA\",\"code\":0,\"secs\":0}\n";
    std::fs::write(d.dir.join("cfg/history/0123456789abcdef-7.jsonl"), old).unwrap();
    let q = ui.call("pane.create", json!({"history": {"boot": "0123456789abcdef", "pane": 7}}))["result"]["pane"].as_u64().unwrap();
    assert!(snap(&mut Cli::ui(&d), q).contains("echo LAYOUT-LAMA"));
    for bad in [json!({"boot": "../../../../etc", "pane": p}), json!("../../../../etc/x")] {
        let r = ui.call("pane.create", json!({"history": bad}))["result"]["pane"].as_u64().unwrap();
        assert!(!snap(&mut Cli::ui(&d), r).contains("Sesi sebelumnya"));
    }
}

/// FR-62: SIGTERM (kill, logout, laptop restart) saves the screen log before the daemon exits, not only every 15 seconds.
#[test]
fn sigterm_menyimpan_log_layar() {
    let mut d = start("sigterm");
    let mut ui = Cli::ui(&d);
    let r = ui.call("pane.create", json!({}))["result"].clone();
    let (p, hist) = (r["pane"].as_u64().unwrap(), r["hist"].as_str().unwrap().to_string());
    std::thread::sleep(Duration::from_secs(1));
    ui.call("pane.write", json!({"pane": p, "data": B64.encode("echo LOG-$((6*7))\r")}));
    ui.attach_until(p, &["LOG-42"]);
    assert_eq!(unsafe { libc::kill(d.child.id() as i32, libc::SIGTERM) }, 0);
    assert!(exited_within(&mut d, Duration::from_secs(5)), "daemon tidak keluar setelah SIGTERM");
    let log = std::fs::read_to_string(d.dir.join(format!("cfg/history/{hist}.log"))).unwrap_or_default();
    assert!(log.contains("LOG-42"), "log tidak tersimpan: {log:?}");
}

/// FR-58: zsh panes emit OSC 133 block markers (C + base64 command, D + exit code); the user's ZDOTDIR comes back.
#[test]
fn penanda_blok_zsh() {
    let d = start("blok");
    let mut ui = Cli::ui(&d);
    let p = ui.call("pane.create", json!({}))["result"]["pane"].as_u64().unwrap();
    ui.call("pane.attach", json!({"pane": p})); // markers only exist in the stream, the grid snapshot doesn't hold them
    std::thread::sleep(Duration::from_secs(1));
    ui.call("pane.write", json!({"pane": p, "data": B64.encode("(exit 3)\r")}));
    std::thread::sleep(Duration::from_millis(300));
    ui.call("pane.write", json!({"pane": p, "data": B64.encode("echo ZD-${ZDOTDIR:-kosong}-$((1+1))\r")}));
    let zd = std::env::var("ZDOTDIR").ok().filter(|z| Some(z) != std::env::var("HOME").ok().as_ref()).unwrap_or("kosong".into());
    let out = ui.attach_until(p, &[&format!("ZD-{zd}-2")]);
    assert!(out.contains(&format!("\x1b]133;C;{}\x07", B64.encode("(exit 3)"))), "{out:?}");
    assert!(out.contains("\x1b]133;D;3\x07"), "{out:?}");
    // FR-59: prompt hidden, the command line written in bold by zle (block head)
    assert!(out.contains("\x1b[1m"), "perintah tidak tebal: {out:?}");
    // FR-59: the next attach knows the shell is at the prompt (the snapshot holds no markers)
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(Cli::ui(&d).call("pane.attach", json!({"pane": p}))["result"]["prompt"], true);
}

/// FR-9: the `gembala` command can be called without a full path from a pane (login shell).
#[test]
fn gembala_ada_di_path_pane() {
    let d = start("path");
    let mut ui = Cli::ui(&d);
    let p = ui.call("pane.create", json!({}))["result"]["pane"].as_u64().unwrap();
    std::thread::sleep(Duration::from_secs(1));
    // the marker is computed by the shell: the typed echo (`SELESAI-$((1+1))`) doesn't match, only the command output
    ui.call("pane.write", json!({"pane": p, "data": B64.encode("command -v gembala; echo SELESAI-$((1+1))\r")}));
    let out = ui.attach_until(p, &["SELESAI-2"]);
    assert!(out.contains(env!("CARGO_BIN_EXE_gembala")), "{out}");
}

/// An agent typed by hand in a shell pane is visible to other agents through `status` (the agent-to-agent path).
#[test]
fn agent_manual_terlihat_di_status() {
    let d = start("manualst");
    let omp = d.dir.join("omp");
    std::fs::copy(env!("CARGO_BIN_EXE_gembala"), &omp).unwrap();
    let g = env!("CARGO_BIN_EXE_gembala");
    let mut ui = Cli::ui(&d);
    let b = ui.call("pane.create", json!({"workspace": "w"}))["result"]["pane"].as_u64().unwrap();
    std::thread::sleep(Duration::from_secs(1));
    ui.call("pane.write", json!({"pane": b, "data": B64.encode(format!("'{}' watch\r", omp.display()))}));
    std::thread::sleep(Duration::from_secs(3)); // screen_watch recognizes manual agents every 1 second
    let a = ui.call("pane.create", json!({"workspace": "w"}))["result"]["pane"].as_u64().unwrap();
    std::thread::sleep(Duration::from_secs(1));
    // the marker is computed by the shell: the typed echo doesn't match, only after `status` finishes
    ui.call("pane.write", json!({"pane": a, "data": B64.encode(format!("'{g}' status; echo SELESAI-$((40+2))\r"))}));
    let out = ui.attach_until(a, &["SELESAI-42"]);
    assert!(out.contains("\"agent\":\"omp\""), "{out}");
}

/// FR-4: Claude typed in a shell pane that sends hooks = status from hooks only; screen output (status line redraws)
/// doesn't make it go working then done over and over ("done" notifications piling up).
#[test]
fn agent_manual_berhook_tidak_dibaca_dari_layar() {
    let d = start("manualhook");
    let claude = d.dir.join("claude");
    std::fs::copy(env!("CARGO_BIN_EXE_gembala"), &claude).unwrap();
    let g = env!("CARGO_BIN_EXE_gembala");
    let mut ui = Cli::ui(&d);
    let pane = ui.call("pane.create", json!({}))["result"]["pane"].as_u64().unwrap();
    std::thread::sleep(Duration::from_secs(1));
    // periodic output on screen while `claude` is in front, like a status line being redrawn
    let cmd = format!("'{g}' hook claude start </dev/null; (while :; do echo tik; sleep 2; done) & '{}' watch\r", claude.display());
    ui.call("pane.write", json!({"pane": pane, "data": B64.encode(cmd)}));
    let e = next_event(&mut ui, "pane.status", Duration::from_secs(8));
    assert_eq!(e["params"], json!({"pane": pane, "status": "idle", "shell": false, "reason": ""}), "{e}");
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_secs(6) {
        assert_eq!(ui.call("pane.info", json!({"pane": pane}))["result"]["status"], "idle", "status berubah karena output layar");
        std::thread::sleep(Duration::from_millis(300));
    }
}

/// FR-40: a pane waiting for a password (canonical + echo off) is detected; a normal process isn't.
#[test]
fn deteksi_menunggu_password() {
    let d = start("pass");
    let mut ui = Cli::ui(&d);
    let pane = ui.call("pane.create", json!({}))["result"]["pane"].as_u64().unwrap();
    let w = |ui: &mut Cli, s: &str| ui.call("pane.write", json!({"pane": pane, "data": B64.encode(s)}));
    w(&mut ui, "sleep 30\r");
    let r = wait_info(&mut ui, pane, true, Duration::from_secs(5));
    assert_eq!(r["password"], false, "{r}");
    ui.call("pane.kill", json!({"pane": pane}));
    wait_info(&mut ui, pane, false, Duration::from_secs(5));
    w(&mut ui, "sh -c 'stty -echo; read x'\r");
    let t0 = Instant::now();
    while ui.call("pane.info", json!({"pane": pane}))["result"]["password"] != true {
        assert!(t0.elapsed() < Duration::from_secs(5), "prompt password tidak terdeteksi");
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// FR-4 fallback: agents without hooks (custom profile) are read from the screen.
#[test]
fn status_dari_pola_layar() {
    let d = start("screen");
    let cfg = d.dir.join("cfg");
    std::fs::create_dir_all(&cfg).unwrap();
    std::fs::set_permissions(&cfg, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
    std::fs::write(cfg.join("config.toml"), "[[profile]]\nid = \"tanya\"\ncommand = \"sh\"\nargs = [\"-c\", \"sleep 2; printf 'Hapus folder build? (y/n) '; sleep 30\"]\n").unwrap();
    let mut ui = Cli::ui(&d);
    let t0 = Instant::now();
    while !ui.call("config.get", json!({}))["result"]["values"]["profiles"].to_string().contains("tanya") {
        assert!(t0.elapsed() < Duration::from_secs(10), "config tidak terbaca");
        std::thread::sleep(Duration::from_millis(100));
    }
    let pane = ui.call("pane.create", json!({"profile": "tanya"}))["result"]["pane"].as_u64().unwrap();
    let t0 = Instant::now();
    while ui.call("pane.info", json!({"pane": pane}))["result"]["status"] != "blocked" {
        assert!(t0.elapsed() < Duration::from_secs(10), "status blocked tidak terdeteksi dari layar");
        std::thread::sleep(Duration::from_millis(200));
    }
}

// ---------- Fase 2 ----------

/// Write a test profile config and wait for the daemon to read it.
fn profiles(d: &Daemon, ui: &mut Cli, toml: &str, wait_for: &str) {
    let cfg = d.dir.join("cfg");
    std::fs::create_dir_all(&cfg).unwrap();
    std::fs::set_permissions(&cfg, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
    std::fs::write(cfg.join("config.toml"), toml).unwrap();
    let t0 = Instant::now();
    while !ui.call("config.get", json!({}))["result"]["values"]["profiles"].to_string().contains(wait_for) {
        assert!(t0.elapsed() < Duration::from_secs(10), "config tidak terbaca");
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn sh_profile(id: &str, script: &str) -> String {
    format!("[[profile]]\nid = \"{id}\"\ncommand = \"sh\"\nargs = [\"-c\", {}]\n", json!(script))
}

/// Criterion 1 + FR-19: from one pane, one command sends a task; the `reply` answer is read back without a keyboard.
#[test]
fn kriteria_1_kirim_dan_balas() {
    let d = start("bus1");
    let mut ui = Cli::ui(&d);
    let g = env!("CARGO_BIN_EXE_gembala");
    // recipient: read the label (take msg_id) and body, then reply
    let terima = format!(
        "IFS= read -r l; id=$(printf '%s' \"$l\" | sed -n 's/.*· \\(msg_[a-z0-9]*\\)\\].*/\\1/p'); IFS= read -r b; \
         '{g}' reply \"$id\" \"hasil untuk: $(printf '%s' \"$b\" | tr -cd 'a-z ')\"; sleep 30"
    );
    // sender: send, then wait for the reply
    let kirim = format!(
        "sleep 2; id=$('{g}' send terima-1 'tulis unit test' --queue | sed -n 's/.*\"id\":\"\\([^\"]*\\)\".*/\\1/p'); \
         echo ID=$id; '{g}' wait --reply \"$id\" --timeout 20; echo SELESAI-K1; sleep 30"
    );
    profiles(&d, &mut ui, &(sh_profile("terima", &terima) + &sh_profile("kirim", &kirim)), "SELESAI-K1");
    let b = ui.call("pane.create", json!({"profile": "terima", "workspace": "w"}))["result"]["pane"].as_u64().unwrap();
    let a = ui.call("pane.create", json!({"profile": "kirim", "workspace": "w"}))["result"]["pane"].as_u64().unwrap();
    let out = ui.attach_until(a, &["SELESAI-K1"]);
    let mut ui2 = Cli::ui(&d);
    let other = ui2.attach_until(b, &["tidak-ada"]);
    let h = ui.call("msg.list", json!({}))["result"]["history"].to_string();
    assert!(out.contains("hasil untuk: tulis unit test"), "PENGIRIM:\n{out}\nPENERIMA:\n{other}\nRIWAYAT: {h}");
    let h = ui.call("msg.list", json!({}))["result"]["history"].to_string();
    assert!(h.contains("\"sent\"") && h.contains("\"reply\""), "{h}");
}

/// FR-66: a sandboxed agent replies through the end of its turn (Codex notify hook), and uses `gembala` through the spool.
#[test]
fn fr66_balasan_akhir_giliran_dan_spool() {
    use std::os::unix::fs::PermissionsExt;
    let d = start("bus66");
    let mut ui = Cli::ui(&d);
    let g = env!("CARGO_BIN_EXE_gembala");
    let tok = d.dir.join("tok");
    // the recipient doesn't call `reply`: only the Codex notify hook with the turn's last message
    let terima = format!(
        "'{g}' token > '{}'; IFS= read -r l; IFS= read -r b; \
         '{g}' hook codex notify '{{\"type\":\"agent-turn-complete\",\"last-assistant-message\":\"JAWABAN-OTOMATIS\"}}'; sleep 30",
        tok.display()
    );
    let kirim = format!(
        "sleep 2; id=$('{g}' send terima-1 'cek ini' --queue | sed -n 's/.*\"id\":\"\\([^\"]*\\)\".*/\\1/p'); \
         '{g}' wait --reply \"$id\" --timeout 20; echo SELESAI-66; sleep 30"
    );
    profiles(&d, &mut ui, &(sh_profile("terima", &terima) + &sh_profile("kirim", &kirim)), "SELESAI-66");
    ui.call("pane.create", json!({"profile": "terima", "workspace": "w"}));
    let a = ui.call("pane.create", json!({"profile": "kirim", "workspace": "w"}))["result"]["pane"].as_u64().unwrap();
    let out = ui.attach_until(a, &["SELESAI-66"]);
    assert!(out.contains("JAWABAN-OTOMATIS"), "{out}");
    // spool: the recipient pane's token, the request as a file like from inside the sandbox
    let token = std::fs::read_to_string(&tok).unwrap().trim().to_string();
    assert_eq!(token.len(), 32, "{token}");
    let spool = d.dir.join("spool");
    let ask = |name: &str, body: Value| {
        std::fs::write(spool.join(format!("{name}.tmp")), body.to_string()).unwrap();
        std::fs::rename(spool.join(format!("{name}.tmp")), spool.join(format!("{name}.req"))).unwrap();
        let t0 = Instant::now();
        loop {
            if let Ok(t) = std::fs::read_to_string(spool.join(format!("{name}.res"))) {
                return t;
            }
            assert!(t0.elapsed() < Duration::from_secs(5), "tanpa jawaban spool");
            std::thread::sleep(Duration::from_millis(50));
        }
    };
    let ok = ask("a1", json!({"token": token, "method": "status", "params": {}}));
    assert!(ok.contains("terima-1") && ok.contains("kirim-1"), "{ok}");
    let ditolak = ask("a2", json!({"token": "x".repeat(32), "method": "status", "params": {}}));
    assert!(ditolak.contains("unauthorized"), "{ditolak}");
    assert_eq!(std::fs::metadata(spool.join("a1.res")).unwrap().permissions().mode() & 0o777, 0o600);
}

/// FR-68: session id from the agent hook (Codex `thread-id`, OpenCode `sessionID`) is stored for exact resume; dangerous ids are rejected.
#[test]
fn fr68_id_sesi_dari_hook() {
    let d = start("sesi68");
    let mut ui = Cli::ui(&d);
    let g = env!("CARGO_BIN_EXE_gembala");
    let script = format!(
        "'{g}' hook codex notify '{{\"type\":\"agent-turn-complete\",\"thread-id\":\"01a19b1c-2cfb-7290-b33f-af39083bb39b\"}}'; \
         '{g}' hook opencode idle 'x;rm'; echo SELESAI-68; sleep 30"
    );
    profiles(&d, &mut ui, &sh_profile("sesi", &script), "SELESAI-68");
    let p = ui.call("pane.create", json!({"profile": "sesi", "workspace": "w"}))["result"]["pane"].as_u64().unwrap();
    ui.attach_until(p, &["SELESAI-68"]);
    assert_eq!(ui.call("pane.info", json!({"pane": p}))["result"]["session"], "01a19b1c-2cfb-7290-b33f-af39083bb39b");
}

/// FR-61: account created in a temp HOME: folder 0700, links from Utama, existing files aren't overwritten,
/// folders outside home rejected, email read, removing the account doesn't touch the folder.
#[test]
fn fr61_akun_agent() {
    use std::os::unix::fs::PermissionsExt;
    let home = std::env::temp_dir().join(format!("gbl-{}-home61", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(home.join(".claude/skills")).unwrap();
    std::fs::write(home.join(".claude/CLAUDE.md"), "aturan").unwrap();
    std::fs::create_dir_all(home.join(".claude-klien")).unwrap();
    std::fs::write(home.join(".claude-klien/CLAUDE.md"), "punya klien").unwrap();
    let d = start_env("akun61", 3600, &[("HOME", &home)]);
    let mut ui = Cli::ui(&d);
    let r = ui.call("account.create", json!({"name": "Kantor"}))["result"].clone();
    assert_eq!(r["id"], "kantor", "{r}");
    let k = home.join(".claude-kantor");
    assert_eq!(std::fs::metadata(&k).unwrap().permissions().mode() & 0o777, 0o700);
    assert_eq!(std::fs::read_link(k.join("CLAUDE.md")).unwrap(), home.join(".claude/CLAUDE.md"));
    assert!(std::fs::read_link(k.join("skills")).is_ok() && !k.join("settings.json").exists());
    let r = ui.call("account.create", json!({"name": "Klien", "share": ["CLAUDE.md"]}))["result"].clone();
    assert_eq!(r["skipped"], json!(["CLAUDE.md"]), "{r}");
    assert_eq!(std::fs::read_to_string(home.join(".claude-klien/CLAUDE.md")).unwrap(), "punya klien");
    let r = ui.call("account.create", json!({"name": "Luar", "dir": "/tmp/akun-luar"}))["result"].clone();
    assert!(r["error"].as_str().is_some_and(|e| e.contains("home")), "{r}");
    assert!(ui.call("account.create", json!({"name": "Kantor"}))["result"]["error"].as_str().is_some());
    std::fs::write(k.join(".claude.json"), r#"{"oauthAccount":{"emailAddress":"kerja@kantor.id"}}"#).unwrap();
    let list = ui.call("account.list", json!({}))["result"]["accounts"].to_string();
    assert!(list.contains("kerja@kantor.id") && list.contains("\"Utama\"") && list.contains("\"Klien\""), "{list}");
    ui.call("account.remove", json!({"id": "kantor"}));
    let list = ui.call("account.list", json!({}))["result"]["accounts"].to_string();
    // FR-61 (7): the account folder goes too, links dropped without touching Utama's content
    assert!(!list.contains("\"Kantor\"") && !k.exists() && home.join(".claude/CLAUDE.md").exists(), "{list}");
    let _ = std::fs::remove_dir_all(&home);
}

/// FR-61 (8): item becomes Own = copy of Utama's content (projects empty); back to Follow Utama = account content kept as
/// `.gembala-bak`, then linked; model/effort saved and foreign values rejected.
#[test]
fn fr61_setelan_per_akun() {
    let home = std::env::temp_dir().join(format!("gbl-{}-home61b", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(home.join(".claude/projects/p")).unwrap();
    std::fs::write(home.join(".claude/CLAUDE.md"), "aturan").unwrap();
    let d = start_env("akun61b", 3600, &[("HOME", &home)]);
    let mut ui = Cli::ui(&d);
    ui.call("account.create", json!({"name": "Kantor", "share": ["CLAUDE.md", "projects"]}));
    let k = home.join(".claude-kantor");
    let up = |ui: &mut Cli, p: serde_json::Value| ui.call("account.update", p)["result"].clone();
    assert_eq!(up(&mut ui, json!({"id": "kantor", "item": "CLAUDE.md", "follow": false})), json!({}));
    assert!(std::fs::read_link(k.join("CLAUDE.md")).is_err());
    assert_eq!(std::fs::read_to_string(k.join("CLAUDE.md")).unwrap(), "aturan");
    std::fs::write(k.join("CLAUDE.md"), "aturan kantor").unwrap();
    up(&mut ui, json!({"id": "kantor", "item": "projects", "follow": false}));
    assert!(k.join("projects").is_dir() && !k.join("projects/p").exists());
    up(&mut ui, json!({"id": "kantor", "item": "CLAUDE.md", "follow": true}));
    assert_eq!(std::fs::read_link(k.join("CLAUDE.md")).unwrap(), home.join(".claude/CLAUDE.md"));
    assert_eq!(std::fs::read_to_string(k.join("CLAUDE.md.gembala-bak")).unwrap(), "aturan kantor");
    up(&mut ui, json!({"id": "kantor", "model": "opus", "effort": "high", "trust": false}));
    let acc = ui.call("account.list", json!({}))["result"]["accounts"][1]["account"].clone();
    assert_eq!((acc["model"].as_str(), acc["effort"].as_str(), acc["trust"].as_bool()), (Some("opus"), Some("high"), Some(false)), "{acc}");
    assert_eq!(acc["share"], json!(["CLAUDE.md"]), "{acc}");
    assert!(up(&mut ui, json!({"id": "kantor", "effort": "super"}))["error"].as_str().is_some());
    let _ = std::fs::remove_dir_all(&home);
}

/// Criterion 10 + SEC-21: read only within the same workspace; shell panes only when the user allows it.
#[test]
fn kriteria_10_baca_dibatasi() {
    let d = start("bus10");
    let mut ui = Cli::ui(&d);
    let g = env!("CARGO_BIN_EXE_gembala");
    let script = format!("sleep 1; '{g}' read lain-1; '{g}' read shell-1; echo SELESAI-K10; sleep 30");
    profiles(&d, &mut ui, &(sh_profile("lain", "sleep 60") + &sh_profile("baca", &script)), "SELESAI-K10");
    ui.call("pane.create", json!({"workspace": "w1"})); // shell-1, not allowed to be read
    ui.call("pane.create", json!({"profile": "lain", "workspace": "w2"})); // workspace lain
    let p = ui.call("pane.create", json!({"profile": "baca", "workspace": "w1"}))["result"]["pane"].as_u64().unwrap();
    let out = ui.attach_until(p, &["SELESAI-K10"]);
    assert_eq!(out.matches("acl_denied").count(), 2, "{out}");
}

/// Criterion 4 + FR-12: no messages to `blocked`; policy `confirm` = a card only the UI channel can decide.
#[test]
fn blocked_ditolak_dan_konfirmasi_lewat_ui() {
    let d = start("bus4");
    let mut ui = Cli::ui(&d);
    let g = env!("CARGO_BIN_EXE_gembala");
    // body with a private key pattern: used to be redacted on the card but still sent whole (audit run-1)
    let script = format!("sleep 2; '{g}' send tanya-1 halo; '{g}' send sopan-1 halo -----BEGIN PRIVATE KEY----- TERSEMBUNYI; echo SELESAI-K4; sleep 30");
    let toml = sh_profile("tanya", "printf 'Hapus semua? (y/n) '; sleep 60")
        + &sh_profile("sopan", "sleep 60")
        + "policy = \"confirm\"\n"
        + &sh_profile("kirim", &script);
    profiles(&d, &mut ui, &toml, "SELESAI-K4");
    ui.call("pane.create", json!({"profile": "tanya", "workspace": "w"}));
    ui.call("pane.create", json!({"profile": "sopan", "workspace": "w"}));
    let p = ui.call("pane.create", json!({"profile": "kirim", "workspace": "w"}))["result"]["pane"].as_u64().unwrap();
    let out = ui.attach_until(p, &["SELESAI-K4"]);
    assert!(out.contains("target_blocked") && out.contains("needs_confirmation"), "{out}");
    let pending = ui.call("msg.list", json!({}))["result"]["pending"].clone();
    assert_eq!(pending.as_array().unwrap().len(), 1, "{pending}");
    // card = exactly what will be sent (SEC-12 exception for FR-12)
    assert!(pending[0]["body"].as_str().unwrap().contains("TERSEMBUNYI"), "{pending}");
    let id = pending[0]["id"].as_str().unwrap().to_string();
    // the pane channel can't approve (criterion 9)
    let mut outsider = Cli::connect(&d, "pane.sock");
    assert_eq!(outsider.call("hello", json!({"proto": 1}))["error"]["code"], "not_a_pane");
    let r = ui.call("msg.approve", json!({"id": id}))["result"].clone();
    assert!(r["result"] == "accepted" || r["result"] == "queued", "{r}");
}

/// FR-77 (H5): Atur pakai AI. The brief reaches the agent pane as the user's own message (no reply id) with the draft
/// path; the draft is checked like a recipe; "Setujui dan simpan" writes and approves only the reviewed bytes.
#[test]
fn h5_draf_resep_pakai_ai() {
    let d = start("h5");
    let mut ui = Cli::ui(&d);
    profiles(&d, &mut ui, &sh_profile("cat", "stty raw -echo; cat -v"), "cat -v");
    let t = ui.call("pane.create", json!({"profile": "cat", "workspace": "w"}))["result"]["pane"].as_u64().unwrap();
    let r = ui.call("recipe.draft_new", json!({"pane": t, "id": "uji-h5", "ask": "tambah tahap test"}))["result"].clone();
    let (draft, path) = (r["draft"].as_str().unwrap().to_string(), r["path"].as_str().unwrap().to_string());
    let shown = ui.attach_until(t, &["tambah tahap test", "draft written"]);
    assert!(shown.contains("[Gembala · from user]") && shown.contains(&draft), "{shown}");
    // nothing written yet: the UI keeps waiting
    assert_eq!(ui.call("recipe.draft_get", json!({"draft": draft, "id": "uji-h5"}))["result"]["exists"], false);
    // the agent writes the draft
    std::fs::write(&path, "[[step]]\nid = \"a\"\nagents = [\"claude\"]\n").unwrap();
    let got = ui.call("recipe.draft_get", json!({"draft": draft, "id": "uji-h5"}))["result"].clone();
    assert_eq!((got["exists"].clone(), got["errors"].clone(), got["current"].clone()), (json!(true), json!([]), json!("")), "{got}");
    // a stale hash (the file changed after the review) is refused, the reviewed one is saved and approved
    assert_eq!(ui.call("recipe.draft_apply", json!({"draft": draft, "id": "uji-h5", "hash": "x"}))["result"]["error"], "recipe_changed");
    let ok = ui.call("recipe.draft_apply", json!({"draft": draft, "id": "uji-h5", "hash": got["hash"]}));
    assert_eq!(ok["result"], json!({}), "{ok}");
    assert_eq!(ui.call("recipe.get", json!({"id": "uji-h5"}))["result"]["status"], "approved");
    assert!(!std::path::Path::new(&path).exists());
    // a draft id that walks out of the folder is refused
    assert_eq!(ui.call("recipe.draft_get", json!({"draft": "../../etc/passwd", "id": "uji-h5"}))["error"]["code"], "bad_params");
}

/// FR-77 (H5) targets 2 and 3: config.toml through a draft (guarded parts refused), AGENTS.md brief only (Gembala writes nothing).
#[test]
fn h5_config_dan_instruksi_agent() {
    let d = start("h5c");
    let mut ui = Cli::ui(&d);
    profiles(&d, &mut ui, &sh_profile("cat", "stty raw -echo; cat -v"), "cat -v");
    let t = ui.call("pane.create", json!({"profile": "cat", "workspace": "w"}))["result"]["pane"].as_u64().unwrap();
    let r = ui.call("config.draft_new", json!({"pane": t, "ask": "matiin suara notif", "section": "umum"}))["result"].clone();
    let (draft, path) = (r["draft"].as_str().unwrap().to_string(), r["path"].as_str().unwrap().to_string());
    let shown = ui.attach_until(t, &["matiin suara notif", "draft written"]);
    assert!(shown.contains("[Gembala · from user]") && shown.contains(&draft), "{shown}");
    let cfg_now = std::fs::read_to_string(d.dir.join("cfg/config.toml")).unwrap();
    // a draft touching [telegram] is refused, even when the rest is fine
    std::fs::write(&path, format!("{cfg_now}\n[telegram]\nsnippet = false\n")).unwrap();
    let got = ui.call("config.draft_get", json!({"draft": draft}))["result"].clone();
    assert!(got["errors"].to_string().contains("ai_guarded"), "{got}");
    assert_eq!(ui.call("config.draft_apply", json!({"draft": draft, "hash": got["hash"]}))["result"]["error"], "ai_draft_refused");
    // an ordinary change goes through and the daemon picks it up
    std::fs::write(&path, format!("{cfg_now}\n[notify]\nsound = false\n")).unwrap();
    let got = ui.call("config.draft_get", json!({"draft": draft}))["result"].clone();
    assert_eq!(got["errors"], json!([]), "{got}");
    assert_eq!(ui.call("config.draft_apply", json!({"draft": draft, "hash": got["hash"]}))["result"], json!({}));
    assert!(std::fs::read_to_string(d.dir.join("cfg/config.toml")).unwrap().contains("sound = false"));
    // AGENTS.md: only the brief, and only for the two known file names
    assert_eq!(ui.call("agent_file.brief", json!({"pane": t, "file": "AGENTS.md", "ask": "pakai bahasa Indonesia"}))["result"], json!({}));
    assert_eq!(ui.call("agent_file.brief", json!({"pane": t, "file": "../x.md", "ask": "x"}))["error"]["code"], "bad_params");
    let mut ui2 = Cli::ui(&d);
    ui2.attach_until(t, &["`AGENTS.md`", "pakai bahasa Indonesia"]);
}

/// FR-75: `send` to an agent not open yet = a card on the UI channel; approve = the task reaches the pane the UI opened,
/// reject = the requester's `wait --reply` gets the refusal. Unknown names stay `no_such_pane`.
#[test]
fn fr75_buka_dan_kirim() {
    let d = start("fr75");
    let mut ui = Cli::ui(&d);
    let g = env!("CARGO_BIN_EXE_gembala");
    let minta = format!(
        "g='{g}'; sleep 1; $g send nggakada-9 x; $g send cat TUGAS-A; r=$($g send cat TUGAS-B); \
         id=$(printf '%s' \"$r\" | sed -n 's/.*\"id\":\"\\([^\"]*\\)\".*/\\1/p'); echo DIMINTA; $g wait --reply \"$id\"; echo SELESAI-75; sleep 30"
    );
    profiles(&d, &mut ui, &(sh_profile("cat", "stty raw -echo; cat -v") + &sh_profile("minta", &minta)), "SELESAI-75");
    let k = ui.call("pane.create", json!({"profile": "minta", "workspace": "w"}))["result"]["pane"].as_u64().unwrap();
    let out = ui.attach_until(k, &["DIMINTA"]);
    assert!(out.contains("no_such_pane") && out.contains("needs_open"), "{out}");
    let opens = ui.call("msg.list", json!({}))["result"]["opens"].clone();
    assert_eq!(opens.as_array().unwrap().len(), 2, "{opens}");
    assert_eq!((opens[0]["profile"].as_str(), opens[0]["from_pane"].as_u64(), opens[0]["body"].as_str()), (Some("cat"), Some(k), Some("TUGAS-A")), "{opens}");
    let (a, b) = (opens[0]["id"].as_str().unwrap().to_string(), opens[1]["id"].as_str().unwrap().to_string());
    let t = ui.call("pane.create", json!({"profile": "cat", "workspace": "w"}))["result"]["pane"].as_u64().unwrap();
    assert_eq!(ui.call("open.approve", json!({"id": a, "pane": t}))["result"]["result"], "queued");
    assert_eq!(ui.call("open.reject", json!({"id": b}))["result"]["reason"], "rejected_by_user");
    assert_eq!(ui.call("msg.list", json!({}))["result"]["opens"], json!([]));
    let mut ui2 = Cli::ui(&d);
    let done = ui2.attach_until(k, &["SELESAI-75"]);
    assert!(done.contains("cat was not opened: rejected_by_user"), "{done}");
    let mut ui3 = Cli::ui(&d);
    ui3.attach_until(t, &["TUGAS-A^M"]);
}

/// FR-86: `gembala close` closes only a helper opened through the caller's own card; any other pane is `not_allowed`.
#[test]
fn fr86_tutup_pembantu() {
    let d = start("fr86");
    let mut ui = Cli::ui(&d);
    let g = env!("CARGO_BIN_EXE_gembala");
    let go = d.dir.join("go");
    let minta = format!(
        "g='{g}'; sleep 1; $g send cat TUGAS; echo DIMINTA; while [ ! -e '{}' ]; do sleep 0.1; done; \
         $g close nggakada-9; $g close cat-2; $g close cat-1; echo SELESAI-86; sleep 30",
        go.display()
    );
    profiles(&d, &mut ui, &(sh_profile("cat", "stty raw -echo; cat -v") + &sh_profile("minta", &minta)), "SELESAI-86");
    let k = ui.call("pane.create", json!({"profile": "minta", "workspace": "w"}))["result"]["pane"].as_u64().unwrap();
    ui.attach_until(k, &["DIMINTA"]);
    let id = ui.call("msg.list", json!({}))["result"]["opens"][0]["id"].as_str().unwrap().to_string();
    let helper = ui.call("pane.create", json!({"profile": "cat", "workspace": "w"}))["result"]["pane"].as_u64().unwrap();
    let other = ui.call("pane.create", json!({"profile": "cat", "workspace": "w"}))["result"]["pane"].as_u64().unwrap();
    assert_eq!(ui.call("open.approve", json!({"id": id, "pane": helper}))["result"]["result"], "queued");
    std::fs::write(&go, "").unwrap();
    let mut ui2 = Cli::ui(&d);
    let out = ui2.attach_until(k, &["SELESAI-86"]);
    assert!(out.contains("no_such_pane") && out.contains("not_allowed") && out.contains("\"closed\""), "{out}");
    let t0 = Instant::now();
    while ui.call("pane.list", json!({}))["result"].as_array().unwrap().contains(&json!(helper)) {
        assert!(t0.elapsed() < Duration::from_secs(5), "helper still open");
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(ui.call("pane.list", json!({}))["result"].as_array().unwrap().contains(&json!(other)));
}

/// Criterion 5: a message with ESC, CR, `\e[201~`, C1, and newlines = exactly one send (one Enter) at the target.
#[test]
fn kriteria_5_satu_kiriman() {
    let d = start("bus5");
    let mut ui = Cli::ui(&d);
    let g = env!("CARGO_BIN_EXE_gembala");
    let body = "a\\rb\\x1b[201~c\\xc2\\x9bd\\ne";
    let kirim = format!("sleep 4; '{g}' send cat-1 \"$(printf '{body}')\" --queue; echo SELESAI-K5; sleep 30");
    // target: raw-mode terminal, every control byte shows (cat -v): Enter = ^M
    profiles(&d, &mut ui, &(sh_profile("cat", "stty raw -echo; cat -v") + &sh_profile("kirim", &kirim)), "SELESAI-K5");
    let t = ui.call("pane.create", json!({"profile": "cat", "workspace": "w"}))["result"]["pane"].as_u64().unwrap();
    let k = ui.call("pane.create", json!({"profile": "kirim", "workspace": "w"}))["result"]["pane"].as_u64().unwrap();
    let out = ui.attach_until(k, &["SELESAI-K5"]);
    assert!(out.contains("accepted") || out.contains("queued"), "{out}");
    let mut ui2 = Cli::ui(&d);
    let got = ui2.attach_until(t, &["e^[[201~^M"]);
    assert_eq!(got.matches("^M").count(), 1, "tepat satu Enter: {got}");
    assert_eq!(got.matches("^[[201~").count(), 1, "penutup paste hanya milik daemon: {got}");
    assert!(!got.contains("M-^["), "C1 dibuang: {got}");
}

/// Criterion 8: two agents programmed to answer each other stop at hop 3.
#[test]
fn kriteria_8_berhenti_di_hop_3() {
    let d = start("bus8");
    let mut ui = Cli::ui(&d);
    let g = env!("CARGO_BIN_EXE_gembala");
    // each agent: read the incoming label, reply to the other with --reply-to, repeat
    let pingpong = |lawan: &str| {
        format!(
            "while IFS= read -r l; do id=$(printf '%s' \"$l\" | sed -n 's/.*· \\(msg_[a-z0-9]*\\)\\].*/\\1/p'); \
             [ -n \"$id\" ] || continue; IFS= read -r b; sleep 1; '{g}' send {lawan} balas --reply-to \"$id\" --queue; done"
        )
    };
    let toml = sh_profile("pa", &pingpong("pb-1")) + &sh_profile("pb", &pingpong("pa-1")) + &sh_profile("mulai", &format!("sleep 4; '{g}' send pa-1 mulai --queue; sleep 60"));
    profiles(&d, &mut ui, &toml, "mulai --queue");
    for prof in ["pa", "pb", "mulai"] {
        ui.call("pane.create", json!({"profile": prof, "workspace": "w"}));
    }
    let t0 = Instant::now();
    loop {
        let h = ui.call("msg.list", json!({}))["result"]["history"].to_string();
        if h.contains("hop_limit") {
            let sent = h.matches("\"kind\":\"sent\"").count();
            assert_eq!(sent, 4, "hop 0 sampai 3 terkirim, hop 4 ditolak: {h}");
            break;
        }
        assert!(t0.elapsed() < Duration::from_secs(60), "rantai tidak berhenti: {h}");
        std::thread::sleep(Duration::from_millis(300));
    }
}

/// Criterion 7: a fake API key printed on screen and included in a message isn't stored in any log or on disk.
#[test]
fn kriteria_7_key_tidak_ke_disk() {
    let d = start("bus7");
    let mut ui = Cli::ui(&d);
    let g = env!("CARGO_BIN_EXE_gembala");
    // the key is built when the script runs, so its text isn't in this test's own config.toml
    let key = "sk-ant-api03-PALSU$((1234567000+890))";
    let kirim = format!("echo {key}; sleep 4; '{g}' send diam-1 'pakai {key} ya' --queue; '{g}' read diam-1; echo SELESAI-K7; sleep 30");
    profiles(&d, &mut ui, &(sh_profile("diam", &format!("echo {key}; sleep 60")) + &sh_profile("kirim", &kirim)), "SELESAI-K7");
    ui.call("pane.create", json!({"profile": "diam", "workspace": "w"}));
    let k = ui.call("pane.create", json!({"profile": "kirim", "workspace": "w"}))["result"]["pane"].as_u64().unwrap();
    let out = ui.attach_until(k, &["SELESAI-K7"]);
    assert!(out.contains("[disamarkan]"), "read harus tersamar: {out}");
    ui.call("layout.save", json!({"layout": {"version": 1}}));
    std::thread::sleep(Duration::from_millis(500));
    fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            if e.path().is_dir() { walk(&e.path(), out) } else { out.push(e.path()) }
        }
    }
    let mut files = vec![];
    walk(&d.dir, &mut files);
    assert!(files.iter().any(|f| f.to_string_lossy().contains("audit-")), "log audit harus ada: {files:?}");
    for f in files {
        let bytes = std::fs::read(&f).unwrap_or_default();
        assert!(!String::from_utf8_lossy(&bytes).contains("PALSU1234567890"), "key bocor ke {}", f.display());
    }
}

/// FR-8 + SEC-21: status subscriptions only for panes in the same workspace.
#[test]
fn langganan_status_per_workspace() {
    let d = start("sub");
    let mut ui = Cli::ui(&d);
    let g = env!("CARGO_BIN_EXE_gembala");
    let toml = sh_profile("lihat", &format!("'{g}' watch"))
        + &sh_profile("sini", "sleep 5; printf 'Lanjut? (y/n) '; sleep 60")
        + &sh_profile("sana", "sleep 5; printf 'Lanjut? (y/n) '; sleep 60");
    profiles(&d, &mut ui, &toml, "watch");
    let w = ui.call("pane.create", json!({"profile": "lihat", "workspace": "w1"}))["result"]["pane"].as_u64().unwrap();
    ui.call("pane.create", json!({"profile": "sini", "workspace": "w1"}));
    ui.call("pane.create", json!({"profile": "sana", "workspace": "w2"}));
    let out = ui.attach_until(w, &["\"name\":\"sini-1\",\"reason\":\"\",\"shell\":false,\"status\":\"blocked\""]);
    assert!(out.contains("\"name\":\"sini-1\",\"reason\":\"\",\"shell\":false,\"status\":\"blocked\""), "{out}");
    std::thread::sleep(Duration::from_secs(1));
    let mut ui2 = Cli::ui(&d);
    let all = ui2.attach_until(w, &["tidak-ada"]);
    assert!(!all.contains("sana-1"), "workspace lain tidak boleh terlihat: {all}");
}

/// SEC-3 fallback: a process detached from the pane session is only recognized through a profile token `token = true`;
/// the token is redacted in `read` (SEC-12).
#[test]
fn token_cadangan() {
    let d = start("token");
    let mut ui = Cli::ui(&d);
    let g = env!("CARGO_BIN_EXE_gembala");
    // setsid + a subshell that exits right away = the process leaves the pane session and tree (parent becomes launchd), like a tmux server
    let lepas = format!("sleep 1; (perl -e 'use POSIX; POSIX::setsid(); sleep 1; exec @ARGV' '{g}' status &); sleep 3; echo; echo TOKEN=$GEMBALA_TOKEN; echo SELESAI-T; sleep 30");
    let toml = sh_profile("bertoken", &lepas) + "token = true\n" + &sh_profile("tanpa", &lepas);
    profiles(&d, &mut ui, &toml, "SELESAI-T");
    let a = ui.call("pane.create", json!({"profile": "bertoken", "workspace": "w"}))["result"]["pane"].as_u64().unwrap();
    let b = ui.call("pane.create", json!({"profile": "tanpa", "workspace": "w"}))["result"]["pane"].as_u64().unwrap();
    let out_a = ui.attach_until(a, &["SELESAI-T"]);
    let mut ui2 = Cli::ui(&d);
    let out_b = ui2.attach_until(b, &["SELESAI-T"]);
    assert!(out_a.contains("\"me\":\"bertoken-1\""), "{out_a}");
    assert!(out_b.contains("not_a_pane"), "{out_b}");
    // the token isn't read by other agents
    let token: String = out_a.split("TOKEN=").nth(1).unwrap_or_default().chars().take(32).collect();
    assert_eq!(token.len(), 32, "{out_a}");
    let read_script = format!("sleep 1; '{g}' read bertoken-1 -n 50; echo SELESAI-R; sleep 30");
    profiles(&d, &mut ui, &(sh_profile("bertoken", &lepas) + "token = true\n" + &sh_profile("baca", &read_script)), "SELESAI-R");
    let r = ui.call("pane.create", json!({"profile": "baca", "workspace": "w"}))["result"]["pane"].as_u64().unwrap();
    let mut ui3 = Cli::ui(&d);
    let read = ui3.attach_until(r, &["SELESAI-R"]);
    assert!(read.contains("TOKEN=[disamarkan]") && !read.contains(&token), "{read}");
}

/// SEC-3: a process that creates a new session but still descends from the pane shell (e.g. Claude Code's Bash tool) is recognized without a token.
#[test]
fn sesi_baru_masih_anak_pane() {
    let d = start("anak");
    let mut ui = Cli::ui(&d);
    let g = env!("CARGO_BIN_EXE_gembala");
    let script = format!("sleep 1; perl -e 'use POSIX; POSIX::setsid(); exec @ARGV' '{g}' status; echo; echo SELESAI-A; sleep 30");
    profiles(&d, &mut ui, &sh_profile("anak", &script), "SELESAI-A");
    let a = ui.call("pane.create", json!({"profile": "anak", "workspace": "w"}))["result"]["pane"].as_u64().unwrap();
    let out = ui.attach_until(a, &["SELESAI-A"]);
    assert!(out.contains("\"me\":\"anak-1\""), "{out}");
}

/// SEC-5: skip-permission flags not visible in the profile (wrapped in a script) are still read from the process arguments.
#[test]
fn lewati_izin_dari_argumen_proses() {
    let d = start("yolo");
    let mut ui = Cli::ui(&d);
    let g = env!("CARGO_BIN_EXE_gembala");
    let toml = sh_profile("nekat", "exec perl -e 'sleep 60' -- --yolo")
        + &sh_profile("kirim", &format!("sleep 4; '{g}' send nekat-1 halo; echo SELESAI-Y; sleep 30"));
    profiles(&d, &mut ui, &toml, "SELESAI-Y");
    ui.call("pane.create", json!({"profile": "nekat", "workspace": "w"}));
    let k = ui.call("pane.create", json!({"profile": "kirim", "workspace": "w"}))["result"]["pane"].as_u64().unwrap();
    let out = ui.attach_until(k, &["SELESAI-Y"]);
    assert!(out.contains("needs_confirmation"), "{out}");
}

/// FR-74, SEC-27: recipes are approved by bytes; an edit outside the app, or a stale hash, cancels the approval.
#[test]
fn resep_disetujui_per_isi() {
    let d = start("resep");
    let mut ui = Cli::ui(&d);
    let rows = ui.call("recipe.list", json!({}))["result"].clone();
    assert_eq!(rows.as_array().map(Vec::len), Some(2), "{rows}");
    assert!(rows.as_array().unwrap().iter().all(|r| r["status"] == "needs_approval"), "{rows}");
    let got = ui.call("recipe.get", json!({"id": "fitur-baru"}))["result"].clone();
    let h = got["hash"].as_str().unwrap().to_string();
    assert_eq!(ui.call("recipe.approve", json!({"id": "fitur-baru", "hash": "00"}))["result"]["error"], "recipe_changed");
    assert_eq!(ui.call("recipe.approve", json!({"id": "fitur-baru", "hash": h}))["result"], json!({}));
    assert_eq!(ui.call("recipe.get", json!({"id": "fitur-baru"}))["result"]["status"], "approved");
    let file = d.dir.join("cfg/recipes/fitur-baru.toml");
    std::fs::write(&file, format!("{}# edited outside\n", got["text"].as_str().unwrap())).unwrap();
    assert_eq!(ui.call("recipe.get", json!({"id": "fitur-baru"}))["result"]["status"], "changed");
    assert_eq!(ui.call("recipe.get", json!({"id": "../x"}))["result"]["error"], "recipe_id");
    let v = ui.call("recipe.validate", json!({"text": "[[step]]\nid = \"a\"\n"}))["result"].clone();
    assert_eq!(v["errors"][0]["code"], "no_agent", "{v}");
    assert_eq!(ui.call("recipe.delete", json!({"id": "fitur-baru"}))["result"], json!({}));
    assert!(!file.exists());
}

/// 3b B1: the builder gets cards and templates, renders a recipe for review without touching the file, and saves only
/// the render whose hash the review showed.
#[test]
fn b1_builder_render_dan_simpan() {
    let d = start("b1");
    let mut ui = Cli::ui(&d);
    let lib = ui.call("recipe.library", json!({}))["result"].clone();
    assert_eq!(lib["steps"].as_array().map(Vec::len), Some(9), "{lib}");
    assert_eq!(lib["templates"][2]["id"], "review-pr", "{lib}");
    assert_eq!(lib["agents"].as_array().unwrap().iter().find(|a| a["agent"] == "omp").unwrap()["flags"]["cost"], true);
    ui.call("recipe.list", json!({}));
    let full = ui.call("recipe.get", json!({"id": "perbaiki-bug"}))["result"]["full"].clone();
    let mut r = full.clone();
    r["steps"].as_array_mut().unwrap().retain(|s| s["id"] != "test");
    let v = ui.call("recipe.render", json!({"id": "perbaiki-bug", "recipe": r}))["result"].clone();
    assert!(v["errors"] == json!([]) && !v["text"].as_str().unwrap().contains("id = \"test\""), "{v}");
    let file = d.dir.join("cfg/recipes/perbaiki-bug.toml");
    assert!(std::fs::read_to_string(&file).unwrap().contains("id = \"test\""));
    assert_eq!(ui.call("recipe.save", json!({"id": "perbaiki-bug", "hash": "00"}))["result"]["error"], "recipe_changed");
    assert_eq!(ui.call("recipe.save", json!({"id": "perbaiki-bug", "hash": v["hash"]}))["result"], json!({}));
    let got = ui.call("recipe.get", json!({"id": "perbaiki-bug"}))["result"].clone();
    assert_eq!((got["status"].clone(), got["text"].clone()), (json!("approved"), v["text"].clone()));
}

fn script(path: &std::path::Path, body: &str) {
    std::fs::write(path, body).unwrap();
    std::fs::set_permissions(path, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
}

fn wait_state(ui: &mut Cli, run: &str, want: &[&str]) -> Value {
    let t0 = Instant::now();
    loop {
        let s = ui.call("harness.state", json!({"run": run}))["result"].clone();
        if want.contains(&s["state"].as_str().unwrap_or_default()) {
            return s;
        }
        assert!(t0.elapsed() < Duration::from_secs(60), "state {want:?} not reached: {s}");
        std::thread::sleep(Duration::from_millis(300));
    }
}

/// FR-16/17, SEC-30: a recipe runs Plan -> Test -> Ship on fake agents. The first agent dies and the stage moves to
/// the next one, the driven pane refuses other senders, the shell stage judges by exit code, and Ship waits for the user.
#[test]
fn harness_runs_a_recipe_with_fallback_and_a_gate() {
    let d = start("harness1");
    let mut ui = Cli::ui(&d);
    let g = env!("CARGO_BIN_EXE_gembala");
    let cfg = d.dir.join("cfg");
    std::fs::create_dir_all(cfg.join("recipes")).unwrap();
    std::fs::set_permissions(&cfg, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
    // claude: starts, then dies before doing anything. omp: reads the stage message, writes the result, replies
    script(&cfg.join("die-claude.sh"), &format!("#!/bin/sh\n'{g}' hook claude start </dev/null\nsleep 1\nexit 1\n"));
    script(
        &cfg.join("worker-omp.sh"),
        &format!(
            "#!/bin/sh\n'{g}' hook omp end </dev/null\nIFS= read -r l; id=$(printf '%s' \"$l\" | sed -n 's/.*· \\(msg_[a-z0-9]*\\)\\].*/\\1/p')\nout=''\n\
             while IFS= read -r b; do case \"$b\" in *'Write your result to '*) out=$(printf '%s' \"$b\" | sed -n 's/.*to \"\\(.*\\)\"\\..*/\\1/p');; *'When you are done'*) break;; esac; done\n\
             '{g}' hook omp start </dev/null\nsleep 1\necho plan > \"$out\"\n'{g}' reply \"$id\" done\n'{g}' hook omp end </dev/null\nsleep 60\n"
        ),
    );
    let toml = format!(
        "[[profile]]\nid = \"claude\"\ncommand = {}\n[[profile]]\nid = \"omp\"\ncommand = {}\n",
        json!(cfg.join("die-claude.sh")),
        json!(cfg.join("worker-omp.sh"))
    );
    profiles(&d, &mut ui, &toml, "worker-omp");
    std::fs::write(
        cfg.join("recipes/t.toml"),
        "[[step]]\nid = \"plan\"\nagents = [\"claude\", \"omp\"]\noutput = \"plan.md\"\ntimeout = \"60s\"\n[[step]]\nid = \"test\"\nkind = \"check\"\nshell = true\n[[step]]\nid = \"ship\"\nhuman = true\n",
    )
    .unwrap();
    let repo = d.dir.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    assert!(std::process::Command::new("git").args(["init", "-q"]).current_dir(&repo).status().unwrap().success());
    let params = json!({"recipe": "t", "request": "make a plan", "cwd": repo, "workspace": "w", "test": "true"});

    // unapproved: the dialog is told why, and start refuses
    let pf = ui.call("harness.preflight", params.clone())["result"].clone();
    assert_eq!(pf["ok"], false, "{pf}");
    assert_eq!(ui.call("harness.start", params.clone())["result"]["error"], "preflight");
    let h = ui.call("recipe.get", json!({"id": "t"}))["result"]["hash"].as_str().unwrap().to_string();
    ui.call("recipe.approve", json!({"id": "t", "hash": h}));
    let pf = ui.call("harness.preflight", params.clone())["result"].clone();
    assert_eq!(pf["ok"], true, "{pf}");

    let s = ui.call("harness.start", params.clone())["result"].clone();
    let run = s["run"].as_str().unwrap_or_else(|| panic!("{s}")).to_string();
    // one run per workspace
    assert_eq!(ui.call("harness.start", params.clone())["result"]["args"]["check"], "no_other_run");

    // the shell stage and the gate are reached after claude died and omp did the stage
    let s = wait_state(&mut ui, &run, &["gate", "halted"]);
    assert_eq!(s["state"], "gate", "{s}");
    assert_eq!(s["gate"], 2, "{s}");
    let trace: Vec<&str> = s["trace"].as_array().unwrap().iter().filter_map(|e| e["code"].as_str()).collect();
    assert!(trace.contains(&"exit") && trace.contains(&"shell_pass"), "{trace:?}");
    assert!(s["steps"][0]["pane_name"].as_str().unwrap().starts_with("omp"), "{s}");
    assert!(cfg.join("harness").join(&run).join("artefak/plan.md").exists());
    assert!(cfg.join("harness").join(&run).join("artefak/permintaan.md").exists());

    // FR-17: the pane that did the work is still held; the user's own message is refused
    let pane = s["steps"][0]["pane"].as_u64().unwrap();
    assert_eq!(ui.call("msg.send", json!({"to": pane, "body": "hi"}))["error"]["code"], "driver_locked");
    // not at a gate = no gate; Ship only moves when the user says so
    assert_eq!(ui.call("harness.gate", json!({"run": run}))["result"]["state"], "done");
    let s = wait_state(&mut ui, &run, &["done"]);
    assert_eq!(s["state"], "done");
    assert_ne!(ui.call("msg.send", json!({"to": pane, "body": "hi", "force": true}))["error"]["code"], "driver_locked");
    // FR-86: a run that went all the way closes its stage panes, the dead claude pane the fallback replaced too
    let opened: Vec<u64> = s["panes"].as_array().unwrap().iter().filter_map(Value::as_u64).collect();
    assert!(opened.len() == 2 && opened.contains(&pane), "{s}");
    for p in opened {
        gone(&mut ui, p);
    }
    // a finished run can be read again from its saved row
    assert_eq!(ui.call("harness.state", json!({"run": run}))["result"]["state"], "done");
}

fn gone(ui: &mut Cli, pane: u64) {
    let t0 = Instant::now();
    while ui.call("pane.info", json!({"pane": pane}))["error"]["code"] != "no_such_pane" {
        assert!(t0.elapsed() < Duration::from_secs(10), "pane {pane} is still open");
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// FR-27, SEC-31, T-17: with the recipe's saver on, a failing Test writes test.ringkas.log with the error line and the
/// exit code; the stage verdict still comes from the exit code.
#[test]
fn harness_saver_writes_the_reduced_test_log() {
    let d = start("harness3");
    let mut ui = Cli::ui(&d);
    let g = env!("CARGO_BIN_EXE_gembala");
    let cfg = d.dir.join("cfg");
    std::fs::create_dir_all(cfg.join("recipes")).unwrap();
    std::fs::set_permissions(&cfg, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
    script(
        &cfg.join("worker-omp.sh"),
        &format!(
            "#!/bin/sh\n'{g}' hook omp end </dev/null\nIFS= read -r l; id=$(printf '%s' \"$l\" | sed -n 's/.*· \\(msg_[a-z0-9]*\\)\\].*/\\1/p')\nout=''\n\
             while IFS= read -r b; do case \"$b\" in *'Write your result to '*) out=$(printf '%s' \"$b\" | sed -n 's/.*to \"\\(.*\\)\"\\..*/\\1/p');; *'When you are done'*) break;; esac; done\n\
             '{g}' hook omp start </dev/null\nsleep 1\necho plan > \"$out\"\n'{g}' reply \"$id\" done\n'{g}' hook omp end </dev/null\nsleep 60\n"
        ),
    );
    profiles(&d, &mut ui, &format!("[[profile]]\nid = \"omp\"\ncommand = {}\n", json!(cfg.join("worker-omp.sh"))), "worker-omp");
    std::fs::write(
        cfg.join("recipes/t.toml"),
        "token_saver = true\n[[step]]\nid = \"plan\"\nagents = [\"omp\"]\noutput = \"plan.md\"\ntimeout = \"60s\"\n[[step]]\nid = \"test\"\nkind = \"check\"\nshell = true\n[[step]]\nid = \"ship\"\nhuman = true\n",
    )
    .unwrap();
    let repo = d.dir.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    assert!(std::process::Command::new("git").args(["init", "-q"]).current_dir(&repo).status().unwrap().success());
    let h = ui.call("recipe.get", json!({"id": "t"}))["result"]["hash"].as_str().unwrap().to_string();
    ui.call("recipe.approve", json!({"id": "t", "hash": h}));
    let test = "i=0; while [ $i -lt 300 ]; do echo baris-$i; i=$((i+1)); done; echo 'FAIL: boom'; exit 3";
    let s = ui.call("harness.start", json!({"recipe": "t", "request": "x", "cwd": repo, "workspace": "w", "test": test}))["result"].clone();
    let run = s["run"].as_str().unwrap_or_else(|| panic!("{s}")).to_string();
    let ringkas = cfg.join("harness").join(&run).join("artefak/test.ringkas.log");
    let t0 = Instant::now();
    while !ringkas.exists() {
        let s = ui.call("harness.state", json!({"run": run}))["result"].clone();
        assert!(matches!(s["state"].as_str(), Some("running" | "gate" | "taken_over")), "run berhenti sebelum Test: {s}");
        assert!(t0.elapsed() < Duration::from_secs(60), "test.ringkas.log belum ada: {s}");
        std::thread::sleep(Duration::from_millis(200));
    }
    let text = std::fs::read_to_string(&ringkas).unwrap();
    assert!(text.contains("FAIL: boom") && text.contains("exit code: 3"), "{text}");
    assert!(text.contains("lines omitted"), "{text}");
    // the full log is still there: the verdict and a later reader that wants everything are not fooled
    let full = std::fs::read_to_string(cfg.join("harness").join(&run).join("artefak/test.log")).unwrap();
    assert!(full.matches("baris-").count() >= 300, "log utuh hilang");
    // FR-26/H6: the Laporan input is written from the trace, without an agent
    let trail = std::fs::read_to_string(cfg.join("harness").join(&run).join("artefak/jejak.md")).unwrap();
    assert!(trail.starts_with("# Jejak jalan \"t\"") && trail.contains("- plan [plan] agent: omp"), "{trail}");
    assert!(trail.contains("## Kejadian") && trail.contains("| shell_fail |"), "{trail}");
    ui.call("harness.stop", json!({"run": run}));
}

/// BRD 8: Execute passes only with a reply and a non-empty diff. A dirty file that was already there at run start
/// doesn't count; the same stage passes once the agent changes the folder.
#[test]
fn harness_execute_needs_a_diff() {
    let d = start("harness4");
    let mut ui = Cli::ui(&d);
    let g = env!("CARGO_BIN_EXE_gembala");
    let cfg = d.dir.join("cfg");
    std::fs::create_dir_all(cfg.join("recipes")).unwrap();
    std::fs::set_permissions(&cfg, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
    let reply_script = |work: &str| {
        format!(
            "#!/bin/sh\n'{g}' hook omp end </dev/null\nIFS= read -r l; id=$(printf '%s' \"$l\" | sed -n 's/.*· \\(msg_[a-z0-9]*\\)\\].*/\\1/p')\n\
             while IFS= read -r b; do case \"$b\" in *'When you are done'*) break;; esac; done\n'{g}' hook omp start </dev/null\nsleep 1\n{work}'{g}' reply \"$id\" done\n'{g}' hook omp end </dev/null\nsleep 60\n"
        )
    };
    script(&cfg.join("noop.sh"), &reply_script(""));
    script(&cfg.join("work.sh"), &reply_script("echo kerja > hasil.txt\n"));
    let toml = format!("[[profile]]\nid = \"omp\"\ncommand = {}\n", json!(cfg.join("noop.sh")));
    profiles(&d, &mut ui, &toml, "noop");
    let recipe = "[[step]]\nid = \"execute\"\nagents = [\"omp\"]\ntimeout = \"60s\"\n[[step]]\nid = \"ship\"\nhuman = true\n";
    std::fs::write(cfg.join("recipes/t.toml"), recipe).unwrap();
    let repo = d.dir.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    assert!(std::process::Command::new("git").args(["init", "-q"]).current_dir(&repo).status().unwrap().success());
    std::fs::write(repo.join("sudah-kotor.txt"), "dari sebelum run\n").unwrap();
    let h = ui.call("recipe.get", json!({"id": "t"}))["result"]["hash"].as_str().unwrap().to_string();
    ui.call("recipe.approve", json!({"id": "t", "hash": h}));
    let params = json!({"recipe": "t", "request": "x", "cwd": repo, "workspace": "w"});
    let s = ui.call("harness.start", params.clone())["result"].clone();
    let run = s["run"].as_str().unwrap_or_else(|| panic!("{s}")).to_string();
    let s = wait_state(&mut ui, &run, &["gate", "halted"]);
    assert_eq!((s["state"].as_str(), s["halt"].as_str()), (Some("halted"), Some("step_failed")), "{s}");

    // same recipe, an agent that writes something: the stage passes and Ship waits for the user
    profiles(&d, &mut ui, &format!("[[profile]]\nid = \"omp\"\ncommand = {}\n", json!(cfg.join("work.sh"))), "work");
    let s = ui.call("harness.start", params)["result"].clone();
    let run = s["run"].as_str().unwrap_or_else(|| panic!("{s}")).to_string();
    let s = wait_state(&mut ui, &run, &["gate", "halted"]);
    assert_eq!((s["state"].as_str(), s["gate"].as_u64()), (Some("gate"), Some(1)), "{s}");
    ui.call("harness.stop", json!({"run": run}));
}

/// SEC-30: Stop ends the run from a gate or while a stage works, and a run left over from an earlier daemon is never resumed.
#[test]
fn harness_stop_and_stale_run() {
    let d = start("harness2");
    let mut ui = Cli::ui(&d);
    let g = env!("CARGO_BIN_EXE_gembala");
    let cfg = d.dir.join("cfg");
    std::fs::create_dir_all(cfg.join("recipes")).unwrap();
    std::fs::set_permissions(&cfg, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
    script(&cfg.join("idle-claude.sh"), &format!("#!/bin/sh\n'{g}' hook claude start </dev/null\nsleep 60\n"));
    profiles(&d, &mut ui, &format!("[[profile]]\nid = \"claude\"\ncommand = {}\n", json!(cfg.join("idle-claude.sh"))), "idle-claude");
    std::fs::write(cfg.join("recipes/t.toml"), "[[step]]\nid = \"a\"\nagents = [\"claude\"]\noutput = \"a.md\"\n").unwrap();
    let repo = d.dir.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    assert!(std::process::Command::new("git").args(["init", "-q"]).current_dir(&repo).status().unwrap().success());
    let h = ui.call("recipe.get", json!({"id": "t"}))["result"]["hash"].as_str().unwrap().to_string();
    ui.call("recipe.approve", json!({"id": "t", "hash": h}));
    let s = ui.call("harness.start", json!({"recipe": "t", "request": "x", "cwd": repo, "workspace": "w"}))["result"].clone();
    let run = s["run"].as_str().unwrap_or_else(|| panic!("{s}")).to_string();
    let pane = s["panes"][0].as_u64().unwrap_or_else(|| panic!("{s}"));
    assert_eq!(ui.call("harness.close_panes", json!({"run": run}))["error"]["code"], "run_active");
    let s = ui.call("harness.stop", json!({"run": run}))["result"].clone();
    assert_eq!((s["state"].as_str(), s["halt"].as_str()), (Some("halted"), Some("stopped")), "{s}");
    // after Stop the workspace is free for the next run, and the stopped row stays readable
    assert_eq!(ui.call("harness.state", json!({"run": run}))["result"]["halt"], "stopped");
    assert_eq!(ui.call("harness.gate", json!({"run": run}))["error"]["code"], "no_such_run");
    // FR-86: a stopped run leaves its panes for a look; the user's button closes them
    assert!(ui.call("pane.info", json!({"pane": pane}))["result"].is_object());
    assert_eq!(ui.call("harness.close_panes", json!({"run": run}))["result"]["closed"], 1);
    gone(&mut ui, pane);
    assert_eq!(ui.call("harness.close_panes", json!({"run": run}))["error"]["code"], "no_such_run");

    // a row left "running" by a daemon that died is reported as halted, not resumed
    let stale = cfg.join("harness/run-1-1");
    std::fs::create_dir_all(&stale).unwrap();
    std::fs::write(stale.join("run.json"), r#"{"run":"run-1-1","state":"running"}"#).unwrap();
    let s = ui.call("harness.state", json!({"run": "run-1-1"}))["result"].clone();
    assert_eq!((s["state"].as_str(), s["halt"].as_str()), (Some("halted"), Some("daemon_restart")), "{s}");
    assert_eq!(ui.call("harness.state", json!({"run": "../x"}))["error"]["code"], "no_such_run");
}
