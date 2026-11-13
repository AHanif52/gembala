//! PLAN-CC-PANE phase 2: isolated daemon and deterministic fake CLI, no model/account/network.

use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::os::fd::FromRawFd;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct Daemon { child: Child, dir: PathBuf, secret: String }

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill(); let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

const FAKE: &str = r#"#!/usr/bin/env python3
import json, os, signal, socket, sys, time
from pathlib import Path
p = next(x[len('--print='):] for x in sys.argv if x.startswith('--print='))
assert sys.argv[sys.argv.index('--output-format') + 1] == 'json'
assert not any(x in sys.argv for x in ['--tools-all', '--permission-mode'])
assert not any(os.isatty(fd) for fd in [0, 1, 2])
try:
    fd = os.open('/dev/tty', os.O_RDWR)
except OSError:
    pass
else:
    os.close(fd)
    raise AssertionError('unexpected controlling terminal')
Path('pid').write_text(str(os.getpid()))
Path('argv.json').write_text(json.dumps(sys.argv[1:]))
def emit(v):
    print(json.dumps(v), flush=True)
def result(kind='success', text='done'):
    emit(dict(type='result', subtype=kind, sessionId='fake-session', finalText=text))
if p == 'slow':
    signal.signal(signal.SIGINT, signal.SIG_IGN)
    signal.signal(signal.SIGTERM, signal.SIG_IGN)
    emit(dict(type='event', event=dict(type='text_delta', delta='waiting')))
    time.sleep(60)
elif p == 'eof':
    emit(dict(type='event', event=dict(type='run_end')))
elif p.startswith('exit:'):
    code = int(p.split(':')[1])
    result('max_turns' if code == 8 else 'error')
    sys.exit(code)
elif p == 'mismatch':
    result()
    sys.exit(3)
elif p == 'max-mismatch':
    result('max_turns')
    sys.exit(3)
elif p == 'success-interrupted':
    result()
    sys.exit(130)
elif p == 'malformed':
    signal.signal(signal.SIGINT, lambda *_: sys.exit(130))
    print('not JSON', flush=True)
    time.sleep(60)
elif p == 'ring':
    for _ in range(600):
        emit(dict(type='event', event=dict(type='text_delta', delta='x' * 16384)))
    result()
elif p == 'spoof':
    root = Path.cwd()
    address = (root/'ui.addr').read_text().strip() if (root/'ui.addr').exists() else str(root/'ui.sock')
    if address != str(root/'ui.sock'): address = '\0' + address
    s = socket.socket(socket.AF_UNIX)
    s.settimeout(3)
    s.connect(address)
    s.sendall((json.dumps(dict(id=1,method='hello',params=dict(proto=1,secret=(root/'test-secret').read_text()))) + '\n').encode())
    answer = json.loads(s.makefile().readline())
    result(text=answer.get('error', {}).get('code', 'unexpected'))
else:
    sys.stdout.write(Path('fixture.ndjson').read_text())
    sys.stdout.flush()
"#;

fn start(tag: &str) -> Daemon { spawn(tag, false) }

/// Same daemon, but `HOME` points at the sandbox: history browsing reads `$HOME/.commandcode/projects`.
fn start_home(tag: &str) -> Daemon { spawn(tag, true) }

fn spawn(tag: &str, home: bool) -> Daemon {
    let dir = std::env::temp_dir().join(format!("gbl-cc-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::DirBuilder::new().mode(0o700).create(&dir).unwrap();
    let fake = dir.join("fake-cmd");
    std::fs::write(&fake, FAKE).unwrap();
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::write(dir.join("fixture.ndjson"), include_str!("fixtures/commandcode/stream.ndjson")).unwrap();
    std::fs::write(dir.join("config.toml"), format!("[[profile]]\nid = \"commandcode\"\ncommand = {:?}\n[[profile]]\nid = \"shell\"\ntoken = true\n", fake.to_string_lossy())).unwrap();
    let mut fds = [0; 2];
    assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
    let (r, w) = (fds[0], fds[1]);
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_gembala"));
    cmd.args(["daemon", "--secret-fd", "3"]).current_dir(&dir)
        .env("GEMBALA_RUNDIR", &dir).env("GEMBALA_CONFIG", dir.join("config.toml"))
        .env("GEMBALA_IDLE_SECS", "3600").stdout(Stdio::null());
    if home { cmd.env("HOME", &dir); }
    unsafe { cmd.pre_exec(move || {
        libc::close(r);
        if libc::dup2(w, 3) == -1 { Err(std::io::Error::last_os_error()) } else { Ok(()) }
    }); }
    let child = cmd.spawn().unwrap();
    unsafe { libc::close(w); }
    let mut secret = String::new();
    BufReader::new(unsafe { std::fs::File::from_raw_fd(r) }).read_line(&mut secret).unwrap();
    let daemon = Daemon { child, dir, secret: secret.trim().into() };
    assert!(!daemon.secret.is_empty(), "daemon failed to start");
    std::fs::write(daemon.dir.join("test-secret"), &daemon.secret).unwrap();
    daemon
}

struct Client { r: BufReader<UnixStream>, w: UnixStream, id: u64, events: Vec<Value> }

impl Client {
    fn ui(d: &Daemon) -> Self {
        #[cfg(target_os = "linux")]
        let socket = if let Ok(name) = std::fs::read_to_string(d.dir.join("ui.addr")) {
            use std::os::linux::net::SocketAddrExt;
            let addr = std::os::unix::net::SocketAddr::from_abstract_name(name.trim().as_bytes()).unwrap();
            UnixStream::connect_addr(&addr).unwrap()
        } else { UnixStream::connect(d.dir.join("ui.sock")).unwrap() };
        #[cfg(not(target_os = "linux"))]
        let socket = UnixStream::connect(d.dir.join("ui.sock")).unwrap();
        socket.set_read_timeout(Some(Duration::from_secs(15))).unwrap();
        let mut c = Self { r:BufReader::new(socket.try_clone().unwrap()), w:socket, id:0, events:Vec::new() };
        assert_eq!(c.call("hello", json!({"proto":1,"secret":d.secret}))["result"]["proto"], 1);
        c
    }

    fn call(&mut self, method: &str, params: Value) -> Value {
        self.id += 1;
        writeln!(self.w, "{}", json!({"id":self.id,"method":method,"params":params})).unwrap();
        loop {
            let mut line = String::new();
            self.r.read_line(&mut line).unwrap();
            let v: Value = serde_json::from_str(&line).unwrap();
            if v["id"] == self.id { return v; }
            self.events.push(v);
        }
    }

    fn create(&mut self, d: &Daemon) -> u64 {
        let r = self.call("cc.create", json!({"workspace":"test", "cwd":d.dir, "profile":"commandcode"}));
        assert_eq!(r["result"]["kind"], "commandcode", "{r}");
        r["result"]["pane"].as_u64().unwrap()
    }

    fn run(&mut self, pane: u64, request: &str, prompt: &str) -> Value {
        self.call("cc.run", json!({"pane":pane,"requestId":request,"prompt":prompt}))
    }

    fn finish(&mut self, pane: u64) -> Value {
        let start = Instant::now();
        loop {
            let r = self.call("cc.attach", json!({"pane":pane}));
            let state = &r["result"]["snapshot"]["state"];
            if state != "working" { return r["result"].clone(); }
            assert!(start.elapsed() < Duration::from_secs(15), "run did not finish");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

fn assert_gone(pid: i32) {
    assert_eq!(unsafe { libc::kill(pid, 0) }, -1, "child {pid} still exists (possibly zombie)");
    assert_eq!(std::io::Error::last_os_error().raw_os_error(), Some(libc::ESRCH));
}

#[test]
fn create_run_attach_reconnect_and_shared_identity() {
    let d = start("reconnect"); let mut c = Client::ui(&d); let pane = c.create(&d);
    assert!(c.call("pane.list", json!({}))["result"].as_array().unwrap().contains(&json!(pane)));
    assert_eq!(c.call("pane.rename", json!({"pane":pane,"name":"structured"}))["result"]["name"], "structured");
    let run = c.run(pane, "first", "fixture");
    assert!(run["result"]["runId"].is_string(), "{run}");
    assert_eq!(c.run(pane, "first", "fixture")["result"], run["result"]);
    let finished = c.finish(pane);
    assert_eq!(finished["snapshot"]["state"], "done", "{finished}");
    assert_eq!(finished["snapshot"]["result"]["finalText"], "Halo 🌱");
    assert_eq!(finished["snapshot"]["messages"].as_array().unwrap().len(), 1);
    assert_gone(std::fs::read_to_string(d.dir.join("pid")).unwrap().parse().unwrap());
    assert_eq!(c.call("cc.detach", json!({"pane":pane}))["result"]["ok"], true);
    drop(c);
    let mut c = Client::ui(&d);
    assert_eq!(c.call("cc.attach", json!({"pane":pane}))["result"]["snapshot"], finished["snapshot"]);
    assert_eq!(c.call("pane.info", json!({"pane":pane}))["result"]["pane_name"], "structured");
    assert!(c.call("pane.close", json!({"pane":pane}))["result"].is_object());
    assert_eq!(c.call("pane.list", json!({}))["result"], json!([]));
}

#[test]
fn busy_cancel_idempotent_reap_and_close_active() {
    let d = start("cancel"); let mut c = Client::ui(&d); let pane = c.create(&d);
    let run = c.run(pane, "one", "slow")["result"]["runId"].clone();
    assert_eq!(c.run(pane, "two", "slow")["error"]["code"], "busy");
    let start = Instant::now();
    while !d.dir.join("pid").exists() { assert!(start.elapsed() < Duration::from_secs(5)); std::thread::sleep(Duration::from_millis(10)); }
    for _ in 0..2 { assert_eq!(c.call("cc.cancel", json!({"pane":pane,"runId":run}))["result"]["accepted"], true); }
    assert_eq!(c.finish(pane)["snapshot"]["state"], "interrupted");
    assert_gone(std::fs::read_to_string(d.dir.join("pid")).unwrap().parse().unwrap());
    assert_eq!(c.call("cc.cancel", json!({"pane":pane,"runId":run}))["result"]["accepted"], true);
    assert_eq!(c.call("cc.cancel", json!({"pane":pane,"runId":"stale"}))["error"]["code"], "stale_run");
    std::fs::remove_file(d.dir.join("pid")).unwrap();
    c.run(pane, "three", "slow");
    let start = Instant::now();
    while !d.dir.join("pid").exists() { assert!(start.elapsed() < Duration::from_secs(5)); std::thread::sleep(Duration::from_millis(10)); }
    assert!(c.call("pane.close", json!({"pane":pane}))["result"].is_object());
    assert_gone(std::fs::read_to_string(d.dir.join("pid")).unwrap().parse().unwrap());
}

#[test]
fn eof_exit_codes_mismatch_and_malformed_are_not_success() {
    let d = start("exits"); let mut c = Client::ui(&d); let pane = c.create(&d);
    for (prompt, state) in [("eof","interrupted"), ("exit:1","error"), ("exit:3","auth"),
        ("exit:4","permission"), ("exit:5","rate_limit"), ("exit:6","network"), ("exit:7","server"),
        ("exit:8","max_turns"), ("exit:9","no_response"), ("exit:10","credits"), ("exit:130","interrupted"),
        ("mismatch","protocol_error"), ("max-mismatch","protocol_error"), ("success-interrupted","protocol_error"), ("malformed","protocol_error")] {
        assert!(c.run(pane, prompt, prompt)["result"]["runId"].is_string());
        let r = c.finish(pane);
        assert_eq!(r["snapshot"]["state"], state, "{prompt}: {r}");
        assert_gone(std::fs::read_to_string(d.dir.join("pid")).unwrap().parse().unwrap());
    }
}

#[test]
fn terminal_operations_and_untrusted_flags_are_rejected() {
    let d = start("guards"); let mut c = Client::ui(&d); let pane = c.create(&d);
    for (method, params) in [("pane.write",json!({"pane":pane,"data":"YQ=="})),
        ("pane.resize",json!({"pane":pane,"rows":24,"cols":80})), ("pane.attach",json!({"pane":pane})),
        ("pane.kill",json!({"pane":pane}))] {
        assert_eq!(c.call(method, params)["error"]["code"], "wrong_kind", "{method}");
    }
    assert_eq!(c.call("cc.run", json!({"pane":pane,"requestId":"flags","prompt":"x","yolo":true}))["error"]["code"], "bad_params");
    assert_eq!(c.call("harness.preflight", json!({"from_pane":pane}))["error"]["code"], "unsupported_kind");
    assert!(!d.dir.join("pid").exists());
    for prompt in ["--yolo", "--tools-all", "$(touch injected)"] {
        assert!(c.run(pane, prompt, prompt)["result"]["runId"].is_string());
        assert_eq!(c.finish(pane)["snapshot"]["state"], "done");
        let args: Vec<String> = serde_json::from_str(&std::fs::read_to_string(d.dir.join("argv.json")).unwrap()).unwrap();
        assert!(args.contains(&format!("--print={prompt}")));
        assert!(args.iter().all(|a| a != "--yolo"), "a prompt must never become an access flag");
    }
    assert!(!d.dir.join("injected").exists());
    // A terminal pane authenticates via token on the pane channel; cc.* stays UI-only.
    let terminal = c.call("pane.create", json!({"profile":"shell", "cmd":"printf '%s' \"$GEMBALA_TOKEN\" > remote-token; sleep 60", "cwd":d.dir, "workspace":"test"}))["result"]["pane"].clone();
    let start = Instant::now();
    let token = loop {
        if let Ok(token) = std::fs::read_to_string(d.dir.join("remote-token")) && token.len() == 32 { break token; }
        assert!(start.elapsed() < Duration::from_secs(5));
        std::thread::sleep(Duration::from_millis(10));
    };
    let socket = UnixStream::connect(d.dir.join("pane.sock")).unwrap();
    socket.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let mut remote = Client { r:BufReader::new(socket.try_clone().unwrap()), w:socket, id:0, events:Vec::new() };
    assert!(remote.call("hello", json!({"proto":1,"token":token}))["result"].is_object());
    for method in ["send", "read", "wait"] {
        assert_eq!(remote.call(method, json!({"to":"commandcode-1","target":"commandcode-1","body":"x"}))["error"]["code"], "unsupported_kind", "{method}");
    }
    for method in ["cc.create","cc.run","cc.attach","cc.detach","cc.cancel","cc.sessions","cc.resume","cc.access"] {
        assert_eq!(remote.call(method, json!({"pane":pane}))["error"]["code"], "not_allowed", "{method}");
    }
    c.call("pane.close", json!({"pane":terminal}));
}

#[test]
fn ring_eviction_preserves_reduced_snapshot() {
    let d = start("ring"); let mut c = Client::ui(&d); let pane = c.create(&d);
    c.run(pane, "large", "ring");
    let r = c.finish(pane);
    assert_eq!(r["snapshot"]["state"], "done");
    assert_eq!(r["snapshot"]["text"].as_str().unwrap().len(), 256 << 10);
    assert_eq!(r["snapshot"]["truncated"], true);
    let r = c.call("cc.attach", json!({"pane":pane,"afterSeq":0}));
    assert_eq!(r["result"]["gap"], true);
    let bytes: usize = r["result"]["events"].as_array().unwrap().iter().map(|e| e.to_string().len()).sum();
    assert!(bytes <= 8 << 20);
}

#[test]
fn child_cannot_authenticate_as_ui_even_with_secret() {
    let d = start("spoof"); let mut c = Client::ui(&d); let pane = c.create(&d);
    c.run(pane, "spoof", "spoof");
    assert_eq!(c.finish(pane)["snapshot"]["result"]["finalText"], "unauthorized");
}

fn wait_pid(d: &Daemon) -> i32 {
    let start = Instant::now();
    loop {
        if let Ok(text) = std::fs::read_to_string(d.dir.join("pid"))
            && let Ok(pid) = text.trim().parse() { return pid; }
        assert!(start.elapsed() < Duration::from_secs(5), "fake never reported its pid");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn wait_gone(pid: i32) {
    let start = Instant::now();
    while unsafe { libc::kill(pid, 0) } == 0 {
        assert!(start.elapsed() < Duration::from_secs(10), "child {pid} survived shutdown");
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(std::io::Error::last_os_error().raw_os_error(), Some(libc::ESRCH));
}

#[test]
fn daemon_stop_cancels_and_reaps_running_child() {
    let d = start("stop"); let mut c = Client::ui(&d); let pane = c.create(&d);
    assert!(c.run(pane, "stop", "slow")["result"]["runId"].is_string());
    let pid = wait_pid(&d);
    assert!(c.call("daemon.stop", json!({}))["result"].is_object());
    wait_gone(pid);
}

#[test]
fn sigterm_cancels_and_reaps_running_child() {
    let d = start("sigterm"); let mut c = Client::ui(&d); let pane = c.create(&d);
    assert!(c.run(pane, "term", "slow")["result"]["runId"].is_string());
    let pid = wait_pid(&d);
    assert_eq!(unsafe { libc::kill(d.child.id() as i32, libc::SIGTERM) }, 0);
    wait_gone(pid);
}

#[test]
fn access_gate_grants_yolo_then_revokes() {
    let d = start("access"); let mut c = Client::ui(&d); let pane = c.create(&d);
    let info = c.call("pane.info", json!({"pane":pane}))["result"].clone();
    assert_eq!(info["access"], "restricted");
    assert_eq!(info["access_revision"], 0);
    assert!(c.run(pane, "plain", "fixture")["result"]["runId"].is_string());
    assert_eq!(c.finish(pane)["snapshot"]["state"], "done");
    assert!(!argv(&d).iter().any(|a| a == "--yolo"), "restricted runs carry no yolo");
    // The gate needs the current revision and an answered dialog id.
    assert_eq!(c.call("cc.access", json!({"pane":pane,"mode":"yolo","revision":0}))["error"]["code"], "bad_gate");
    assert_eq!(c.call("cc.access", json!({"pane":pane,"mode":"yolo","revision":7,"gateId":"g"}))["error"]["code"], "stale_revision");
    assert_eq!(c.call("cc.access", json!({"pane":pane,"mode":"everything","revision":0,"gateId":"g"}))["error"]["code"], "bad_params");
    let granted = c.call("cc.access", json!({"pane":pane,"mode":"yolo","revision":0,"gateId":"gate-1"}))["result"].clone();
    assert_eq!(granted["mode"], "yolo");
    assert_eq!(granted["revision"], 1);
    assert!(c.run(pane, "wide", "fixture")["result"]["runId"].is_string());
    assert_eq!(c.finish(pane)["snapshot"]["state"], "done");
    assert_eq!(c.finish(pane)["snapshot"]["access"], "yolo");
    assert!(argv(&d).iter().any(|a| a == "--yolo"), "granted run carries yolo");
    // Revoking stops a live run and the next submission is restricted again.
    assert!(c.run(pane, "slow-wide", "slow")["result"]["runId"].is_string());
    let info = c.call("pane.info", json!({"pane":pane}))["result"].clone();
    let revoked = c.call("cc.access", json!({"pane":pane,"mode":"restricted","revision":info["access_revision"]}))["result"].clone();
    assert_eq!(revoked["mode"], "restricted");
    assert_eq!(c.finish(pane)["snapshot"]["state"], "interrupted");
    assert_eq!(c.call("cc.access", json!({"pane":pane,"mode":"restricted","revision":info["access_revision"]}))["error"]["code"], "stale_revision");
}

#[test]
fn grant_before_the_first_run_follows_that_session() {
    let d = start("access-first"); let mut c = Client::ui(&d); let pane = c.create(&d);
    let granted = c.call("cc.access", json!({"pane":pane,"mode":"yolo","revision":0,"gateId":"gate-first"}))["result"].clone();
    assert_eq!(granted["session"], Value::Null);
    assert!(c.run(pane, "wide-first", "fixture")["result"]["runId"].is_string());
    assert_eq!(c.finish(pane)["snapshot"]["state"], "done");
    assert_eq!(c.finish(pane)["snapshot"]["sessionId"], "fixture-session");
    assert!(argv(&d).iter().any(|a| a == "--yolo"));
    // The grant now covers that session, so the follow-up is wide as well.
    assert!(c.run(pane, "wide-second", "fixture")["result"]["runId"].is_string());
    assert_eq!(c.finish(pane)["snapshot"]["state"], "done");
    assert!(argv(&d).iter().any(|a| a == "--yolo"));
}

#[test]
fn git_status_reads_the_commandcode_pane_folder() {
    let d = start("git");
    let repo = d.dir.join("repo");
    std::fs::create_dir(&repo).unwrap();
    assert!(Command::new("git").args(["init", "-q"]).current_dir(&repo).status().unwrap().success());
    let mut c = Client::ui(&d);
    let r = c.call("cc.create", json!({"workspace":"test", "cwd":repo, "profile":"commandcode"}));
    let pane = r["result"]["pane"].as_u64().unwrap();
    assert_eq!(c.call("git.status", json!({"pane":pane}))["result"]["repo"], true);
}

fn argv(d: &Daemon) -> Vec<String> {
    serde_json::from_str(&std::fs::read_to_string(d.dir.join("argv.json")).unwrap()).unwrap()
}

/// Writes a v3 transcript into the sandbox `HOME`, with the header pointing at the pane folder.
fn write_session(home: &Path, name: &str, id: &str, cwd: &Path, version: u64) {
    let project = home.join(".commandcode/projects/demo");
    std::fs::create_dir_all(&project).unwrap();
    let text = include_str!("fixtures/commandcode/session-v3.jsonl")
        .replacen("\"version\":3", &format!("\"version\":{version}"), 1)
        .replacen("\"id\":\"fixture-session\"", &format!("\"id\":\"{id}\""), 1)
        .replacen("\"cwd\":\"/fixture/project\"", &format!("\"cwd\":\"{}\"", cwd.display()), 1);
    std::fs::write(project.join(name), text).unwrap();
}

#[test]
fn second_run_resumes_the_bound_session() {
    let d = start("continuity"); let mut c = Client::ui(&d); let pane = c.create(&d);
    assert!(c.run(pane, "one", "fixture")["result"]["runId"].is_string());
    assert_eq!(c.finish(pane)["snapshot"]["state"], "done");
    assert!(!argv(&d).iter().any(|a| a == "--resume"));
    assert!(c.run(pane, "two", "fixture")["result"]["runId"].is_string());
    assert_eq!(c.finish(pane)["snapshot"]["state"], "done");
    let args = argv(&d);
    let at = args.iter().position(|a| a == "--resume").expect("second run must resume");
    assert_eq!(args[at + 1], "fixture-session");
    assert!(!args.iter().any(|a| a == "--continue"));
    assert_eq!(c.call("pane.info", json!({"pane":pane}))["result"]["session"], "fixture-session");
}

#[test]
fn sessions_listing_is_scoped_to_the_pane_folder() {
    let d = start_home("sessions"); let mut c = Client::ui(&d); let pane = c.create(&d);
    let cwd = std::fs::canonicalize(&d.dir).unwrap();
    write_session(&d.dir, "mine.jsonl", "fixture-session", &cwd, 3);
    write_session(&d.dir, "other.jsonl", "other-session", Path::new("/somewhere/else"), 3);
    write_session(&d.dir, "old.jsonl", "old-session", &cwd, 2);
    std::fs::write(d.dir.join(".commandcode/projects/demo/mine.prompts.jsonl"), "{}").unwrap();
    let outside = std::env::temp_dir().join(format!("gbl-cc-outside-{}", std::process::id()));
    std::fs::write(&outside, include_str!("fixtures/commandcode/session-v3.jsonl")).unwrap();
    std::os::unix::fs::symlink(&outside, d.dir.join(".commandcode/projects/demo/link.jsonl")).unwrap();
    let r = c.call("cc.sessions", json!({"pane":pane}))["result"].clone();
    let ids: Vec<&str> = r["sessions"].as_array().unwrap().iter().filter_map(|s| s["id"].as_str()).collect();
    assert_eq!(ids, vec!["fixture-session", "old-session"], "{r}");
    assert!(r["sessions"][1]["unsupported"].as_bool().unwrap(), "{r}");
    assert!(r["sessions"][0]["path"].as_str().unwrap().ends_with("mine.jsonl"));
    assert!(r["sessions"][0]["updatedAt"].as_u64().is_some());
    let empty = std::env::temp_dir().join(format!("gbl-cc-elsewhere-{}", std::process::id()));
    std::fs::create_dir_all(&empty).unwrap();
    let other = c.call("cc.create", json!({"workspace":"test", "cwd":empty, "profile":"commandcode"}));
    let other = other["result"]["pane"].as_u64().unwrap();
    assert_eq!(c.call("cc.sessions", json!({"pane":other}))["result"]["sessions"], json!([]));
    let _ = std::fs::remove_dir_all(&empty);
    let _ = std::fs::remove_file(&outside);
}

#[test]
fn resume_binds_session_history_and_guards() {
    let d = start_home("resume"); let mut c = Client::ui(&d); let pane = c.create(&d);
    let cwd = std::fs::canonicalize(&d.dir).unwrap();
    write_session(&d.dir, "mine.jsonl", "fixture-session", &cwd, 3);
    let r = c.call("cc.resume", json!({"pane":pane, "sessionId":"fixture-session"}))["result"].clone();
    assert_eq!(r["sessionId"], "fixture-session");
    let messages = r["history"]["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 4, "{r}");
    assert_eq!(messages[0]["role"], "user");
    assert_eq!(messages[1]["content"][0]["type"], "thinking");
    assert_eq!(messages[1]["content"][1]["type"], "tool_use");
    assert!(!r["history"]["messages"].to_string().contains("Discarded branch"), "sibling branch leaked");
    assert_eq!(r["history"]["partial"], Value::Null);
    assert_eq!(c.call("pane.info", json!({"pane":pane}))["result"]["session"], "fixture-session");
    assert!(std::fs::read_to_string(d.dir.join("pid")).is_err(), "resume must not spawn");
    assert!(c.run(pane, "after-resume", "fixture")["result"]["runId"].is_string());
    assert_eq!(c.finish(pane)["snapshot"]["state"], "done");
    let args = argv(&d);
    assert!(args.windows(2).any(|w| w[0] == "--resume" && w[1] == "fixture-session"), "{args:?}");
    assert_eq!(c.call("cc.resume", json!({"pane":pane, "sessionId":"nope"}) )["error"]["code"], "no_such_session");
    assert_eq!(c.call("cc.resume", json!({"pane":pane, "sessionId":"bad/id"}))["error"]["code"], "bad_params");
    write_session(&d.dir, "old.jsonl", "old-session", &cwd, 2);
    assert_eq!(c.call("cc.resume", json!({"pane":pane, "sessionId":"old-session"}))["error"]["code"], "unsupported_format");
    // A session has one writer: another pane binding the same id is refused.
    let second = c.create(&d);
    assert_eq!(c.call("cc.resume", json!({"pane":second, "sessionId":"fixture-session"}))["error"]["code"], "session_in_use");
    // And a live run wins over history.
    let run = c.run(second, "slow-one", "slow")["result"]["runId"].as_str().unwrap().to_string();
    assert_eq!(c.call("cc.resume", json!({"pane":second, "sessionId":"fixture-session"}))["error"]["code"], "busy");
    assert_eq!(c.call("cc.cancel", json!({"pane":second, "runId":run}))["result"]["accepted"], true);
    assert_eq!(c.finish(second)["snapshot"]["state"], "interrupted");
}
