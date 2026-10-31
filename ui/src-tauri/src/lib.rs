// UI hanya klien daemon (BRD R-5): jangan buka PTY di sini.
mod daemon;

use serde_json::Value;
use std::sync::{Arc, Mutex, OnceLock};
use tauri::{Emitter, Manager};

struct Daemon(OnceLock<Result<daemon::Client, String>>);

/// Webview hanya boleh memanggil method pane; aksi manusia lain ditambah per fitur.
const ALLOWED: &[&str] =
    &["pane.list", "pane.create", "pane.write", "pane.resize", "pane.attach", "pane.close", "pane.info", "pane.kill", "daemon.stop", "config.get", "config.set", "agents.detect", "daemon.info", "layout.load", "layout.save", "daemon.reset", "git.status", "git.diff"];

/// Pintasan pane (FR-37, 38, 39, 50), default ala Warp. Id = aksi yang dijalankan webview.
const PANE_KEYS: &[(&str, &str, &str)] = &[
    ("pane.split.right", "Split ke kanan", "Cmd+D"),
    ("pane.split.down", "Split ke bawah", "Cmd+Shift+D"),
    ("pane.focus.left", "Fokus ke kiri", "Cmd+Alt+Left"),
    ("pane.focus.right", "Fokus ke kanan", "Cmd+Alt+Right"),
    ("pane.focus.up", "Fokus ke atas", "Cmd+Alt+Up"),
    ("pane.focus.down", "Fokus ke bawah", "Cmd+Alt+Down"),
    ("pane.resize.left", "Geser pemisah ke kiri", "Ctrl+Cmd+Left"),
    ("pane.resize.right", "Geser pemisah ke kanan", "Ctrl+Cmd+Right"),
    ("pane.resize.up", "Geser pemisah ke atas", "Ctrl+Cmd+Up"),
    ("pane.resize.down", "Geser pemisah ke bawah", "Ctrl+Cmd+Down"),
    ("pane.zoom.toggle", "Maksimalkan pane", "Cmd+Shift+Enter"),
    ("pane.input.rich", "Rich Input", "Cmd+G"),
    ("pane.diff.toggle", "Panel perubahan", "Cmd+Shift+G"),
    ("pane.proc.kill", "Hentikan proses", "Cmd+."),
    ("pane.proc.close", "Tutup pane", "Cmd+W"),
];

/// Menu "Tab" (FR-52, FR-39 default Warp).
const TAB_KEYS: &[(&str, &str, &str)] = &[
    ("tab.new", "Tab baru dari profil…", "Cmd+T"),
    ("tab.prev", "Tab sebelumnya", "Cmd+Shift+["),
    ("tab.next", "Tab berikutnya", "Cmd+Shift+]"),
    ("tab.goto.1", "Tab 1", "Cmd+1"),
    ("tab.goto.2", "Tab 2", "Cmd+2"),
    ("tab.goto.3", "Tab 3", "Cmd+3"),
    ("tab.goto.4", "Tab 4", "Cmd+4"),
    ("tab.goto.5", "Tab 5", "Cmd+5"),
    ("tab.goto.6", "Tab 6", "Cmd+6"),
    ("tab.goto.7", "Tab 7", "Cmd+7"),
    ("tab.goto.8", "Tab 8", "Cmd+8"),
    ("tab.goto.9", "Tab 9", "Cmd+9"),
    ("tab.sidebar", "Tampilkan/sembunyikan sidebar", "Cmd+B"),
];

/// Menu "View" (FR-34, FR-36).
const VIEW_KEYS: &[(&str, &str, &str)] = &[
    ("term.find", "Cari…", "Cmd+F"),
    ("term.clear", "Bersihkan layar", "Cmd+K"),
    ("term.zoom.in", "Perbesar", "Cmd+="),
    ("term.zoom.out", "Perkecil", "Cmd+-"),
    ("term.zoom.reset", "Ukuran asli", "Cmd+0"),
];

/// SEC-14: hanya http/https, tanpa spasi atau karakter kontrol; dibuka lewat `open`, bukan shell.
fn url_ok(url: &str) -> bool {
    (url.starts_with("https://") || url.starts_with("http://")) && !url.chars().any(|c| c.is_whitespace() || c.is_control())
}

#[tauri::command]
fn open_url(url: String) -> Result<(), String> {
    if !url_ok(&url) {
        return Err("hanya tautan http/https".into());
    }
    std::process::Command::new("/usr/bin/open").arg(&url).spawn().map(drop).map_err(|e| e.to_string())
}

#[tauri::command]
async fn daemon_call(app: tauri::AppHandle, method: String, params: Value) -> Result<Value, String> {
    if !ALLOWED.contains(&method.as_str()) {
        return Err(format!("method tidak diizinkan: {method}"));
    }
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<Daemon>();
        let app2 = app.clone();
        let c = st.0.get_or_init(|| {
            let emit: daemon::OnEvent = Arc::new(move |v| {
                if v["event"] == "config.changed" {
                    apply_keys(&app2, &v["params"]["values"]["keys"]);
                }
                let _ = app2.emit("daemon", v);
            });
            daemon::connect(emit).map_err(|e| e.to_string())
        });
        let r = c.as_ref().map_err(Clone::clone)?.call(&method, params)?;
        if method == "config.get" {
            apply_keys(&app, &r["values"]["keys"]);
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
        return Err("bukan gambar".into());
    }
    let bytes = STANDARD.decode(data).map_err(|e| e.to_string())?;
    if bytes.len() > 20 << 20 {
        return Err("gambar lebih dari 20 MB".into());
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
fn canon_key(raw: &str) -> Result<String, String> {
    let parts: Vec<String> = raw.split('+').map(|t| t.trim().to_lowercase()).collect();
    let (key, mods) = parts.split_last().ok_or("kosong")?;
    let mut out: Vec<&str> = Vec::new();
    for (name, aliases) in MODS {
        if mods.iter().any(|m| aliases.contains(&m.as_str())) {
            out.push(name);
        }
    }
    if mods.iter().any(|m| !MODS.iter().any(|(_, a)| a.contains(&m.as_str()))) {
        return Err(format!("modifier tidak dikenal di \"{raw}\""));
    }
    if !out.contains(&"Cmd") {
        return Err(format!("\"{raw}\" harus memakai cmd; tombol lain milik terminal"));
    }
    let key = if key.chars().count() == 1 && key.chars().all(|c| c.is_ascii_alphanumeric() || ".,=-;'/[]\\`".contains(c)) {
        key.to_uppercase()
    } else if NAMED.contains(&key.as_str()) || (key.starts_with('f') && key[1..].parse::<u8>().is_ok_and(|n| (1..=12).contains(&n))) {
        let mut c = key.chars();
        c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
    } else {
        return Err(format!("tombol tidak dikenal di \"{raw}\""));
    };
    Ok(out.into_iter().chain([key.as_str()]).collect::<Vec<_>>().join("+"))
}

/// Pintasan bawaan ditimpa `[keys]` ("kombinasi" = "id aksi"). Bentrok: aksi yang ditimpa menang,
/// lawannya kehilangan pintasan dan ditandai.
fn resolve_keys(overrides: &serde_json::Map<String, Value>) -> (Vec<Shortcut>, Vec<String>) {
    let mut items: Vec<(Shortcut, bool)> =
        TAB_KEYS.iter().chain(PANE_KEYS).chain(VIEW_KEYS).map(|(id, label, key)| ((*id, *label, canon_key(key).unwrap_or_default()), false)).collect();
    let mut errors = Vec::new();
    for (raw, action) in overrides {
        let Some(action) = action.as_str() else { continue };
        let Some(item) = items.iter_mut().find(|((id, _, _), _)| *id == action) else {
            errors.push(format!("keys.\"{raw}\": aksi \"{action}\" tidak dikenal"));
            continue;
        };
        match canon_key(raw) {
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
            errors.push(format!("bentrok: {key} dipakai {owner} dan {id}; {id} tanpa pintasan"));
            key.clear();
        } else {
            seen.push((key.clone(), id));
        }
    }
    (items.into_iter().map(|(s, _)| s).collect(), errors)
}

fn build_menu<R: tauri::Runtime>(app: &tauri::AppHandle<R>, keys: &[Shortcut]) -> tauri::Result<tauri::menu::Menu<R>> {
    use tauri::menu::{MenuBuilder, MenuItemBuilder, SubmenuBuilder};
    let item = |(id, label, key): &Shortcut| {
        let b = MenuItemBuilder::with_id(*id, *label);
        if key.is_empty() { b.build(app) } else { b.accelerator(key).build(app) }
    };
    let main = SubmenuBuilder::new(app, "Gembala")
        .about(None)
        .separator()
        .item(&MenuItemBuilder::with_id("app.settings", "Pengaturan…").accelerator("Cmd+,").build(app)?)
        .separator()
        .hide()
        .hide_others()
        .show_all()
        .separator()
        // FR-46: ⌘Q menutup jendela saja; agent jalan terus di daemon (FR-20)
        .item(&MenuItemBuilder::with_id("app.quit_all", "Keluar dan hentikan semua agent…").build(app)?)
        .quit()
        .build()?;
    let edit = SubmenuBuilder::new(app, "Edit").undo().redo().separator().cut().copy().paste().select_all().build()?;
    // pintasan lewat key equivalent native: WKWebView tidak selalu meneruskan ⌘⇧/⌘⌥/⌃⌘ ke keydown JS
    let mut view = SubmenuBuilder::new(app, "View");
    let mut tab = SubmenuBuilder::new(app, "Tab");
    let mut pane = SubmenuBuilder::new(app, "Pane");
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
        let g = k.0.split('.').nth(1).unwrap_or("");
        if !group.is_empty() && g != group {
            pane = pane.separator();
        }
        group = g;
        pane = pane.item(&item(k)?);
    }
    let window = SubmenuBuilder::new(app, "Window").minimize().fullscreen().build()?;
    MenuBuilder::new(app).items(&[&main, &edit, &view.build()?, &tab.build()?, &pane.build()?, &window]).build()
}

/// `[keys]` dari config → menu baru (FR-39, berlaku tanpa restart).
fn apply_keys(app: &tauri::AppHandle, keys: &Value) {
    let empty = serde_json::Map::new();
    let (items, errors) = resolve_keys(keys.as_object().unwrap_or(&empty));
    *app.state::<Shortcuts>().0.lock().unwrap() = (items.clone(), errors);
    let app2 = app.clone();
    let _ = app.run_on_main_thread(move || {
        if let Ok(menu) = build_menu(&app2, &items) {
            let _ = app2.set_menu(menu);
        }
    });
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    sweep_paste();
    tauri::Builder::default()
        .manage(Daemon(OnceLock::new()))
        .manage(Shortcuts(Mutex::new(resolve_keys(&serde_json::Map::new()))))
        // menu bawaan Tauri memasang ⌘W = tutup jendela; ⌘W di sini milik "tutup pane" (FR-38, FR-39)
        .menu(|app| build_menu(app, &resolve_keys(&serde_json::Map::new()).0))
        .on_menu_event(|app, ev| {
            if ["pane.", "app.", "term.", "tab."].iter().any(|p| ev.id().0.starts_with(p)) {
                let _ = app.emit("shortcut", ev.id().0.as_str());
            }
        })
        .invoke_handler(tauri::generate_handler![daemon_call, quit, open_url, shortcuts, badge, attention, forget_secret, save_paste, forget_paste])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod tests {
    use serde_json::json;

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
        assert_eq!(super::canon_key("cmd+shift+d").unwrap(), "Shift+Cmd+D");
        assert_eq!(super::canon_key("Option+Command+Left").unwrap(), "Alt+Cmd+Left");
        assert_eq!(super::canon_key("cmd+,").unwrap(), "Cmd+,");
        for bad in ["ctrl+c", "cmd+hyper+x", "cmd+foo", "", "shift+tab"] {
            assert!(super::canon_key(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn keys_override_dan_bentrok() {
        let o = json!({"cmd+t": "pane.split.right", "cmd+k": "pane.zoom.toggle", "cmd+y": "tidak.ada", "ctrl+x": "pane.proc.kill"});
        let (items, errors) = super::resolve_keys(o.as_object().unwrap());
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

    #[test]
    fn url_ok_hanya_http() {
        assert!(super::url_ok("https://github.com/AHanif52/gembala"));
        assert!(super::url_ok("http://localhost:1420/"));
        for bad in ["javascript:alert(1)", "file:///etc/passwd", "ssh://x", "https://a b", "https://x\n-a", "HTTPS://x"] {
            assert!(!super::url_ok(bad), "{bad}");
        }
    }
}
