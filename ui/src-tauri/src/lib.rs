// UI hanya klien daemon (BRD R-5): jangan buka PTY di sini.
mod daemon;
mod i18n;

use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use tauri::{Emitter, Manager};

struct Daemon(Mutex<Option<Arc<daemon::Client>>>);

/// Webview hanya boleh memanggil method pane; aksi manusia lain ditambah per fitur.
const ALLOWED: &[&str] =
    &["pane.list", "pane.create", "pane.write", "pane.resize", "pane.attach", "pane.close", "pane.info", "pane.kill", "daemon.stop", "config.get", "config.set", "agents.detect", "daemon.info", "layout.load", "layout.save", "daemon.reset", "git.status", "git.diff", "pane.rename", "pane.agent_output", "profile.save", "profile.reset", "keys.set",
      "account.list", "account.create", "account.remove", "account.update", "git.worktree_add", "git.worktree_remove", "git.turns", "git.turn_diff", "git.turn_preview", "git.turn_restore", "git.file_preview", "git.file_restore", "git.file_commit", "ctx.compose", "msg.list", "msg.approve", "msg.reject", "msg.pause", "msg.mute", "msg.send", "pane.policy", "pane.readable", "pane.workspace",
      "addon.plugin", "addon.mcp.get", "addon.mcp.set", "addon.status",
      "recipe.list", "recipe.get", "recipe.approve", "recipe.delete", "recipe.validate",
      "harness.preflight", "harness.start", "harness.state", "harness.stop", "harness.gate", "harness.takeover", "harness.release"];

/// Pintasan pane (FR-37, 38, 39, 50), default ala Warp. Id = aksi yang dijalankan webview; label di i18n.rs.
const PANE_KEYS: &[(&str, &str)] = &[
    ("pane.split.right", "Cmd+D"),
    ("pane.split.down", "Cmd+Shift+D"),
    ("pane.focus.left", "Cmd+Alt+Left"),
    ("pane.focus.right", "Cmd+Alt+Right"),
    ("pane.focus.up", "Cmd+Alt+Up"),
    ("pane.focus.down", "Cmd+Alt+Down"),
    ("pane.resize.left", "Ctrl+Cmd+Left"),
    ("pane.resize.right", "Ctrl+Cmd+Right"),
    ("pane.resize.up", "Ctrl+Cmd+Up"),
    ("pane.resize.down", "Ctrl+Cmd+Down"),
    ("pane.swap.left", "Cmd+Shift+Alt+Left"),
    ("pane.swap.right", "Cmd+Shift+Alt+Right"),
    ("pane.swap.up", "Cmd+Shift+Alt+Up"),
    ("pane.swap.down", "Cmd+Shift+Alt+Down"),
    ("pane.zoom.toggle", "Cmd+Shift+Enter"),
    ("pane.move.newtab", ""),
    ("pane.input.rich", "Cmd+G"),
    ("pane.diff.toggle", "Cmd+Shift+="), // = Cmd+Shift++ bawaan Warp (tombol yang sama)
    ("pane.proc.kill", "Cmd+."),
    ("pane.proc.close", "Cmd+W"),
];

/// Menu "Tab" (FR-52, FR-39 default Warp).
const TAB_KEYS: &[(&str, &str)] = &[
    ("tab.new", "Cmd+T"),
    ("tab.prev", "Cmd+Shift+["),
    ("tab.next", "Cmd+Shift+]"),
    ("tab.goto.1", "Cmd+1"),
    ("tab.goto.2", "Cmd+2"),
    ("tab.goto.3", "Cmd+3"),
    ("tab.goto.4", "Cmd+4"),
    ("tab.goto.5", "Cmd+5"),
    ("tab.goto.6", "Cmd+6"),
    ("tab.goto.7", "Cmd+7"),
    ("tab.goto.8", "Cmd+8"),
    ("tab.goto.9", "Cmd+9"),
    ("tab.sidebar", "Cmd+B"),
    ("tab.search", "Cmd+P"),
    ("pane.attention.next", "Cmd+Shift+U"), // FR-65
];

/// Menu "Pesan" (Fase 2: FR-10 sampai 12, SEC-10). ⌘K dan ⌘. sudah dipakai Fase 1 (TECH, keputusan Fase 2).
const MSG_KEYS: &[(&str, &str)] = &[
    ("msg.compose", "Cmd+Shift+K"),
    ("msg.drawer", "Cmd+J"),
    ("msg.stop", "Cmd+Shift+."),
];

/// Menu "View" (FR-34, FR-36).
const VIEW_KEYS: &[(&str, &str)] = &[
    ("term.find", "Cmd+F"),
    ("term.clear", "Cmd+K"),
    ("term.block.prev", "Cmd+Up"),
    ("term.block.next", "Cmd+Down"),
    ("term.zoom.in", "Cmd+="),
    ("term.zoom.out", "Cmd+-"),
    ("term.zoom.reset", "Cmd+0"),
    ("term.zoom.pane_in", "Alt+Cmd+="),
    ("term.zoom.pane_out", "Alt+Cmd+-"),
    ("term.zoom.pane_reset", "Alt+Cmd+0"),
];

/// SEC-14: hanya http/https, tanpa spasi atau karakter kontrol; dibuka lewat `open`, bukan shell.
fn url_ok(url: &str) -> bool {
    (url.starts_with("https://") || url.starts_with("http://")) && !url.chars().any(|c| c.is_whitespace() || c.is_control())
}

#[tauri::command]
fn open_url(url: String) -> Result<(), String> {
    if !url_ok(&url) {
        return Err("url_not_http".into());
    }
    std::process::Command::new("/usr/bin/open").arg(&url).spawn().map(drop).map_err(|e| e.to_string())
}

/// Folder di sebelah config.toml yang boleh dibuka dari Finder (Pengaturan > Keamanan > log audit).
/// `sub` adalah nama folder dari daftar ini, bukan path, jadi webview tidak bisa meminta folder lain.
const REVEAL: &[&str] = &["", "audit", "hooks", "history"];

/// Tujuan tombol "Buka": folder di sebelah config.toml, hanya untuk nama di REVEAL.
fn reveal_target(config: &str, sub: &str) -> Result<std::path::PathBuf, String> {
    if !REVEAL.contains(&sub) {
        return Err("dir_not_allowed".into());
    }
    Ok(std::path::Path::new(config).with_file_name(sub))
}

#[tauri::command]
fn reveal_dir(config: String, sub: String) -> Result<(), String> {
    let dir = reveal_target(&config, &sub)?;
    std::process::Command::new("/usr/bin/open").arg(&dir).spawn().map(drop).map_err(|e| e.to_string())
}

#[tauri::command]
async fn daemon_call(app: tauri::AppHandle, method: String, params: Value) -> Result<Value, String> {
    if !ALLOWED.contains(&method.as_str()) {
        return Err("method_not_allowed".into());
    }
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<Daemon>();
        let app2 = app.clone();
        let c = {
            let mut slot = st.0.lock().unwrap();
            match slot.as_ref().filter(|c| c.alive.load(std::sync::atomic::Ordering::Relaxed)) {
                Some(c) => c.clone(),
                // belum tersambung, atau daemon mati: sambung/jalankan lagi (kriteria 17)
                None => {
                    let emit: daemon::OnEvent = Arc::new(move |v| {
                        if v["event"] == "config.changed" {
                            apply_keys(&app2, &v["params"]["values"]);
                        }
                        let _ = app2.emit("daemon", v);
                    });
                    let c = Arc::new(daemon::connect(emit).map_err(|e| e.to_string())?);
                    *slot = Some(c.clone());
                    c
                }
            }
        };
        let r = c.call(&method, params)?;
        if method == "config.get" {
            apply_keys(&app, &r["values"]);
        }
        Ok(r)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Pintasan yang berlaku + masalah `[keys]`, untuk Pengaturan > Pintasan (FR-39).
#[tauri::command]
fn shortcuts(app: tauri::AppHandle) -> (Vec<Shortcut>, Vec<String>) {
    app.state::<Shortcuts>().0.lock().unwrap().clone()
}

/// FR-40: badge Dock = jumlah pane butuh perhatian saat jendela tidak dilihat; 0 = hapus.
#[tauri::command]
fn badge(app: tauri::AppHandle, count: i64) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.set_badge_count((count > 0).then_some(count));
    }
}

/// FR-40: Dock melompat sekali saat agent butuh izin dan jendela tidak dilihat.
#[tauri::command]
fn attention(app: tauri::AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.request_user_attention(Some(tauri::UserAttentionType::Informational));
    }
}

/// FR-48: lupakan rahasia kanal UI di Keychain (item milik app ini saja).
#[tauri::command]
fn forget_secret() {
    let account = format!("ui-secret:{}", daemon::rundir().display());
    let _ = security_framework::passwords::delete_generic_password(daemon::KC_SERVICE, &account);
}

/// FR-54/SEC-34: gambar tempelan disimpan di `$TMPDIR/gembala/paste/` (`0700`), file `0600` bernama acak.
/// Hanya PNG/JPEG/GIF/WebP dan maks 20 MB; path dikembalikan untuk ditempel ke pane.
#[tauri::command]
fn save_paste(data: String, ext: String) -> Result<String, String> {
    use base64::{Engine, engine::general_purpose::STANDARD};
    use std::io::{Read, Write};
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
    if !["png", "jpg", "gif", "webp"].contains(&ext.as_str()) {
        return Err("not_image".into());
    }
    let bytes = STANDARD.decode(data).map_err(|e| e.to_string())?;
    if bytes.len() > 20 << 20 {
        return Err("image_too_big".into());
    }
    let dir = daemon::rundir().join("paste");
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&dir).map_err(|e| e.to_string())?;
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())?;
    let mut rnd = [0u8; 12];
    std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut rnd)).map_err(|e| e.to_string())?;
    let name: String = rnd.iter().map(|b| format!("{b:02x}")).collect();
    let path = dir.join(format!("{name}.{ext}"));
    let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&path).map_err(|e| e.to_string())?;
    f.write_all(&bytes).map_err(|e| e.to_string())?;
    Ok(path.to_string_lossy().into_owned())
}

/// SEC-34: hapus gambar tempelan milik pane yang ditutup; hanya file di folder paste milik Gembala.
#[tauri::command]
fn forget_paste(paths: Vec<String>) {
    let dir = daemon::rundir().join("paste");
    for p in paths {
        let p = std::path::PathBuf::from(p);
        if p.parent() == Some(dir.as_path()) {
            let _ = std::fs::remove_file(p);
        }
    }
}

/// SEC-34: gambar tempelan lebih dari 24 jam dibuang saat app dibuka.
fn sweep_paste() {
    let Ok(entries) = std::fs::read_dir(daemon::rundir().join("paste")) else { return };
    for e in entries.flatten() {
        let old = e.metadata().and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_some_and(|d| d.as_secs() > 24 * 3600);
        if old {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// FR-15: path asli (symlink diselesaikan); yang gagal diselesaikan dibuang.
fn canonical(paths: impl IntoIterator<Item = std::path::PathBuf>) -> Vec<String> {
    paths.into_iter().filter_map(|p| std::fs::canonicalize(p).ok()).map(|p| p.to_string_lossy().into_owned()).collect()
}

/// FR-15: dialog pilih file; path yang dipilih ditempel UI ke prompt agent (agent membaca dengan izinnya sendiri).
#[tauri::command]
async fn pick_files(app: tauri::AppHandle) -> Vec<String> {
    use tauri_plugin_dialog::DialogExt;
    let picked = tauri::async_runtime::spawn_blocking(move || app.dialog().file().blocking_pick_files()).await.ok().flatten();
    canonical(picked.unwrap_or_default().into_iter().filter_map(|f| f.into_path().ok()))
}

/// FR-44: dialog pilih folder akar workspace.
#[tauri::command]
async fn pick_folder(app: tauri::AppHandle) -> Option<String> {
    use tauri_plugin_dialog::DialogExt;
    let picked = tauri::async_runtime::spawn_blocking(move || app.dialog().file().blocking_pick_folder()).await.ok().flatten();
    canonical(picked.and_then(|f| f.into_path().ok())).pop()
}

/// FR-15: file yang diseret ke pane juga diselesaikan symlink-nya.
#[tauri::command]
fn resolve_paths(paths: Vec<String>) -> Vec<String> {
    canonical(paths.into_iter().map(Into::into))
}

/// FR-40 banner macOS (T5). Isi hanya nama pane + status (SEC-33): bisa tampil di layar kunci.
#[tauri::command]
fn notify_pane(app: tauri::AppHandle, body: String, sound: bool, pane: Option<u64>) {
    use tauri_plugin_notification::NotificationExt;
    let body: String = body.chars().filter(|c| !c.is_control()).take(80).collect();
    // id = pane: klik banner (`on_action` di `run`) memindah ke pane itu
    let mut n = app.notification().builder().title("Gembala").body(body).id(pane.and_then(|p| i32::try_from(p).ok()).unwrap_or(0));
    if sound {
        n = n.sound("default");
    }
    let _ = n.show();
}

/// FR-60: proses milik Gembala untuk Pengaturan > Proses: UI ini, WebKit-nya, daemon, dan semua turunan daemon.
/// Dibaca dari `ps` tanpa argumen proses (argumen proses lain bisa memuat rahasia).
#[tauri::command]
fn proc_list() -> Vec<Value> {
    let Ok(out) = std::process::Command::new("/bin/ps").args(["-axo", "pid=,ppid=,rss=,pcpu=,etime=,comm="]).output() else {
        return vec![];
    };
    let daemon = daemon::daemon_exe().map(|p| p.to_string_lossy().into_owned());
    procs_of(&String::from_utf8_lossy(&out.stdout), std::process::id(), daemon.as_deref().unwrap_or(""))
}

/// `[[dd-]hh:]mm:ss` → detik
fn etime_secs(s: &str) -> u64 {
    let (days, rest) = s.split_once('-').map_or((0, s), |(d, r)| (d.parse().unwrap_or(0), r));
    rest.split(':').fold(0, |acc, p| acc * 60 + p.parse::<u64>().unwrap_or(0)) + days * 86400
}

fn procs_of(ps: &str, me: u32, daemon_exe: &str) -> Vec<Value> {
    struct P<'a> { pid: u32, ppid: u32, rss: u64, cpu: f64, age: u64, comm: &'a str }
    let all: Vec<P> = ps
        .lines()
        .filter_map(|l| {
            // lima kolom angka, sisanya comm (boleh berisi spasi)
            let mut rest = l.trim_start();
            let mut f = [""; 5];
            for x in f.iter_mut() {
                let (a, b) = rest.split_once(char::is_whitespace)?;
                (*x, rest) = (a, b.trim_start());
            }
            let [pid, ppid, rss, cpu, age] = f;
            Some(P { pid: pid.parse().ok()?, ppid: ppid.parse().ok()?, rss: rss.parse().ok()?, cpu: cpu.parse().ok()?, age: etime_secs(age), comm: rest.trim_end() })
        })
        .collect();
    let row = |p: &P, role: &str| json!({"pid": p.pid, "ppid": p.ppid, "rss_kb": p.rss, "cpu": p.cpu, "role": role,
        "name": p.comm.rsplit('/').next().unwrap_or(p.comm)});
    let mut rows = vec![];
    let Some(ui) = all.iter().find(|p| p.pid == me) else { return rows };
    rows.push(row(ui, "ui"));
    // WebKit milik UI dijalankan launchd, bukan anak UI: dikenali dari waktu mulai yang hampir sama
    // ponytail: dua app WebKit yang dibuka di detik yang sama bisa tertukar; pakai audit token XPC kalau perlu pasti
    for (svc, role) in [("WebContent", "webkit-content"), ("GPU", "webkit-gpu"), ("Networking", "webkit-net")] {
        let name = format!("com.apple.WebKit.{svc}");
        if let Some(p) = all.iter().filter(|p| p.comm.ends_with(&name) && p.age <= ui.age && ui.age - p.age <= 10).min_by_key(|p| ui.age - p.age) {
            rows.push(row(p, role));
        }
    }
    let mut tree: Vec<u32> = all.iter().filter(|p| p.comm == daemon_exe).map(|p| p.pid).collect();
    let mut i = 0;
    while i < tree.len() {
        let parent = tree[i];
        tree.extend(all.iter().filter(|p| p.ppid == parent).map(|p| p.pid));
        i += 1;
    }
    for pid in tree {
        if let Some(p) = all.iter().find(|p| p.pid == pid) {
            rows.push(row(p, if p.comm == daemon_exe { "daemon" } else { "pane" }));
        }
    }
    rows
}

/// FR-37/52: salin dari menu klik kanan (teks pilihan pengguna, folder tab) lewat pbcopy;
/// clipboard web butuh gesture yang tidak ada di callback menu native.
#[tauri::command]
fn copy_text(text: String) -> Result<(), String> {
    use std::io::Write;
    let mut c = std::process::Command::new("/usr/bin/pbcopy").stdin(std::process::Stdio::piped()).spawn().map_err(|e| e.to_string())?;
    c.stdin.take().ok_or("stdin")?.write_all(text.as_bytes()).map_err(|e| e.to_string())?;
    c.wait().map(drop).map_err(|e| e.to_string())
}

/// FR-59 autocomplete: isi folder yang diawali `prefix` (nama + folder?), folder dulu, maks 200.
/// Berkas tersembunyi hanya bila awalan diawali titik. Baca saja.
#[tauri::command]
fn list_dir(dir: String, prefix: String) -> Vec<(String, bool)> {
    let Ok(rd) = std::fs::read_dir(&dir) else { return vec![] };
    let mut out: Vec<(String, bool)> = rd
        .flatten()
        .filter_map(|e| {
            let n = e.file_name().into_string().ok()?;
            (n.starts_with(&prefix) && (prefix.starts_with('.') || !n.starts_with('.'))).then(|| (n, e.path().is_dir()))
        })
        .take(2000)
        .collect();
    out.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.to_lowercase().cmp(&b.0.to_lowercase())));
    out.truncate(200);
    out
}

/// FR-59 autocomplete kata pertama: perintah di PATH (app dari Finder punya PATH minim, jadi folder umum ditambah).
#[tauri::command]
fn list_commands(prefix: String) -> Vec<String> {
    use std::os::unix::fs::PermissionsExt;
    let home = std::env::var("HOME").unwrap_or_default();
    let path = std::env::var("PATH").unwrap_or_default();
    let extra = ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin", "/usr/sbin", "/sbin"].map(String::from);
    let mut out: Vec<String> = path
        .split(':')
        .map(String::from)
        .chain(extra)
        .chain([format!("{home}/.local/bin"), format!("{home}/.cargo/bin")])
        .filter_map(|d| std::fs::read_dir(d).ok())
        .flat_map(|rd| rd.flatten())
        .filter(|e| e.metadata().is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0))
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| n.starts_with(&prefix))
        .collect();
    out.sort();
    out.dedup();
    out.truncate(200);
    out
}

/// FR-59 autocomplete argumen git: branch lokal dan remote (tanpa `origin/HEAD`). Baca saja.
#[tauri::command]
fn git_branches(dir: String) -> Vec<String> {
    let out = std::process::Command::new("git").args(["-C", &dir, "for-each-ref", "--format=%(refname:short)", "refs/heads", "refs/remotes"]).output();
    let Ok(out) = out else { return vec![] };
    String::from_utf8_lossy(&out.stdout).lines().filter(|b| !b.ends_with("/HEAD") && !b.is_empty()).map(String::from).collect()
}

/// FR-59 autocomplete `npm run`: nama script di package.json folder itu.
#[tauri::command]
fn npm_scripts(dir: String) -> Vec<String> {
    let text = std::fs::read_to_string(std::path::Path::new(&dir).join("package.json")).unwrap_or_default();
    let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    v["scripts"].as_object().map(|m| m.keys().cloned().collect()).unwrap_or_default()
}

/// FR-59 saran abu-abu: riwayat zsh pengguna, terbaru dulu, tanpa duplikat (format extended `: ts:0;cmd` atau polos).
#[tauri::command]
fn shell_history() -> Vec<String> {
    let path = std::env::var("HISTFILE").unwrap_or_else(|_| format!("{}/.zsh_history", std::env::var("HOME").unwrap_or_default()));
    let Ok(bytes) = std::fs::read(path) else { return vec![] };
    let text = String::from_utf8_lossy(&bytes);
    let mut seen = std::collections::HashSet::new();
    text.lines()
        .rev()
        .map(|l| if l.starts_with(": ") { l.split_once(';').map_or(l, |(_, c)| c) } else { l })
        .filter(|c| !c.trim().is_empty() && !c.ends_with('\\') && seen.insert(c.to_string()))
        .take(3000)
        .map(String::from)
        .collect()
}

/// FR-37: Tempel dari menu klik kanan terminal (teks saja; gambar tetap lewat ⌘V, FR-54).
#[tauri::command]
fn paste_text() -> Result<String, String> {
    let o = std::process::Command::new("/usr/bin/pbpaste").output().map_err(|e| e.to_string())?;
    Ok(String::from_utf8_lossy(&o.stdout).into_owned())
}

/// TECH 20: macOS language list for `[ui] language = "system"`
#[tauri::command]
fn system_lang() -> Vec<String> {
    i18n::system_langs()
}

#[tauri::command]
fn quit(app: tauri::AppHandle) {
    app.exit(0);
}

type Shortcut = (&'static str, &'static str, String); // id aksi, label, kombinasi ("" = tanpa)

/// Pintasan yang berlaku + masalah `[keys]` (untuk Pengaturan > Pintasan).
struct Shortcuts(Mutex<(Vec<Shortcut>, Vec<String>)>);

const MODS: &[(&str, &[&str])] =
    &[("Ctrl", &["ctrl", "control"]), ("Alt", &["alt", "option"]), ("Shift", &["shift"]), ("Cmd", &["cmd", "command", "super", "meta"])];
const NAMED: &[&str] = &["enter", "tab", "space", "left", "right", "up", "down", "escape", "backspace", "delete", "home", "end", "pageup", "pagedown"];

/// "cmd+shift+d" → "Shift+Cmd+D". Wajib memakai ⌘: tombol lain milik terminal (FR-39).
fn canon_key(raw: &str, lang: &str) -> Result<String, String> {
    let parts: Vec<String> = raw.split('+').map(|t| t.trim().to_lowercase()).collect();
    let (key, mods) = parts.split_last().ok_or(i18n::tr(lang, "key.empty"))?;
    let mut out: Vec<&str> = Vec::new();
    for (name, aliases) in MODS {
        if mods.iter().any(|m| aliases.contains(&m.as_str())) {
            out.push(name);
        }
    }
    if mods.iter().any(|m| !MODS.iter().any(|(_, a)| a.contains(&m.as_str()))) {
        return Err(i18n::fmt(lang, "key.bad_mod", &[("raw", raw)]));
    }
    if !out.contains(&"Cmd") {
        return Err(i18n::fmt(lang, "key.need_cmd", &[("raw", raw)]));
    }
    let key = if key.chars().count() == 1 && key.chars().all(|c| c.is_ascii_alphanumeric() || ".,=-;'/[]\\`".contains(c)) {
        key.to_uppercase()
    } else if NAMED.contains(&key.as_str()) || (key.starts_with('f') && key[1..].parse::<u8>().is_ok_and(|n| (1..=12).contains(&n))) {
        let mut c = key.chars();
        c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
    } else {
        return Err(i18n::fmt(lang, "key.bad_key", &[("raw", raw)]));
    };
    Ok(out.into_iter().chain([key.as_str()]).collect::<Vec<_>>().join("+"))
}

/// Pintasan bawaan ditimpa `[keys]` ("kombinasi" = "id aksi"). Bentrok: aksi yang ditimpa menang,
/// lawannya kehilangan pintasan dan ditandai.
fn resolve_keys(overrides: &serde_json::Map<String, Value>, lang: &str) -> (Vec<Shortcut>, Vec<String>) {
    let mut items: Vec<(Shortcut, bool)> =
        TAB_KEYS.iter().chain(PANE_KEYS).chain(VIEW_KEYS).chain(MSG_KEYS).map(|(id, key)| ((*id, i18n::tr(lang, id), canon_key(key, lang).unwrap_or_default()), false)).collect();
    let mut errors = Vec::new();
    for (raw, action) in overrides {
        let Some(action) = action.as_str() else { continue };
        let Some(item) = items.iter_mut().find(|((id, _, _), _)| *id == action) else {
            errors.push(i18n::fmt(lang, "key.bad_action", &[("raw", raw), ("action", action)]));
            continue;
        };
        match canon_key(raw, lang) {
            Ok(k) => *item = ((item.0.0, item.0.1, k), true),
            Err(why) => errors.push(format!("keys: {why}")),
        }
    }
    let mut order: Vec<usize> = (0..items.len()).collect();
    order.sort_by_key(|&i| !items[i].1); // yang ditimpa diperiksa duluan
    let mut seen: Vec<(String, &'static str)> = Vec::new();
    for i in order {
        let ((id, _, key), _) = &mut items[i];
        if key.is_empty() {
            continue;
        }
        if let Some((_, owner)) = seen.iter().find(|(k, _)| k == key) {
            errors.push(i18n::fmt(lang, "key.clash", &[("key", key), ("owner", owner), ("id", id)]));
            key.clear();
        } else {
            seen.push((key.clone(), id));
        }
    }
    (items.into_iter().map(|(s, _)| s).collect(), errors)
}

fn build_menu<R: tauri::Runtime>(app: &tauri::AppHandle<R>, keys: &[Shortcut], lang: &str) -> tauri::Result<tauri::menu::Menu<R>> {
    use tauri::menu::{MenuBuilder, MenuItemBuilder, SubmenuBuilder};
    let tr = |k| i18n::tr(lang, k);
    let item = |(id, label, key): &Shortcut| {
        let b = MenuItemBuilder::with_id(*id, *label);
        if key.is_empty() { b.build(app) } else { b.accelerator(key).build(app) }
    };
    // predefined items get our text so they follow the app language, not the macOS one (TECH 20)
    let main = SubmenuBuilder::new(app, "Gembala")
        .about_with_text(tr("menu.about"), None)
        .separator()
        .item(&MenuItemBuilder::with_id("app.settings", tr("menu.settings")).accelerator("Cmd+,").build(app)?)
        .separator()
        .hide_with_text(tr("menu.hide"))
        .hide_others_with_text(tr("menu.hide_others"))
        .show_all_with_text(tr("menu.show_all"))
        .separator()
        // FR-46: ⌘Q menutup jendela saja; agent jalan terus di daemon (FR-20)
        .item(&MenuItemBuilder::with_id("app.quit_all", tr("menu.quit_all")).build(app)?)
        .quit_with_text(tr("menu.quit"))
        .build()?;
    let edit = SubmenuBuilder::new(app, tr("menu.edit"))
        .undo_with_text(tr("menu.undo"))
        .redo_with_text(tr("menu.redo"))
        .separator()
        .cut_with_text(tr("menu.cut"))
        .copy_with_text(tr("menu.copy"))
        .paste_with_text(tr("menu.paste"))
        .select_all_with_text(tr("menu.select_all"))
        .build()?;
    // pintasan lewat key equivalent native: WKWebView tidak selalu meneruskan ⌘⇧/⌘⌥/⌃⌘ ke keydown JS
    let mut view = SubmenuBuilder::new(app, tr("menu.view"));
    let mut tab = SubmenuBuilder::new(app, tr("menu.tab"));
    let mut pane = SubmenuBuilder::new(app, tr("menu.pane"));
    let mut msgs = SubmenuBuilder::new(app, tr("menu.msg"));
    let mut group = "";
    for k in keys {
        if k.0.starts_with("term.") {
            view = view.item(&item(k)?);
            continue;
        }
        if k.0.starts_with("tab.") {
            tab = tab.item(&item(k)?);
            continue;
        }
        if k.0.starts_with("msg.") {
            msgs = msgs.item(&item(k)?);
            continue;
        }
        let g = k.0.split('.').nth(1).unwrap_or("");
        if !group.is_empty() && g != group {
            pane = pane.separator();
        }
        group = g;
        pane = pane.item(&item(k)?);
    }
    let window = SubmenuBuilder::new(app, tr("menu.window")).minimize_with_text(tr("menu.minimize")).fullscreen_with_text(tr("menu.fullscreen")).build()?;
    MenuBuilder::new(app).items(&[&main, &edit, &view.build()?, &tab.build()?, &pane.build()?, &msgs.build()?, &window]).build()
}

/// `[keys]` and `[ui] language` from config → new menu (FR-39, TECH 20; no restart).
fn apply_keys(app: &tauri::AppHandle, values: &Value) {
    let empty = serde_json::Map::new();
    let lang = i18n::of_config(values);
    let (items, errors) = resolve_keys(values["keys"].as_object().unwrap_or(&empty), lang);
    *app.state::<Shortcuts>().0.lock().unwrap() = (items.clone(), errors);
    let app2 = app.clone();
    let _ = app.run_on_main_thread(move || {
        if let Ok(menu) = build_menu(&app2, &items, lang) {
            let _ = app2.set_menu(menu);
        }
    });
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    sweep_paste();
    // until the first config.get: macOS language (TECH 20)
    let lang0 = i18n::resolve("system", &i18n::system_langs());
    tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(Daemon(Mutex::new(None)))
        // FR-40: klik banner = jendela tampil + pindah ke pane-nya (id notifikasi = id pane, lihat `notify_pane`).
        // ponytail: plugin menunggu klik di satu thread per banner sampai banner hilang dari Notification Center
        .setup(|app| {
            use tauri_plugin_notification::NotificationExt;
            let h = app.handle().clone();
            app.notification().on_action(move |a| {
                let Some(pane) = a.notification().map(|n| n.id()).filter(|id| a.action_id() == "tap" && *id > 0) else { return };
                if let Some(w) = h.get_webview_window("main") {
                    let _ = w.unminimize();
                    let _ = w.show();
                    let _ = w.set_focus();
                }
                let _ = h.emit("notify-click", pane);
            })?;
            Ok(())
        })
        .manage(Shortcuts(Mutex::new(resolve_keys(&serde_json::Map::new(), lang0))))
        // menu bawaan Tauri memasang ⌘W = tutup jendela; ⌘W di sini milik "tutup pane" (FR-38, FR-39)
        .menu(move |app| build_menu(app, &resolve_keys(&serde_json::Map::new(), lang0).0, lang0))
        .on_menu_event(|app, ev| {
            if ["pane.", "app.", "term.", "tab.", "msg."].iter().any(|p| ev.id().0.starts_with(p)) {
                let _ = app.emit("shortcut", ev.id().0.as_str());
            }
        })
        // FR-46: tutup jendela = UI keluar (tidak tinggal di latar tanpa jendela); agent tetap jalan di daemon (FR-20)
        .on_window_event(|w, ev| {
            if let tauri::WindowEvent::Destroyed = ev {
                w.app_handle().exit(0);
            }
        })
        .invoke_handler(tauri::generate_handler![daemon_call, quit, open_url, reveal_dir, shortcuts, badge, attention, forget_secret, save_paste, forget_paste, notify_pane, pick_files, pick_folder, resolve_paths, copy_text, paste_text, list_dir, list_commands, shell_history, git_branches, npm_scripts, proc_list, system_lang])
        .build(tauri::generate_context!())
        .expect("error while running tauri application")
        .run(|app, ev| {
            // Cmd+Q (terminate AppKit) hanya memberi Exit, tanpa ExitRequested
            if let tauri::RunEvent::Exit = ev {
                on_exit(app);
            }
        });
}

/// App keluar (Cmd+Q, tutup jendela). Pengaturan "Saat Cmd+Q: Hentikan semua" = daemon dihentikan (FR-20).
/// Selain itu, FR-60: daemon tetap hidup karena pane masih menjalankan proses = beri tahu pengguna alasannya,
/// supaya proses latar tidak dikira diam-diam. "Hentikan semua" menghentikan daemon dulu, jadi tidak muncul.
fn on_exit(app: &tauri::AppHandle) {
    use tauri_plugin_notification::NotificationExt;
    let Some(c) = app.state::<Daemon>().0.lock().unwrap().clone() else { return };
    let values = c.call("config.get", json!({})).map(|v| v["values"].clone()).unwrap_or_default();
    if values["ui"]["on_quit"] == "stop" {
        let _ = c.call("daemon.stop", json!({}));
        return;
    }
    let lang = i18n::of_config(&values);
    let Ok(ids) = c.call("pane.list", json!({})) else { return };
    let busy: Vec<String> = ids
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|id| c.call("pane.info", json!({"pane": id})).ok())
        .filter(|i| i["busy"] == true)
        .map(|i| i["name"].as_str().unwrap_or(i18n::tr(lang, "quit.process")).to_string())
        .collect();
    if busy.is_empty() {
        return;
    }
    let body = i18n::fmt(lang, "quit.body", &[("n", &busy.len().to_string()), ("names", &busy.join(", "))]);
    let _ = app.notification().builder().title(i18n::tr(lang, "quit.title")).body(body).show();
    std::thread::sleep(std::time::Duration::from_millis(300)); // beri waktu notifikasi terkirim sebelum proses keluar
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    /// FR-60: UI, WebKit yang mulai bersamaan, daemon, dan turunannya; proses lain tidak ikut
    #[test]
    fn proses_milik_gembala() {
        let d = "/Apps/Gembala.app/Contents/MacOS/gembala";
        let ps = format!(
            "  10     1  100000   2.0    05:00 /Apps/Gembala.app/Contents/MacOS/gembala-ui\n\
             \x20 11     1  300000  10.5    04:58 /S/WebKit/com.apple.WebKit.WebContent\n\
             \x20 12     1  300000   0.0 01-02:00:00 /S/WebKit/com.apple.WebKit.WebContent\n\
             \x20 13     1   12000   0.0    04:59 /S/WebKit/com.apple.WebKit.Networking\n\
             \x20 20     1   13000   0.1 02:00:00 {d}\n\
             \x20 21    20    4000   0.0    10:00 /bin/zsh\n\
             \x20 22    21  500000  30.0    09:00 /opt/bin/claude\n\
             \x20 30     1  800000   1.0    09:00 /Applications/Google Chrome.app/Contents/MacOS/Google Chrome\n"
        );
        let rows = super::procs_of(&ps, 10, d);
        let got: Vec<(u64, &str, &str)> = rows.iter().map(|r| (r["pid"].as_u64().unwrap(), r["role"].as_str().unwrap(), r["name"].as_str().unwrap())).collect();
        assert_eq!(got, vec![
            (10, "ui", "gembala-ui"),
            (11, "webkit-content", "com.apple.WebKit.WebContent"),
            (13, "webkit-net", "com.apple.WebKit.Networking"),
            (20, "daemon", "gembala"),
            (21, "pane", "zsh"),
            (22, "pane", "claude"),
        ]);
        assert_eq!(super::etime_secs("01-02:03:04"), 93784);
    }

    /// FR-59: folder dulu, saring awalan, tersembunyi hanya bila awalan diawali titik
    #[test]
    fn list_dir_saring_dan_urut() {
        let d = std::env::temp_dir().join(format!("gbl-lsdir-{}", std::process::id()));
        for sub in ["Beta", "alpha", ".git"] {
            std::fs::create_dir_all(d.join(sub)).unwrap();
        }
        std::fs::write(d.join("aku.txt"), "").unwrap();
        let dir = d.to_string_lossy().to_string();
        assert_eq!(super::list_dir(dir.clone(), "".into()), vec![("alpha".into(), true), ("Beta".into(), true), ("aku.txt".into(), false)]);
        assert_eq!(super::list_dir(dir.clone(), "a".into()), vec![("alpha".into(), true), ("aku.txt".into(), false)]);
        assert_eq!(super::list_dir(dir.clone(), ".".into()), vec![(".git".into(), true)]);
        // package.json tanpa scripts = kosong; dengan scripts = namanya
        assert!(super::npm_scripts(dir.clone()).is_empty());
        std::fs::write(d.join("package.json"), r#"{"scripts":{"dev":"vite","test":"node --test"}}"#).unwrap();
        assert_eq!(super::npm_scripts(dir), vec!["dev".to_string(), "test".to_string()]);
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn path_symlink_diselesaikan() {
        let d = std::env::temp_dir().join(format!("gembala-canon-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("asli.txt"), "x").unwrap();
        std::os::unix::fs::symlink(d.join("asli.txt"), d.join("tautan.txt")).unwrap();
        let out = super::resolve_paths(vec![d.join("tautan.txt").to_string_lossy().into(), d.join("tidak-ada").to_string_lossy().into()]);
        assert_eq!(out.len(), 1);
        assert!(out[0].ends_with("/asli.txt"), "{out:?}");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn paste_gambar_aman() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("gembala-paste-{}", std::process::id()));
        // satu-satunya test yang memakai env ini; test lain di crate ini tidak membaca env
        unsafe { std::env::set_var("GEMBALA_RUNDIR", &dir) };
        let p = super::save_paste("iVBORw0KGgo=".into(), "png".into()).unwrap();
        let meta = std::fs::metadata(&p).unwrap();
        assert_eq!(meta.permissions().mode() & 0o777, 0o600);
        assert_eq!(std::fs::metadata(dir.join("paste")).unwrap().permissions().mode() & 0o777, 0o700);
        assert!(super::save_paste("aGFp".into(), "sh".into()).is_err());
        let outside = dir.join("jangan.txt");
        std::fs::write(&outside, "x").unwrap();
        super::forget_paste(vec![p.clone(), outside.to_string_lossy().into_owned()]);
        assert!(!std::path::Path::new(&p).exists() && outside.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn canon_key_wajib_cmd() {
        assert_eq!(super::canon_key("cmd+shift+d", "en").unwrap(), "Shift+Cmd+D");
        assert_eq!(super::canon_key("Option+Command+Left", "en").unwrap(), "Alt+Cmd+Left");
        assert_eq!(super::canon_key("cmd+,", "en").unwrap(), "Cmd+,");
        for bad in ["ctrl+c", "cmd+hyper+x", "cmd+foo", "", "shift+tab"] {
            assert!(super::canon_key(bad, "en").is_err(), "{bad}");
        }
    }

    #[test]
    fn keys_override_dan_bentrok() {
        let o = json!({"cmd+t": "pane.split.right", "cmd+k": "pane.zoom.toggle", "cmd+y": "tidak.ada", "ctrl+x": "pane.proc.kill"});
        let (items, errors) = super::resolve_keys(o.as_object().unwrap(), "id");
        let key = |id: &str| items.iter().find(|i| i.0 == id).unwrap().2.clone();
        assert_eq!(key("pane.split.right"), "Cmd+T");
        assert_eq!(key("pane.zoom.toggle"), "Cmd+K"); // ditimpa = menang
        assert_eq!(key("term.clear"), ""); // bawaan Cmd+K kalah
        assert_eq!(key("pane.proc.kill"), "Cmd+."); // override salah = bawaan tetap
        let all = errors.join("\n");
        for want in ["tidak.ada", "ctrl+x", "bentrok: Cmd+K"] {
            assert!(all.contains(want), "{want}: {all}");
        }
    }

    /// TECH 20: every shortcut has a label in both languages (menu and Settings > Shortcuts)
    #[test]
    fn shortcut_labels_in_both_languages() {
        for lang in ["en", "id"] {
            let (items, errors) = super::resolve_keys(&serde_json::Map::new(), lang);
            assert!(errors.is_empty(), "{errors:?}");
            for (id, label, _) in items {
                assert!(!label.is_empty(), "{lang} {id}");
            }
        }
        assert_eq!(super::i18n::tr("en", "pane.split.right"), "Split Right");
        assert_eq!(super::i18n::tr("id", "pane.split.right"), "Split ke Kanan");
    }

    #[test]
    fn url_ok_hanya_http() {
        assert!(super::url_ok("https://github.com/AHanif52/gembala"));
        assert!(super::url_ok("http://localhost:1420/"));
        for bad in ["javascript:alert(1)", "file:///etc/passwd", "ssh://x", "https://a b", "https://x\n-a", "HTTPS://x"] {
            assert!(!super::url_ok(bad), "{bad}");
        }
    }

    /// Tombol "Buka" (Pengaturan > Keamanan) tidak boleh membuka folder di luar folder config Gembala.
    #[test]
    fn reveal_hanya_folder_di_sebelah_config() {
        assert_eq!(super::reveal_target("/u/Library/Application Support/Gembala/config.toml", "audit").unwrap(), std::path::Path::new("/u/Library/Application Support/Gembala/audit"));
        for bad in ["..", "../../etc", "/etc", "audit/../../..", "Audit"] {
            assert!(super::reveal_target("/u/cfg/config.toml", bad).is_err(), "{bad}");
        }
    }
}
