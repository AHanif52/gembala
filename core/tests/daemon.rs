//! Uji daemon nyata lewat socket (TECH bagian 16): kanal UI, attach, Kriteria 9 BRD.

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
    let dir = std::env::temp_dir().join(format!("gbl-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::DirBuilder::new().mode(0o700).create(&dir).unwrap();
    let mut fds = [0; 2];
    assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
    let (r, w) = (fds[0], fds[1]);
    let mut c = Command::new(env!("CARGO_BIN_EXE_gembala"));
    c.args(["daemon", "--secret-fd", "3"])
        .env("GEMBALA_RUNDIR", &dir)
        // seolah Gembala dijalankan dari dalam sesi agent (SEC-35)
        .env("CLAUDECODE", "1")
        .env("CLAUDE_CODE_MESSAGING_TOKEN", "token-bocor-uji")
        .env("CODEX_THREAD_ID", "thread-bocor-uji");
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
        self.r.read_line(&mut l).unwrap();
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

    /// Kumpulkan output pane (snapshot + event) sampai `want` muncul atau pane keluar.
    fn attach_until(&mut self, pane: u64, want: &[&str]) -> String {
        let r = self.call("pane.attach", json!({"pane": pane}));
        let mut out = B64.decode(r["result"]["snapshot"].as_str().unwrap()).unwrap();
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
    // proses di luar pane tidak dikenali kanal pane
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
    // UI kedua attach ulang: snapshot sudah memuat output lama
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

/// Kriteria 9: skrip di dalam pane, bahkan yang memegang rahasia UI, tidak bisa memakai kanal UI;
/// lewat kanal pane, aksi manusia ditolak.
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

/// Klien yang attach tapi tidak membaca diputus saat antreannya penuh; pane tetap jalan sampai selesai.
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
            break; // pane selesai; daemon tidak tertahan oleh klien lambat
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

/// SEC-35: env pane tidak membawa penanda sesi agent dari proses yang menjalankan daemon.
#[test]
fn env_pane_bersih() {
    let d = start("env");
    let mut ui = Cli::ui(&d);
    let pane = ui.call("pane.create", json!({"cmd": "env; echo SELESAI-ENV", "rows": 50, "cols": 200}))["result"]["pane"].as_u64().unwrap();
    let out = ui.attach_until(pane, &["SELESAI-ENV"]);
    assert!(out.contains("GEMBALA_SOCKET="), "{out}");
    for bocor in ["CLAUDECODE=", "token-bocor-uji", "thread-bocor-uji", "GEMBALA_RUNDIR="] {
        assert!(!out.contains(bocor), "{bocor} bocor ke pane");
    }
}
