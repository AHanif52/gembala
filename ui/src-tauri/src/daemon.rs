//! Klien kanal UI (TECH bagian 8, T1): rahasia dari Keychain; bila daemon belum jalan,
//! UI menjalankannya dan menerima rahasia lewat descriptor 3.

use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::{self, BufRead, BufReader, Write};
use std::os::fd::{FromRawFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex};
use std::thread;

pub const KC_SERVICE: &str = "dev.gembala.app";

pub struct Client {
    /// Daemon putus = false; pemanggil membuat klien baru (kriteria 17: daemon dijalankan lagi).
    pub alive: Arc<std::sync::atomic::AtomicBool>,
    /// SEC-20: daemon mengenali UI ini dari tanda tangan kodenya (rahasia tidak dipakai)
    pub signed: bool,
    w: Mutex<UnixStream>,
    pending: Arc<Mutex<HashMap<u64, Sender<Value>>>>,
    next: Mutex<u64>,
}

fn err(msg: impl Into<String>) -> io::Error {
    io::Error::other(msg.into())
}

pub fn rundir() -> PathBuf {
    std::env::var_os("GEMBALA_RUNDIR").map(PathBuf::from).unwrap_or_else(|| {
        std::env::var_os("TMPDIR").map(PathBuf::from).unwrap_or_else(|| "/tmp".into()).join("gembala")
    })
}

pub type OnEvent = Arc<dyn Fn(Value) + Send + Sync>;

/// Sambung ke daemon; jalankan daemon bila belum ada. `on_event` menerima event daemon.
pub fn connect(on_event: OnEvent) -> io::Result<Client> {
    connect_try(on_event, true)
}

fn connect_try(on_event: OnEvent, retry: bool) -> io::Result<Client> {
    let dir = rundir();
    let sock = dir.join("ui.sock");
    // satu rahasia per folder run: dev, uji, dan app terpasang tidak saling tindih
    let account = format!("ui-secret:{}", dir.display());
    // Hanya build debug (alat ukur, uji): rahasia dari env, tanpa Keychain. Build rilis tidak punya
    // jalur ini, karena SEC-20 melarang rahasia kanal UI ada di environment.
    #[cfg(debug_assertions)]
    let dev = std::env::var("GEMBALA_DEV_SECRET").ok();
    #[cfg(not(debug_assertions))]
    let dev: Option<String> = None;
    if let (Some(secret), Ok(s)) = (&dev, UnixStream::connect(&sock)) {
        return Client::open(s, secret, on_event);
    }
    // socket dulu: Keychain hanya disentuh bila perlu. Item Keychain app self-signed terikat ke cdhash build,
    // jadi membaca/menulis item build lama memunculkan dialog izin macOS tiap update.
    let Ok(s) = UnixStream::connect(&sock) else {
        let secret = spawn_daemon()?;
        let c = Client::open(UnixStream::connect(&sock)?, &secret, on_event)?;
        // SEC-20: daemon mengenali UI dari tanda tangannya = Keychain tidak perlu. Build tanpa sertifikat:
        // rahasia disimpan untuk sambung ulang; gagal simpan tidak menggagalkan sesi ini.
        // Alat ukur/uji (build debug + GEMBALA_DEV_SECRET) tidak menulis Keychain pengguna.
        if !c.signed
            && dev.is_none()
            && let Err(e) = security_framework::passwords::set_generic_password(KC_SERVICE, &account, secret.as_bytes())
        {
            eprintln!("simpan rahasia ke Keychain: {e}");
        }
        return Ok(c);
    };
    // daemon yang sedang berhenti sendiri (FR-20: tanpa UI, pane diam) menutup socket di tengah hello:
    // tunggu dia keluar, lalu sambung atau jalankan daemon baru
    let closing = |e: &io::Error| matches!(e.kind(), io::ErrorKind::BrokenPipe | io::ErrorKind::ConnectionReset | io::ErrorKind::UnexpectedEof);
    // SEC-20: tanpa rahasia dulu (dikenali dari tanda tangan); Keychain hanya cadangan untuk daemon lama/tanpa sertifikat
    match Client::open(s, "", on_event.clone()) {
        Ok(c) => return Ok(c),
        Err(e) if closing(&e) && retry => {
            thread::sleep(std::time::Duration::from_millis(500));
            return connect_try(on_event, false);
        }
        Err(e) if closing(&e) => return Err(e),
        Err(_) => {} // ditolak: coba rahasia dari Keychain
    }
    let s = UnixStream::connect(&sock)?;
    let kc = security_framework::passwords::get_generic_password(KC_SERVICE, &account).ok();
    match kc.and_then(|s| String::from_utf8(s).ok()) {
        // ponytail: daemon hidup tapi rahasia Keychain basi/tidak ada; reset otomatis di FR-49 (Fase 1)
        Some(secret) => match Client::open(s, &secret, on_event.clone()) {
            Err(e) if retry && closing(&e) => {
                thread::sleep(std::time::Duration::from_millis(500));
                connect_try(on_event, false)
            }
            r => r.map_err(|e| err(format!("{e}; hentikan daemon lama dengan: pkill -f 'gembala daemon'"))),
        },
        None => Err(err("daemon jalan tapi rahasia tidak ada di Keychain; hentikan dengan: pkill -f 'gembala daemon'")),
    }
}

/// Binary daemon: helper app di dalam bundle (rilis), atau `gembala` di samping binary app (dev: `target/debug`).
pub fn daemon_exe() -> Option<PathBuf> {
    let me = std::env::current_exe().ok()?;
    let helper = me.parent()?.parent()?.join("Helpers/Gembala Daemon.app/Contents/MacOS/gembala");
    Some(if helper.exists() { helper } else { me.with_file_name("gembala") })
}

/// FR-20: daemon dijalankan LaunchServices (`open`) sebagai app latar sendiri. Proses yang dijalankan app
/// (setsid, double-fork, disclaim, bahkan job `launchctl bootstrap`) masuk atau teratribusi ke coalition app,
/// dan saat app ditutup loginwindow menghentikan semuanya, termasuk shell pane dan agent (uji 2 Nov 2026).
/// Tanpa helper (dev) atau `open` gagal = anak langsung seperti dulu, dengan batas itu.
fn spawn_daemon() -> io::Result<String> {
    let exe = daemon_exe().ok_or_else(|| err("path app tidak terbaca"))?;
    let helper = exe.ancestors().nth(3).filter(|p| p.extension().is_some_and(|e| e == "app")).map(|p| p.to_path_buf());
    match helper {
        Some(app) => spawn_helper(&app, &exe).or_else(|e| {
            eprintln!("helper daemon: {e}; daemon dijalankan langsung");
            spawn_child(&exe)
        }),
        None => spawn_child(&exe),
    }
}

/// Rahasia lewat `hand.sock` di folder run (`0700`): daemon hanya menulis bila pemilik socket `gembala-ui`
/// bundle ini, dan UI hanya menerima dari binary daemon bundle ini.
fn spawn_helper(app: &std::path::Path, exe: &std::path::Path) -> io::Result<String> {
    use std::os::unix::fs::DirBuilderExt;
    use std::time::{Duration, Instant};
    let dir = rundir();
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&dir)?;
    let hand = dir.join("hand.sock");
    let _ = std::fs::remove_file(&hand);
    let l = std::os::unix::net::UnixListener::bind(&hand)?;
    let mut c = Command::new("/usr/bin/open");
    c.args(["-g", "-n", "--stderr"]).arg(dir.join("daemon.log"));
    // lingkungan app diteruskan seperti saat daemon jadi anak app (pane mewarisinya)
    for (k, v) in std::env::vars_os() {
        let (Some(k), Some(v)) = (k.to_str(), v.to_str()) else { continue };
        if !k.starts_with("XPC_") && !k.starts_with("__CF") {
            c.arg("--env").arg(format!("{k}={v}"));
        }
    }
    c.arg(app).args(["--args", "daemon", "--secret-sock"]).arg(&hand);
    let out = c.output()?;
    if !out.status.success() {
        return Err(err(format!("open: {}", String::from_utf8_lossy(&out.stderr).trim())));
    }
    l.set_nonblocking(true)?;
    let deadline = Instant::now() + Duration::from_secs(10);
    let s = loop {
        match l.accept() {
            Ok((s, _)) => break s,
            Err(e) if e.kind() == io::ErrorKind::WouldBlock && Instant::now() < deadline => thread::sleep(Duration::from_millis(50)),
            Err(e) => return Err(err(format!("daemon tidak menyerahkan rahasia: {e}"))),
        }
    };
    let _ = std::fs::remove_file(&hand);
    s.set_nonblocking(false)?;
    s.set_read_timeout(Some(Duration::from_secs(5)))?;
    if peer_path(&s) != exe.canonicalize().ok() {
        return Err(err("serah-terima rahasia dari proses selain daemon Gembala"));
    }
    let mut secret = String::new();
    BufReader::new(s).read_line(&mut secret)?;
    let secret = secret.trim().to_string();
    if secret.is_empty() {
        return Err(err("daemon gagal start (daemon lain masih jalan?)"));
    }
    Ok(secret)
}

/// Path binary proses di ujung lain socket.
fn peer_path(s: &UnixStream) -> Option<PathBuf> {
    use std::os::fd::AsRawFd;
    use std::os::unix::ffi::OsStrExt;
    let mut pid: libc::pid_t = 0;
    let mut len = std::mem::size_of::<libc::pid_t>() as libc::socklen_t;
    if unsafe { libc::getsockopt(s.as_raw_fd(), libc::SOL_LOCAL, libc::LOCAL_PEERPID, (&raw mut pid).cast(), &mut len) } != 0 {
        return None;
    }
    let mut buf = [0u8; 4 * libc::MAXPATHLEN as usize];
    let n = unsafe { libc::proc_pidpath(pid, buf.as_mut_ptr().cast(), buf.len() as u32) };
    let p = PathBuf::from(std::ffi::OsStr::from_bytes(&buf[..usize::try_from(n).ok().filter(|&n| n > 0)?]));
    p.canonicalize().ok()
}

/// Cara lama: anak langsung app, rahasia lewat descriptor 3.
fn spawn_child(exe: &std::path::Path) -> io::Result<String> {
    let mut fds = [0; 2];
    if unsafe { libc::pipe(fds.as_mut_ptr()) } == -1 {
        return Err(io::Error::last_os_error());
    }
    let (r, w) = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
    for fd in fds {
        unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
    }
    let wfd = fds[1];
    let mut c = Command::new(exe);
    c.args(["daemon", "--secret-fd", "3"]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    unsafe {
        c.pre_exec(move || {
            libc::setsid();
            if libc::dup2(wfd, 3) == -1 { Err(io::Error::last_os_error()) } else { Ok(()) }
        });
    }
    c.spawn().map_err(|e| err(format!("jalankan {}: {e}", exe.display())))?;
    drop(c);
    drop(w);
    let mut secret = String::new();
    BufReader::new(std::fs::File::from(r)).read_line(&mut secret)?;
    let secret = secret.trim().to_string();
    if secret.is_empty() {
        return Err(err("daemon gagal start (daemon lain masih jalan?)"));
    }
    Ok(secret)
}

impl Client {
    fn open(s: UnixStream, secret: &str, on_event: OnEvent) -> io::Result<Client> {
        let mut r = BufReader::new(s.try_clone()?);
        let mut w = s;
        let hello = json!({"id": 0, "method": "hello", "params": {"proto": 1, "client": "ui", "secret": secret}});
        writeln!(w, "{hello}")?;
        let mut l = String::new();
        if r.read_line(&mut l)? == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into()); // daemon menutup socket sebelum menjawab
        }
        let v: Value = serde_json::from_str(&l).unwrap_or(Value::Null);
        if v.get("result").is_none() {
            return Err(err(format!("hello ditolak: {}", v["error"]["code"])));
        }
        let pending: Arc<Mutex<HashMap<u64, Sender<Value>>>> = Arc::default();
        let p2 = pending.clone();
        let alive = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let alive2 = alive.clone();
        thread::spawn(move || {
            for l in r.lines() {
                let Ok(v) = serde_json::from_str::<Value>(&l.unwrap_or_default()) else { break };
                match v["id"].as_u64() {
                    Some(id) => {
                        if let Some(tx) = p2.lock().unwrap().remove(&id) {
                            let _ = tx.send(v);
                        }
                    }
                    None => on_event(v),
                }
            }
            // panggilan yang masih menunggu dibatalkan, bukan menggantung selamanya
            alive2.store(false, std::sync::atomic::Ordering::Relaxed);
            p2.lock().unwrap().clear();
            on_event(json!({"event": "daemon.lost"}));
        });
        let signed = v["result"]["signed"].as_bool() == Some(true);
        Ok(Client { alive, signed, w: Mutex::new(w), pending, next: Mutex::new(1) })
    }

    pub fn call(&self, method: &str, params: Value) -> Result<Value, String> {
        let id = {
            let mut n = self.next.lock().unwrap();
            *n += 1;
            *n
        };
        let (tx, rx) = channel();
        self.pending.lock().unwrap().insert(id, tx);
        let req = json!({"id": id, "method": method, "params": params});
        writeln!(self.w.lock().unwrap(), "{req}").map_err(|e| e.to_string())?;
        let v = rx.recv().map_err(|_| "daemon putus".to_string())?;
        match v.get("result") {
            Some(r) => Ok(r.clone()),
            None => Err(v["error"]["code"].as_str().unwrap_or("error").to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Daemon yang sedang berhenti menutup socket sebelum menjawab hello: error harus jenis yang memicu sambung ulang.
    #[test]
    fn hello_putus_bisa_diulang() {
        let (a, b) = UnixStream::pair().unwrap();
        let t = thread::spawn(move || {
            let mut l = String::new();
            let _ = BufReader::new(&b).read_line(&mut l); // terima hello, lalu tutup tanpa jawab
        });
        let e = Client::open(a, "x", Arc::new(|_| {})).err().unwrap();
        t.join().unwrap();
        assert!(matches!(e.kind(), io::ErrorKind::BrokenPipe | io::ErrorKind::ConnectionReset | io::ErrorKind::UnexpectedEof), "{e:?}");
    }
}
