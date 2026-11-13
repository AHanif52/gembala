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

/// Recipes naming `claude`/`omp` are refused when detection doesn't find them; stubs on PATH make that the same on a
/// bare CI runner as on a Mac with the agents installed.
fn start_with_agents(tag: &str) -> Daemon {
    use std::os::unix::fs::PermissionsExt;
    let bin = std::env::temp_dir().join(format!("gbl-{}-{tag}-bin", std::process::id()));
    std::fs::create_dir_all(&bin).unwrap();
    for a in ["claude", "omp"] {
        std::fs::write(bin.join(a), "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(bin.join(a), std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let path = PathBuf::from(format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default()));
    start_env(tag, 3600, &[("PATH", &path)])
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
        .env("NO_COLOR", "1") // Oh My Pi's shell tool sets it
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

/// Linux lapis 1 (SEC-20, TECH 21.8): the UI channel lives on an abstract socket whose name sits in `<rundir>/ui.addr`;
/// a kernel without a Landlock scope keeps the path socket. The harness uses the same rendezvous as the UI.
#[cfg(target_os = "linux")]
fn abstract_connect(name: &str) -> Option<UnixStream> {
    use std::os::fd::FromRawFd;
    let bytes = name.as_bytes();
    let mut sa: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    sa.sun_family = libc::AF_UNIX as libc::sa_family_t;
    if bytes.is_empty() || bytes.len() + 1 >= sa.sun_path.len() {
        return None;
    }
    for (i, b) in bytes.iter().enumerate() {
        sa.sun_path[i + 1] = *b as libc::c_char;
    }
    let len = (std::mem::size_of::<libc::sa_family_t>() + 1 + bytes.len()) as libc::socklen_t;
    let fd = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM | libc::SOCK_CLOEXEC, 0) };
    if fd == -1 {
        return None;
    }
    if unsafe { libc::connect(fd, (&raw const sa).cast(), len) } == -1 {
        unsafe { libc::close(fd) };
        return None;
    }
    Some(unsafe { UnixStream::from_raw_fd(fd) })
}

/// `ui.addr` content, when the daemon named an abstract UI socket.
fn ui_addr(d: &Daemon) -> Option<String> {
    let s = std::fs::read_to_string(d.dir.join("ui.addr")).ok()?;
    let n = s.trim().to_string();
    (!n.is_empty()).then_some(n)
}

/// `nc -U` target of the UI channel: `@name` (abstract) or the path socket.
fn ui_target(d: &Daemon) -> String {
    #[cfg(target_os = "linux")]
    if let Some(name) = ui_addr(d) {
        return format!("@{name}");
    }
    d.dir.join("ui.sock").display().to_string()
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
        let s = match (sock, ui_addr(d)) {
            ("ui.sock", Some(name)) => {
                #[cfg(target_os = "linux")]
                let s = abstract_connect(&name).unwrap();
                #[cfg(not(target_os = "linux"))]
                let s = { let _ = name; UnixStream::connect(d.dir.join(sock)).unwrap() };
                s
            }
            _ => UnixStream::connect(d.dir.join(sock)).unwrap(),
        };
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
            // only this pane's exit ends the wait (another pane closing can arrive before this pane's last output)
            if (e["event"] == "pane.exit" && e["params"]["pane"] == pane) || e.is_null() {
                return s;
            }
            if e["params"]["pane"] == pane {
                out.extend(B64.decode(e["params"]["data"].as_str().unwrap_or("")).unwrap());
            }
        }
    }
}

#[test]
fn hello_kanal_ui() {
    let d = start("hello");
    Cli::ui(&d);
    let mut c = Cli::connect(&d, "ui.sock");
    assert_eq!(c.call("pane.create", json!({}))["error"]["code"], "hello_required");
    // SEC-20: the secret is the proof on both paths (the abstract socket only keeps a pane's agents away, and every
    // process of this uid can read its name); a signature can stand in for it, which only a macOS app build has
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

/// SEC-20 lapis 2 (TECH 21.8): on a kernel without a Landlock scope the UI channel is the path socket, and a peer that
/// carries an injection library is refused even with the right secret (SO_PEERPIDFD pins the pid for the /proc read).
#[cfg(target_os = "linux")]
#[test]
fn lapis_2_peer_disuntik_ditolak() {
    use std::process::Stdio;
    let has = |cmd: &str| std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(cmd).is_file()));
    if !has("nc") {
        eprintln!("lewati: nc (netcat) tidak terpasang");
        return;
    }
    let d = start_env("uipath", 3600, &[("GEMBALA_UI_PATH", std::path::Path::new("1"))]);
    assert!(d.dir.join("ui.sock").exists(), "jalur 2 memakai socket ber-path");
    // a clean peer goes through (the harness is the UI here)
    assert_eq!(Cli::ui(&d).call("daemon.info", json!({}))["result"]["proto"], 1);
    let sock = d.dir.join("ui.sock");
    let mut c = Command::new("nc")
        .args(["-U", &sock.display().to_string()])
        .env("LD_PRELOAD", "/tidak-ada/libx.so")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut w = c.stdin.take().unwrap();
    writeln!(w, "{}", json!({"id": 1, "method": "hello", "params": {"proto": 1, "client": "ui", "secret": d.secret}})).unwrap();
    drop(w);
    let mut out = String::new();
    BufReader::new(c.stdout.take().unwrap()).read_line(&mut out).unwrap();
    let _ = c.wait();
    assert!(out.contains(r#""code":"unauthorized""#), "peer ber-LD_PRELOAD harus ditolak: {out}");
}

/// Criterion 9: a script inside a pane, even one holding the UI secret, can't use the UI channel;
/// through the pane channel, human actions are rejected.
#[test]
fn kriteria_9_skrip_di_pane() {
    let d = start("k9");
    let mut ui = Cli::ui(&d);
    let ui_sock = ui_target(&d);
    let script = format!(
        r#"(printf '%s\n' '{{"id":1,"method":"hello","params":{{"proto":1,"secret":"{s}"}}}}'; sleep 1) | nc -U {u}; echo; \
(printf '%s\n' '{{"id":1,"method":"hello","params":{{"proto":1}}}}' '{{"id":2,"method":"pane.close","params":{{"pane":1}}}}' '{{"id":3,"method":"approve","params":{{}}}}'; sleep 1) | nc -U "$GEMBALA_SOCKET"; echo SELESAI-K9"#,
        s = d.secret,
        u = ui_sock
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
    let b = ui.call("pane.create", json!({"cmd": "i=0; while [ $i -lt 60 ]; do echo SAMA-BARIS; i=$((i+1)); done; echo LOOP-SELESAI; sleep 30"}))["result"].clone();
    let name = b["name"].as_str().unwrap();
    // wait for the whole loop: on a slow runner (CI Linux 11 Nov) compose saw only 8 of 60 lines
    ui.attach_until(b["pane"].as_u64().unwrap(), &["LOOP-SELESAI"]);
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

/// A client that attaches but doesn't read stops receiving that pane's output when its queue fills, while the pane keeps
/// running to the end and the daemon never waits for it. The connection itself stays: a UI client whose socket was shut
/// down here read it as "the daemon stopped or was restarted" and marked every pane dead (a second Restart click then
/// opened an agent twice). Its next request carries `pane.lag`, and a re-attach rebuilds the screen from the snapshot.
/// Nothing depends on how fast the runner is: a reply that timed out says nothing about the pane (ask again).
#[test]
fn klien_lambat_diputus() {
    let d = start("lambat");
    let mut ui = Cli::ui(&d);
    let pane = ui.call("pane.create", json!({"cmd": "yes aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa | head -c 8000000; echo SELESAI-LAMBAT; sleep 30", "rows": 24, "cols": 80}))["result"]["pane"].as_u64().unwrap();
    let mut lambat = Cli::ui(&d);
    lambat.call("pane.attach", json!({"pane": pane}));
    // the pane reaches the end of the burst; a client that doesn't read doesn't hold it (a slow reply on a loaded runner
    // says nothing about the pane: ask again)
    let t0 = Instant::now();
    loop {
        let a = ui.call("pane.attach", json!({"pane": pane}));
        if String::from_utf8_lossy(&written(&a["result"])).contains("SELESAI-LAMBAT") {
            break;
        }
        assert!(t0.elapsed() < Duration::from_secs(120), "pane tertahan klien lambat: {a}");
        std::thread::sleep(Duration::from_millis(200));
    }
    // the same connection answers (no end-of-file) and says which pane it was cut from; the notice rides on the first
    // request that finds room in the queue, so a client still draining its buffer gets it on the next one (the UI polls
    // every 2 seconds)
    let lag_of = |c: &Cli| c.events.iter().any(|e| e["event"] == "pane.lag" && e["params"]["pane"] == pane);
    let boot = ui.call("daemon.info", json!({}))["result"]["boot"].clone();
    let mut info = lambat.call("daemon.info", json!({}));
    for _ in 0..4 {
        if lag_of(&lambat) {
            break;
        }
        info = lambat.call("daemon.info", json!({}));
    }
    assert_eq!(info["result"]["boot"], boot, "sambungan klien lambat diputus: {info}");
    assert!(lag_of(&lambat), "klien lambat nggak dapat pane.lag: {} events", lambat.events.len());
    // re-attach (what the UI does on pane.lag) works on that connection: the burst went through the pane's screen
    let a = lambat.call("pane.attach", json!({"pane": pane}));
    assert_eq!(a["result"]["sync"], json!(true), "{a}");
    assert!(!written(&a["result"]).is_empty(), "snapshot kosong setelah sambung ulang");
}

/// Criterion 17: a UI that reconnects to the same daemon (same boot id) finds its panes still there and re-attaches them
/// with the output printed while it was away. The page does exactly that instead of marking every pane dead, so a dropped
/// socket doesn't become a second agent (see `onLost` in the page).
#[test]
fn ui_sambung_ulang_pane_masih_hidup() {
    let d = start("sambung");
    let mut ui = Cli::ui(&d);
    let pane = ui.call("pane.create", json!({"cmd": "sleep 1; echo LEWAT-SAMBUNG; sleep 30", "rows": 24, "cols": 80}))["result"]["pane"].as_u64().unwrap();
    let boot = ui.call("daemon.info", json!({}))["result"]["boot"].clone();
    // the connection goes away mid-session; the daemon and its panes don't
    drop(ui);
    std::thread::sleep(Duration::from_millis(2000));
    let mut baru = Cli::ui(&d);
    assert_eq!(baru.call("daemon.info", json!({}))["result"]["boot"], boot, "boot id berubah");
    assert!(baru.call("pane.list", json!({}))["result"].as_array().unwrap().contains(&json!(pane)), "pane hilang");
    let a = baru.call("pane.attach", json!({"pane": pane}));
    assert!(String::from_utf8_lossy(&written(&a["result"])).contains("LEWAT-SAMBUNG"), "output selama putus nggak ikut");
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
    for bocor in ["CLAUDECODE=", "token-bocor-uji", "thread-bocor-uji", "akun-bocor-uji", "GEMBALA_RUNDIR=", "NO_COLOR="] {
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

/// Waits until the pane's foreground process is `name`. A bash pane can still be starting (its rc files run in a child
/// `bash`), so the first busy reading after typing a command isn't that command yet.
fn wait_name(ui: &mut Cli, pane: u64, name: &str, within: Duration) -> Value {
    let t0 = Instant::now();
    loop {
        let r = ui.call("pane.info", json!({"pane": pane}))["result"].clone();
        if r["name"] == name || t0.elapsed() > within {
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
    let r = wait_name(&mut ui, pane, "sleep", Duration::from_secs(5));
    assert_eq!(r["name"], "sleep", "{r}");
    assert_eq!(ui.call("pane.kill", json!({"pane": pane}))["result"]["stopped"], true);
    // under 3 s = TERM did it, not the KILL that follows at 3 s; 2 s was too tight on a loaded machine
    assert_eq!(wait_info(&mut ui, pane, false, Duration::from_millis(2800))["busy"], false, "sleep harus mati oleh TERM");

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
    assert_eq!(names, ["claude", "codex", "opencode", "omp", "commandcode"]);
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
    assert_eq!(e["params"]["pane"], pane);
    assert_eq!(e["params"]["status"], "working");
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    assert!(e["params"]["status_at"].as_u64().is_some_and(|at| at <= now && now - at < 8), "{e}");
    assert!(e["params"]["last"].is_null(), "{e}");
    let e = next_event(&mut ui, "pane.status", Duration::from_secs(8));
    assert_eq!(e["params"]["status"], "blocked", "{e}");
    let info = &ui.call("pane.info", json!({"pane": pane}))["result"];
    assert_eq!(info["status"], "blocked");
    assert_eq!(info["status_at"], e["params"]["status_at"]);
    // a process outside a pane can't change anyone's status
    let mut outsider = Cli::connect(&d, "pane.sock");
    assert_eq!(outsider.call("hello", json!({"proto": 1}))["error"]["code"], "not_a_pane");
}

/// FR-101 item 1: Claude's SubagentStart/SubagentStop hooks count the pane's subagents (never below zero) without
/// touching its status; a new session starts the count over.
#[test]
fn subagent_dari_hook() {
    let d = start("subagent");
    let mut ui = Cli::ui(&d);
    let exe = env!("CARGO_BIN_EXE_gembala");
    let cfg = d.dir.join("cfg");
    std::fs::create_dir_all(&cfg).unwrap();
    std::fs::set_permissions(&cfg, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
    let fake = cfg.join("sub-claude.sh");
    let h = |ev: &str| format!("'{exe}' hook claude {ev} </dev/null\n");
    std::fs::write(
        &fake,
        format!("#!/bin/sh\n{}{}{}{}{}{}sleep 1\n{}{}sleep 1\n{}sleep 30\n", h("prompt"), h("substart"), h("substart"), h("substart"), h("substop"), h("substop"), h("substop"), h("substop"), h("substart")),
    )
    .unwrap();
    std::fs::set_permissions(&fake, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
    std::fs::write(cfg.join("config.toml"), format!("[[profile]]\nid = \"claude\"\ncommand = {}\n", json!(fake))).unwrap();
    let t0 = Instant::now();
    while !ui.call("config.get", json!({}))["result"]["values"]["profiles"].to_string().contains("sub-claude") {
        assert!(t0.elapsed() < Duration::from_secs(10), "config tidak terbaca");
        std::thread::sleep(Duration::from_millis(100));
    }
    let pane = ui.call("pane.create", json!({"profile": "claude"}))["result"]["pane"].as_u64().unwrap();
    // 3 started, 2 ended = 1; one more stop = 0 and a stop below zero stays 0; one start = 1 again; status stays "working"
    let t0 = Instant::now();
    let mut seen = vec![];
    loop {
        let info = &ui.call("pane.info", json!({"pane": pane}))["result"];
        let n = info["subagents"].as_u64().unwrap();
        if seen.last() != Some(&n) {
            seen.push(n);
        }
        if seen.windows(3).any(|w| w == [1, 0, 1]) {
            assert_eq!(info["status"], "working", "{info}");
            break;
        }
        assert!(t0.elapsed() < Duration::from_secs(10), "hitungan subagent: {seen:?}");
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(seen.iter().all(|n| *n <= 3), "{seen:?}");
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

/// FR-58: shell panes emit OSC 133 block markers (C + base64 command, D + exit code); the user's ZDOTDIR comes back
/// (zsh; bash has no ZDOTDIR, the hook files are read through `--rcfile`/`BASH_ENV` instead).
#[test]
fn penanda_blok_shell() {
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
    // FR-59: zsh hides the prompt and zle writes the command line in bold (block head); bash's readline can't style it
    if std::env::var("SHELL").is_ok_and(|s| s.ends_with("/zsh")) {
        assert!(out.contains("\x1b[1m"), "perintah tidak tebal: {out:?}");
    }
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

/// FR-98 / SEC-20: an agent typed in a shell pane goes through its wrapper and `gembala jail --`, so it starts with
/// no_new_privs (the domain that keeps it off the abstract UI socket on a Landlock kernel).
#[cfg(target_os = "linux")]
#[test]
fn wrapper_agent_lewat_jail() {
    use std::os::unix::fs::PermissionsExt;
    let shell = std::env::var("SHELL").unwrap_or_default();
    if !(shell.ends_with("/zsh") || shell.ends_with("/bash")) {
        eprintln!("lewati: butuh pane zsh/bash, SHELL={shell:?}");
        return;
    }
    let bin = std::env::temp_dir().join(format!("gbl-{}-jailwrap-bin", std::process::id()));
    let _ = std::fs::remove_dir_all(&bin);
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::write(bin.join("claude"), "#!/bin/sh\necho \"NNP=$(sed -n 's/^NoNewPrivs:[[:space:]]*//p' /proc/self/status)\"\necho \"ARGS=$*\"\n").unwrap();
    std::fs::set_permissions(bin.join("claude"), std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = PathBuf::from(format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default()));
    let d = start_env("jailwrap", 3600, &[("PATH", &path)]);
    let mut ui = Cli::ui(&d);
    let p = ui.call("pane.create", json!({}))["result"]["pane"].as_u64().unwrap();
    ui.call("pane.attach", json!({"pane": p}));
    std::thread::sleep(Duration::from_secs(1));
    ui.call("pane.write", json!({"pane": p, "data": B64.encode("claude\r")}));
    let out = ui.attach_until(p, &["ARGS="]);
    assert!(out.contains("ARGS=") && out.contains("--append-system-prompt"), "wrapper agent tidak jalan: {out}");
    assert!(out.contains("NNP=1"), "agent tidak lewat jail: {out}");
    let _ = std::fs::remove_dir_all(&bin);
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
    assert_eq!(e["params"]["pane"], pane);
    assert_eq!(e["params"]["status"], "idle");
    assert_eq!(e["params"]["shell"], false);
    assert_eq!(e["params"]["reason"], "");
    assert!(e["params"]["status_at"].is_number(), "{e}");
    assert!(e["params"]["last"].is_null(), "{e}");
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
    // a loaded runner can take >5 s to start the login shell; without the check the kill misses `sleep 30` and the next
    // line waits behind it
    let r = wait_name(&mut ui, pane, "sleep", Duration::from_secs(20));
    assert_eq!((&r["name"], &r["busy"], &r["password"]), (&json!("sleep"), &json!(true), &json!(false)), "{r}");
    ui.call("pane.kill", json!({"pane": pane}));
    let r = wait_info(&mut ui, pane, false, Duration::from_secs(10));
    assert_eq!(r["busy"], false, "{r}");
    w(&mut ui, "sh -c 'stty -echo; read x'\r");
    let t0 = Instant::now();
    loop {
        let info = ui.call("pane.info", json!({"pane": pane}))["result"].clone();
        if info["password"] == true {
            break;
        }
        if t0.elapsed() > Duration::from_secs(15) {
            let screen = B64.decode(snapshot(&ui.call("pane.attach", json!({"pane": pane})))).unwrap();
            panic!("prompt password tidak terdeteksi: {info}\n{}", String::from_utf8_lossy(&screen));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// FR-4: a pane that prints one recorded agent screen and then sits there (140×45, the size the agents were recorded in);
/// `id` = the profile it runs, so the pane gets that agent's status source.
fn pane_with_screen(tag: &str, id: &str, fixture: &str) -> (Daemon, Cli, u64) {
    use std::os::unix::fs::PermissionsExt;
    let d = start(tag);
    let dlg = std::fs::read_to_string(format!("{}/tests/dialogs/{fixture}.txt", env!("CARGO_MANIFEST_DIR"))).unwrap();
    let shown = d.dir.join("dialog.txt");
    std::fs::write(&shown, dlg).unwrap();
    // hook flags land before a profile's own args, so the stand-in is a script that ignores them
    let run = d.dir.join("run.sh");
    std::fs::write(&run, format!("#!/bin/sh\ncat {}; sleep 30\n", shown.display())).unwrap();
    std::fs::set_permissions(&run, std::fs::Permissions::from_mode(0o755)).unwrap();
    let prof = format!("[[profile]]\nid = \"{id}\"\ncommand = {}\n", json!(run.to_string_lossy()));
    let mut ui = Cli::ui(&d);
    profiles(&d, &mut ui, &prof, "run.sh");
    let pane = ui.call("pane.create", json!({"profile": id, "rows": 45, "cols": 140}))["result"]["pane"].as_u64().unwrap();
    (d, ui, pane)
}

fn wait_blocked(ui: &mut Cli, pane: u64, what: &str) {
    let t0 = Instant::now();
    while ui.call("pane.info", json!({"pane": pane}))["result"]["status"] != "blocked" {
        assert!(t0.elapsed() < Duration::from_secs(10), "{what} tidak terbaca dari layar");
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// FR-4, FR-80: a Codex pane showing its real permission dialog reads as blocked from the screen, so the app's "Butuh lu"
/// strip and the Telegram card can name it (11 Nov: Codex asked on screen and nothing came).
#[test]
fn status_blocked_dari_dialog_codex() {
    let (_d, mut ui, pane) = pane_with_screen("codex-dialog", "codex", "codex-0.167.0-net-y");
    wait_blocked(&mut ui, pane, "dialog izin Codex");
}

/// 12 Nov 2026: Command Code's own permission prompt read as working, then done — no card, no buttons. The pane now reads
/// as blocked from that screen and `pane.dialogs` offers only the two grant answers (No opens a feedback box: Mac only).
#[test]
fn status_blocked_dari_dialog_commandcode() {
    let (_d, mut ui, pane) = pane_with_screen("cc-dialog", "commandcode", "commandcode-2.1.311-bash.dialog");
    wait_blocked(&mut ui, pane, "dialog izin Command Code");
    let d = ui.call("pane.dialogs", json!({"panes": [pane], "lang": "en"}))["result"]["panes"][pane.to_string()].clone();
    assert_eq!(d["waiting"], true, "{d}");
    let keys: Vec<&str> = d["options"].as_array().unwrap().iter().map(|o| o["key"].as_str().unwrap()).collect();
    assert_eq!(keys, ["1", "2"], "{d}");
    assert!(d["ask"].as_str().unwrap().contains("Command Code needs to execute"), "{d}");
}

/// FR-99 P2: the permission bar reads the same dialog as the bot (`pane.dialogs`), and both of its answers only write while
/// the pane is still `blocked` (`pane.answer`): a button sends the same bytes as the bot's, a typed answer lands in the pane.
#[test]
fn fr99_bar_izin_dan_jawaban() {
    let (_d, mut ui, pane) = pane_with_screen("p2-bar", "codex", "codex-0.167.0-net-y");
    wait_blocked(&mut ui, pane, "dialog izin Codex");
    let key = pane.to_string();
    let resp = ui.call("pane.dialogs", json!({"panes": [pane], "lang": "id"}));
    let r = resp["result"]["panes"][key.as_str()].clone();
    assert_eq!(r["waiting"], true, "{resp}");
    assert_eq!(r["password"], false);
    assert_eq!(r["ask"], "curl -sI https://example.com -o head.txt");
    assert!(r["why"].as_str().unwrap().starts_with("Allow this exact curl command"), "{r}");
    let opts = r["options"].as_array().unwrap();
    assert_eq!(opts.len(), 3, "{r}");
    assert_eq!((opts[0]["key"].as_str(), opts[0]["label"].as_str()), (Some("1"), Some("1. Izinkan")));
    assert_eq!((opts[1]["label"].as_str(), opts[2]["label"].as_str()), (Some("2. Izinkan terus"), Some("Tolak")));
    let en = ui.call("pane.dialogs", json!({"panes": [pane], "lang": "en"}));
    let en_opts = en["result"]["panes"][key.as_str()]["options"].as_array().unwrap();
    assert_eq!((en_opts[0]["label"].as_str(), en_opts[1]["label"].as_str(), en_opts[2]["label"].as_str()),
        (Some("1. Allow"), Some("2. Allow always"), Some("Deny")));
    // SEC-28: an answer for another dialog generation writes nothing (the bar was rendered before a new question)
    assert_eq!(ui.call("pane.answer", json!({"pane": pane, "key": "1", "seq": 987654}))["result"]["reason"], "changed");
    assert_eq!(ui.call("pane.dialogs", json!({"panes": [pane], "lang": "id"}))["result"]["panes"][key.as_str()]["waiting"], true);
    // a button = the same shortcut the bot sends (`codex_key` read the dialog); the pane echoes it
    assert_eq!(ui.call("pane.answer", json!({"pane": pane, "key": "1", "seq": r["seq"]}))["result"]["sent"], true);
    // typed answers use the same blocked+sequence guard as buttons
    let (_d2, mut ui2, pane2) = pane_with_screen("p2-text", "codex", "codex-0.167.0-net-y");
    wait_blocked(&mut ui2, pane2, "dialog izin Codex");
    let seq2 = ui2.call("pane.dialogs", json!({"panes": [pane2], "lang": "id"}))["result"]["panes"][pane2.to_string().as_str()]["seq"].clone();
    assert_eq!(ui2.call("pane.answer", json!({"pane": pane2, "text": "HALO-99", "seq": seq2}))["result"]["sent"], true);
    let out = ui2.attach_until(pane2, &["HALO-99"]);
    assert!(out.contains("HALO-99"), "{out}");
    // a pane that isn't blocked answers nothing (the bar is gone by then); no key or text is written
    let d = start("p2-gone");
    let mut idle = Cli::ui(&d);
    let shell = idle.call("pane.create", json!({}))["result"]["pane"].as_u64().unwrap();
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(idle.call("pane.answer", json!({"pane": shell, "key": "1", "seq": 0}))["result"]["reason"], "shell");
    assert_eq!(idle.call("pane.answer", json!({"pane": shell, "text": "x", "seq": 0}))["result"]["reason"], "shell");
    assert_eq!(idle.call("pane.dialogs", json!({"panes": [shell]}))["result"]["panes"][shell.to_string().as_str()]["waiting"], false);
    // A password prompt inside an agent pane also rejects both answer forms before checking the dialog generation.
    use std::os::unix::fs::PermissionsExt;
    let d = start("p2-password");
    let run = d.dir.join("password.sh");
    std::fs::write(&run, "#!/bin/sh\nstty -echo\nprintf 'Password: '\nread x\n").unwrap();
    std::fs::set_permissions(&run, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut password_ui = Cli::ui(&d);
    profiles(&d, &mut password_ui, &format!("[[profile]]\nid = \"codex\"\ncommand = {}\n", json!(run.to_string_lossy())), "password.sh");
    let password_pane = password_ui.call("pane.create", json!({"profile": "codex"}))["result"]["pane"].as_u64().unwrap();
    let started = Instant::now();
    while password_ui.call("pane.info", json!({"pane": password_pane}))["result"]["password"] != true {
        assert!(started.elapsed() < Duration::from_secs(5), "password prompt not detected");
        std::thread::sleep(Duration::from_millis(100));
    }
    assert_eq!(password_ui.call("pane.answer", json!({"pane": password_pane, "key": "1", "seq": 0}))["result"]["reason"], "password");
    assert_eq!(password_ui.call("pane.answer", json!({"pane": password_pane, "text": "secret", "seq": 0}))["result"]["reason"], "password");
    // no pane at all: the UI gets an empty map, never an error that breaks the strip
    assert_eq!(idle.call("pane.dialogs", json!({"panes": [9999]}))["result"]["panes"], json!({}));
}

/// FR-4: Claude's WebFetch dialog ("Do you want to allow …") is a permission dialog too, not a finished turn (11 Nov it
/// read as working, then done, and no card came).
#[test]
fn status_blocked_dari_dialog_claude_fetch() {
    let (_d, mut ui, pane) = pane_with_screen("claude-dialog", "tanya", "claude-2.1.311-fetch-esc");
    wait_blocked(&mut ui, pane, "dialog izin Claude");
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
         '{g}' hook codex notify '{{\"type\":\"agent-turn-complete\",\"last-assistant-message\":\"JAWABAN-OTOMATIS   selesai\"}}'; sleep 30",
        tok.display()
    );
    let kirim = format!(
        "sleep 2; id=$('{g}' send terima-1 'cek ini' --queue | sed -n 's/.*\"id\":\"\\([^\"]*\\)\".*/\\1/p'); \
         '{g}' wait --reply \"$id\" --timeout 20; echo SELESAI-66; sleep 30"
    );
    profiles(&d, &mut ui, &(sh_profile("terima", &terima) + &sh_profile("kirim", &kirim)), "SELESAI-66");
    let recipient = ui.call("pane.create", json!({"profile": "terima", "workspace": "w"}))["result"]["pane"].as_u64().unwrap();
    let a = ui.call("pane.create", json!({"profile": "kirim", "workspace": "w"}))["result"]["pane"].as_u64().unwrap();
    let out = ui.attach_until(a, &["SELESAI-66"]);
    assert!(out.contains("JAWABAN-OTOMATIS"), "{out}");
    assert_eq!(ui.call("pane.info", json!({"pane": recipient}))["result"]["last"], "JAWABAN-OTOMATIS selesai");
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

/// The stdin sentinel is not part of the JSON: session and answer reach the daemon intact.
#[test]
fn commandcode_hook_stdin_session() {
    let d = start("commandcode-hook");
    let mut ui = Cli::ui(&d);
    let g = env!("CARGO_BIN_EXE_gembala");
    let script = format!(
        "printf '%s' '{{\"session_id\":\"12345678-abcd\",\"last_assistant_message\":\"Done\"}}' | '{g}' hook commandcode end -; echo COMMANDCODE-HOOK; sleep 30"
    );
    profiles(&d, &mut ui, &sh_profile("commandcode-test", &script), "COMMANDCODE-HOOK");
    let p = ui.call("pane.create", json!({"profile": "commandcode-test", "workspace": "w"}))["result"]["pane"].as_u64().unwrap();
    ui.attach_until(p, &["COMMANDCODE-HOOK"]);
    assert_eq!(ui.call("pane.info", json!({"pane": p}))["result"]["session"], "12345678-abcd");
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
    let d = start_with_agents("h5");
    let mut ui = Cli::ui(&d);
    profiles(&d, &mut ui, &sh_profile("cat", "stty raw -echo; cat -v"), "cat -v");
    let t = ui.call("pane.create", json!({"profile": "cat", "workspace": "w"}))["result"]["pane"].as_u64().unwrap();
    let r = ui.call("recipe.draft_new", json!({"pane": t, "id": "uji-h5", "ask": "tambah tahap test"}))["result"].clone();
    let (draft, path) = (r["draft"].as_str().unwrap().to_string(), r["path"].as_str().unwrap().to_string());
    let shown = ui.attach_until(t, &["tambah tahap test", "draft written"]);
    // GNU `cat -v` prints the "·" as M-BM-7
    assert!(shown.contains(" from user]") && shown.contains(&draft), "{shown}");
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
    // GNU `cat -v` prints the "·" as M-BM-7
    assert!(shown.contains(" from user]") && shown.contains(&draft), "{shown}");
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
fn fr75_models_cache_and_event() {
    use std::os::unix::fs::PermissionsExt;
    let bin = std::env::temp_dir().join(format!("gbl-{}-models-bin", std::process::id()));
    let _ = std::fs::remove_dir_all(&bin);
    std::fs::create_dir_all(&bin).unwrap();
    let count = bin.join("calls");
    let release = bin.join("release");
    // Version detection must not count as a model request.
    let body = format!("#!/bin/sh\nif [ \"$1\" = models ]; then\nprintf x >> '{}'\nwhile [ ! -f '{}' ]; do sleep 0.01; done\nprintf '%s' '[{{\"id\":\"--yolo\"}},{{\"id\":\"gemini-safe\",\"displayName\":\"Gemini safe\"}}]'\nelse\nprintf '0.67.0'\nfi\n", count.display(), release.display());
    std::fs::write(bin.join("gemini"), body).unwrap();
    std::fs::set_permissions(bin.join("gemini"), std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = PathBuf::from(format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default()));
    let d = start_env("models", 3600, &[("PATH", &path)]);
    let mut ui = Cli::ui(&d);
    let params = json!({"agent":"gemini", "account":"utama"});
    let first = ui.call("models.list", params.clone())["result"].clone();
    assert_eq!(first["pending"], true);
    assert_eq!(first["models"], json!([]));
    assert_eq!(ui.call("models.list", params.clone())["result"]["pending"], true);
    std::fs::write(release, "").unwrap();
    loop {
        let event = ui.recv();
        assert!(!event.is_null(), "models.changed never arrived");
        if event["event"] == "models.changed" {
            assert_eq!(event["params"], params);
            break;
        }
    }
    let cached = ui.call("models.list", params.clone())["result"].clone();
    assert_eq!(cached["source"], "cli");
    assert_eq!(cached["pending"], false);
    assert_eq!(cached["models"], json!([{"id":"gemini-safe", "label":"Gemini safe"}]));
    assert!(cached["at"].as_u64().unwrap() > 0);
    assert_eq!(ui.call("models.list", params)["result"], cached);
    assert_eq!(std::fs::read_to_string(&count).unwrap(), "x");
    assert_eq!(ui.call("models.list", json!({"agent":"shell"}))["error"]["code"], "bad_args");
    assert_eq!(ui.call("models.list", json!({"agent":"gemini", "account":"unknown"}))["error"]["code"], "no_such_account");
    assert_eq!(ui.call("models.list", json!({"agent":"claude"}))["result"]["source"], "alias");
    drop(d);
    std::fs::remove_dir_all(bin).unwrap();
}

#[test]
fn fr75_buka_dan_kirim() {
    let d = start("fr75");
    let mut ui = Cli::ui(&d);
    let g = env!("CARGO_BIN_EXE_gembala");
    let minta = format!(
        "g='{g}'; sleep 1; $g send nggakada-9 x; $g send cat BAD --model 'bad model'; $g send cat BAD --effort nonsense; \
         $g send cat TUGAS-A; $g send claude TUGAS-MODEL --model opus --effort high; r=$($g send cat TUGAS-B); \
         id=$(printf '%s' \"$r\" | sed -n 's/.*\"id\":\"\\([^\"]*\\)\".*/\\1/p'); echo DIMINTA; $g wait --reply \"$id\"; echo SELESAI-75; sleep 30"
    );
    profiles(&d, &mut ui, &(sh_profile("cat", "stty raw -echo; cat -v") + &sh_profile("minta", &minta)), "SELESAI-75");
    let k = ui.call("pane.create", json!({"profile": "minta", "workspace": "w"}))["result"]["pane"].as_u64().unwrap();
    let out = ui.attach_until(k, &["DIMINTA"]);
    assert!(out.contains("no_such_pane") && out.contains("bad_args") && out.contains("needs_open"), "{out}");
    let opens = ui.call("msg.list", json!({}))["result"]["opens"].clone();
    assert_eq!(opens.as_array().unwrap().len(), 3, "{opens}");
    assert_eq!((opens[0]["profile"].as_str(), opens[0]["from_pane"].as_u64(), opens[0]["body"].as_str()), (Some("cat"), Some(k), Some("TUGAS-A")), "{opens}");
    assert_eq!((opens[1]["model"].as_str(), opens[1]["effort"].as_str()), (Some("opus"), Some("high")), "{opens}");
    let (a, model, b) = (opens[0]["id"].as_str().unwrap().to_string(), opens[1]["id"].as_str().unwrap().to_string(), opens[2]["id"].as_str().unwrap().to_string());
    let t = ui.call("pane.create", json!({"profile": "cat", "workspace": "w"}))["result"]["pane"].as_u64().unwrap();
    assert_eq!(ui.call("open.approve", json!({"id": a, "pane": t}))["result"]["result"], "queued");
    assert_eq!(ui.call("open.reject", json!({"id": b}))["result"]["reason"], "rejected_by_user");
    assert_eq!(ui.call("open.reject", json!({"id": model}))["result"]["reason"], "rejected_by_user");
    assert_eq!(ui.call("msg.list", json!({}))["result"]["opens"], json!([]));
    let mut ui2 = Cli::ui(&d);
    let done = ui2.attach_until(k, &["SELESAI-75"]);
    assert!(done.contains("cat was not opened: rejected_by_user"), "{done}");
    let mut ui3 = Cli::ui(&d);
    ui3.attach_until(t, &["TUGAS-A^M"]);
}

/// SEC-5 exception: the owner's later messages reach its helper (opened through an approved card) without a card, even when
/// the helper skips permission prompts (`--yolo` = forced confirm); anyone else sending to that helper still gets the card.
#[test]
fn sec5_pemilik_ke_pembantu_tanpa_kartu() {
    let d = start("sec5");
    let mut ui = Cli::ui(&d);
    let g = env!("CARGO_BIN_EXE_gembala");
    let toml = sh_profile("nekat", "stty raw -echo; exec perl -e '$|=1; while (sysread(STDIN, $b, 1)) { print $b }' -- --yolo")
        + &sh_profile("bos", &format!("g='{g}'; sleep 1; $g send nekat TUGAS-A; echo DIMINTA; sleep 7; $g send nekat-1 TUGAS-B; echo SELESAI-B; sleep 30"))
        + &sh_profile("lain", &format!("sleep 9; '{g}' send nekat-1 DARI-LAIN; echo SELESAI-L; sleep 30"));
    profiles(&d, &mut ui, &toml, "SELESAI-L");
    let bos = ui.call("pane.create", json!({"profile": "bos", "workspace": "w"}))["result"]["pane"].as_u64().unwrap();
    ui.attach_until(bos, &["DIMINTA"]);
    let open = ui.call("msg.list", json!({}))["result"]["opens"][0]["id"].as_str().unwrap().to_string();
    let helper = ui.call("pane.create", json!({"profile": "nekat", "workspace": "w"}))["result"]["pane"].as_u64().unwrap();
    assert_eq!(ui.call("open.approve", json!({"id": open, "pane": helper}))["result"]["result"], "queued");
    let lain = ui.call("pane.create", json!({"profile": "lain", "workspace": "w"}))["result"]["pane"].as_u64().unwrap();
    let mut ui2 = Cli::ui(&d);
    let out = ui2.attach_until(bos, &["SELESAI-B"]);
    assert!(!out.contains("needs_confirmation"), "owner to its own helper got a card: {out}");
    let mut ui3 = Cli::ui(&d);
    let out = ui3.attach_until(lain, &["SELESAI-L"]);
    assert!(out.contains("needs_confirmation"), "another sender skipped the card: {out}");
    let mut ui4 = Cli::ui(&d);
    ui4.attach_until(helper, &["TUGAS-B"]);
}

#[test]
fn fr75_model_effort_diteruskan_ke_launch() {
    use std::os::unix::fs::PermissionsExt;
    let bin = std::env::temp_dir().join(format!("gbl-{}-fr75-bin", std::process::id()));
    let _ = std::fs::remove_dir_all(&bin);
    std::fs::create_dir_all(&bin).unwrap();
    let claude = bin.join("claude");
    let args = std::env::temp_dir().join(format!("gbl-{}-fr75-args", std::process::id()));
    let _ = std::fs::remove_file(&args);
    std::fs::write(&claude, format!("#!/bin/sh\nprintf '%s\\n' \"$*\" > {}\nsleep 30\n", json!(args.to_string_lossy().as_ref()))).unwrap();
    std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755)).unwrap();
    let d = start("fr75-model-flags");
    let mut ui = Cli::ui(&d);
    let config = format!("[[profile]]\nid = \"claude\"\ncommand = {}\n", json!(claude.to_string_lossy()));
    profiles(&d, &mut ui, &config, claude.to_str().unwrap());
    let created = ui.call("pane.create", json!({"profile": "claude", "model": "opus", "effort": "high"}));
    assert!(created["result"]["pane"].is_u64(), "{created}");
    let pane = created["result"]["pane"].as_u64().unwrap();
    let started = Instant::now();
    while !args.exists() {
        if started.elapsed() >= Duration::from_secs(5) {
            let screen = ui.call("pane.attach", json!({"pane": pane}));
            panic!("Claude launch args not recorded: {}", String::from_utf8_lossy(&written(&screen["result"])));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let got = std::fs::read_to_string(&args).unwrap();
    assert!(got.contains("--model opus --effort high"), "{got}");
    ui.call("pane.close", json!({"pane": pane}));
    let _ = std::fs::remove_file(args);
    let _ = std::fs::remove_dir_all(bin);
}

/// 11 Nov 2026: a profile agent that has exited is a plain shell again. `send` to the pane is rejected like a shell
/// pane (target_unknown) and not one byte is typed into zsh, `status` no longer lists the pane as a live agent, and
/// `send <profile>` asks to open a new pane. An agent typed there afterwards is tracked and messaged again.
#[test]
fn agent_profil_keluar_ditolak_lalu_kembali() {
    let d = start("agent-exit");
    let mut ui = Cli::ui(&d);
    let g = env!("CARGO_BIN_EXE_gembala");
    // the built-in codex profile stands in: the "agent" prints one line and exits at once, the pane shell comes back;
    // the sender retries until the pane takes messages again
    let kirim = format!(
        "g='{g}'; sleep 5; echo HASIL:$($g send codex-1 TUGAS-LAMA); echo DAFTAR:$($g status); echo BUKA:$($g send codex TUGAS-BARU); \
         until r=$($g send codex-1 TUGAS-BARU-LAGI); do sleep 1; done; echo KEMBALI:$r; echo SELESAI-EXIT; sleep 30"
    );
    profiles(&d, &mut ui, &(sh_profile("codex", "echo AGENT-JALAN") + &sh_profile("kirim", &kirim)), "SELESAI-EXIT");
    let k = ui.call("pane.create", json!({"profile": "kirim", "workspace": "w"}))["result"]["pane"].as_u64().unwrap();
    let c = ui.call("pane.create", json!({"profile": "codex", "workspace": "w"}))["result"]["pane"].as_u64().unwrap();
    // the fake agent exits at once; the sender's checks run ~5 s after both panes are up
    std::thread::sleep(Duration::from_secs(6));
    // not one byte of the brief reached the pane that went back to zsh (the sender's later accepted send comes after this snapshot)
    let mut ui2 = Cli::ui(&d);
    let pane = snapshot(&ui2.call("pane.attach", json!({"pane": c})));
    let pane = String::from_utf8(B64.decode(pane).unwrap()).unwrap();
    assert!(pane.contains("codex-1") && !pane.contains("TUGAS-") && !pane.contains("Gembala · from"), "{pane}");
    // recovery: an agent typed in that pane (a copy of the binary named omp, agents are recognized by process name)
    // is detected again, so the sender's retry is accepted
    let omp = d.dir.join("omp");
    std::fs::copy(env!("CARGO_BIN_EXE_gembala"), &omp).unwrap();
    std::fs::set_permissions(&omp, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    ui2.call("pane.write", json!({"pane": c, "data": B64.encode(format!("'{}' watch\r", omp.display()))}));
    let out = ui.attach_until(k, &["SELESAI-EXIT"]);
    assert!(out.contains("\"result\":\"rejected\"") && out.contains("target_unknown"), "{out}");
    assert!(out.contains("\"me\":\"kirim-1\"") && out.contains("\"panes\":[]"), "{out}");
    assert!(out.contains("needs_open"), "{out}");
    assert!(out.contains("KEMBALI:") && out.contains("\"result\":\"accepted\""), "{out}");
}

#[test]
fn fr100_lapor_masalah_cuma_draf() {
    // FR-100, SEC-40: an agent's report waits as a draft for the user (title + text, secrets masked, home = ~); nothing is sent
    let d = start("fr100");
    let mut ui = Cli::ui(&d);
    let g = env!("CARGO_BIN_EXE_gembala");
    let lapor = format!("g='{g}'; sleep 1; $g report 'Kartu izin nggak muncul' \"langkah: buka $HOME/x, token sk-ant-api03-abcdefghijklmnopqrstuvwxyz0123\" --screenshot; echo SELESAI-100; sleep 30");
    profiles(&d, &mut ui, &sh_profile("lapor", &lapor), "SELESAI-100");
    let k = ui.call("pane.create", json!({"profile": "lapor", "workspace": "w"}))["result"]["pane"].as_u64().unwrap();
    let out = ui.attach_until(k, &["SELESAI-100"]);
    assert!(out.contains("needs_review"), "{out}");
    let r = ui.call("msg.list", json!({}))["result"]["reports"][0].clone();
    assert_eq!(r["title"], "Kartu izin nggak muncul", "{r}");
    assert!(r["at"].as_u64().is_some(), "{r}");
    let body = r["body"].as_str().unwrap();
    assert!(body.contains("~/x") && !body.contains("sk-ant-api03-abcdef"), "{body}");
    let env = ui.call("report.env", json!({}))["result"].clone();
    assert!(env["daemon"].is_string() && env["log"].is_null() && env["screenshot"] == true, "{env}");
    assert_eq!(ui.call("report.env", json!({}))["result"]["screenshot"], false);
    assert_eq!(ui.call("report.discard", json!({"id": r["id"]}))["result"], json!({}));
    assert_eq!(ui.call("msg.list", json!({}))["result"]["reports"], json!([]));
}

/// FR-102: `gembala statusline` passes Claude's `rate_limits` to the daemon (numbers only) and runs the user's own
/// status line command with the same stdin, printing its output as is; `usage.list`/`usage.changed` carry one row per
/// login, Utama and an FR-61 account apart.
#[test]
fn fr102_statusline_dan_usage() {
    let home = std::env::temp_dir().join(format!("gbl-{}-home102", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(home.join(".claude")).unwrap();
    // SEC-15 exception 3: only statusLine.command may be read; the other field must make no difference
    std::fs::write(home.join(".claude/settings.json"), r#"{"statusLine":{"type":"command","command":"/bin/cat"},"env":{"RAHASIA":"x"}}"#).unwrap();
    let d = start_env("fr102", 3600, &[("HOME", &home)]);
    let mut ui = Cli::ui(&d);
    let mut events = Cli::ui(&d);
    assert_eq!(ui.call("account.create", json!({"name": "Kantor"}))["result"]["id"], "kantor");
    let pane = ui.call("pane.create", json!({"rows": 40, "cols": 240}))["result"]["pane"].as_u64().unwrap();
    std::thread::sleep(Duration::from_secs(1));
    let payload = json!({"rate_limits": {"five_hour": {"used_percentage": 14, "resets_at": 1794380400}, "seven_day": {"used_percentage": 49, "resets_at": 1794380400}}, "model": {"id": "x"}});
    let file = d.dir.join("payload.json");
    std::fs::write(&file, payload.to_string()).unwrap();
    let g = env!("CARGO_BIN_EXE_gembala");
    let run = |ui: &mut Cli, dir: Option<&std::path::Path>, sum: &str, want: &str| {
        let env = dir.map(|k| format!("CLAUDE_CONFIG_DIR='{}' ", k.display())).unwrap_or_default();
        let cmd = format!("{env}'{g}' statusline < '{}'; echo; echo SELESAI-$((80+{sum}))\r", file.display());
        ui.call("pane.write", json!({"pane": pane, "data": B64.encode(cmd)}));
        ui.attach_until(pane, &[want])
    };

    let out = run(&mut ui, None, "2", "SELESAI-82");
    assert!(out.contains(&payload.to_string()), "output status line pengguna hilang: {out}");
    let e = next_event(&mut events, "usage.changed", Duration::from_secs(5));
    assert_eq!(e["params"]["rows"][0]["agent"], "claude", "{e}");
    let row = |r: &Value, login: &str| r["rows"].as_array().unwrap().iter().find(|x| x["login"] == login).cloned().unwrap();
    let r = ui.call("usage.list", json!({}))["result"].clone();
    let utama = row(&r, "utama");
    assert_eq!(utama["login_name"], "Utama", "{utama}");
    assert_eq!(utama["source"], "claude_statusline", "{utama}");
    assert_eq!(utama["limited"], false, "{utama}");
    let w = utama["windows"].clone();
    assert_eq!(w[0], json!({"kind": "5h", "used_pct": 14.0, "resets_at": 1794380400, "model": null}), "{w}");
    assert_eq!(w[1], json!({"kind": "week", "used_pct": 49.0, "resets_at": 1794380400, "model": null}), "{w}");

    // an FR-61 account is a row of its own; its settings.json is the same link from Utama
    let k = home.join(".claude-kantor");
    let out = run(&mut ui, Some(&k), "3", "SELESAI-83");
    assert!(out.contains(&payload.to_string()), "{out}");
    let r = ui.call("usage.list", json!({}))["result"].clone();
    let kantor = row(&r, "kantor");
    assert_eq!(kantor["agent"], "claude", "{kantor}");
    assert_eq!(kantor["login_name"], "Kantor", "{kantor}");
    // numbers only reach the daemon: a pane can't put free text in a row
    let bad = d.dir.join("bad.json");
    std::fs::write(&bad, r#"{"rate_limits":{"five_hour":{"used_percentage":"banyak"},"seven_day":{"used_percentage":null}}}"#).unwrap();
    let out = {
        let cmd = format!("'{g}' statusline < '{}'; echo SELESAI-$((80+4))\r", bad.display());
        ui.call("pane.write", json!({"pane": pane, "data": B64.encode(cmd)}));
        ui.attach_until(pane, &["SELESAI-84"])
    };
    assert_eq!(ui.call("usage.list", json!({}))["result"], r, "teks tidak masuk sebagai angka: {out}");
    let _ = std::fs::remove_dir_all(&home);
}

/// FR-86: `gembala close` closes a helper opened through the caller's own card at once; any other pane waits for the
/// user's card (`needs_review`), which is the only thing that closes it.
#[test]
fn fr86_tutup_pembantu() {
    let d = start("fr86");
    let mut ui = Cli::ui(&d);
    let g = env!("CARGO_BIN_EXE_gembala");
    let go = d.dir.join("go");
    // 4 Nov: `gembala send <pane> "/exit"` was the only way to close another pane, and it types text instead of closing.
    // Now it is a card for the user; the asking agent can `wait --reply` for the outcome.
    let minta = format!(
        "g='{g}'; sleep 1; $g send cat TUGAS; echo DIMINTA; while [ ! -e '{}' ]; do sleep 0.1; done; \
         $g close nggakada-9; $g close cat-2; i3=$($g close cat-3 | sed -n 's/.*\"id\":\"\\([^\"]*\\)\".*/\\1/p'); \
         $g close cat-1; echo SELESAI-86; $g wait --reply \"$i3\" --timeout 60; echo BALAS-86; sleep 30",
        go.display()
    );
    profiles(&d, &mut ui, &(sh_profile("cat", "stty raw -echo; cat -v") + &sh_profile("minta", &minta)), "SELESAI-86");
    let k = ui.call("pane.create", json!({"profile": "minta", "workspace": "w"}))["result"]["pane"].as_u64().unwrap();
    ui.attach_until(k, &["DIMINTA"]);
    let id = ui.call("msg.list", json!({}))["result"]["opens"][0]["id"].as_str().unwrap().to_string();
    let helper = ui.call("pane.create", json!({"profile": "cat", "workspace": "w"}))["result"]["pane"].as_u64().unwrap();
    let other = ui.call("pane.create", json!({"profile": "cat", "workspace": "w"}))["result"]["pane"].as_u64().unwrap();
    let third = ui.call("pane.create", json!({"profile": "cat", "workspace": "w"}))["result"]["pane"].as_u64().unwrap();
    assert_eq!(ui.call("open.approve", json!({"id": id, "pane": helper}))["result"]["result"], "queued");
    std::fs::write(&go, "").unwrap();
    let mut ui2 = Cli::ui(&d);
    let out = ui2.attach_until(k, &["SELESAI-86"]);
    // 4 Nov: a pane that isn't my helper is no longer refused, it waits for the user's card
    assert!(out.contains("no_such_pane") && out.contains("needs_review") && out.contains("\"closed\""), "{out}");
    let t0 = Instant::now();
    while ui.call("pane.list", json!({}))["result"].as_array().unwrap().contains(&json!(helper)) {
        assert!(t0.elapsed() < Duration::from_secs(5), "helper still open");
        std::thread::sleep(Duration::from_millis(100));
    }
    // the other two wait as cards: the target's status rides along and nothing was closed yet
    let closes = ui.call("msg.list", json!({}))["result"]["closes"].clone();
    let cards: Vec<(&str, &str)> = closes.as_array().into_iter().flatten().map(|c| (c["target"].as_str().unwrap_or(""), c["from"].as_str().unwrap_or(""))).collect();
    assert_eq!(cards, [("cat-2", "minta-1"), ("cat-3", "minta-1")], "{closes}");
    // approve = the user's ×: the pane goes; reject = it stays
    assert_eq!(ui.call("close.approve", json!({"id": closes[0]["id"]}))["result"]["result"], "closed");
    assert_eq!(ui.call("close.reject", json!({"id": closes[1]["id"]}))["result"]["result"], "rejected");
    assert_eq!(ui.call("close.reject", json!({"id": closes[1]["id"]}))["error"]["code"], "no_such_msg");
    assert_eq!(ui.call("msg.list", json!({}))["result"]["closes"], json!([]));
    let t0 = Instant::now();
    while ui.call("pane.list", json!({}))["result"].as_array().unwrap().contains(&json!(other)) {
        assert!(t0.elapsed() < Duration::from_secs(5), "approved pane still open");
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(ui.call("pane.list", json!({}))["result"].as_array().unwrap().contains(&json!(third)), "rejected pane was closed");
    // the asking agent's `wait --reply` gets the card's answer (it asked before the user decided)
    let out = ui2.attach_until(k, &["BALAS-86"]);
    assert!(out.contains("cat-3 was not closed: rejected_by_user"), "{out}");
}

/// FR-101: a helper may answer its owner, but cannot request another helper.
#[test]
fn fr101_pembantu_tidak_bisa_buka_pembantu() {
    let d = start("fr101");
    let mut ui = Cli::ui(&d);
    let g = env!("CARGO_BIN_EXE_gembala");
    let owner = format!("sleep 1; '{g}' send helper TUGAS; echo DIMINTA-101; sleep 30");
    let helper = format!("sleep 2; '{g}' send other TUGAS; echo SELESAI-101; sleep 30");
    profiles(&d, &mut ui, &(sh_profile("owner", &owner) + &sh_profile("helper", &helper) + &sh_profile("other", "sleep 30")), "SELESAI-101");
    let k = ui.call("pane.create", json!({"profile": "owner", "workspace": "w"}))["result"]["pane"].as_u64().unwrap();
    ui.attach_until(k, &["DIMINTA-101"]);
    let id = ui.call("msg.list", json!({}))["result"]["opens"][0]["id"].as_str().unwrap().to_string();
    let h = ui.call("pane.create", json!({"profile": "helper", "workspace": "w"}))["result"]["pane"].as_u64().unwrap();
    assert_eq!(ui.call("open.approve", json!({"id": id, "pane": h}))["result"]["result"], "queued");
    let mut reader = Cli::ui(&d);
    let out = reader.attach_until(h, &["SELESAI-101"]);
    assert!(out.contains("helper_cant_open"), "{out}");
    assert_eq!(ui.call("msg.list", json!({}))["result"]["opens"], json!([]));
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
    let d = start_with_agents("resep");
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
    let d = start_with_agents("b1");
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
    let d = start_with_agents("harness1");
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
    let d = start_with_agents("harness3");
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
    let d = start_with_agents("harness4");
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
    let d = start_with_agents("harness2");
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
