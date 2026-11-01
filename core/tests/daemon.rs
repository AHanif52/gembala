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
    start_idle(tag, 3600)
}

fn start_idle(tag: &str, idle_secs: u64) -> Daemon {
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
    let pane = ui.call("pane.create", json!({"cmd": "env; echo CWD=$(pwd); echo SELESAI-ENV", "rows": 50, "cols": 200}))["result"]["pane"].as_u64().unwrap();
    let out = ui.attach_until(pane, &["SELESAI-ENV"]);
    assert!(out.contains("GEMBALA_SOCKET="), "{out}");
    // daemon dijalankan dari cwd `/` (seperti app dari Finder), pane tetap mulai di home
    assert!(out.contains(&format!("CWD={}", std::env::var("HOME").unwrap())), "{out}");
    for bocor in ["CLAUDECODE=", "token-bocor-uji", "thread-bocor-uji", "GEMBALA_RUNDIR="] {
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

/// FR-38 nama proses untuk konfirmasi tutup; FR-50 hentikan: TERM dulu, KILL setelah 3 detik, shell tetap.
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

    // TERM diabaikan sebelum exec, jadi sudah berlaku begitu namanya terlihat "perl"
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

/// FR-20: tanpa UI dan semua pane diam = daemon keluar sendiri; ada proses jalan = tetap hidup.
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

/// FR-46: "Keluar dan hentikan semua agent".
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

/// FR-42: config dari file, perubahan file berlaku tanpa restart, file rusak tidak menghapus nilai valid terakhir.
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
    assert!(r["error"].as_str().unwrap().contains("8–32"), "{r}");

    // diedit pengguna di editor: berlaku tanpa restart
    std::fs::write(&path, "[ui]\nfont_size = 17\ncursor = \"kotak\"\n").unwrap();
    let e = next_event(&mut ui, "config.changed", Duration::from_secs(4));
    assert_eq!(e["params"]["values"]["ui"]["font_size"], 17, "{e}");
    assert!(e["params"]["errors"][0].as_str().unwrap().contains("ui.cursor"), "{e}");

    // rusak: nilai valid terakhir tetap, galat ditandai, file tidak ditimpa
    std::fs::write(&path, "[ui\n").unwrap();
    let e = next_event(&mut ui, "config.changed", Duration::from_secs(4));
    assert_eq!(e["params"]["values"]["ui"]["font_size"], 17, "{e}");
    assert!(e["params"]["errors"][0].as_str().unwrap().contains("tidak bisa diurai"), "{e}");
    assert!(ui.call("config.set", json!({"section": "ui", "key": "font_size", "value": 14}))["result"]["error"].is_string());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "[ui\n");
}

/// Bagian 11 BRD: batas pane per daemon, bisa diatur lewat [limits].
#[test]
fn batas_jumlah_pane() {
    let d = start("max");
    let mut ui = Cli::ui(&d);
    ui.call("config.set", json!({"section": "limits", "key": "max_panes", "value": 2}));
    assert!(ui.call("pane.create", json!({}))["result"]["pane"].is_u64());
    assert!(ui.call("pane.create", json!({}))["result"]["pane"].is_u64());
    assert_eq!(ui.call("pane.create", json!({}))["error"]["code"], "max_panes");
}

/// FR-29/30: pane dari profil (nama `<agent>-<n>`, folder ikut pane fokus); profil tambahan dari config;
/// FR-32: perintah tidak ada = pesan jelas, pane tetap hidup sebagai shell.
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
    // watcher config memeriksa tiap 1 detik
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
    let out = ui.attach_until(p, &["tidak ditemukan"]);
    assert!(out.contains("Perintah gembala-tidak-ada-xyz tidak ditemukan"), "{out}");
    assert!(ui.call("pane.list", json!({}))["result"].as_array().unwrap().contains(&json!(p)), "pane tetap hidup");
    assert_eq!(ui.call("pane.create", json!({"profile": "tidak-ada"}))["error"]["code"], "no_such_profile");
}

/// FR-28: deteksi lewat login shell; tiap agent didukung punya baris.
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

/// FR-45: tata letak tersimpan (tanpa isi layar) dan id run daemon untuk membedakan pane lama.
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

/// Baris lebih dari 64 KB: ditolak dan koneksi ditutup, klien tidak menggantung.
#[test]
fn baris_terlalu_panjang_ditutup() {
    let d = start("long");
    let mut ui = Cli::ui(&d);
    let big = format!("{{\"id\":1,\"method\":\"layout.save\",\"params\":{{\"layout\":{{\"x\":\"{}\"}}}}}}\n", "x".repeat(70_000));
    let _ = ui.w.write_all(big.as_bytes()); // boleh gagal bila daemon sudah menutup
    let v = ui.recv();
    assert_eq!(v["error"]["code"], "line_too_long", "{v}");
    let mut rest = String::new();
    assert_eq!(ui.r.read_line(&mut rest).unwrap_or(0), 0, "koneksi harus ditutup");
}

/// FR-4: hook agent di dalam pane mengubah status pane itu saja; UI diberi tahu lewat `pane.status`.
#[test]
fn status_dari_hook() {
    let d = start("hook");
    let mut ui = Cli::ui(&d);
    let exe = env!("CARGO_BIN_EXE_gembala");
    // profil dengan agent "claude" supaya status dilacak; perintahnya skrip yang memanggil hook seperti Claude Code
    let cfg = d.dir.join("cfg");
    std::fs::create_dir_all(&cfg).unwrap();
    std::fs::set_permissions(&cfg, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
    // pengganti `claude`: mengabaikan flag hook yang disisipkan Gembala, memanggil hook seperti Claude Code
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
    assert_eq!(e["params"], json!({"pane": pane, "status": "working"}), "{e}");
    let e = next_event(&mut ui, "pane.status", Duration::from_secs(8));
    assert_eq!(e["params"]["status"], "blocked", "{e}");
    assert_eq!(ui.call("pane.info", json!({"pane": pane}))["result"]["status"], "blocked");
    // proses di luar pane tidak bisa mengubah status siapa pun
    let mut outsider = Cli::connect(&d, "pane.sock");
    assert_eq!(outsider.call("hello", json!({"proto": 1}))["error"]["code"], "not_a_pane");
}

/// FR-48: reset menghapus file milik Gembala saja lalu daemon keluar; file lain di folder itu tidak disentuh.
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

/// FR-3: nama pane unik dan bisa diganti.
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
    // nama otomatis berikutnya tidak bentrok dengan yang sudah dipakai
    ui.call("pane.rename", json!({"pane": b, "name": "shell-3"}));
    assert_eq!(ui.call("pane.create", json!({}))["result"]["name"], "shell-1");
}

/// FR-56: agent keluar = layar diganti blok ringkas; output lengkap tetap bisa dibuka selama pane hidup.
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
    let text = String::from_utf8(B64.decode(e["params"]["data"].as_str().unwrap()).unwrap()).unwrap();
    assert!(text.contains("palsu-1 selesai") && text.contains("keluar 3"), "{text}");
    let mut ui2 = Cli::ui(&d);
    let snap = ui2.call("pane.attach", json!({"pane": pane}))["result"]["snapshot"].as_str().unwrap().to_string();
    let snap = String::from_utf8(B64.decode(snap).unwrap()).unwrap();
    assert!(snap.contains("selesai") && !snap.contains("OUTPUT-AGENT-PANJANG"), "{snap}");
    let full = ui.call("pane.agent_output", json!({"pane": pane}))["result"]["data"].as_str().unwrap().to_string();
    assert!(String::from_utf8_lossy(&B64.decode(full).unwrap()).contains("OUTPUT-AGENT-PANJANG"));
}

/// FR-40: pane yang menunggu password (kanonik + echo mati) terdeteksi; proses biasa tidak.
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

/// FR-4 cadangan: agent tanpa hook (profil custom) dibaca dari layar.
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
