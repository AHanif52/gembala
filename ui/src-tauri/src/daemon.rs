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

const KC_SERVICE: &str = "dev.gembala.app";
const KC_ACCOUNT: &str = "ui-secret";

pub struct Client {
    w: Mutex<UnixStream>,
    pending: Arc<Mutex<HashMap<u64, Sender<Value>>>>,
    next: Mutex<u64>,
}

fn err(msg: impl Into<String>) -> io::Error {
    io::Error::other(msg.into())
}

fn rundir() -> PathBuf {
    std::env::var_os("GEMBALA_RUNDIR").map(PathBuf::from).unwrap_or_else(|| {
        std::env::var_os("TMPDIR").map(PathBuf::from).unwrap_or_else(|| "/tmp".into()).join("gembala")
    })
}

pub type OnEvent = Arc<dyn Fn(Value) + Send + Sync>;

/// Sambung ke daemon; jalankan daemon bila belum ada. `on_event` menerima event daemon.
pub fn connect(on_event: OnEvent) -> io::Result<Client> {
    let sock = rundir().join("ui.sock");
    let kc = security_framework::passwords::get_generic_password(KC_SERVICE, KC_ACCOUNT).ok();
    let secret = kc.and_then(|s| String::from_utf8(s).ok());
    match (secret, UnixStream::connect(&sock)) {
        (Some(secret), Ok(s)) => Client::open(s, &secret, on_event).map_err(|e| {
            // ponytail: daemon hidup tapi rahasia Keychain basi; reset otomatis di FR-49 (Fase 1)
            err(format!("{e}; hentikan daemon lama dengan: pkill -f 'gembala daemon'"))
        }),
        (None, Ok(_)) => Err(err("daemon jalan tapi rahasia tidak ada di Keychain; hentikan dengan: pkill -f 'gembala daemon'")),
        (_, Err(_)) => {
            let secret = spawn_daemon()?;
            security_framework::passwords::set_generic_password(KC_SERVICE, KC_ACCOUNT, secret.as_bytes())
                .map_err(|e| err(format!("simpan rahasia ke Keychain: {e}")))?;
            Client::open(UnixStream::connect(&sock)?, &secret, on_event)
        }
    }
}

/// `gembala` dicari di folder yang sama dengan binary app (dev: `target/debug`, bundle: sidecar).
fn spawn_daemon() -> io::Result<String> {
    let exe = std::env::current_exe()?.with_file_name("gembala");
    let mut fds = [0; 2];
    if unsafe { libc::pipe(fds.as_mut_ptr()) } == -1 {
        return Err(io::Error::last_os_error());
    }
    let (r, w) = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
    for fd in fds {
        unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
    }
    let wfd = fds[1];
    let mut c = Command::new(&exe);
    c.args(["daemon", "--secret-fd", "3"]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    unsafe {
        c.pre_exec(move || {
            // sesi sendiri: daemon tetap hidup saat app ditutup (FR-20)
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
        r.read_line(&mut l)?;
        let v: Value = serde_json::from_str(&l).unwrap_or(Value::Null);
        if v.get("result").is_none() {
            return Err(err(format!("hello ditolak: {}", v["error"]["code"])));
        }
        let pending: Arc<Mutex<HashMap<u64, Sender<Value>>>> = Arc::default();
        let p2 = pending.clone();
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
            on_event(json!({"event": "daemon.lost"}));
        });
        Ok(Client { w: Mutex::new(w), pending, next: Mutex::new(1) })
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
