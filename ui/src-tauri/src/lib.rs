// UI hanya klien daemon (BRD R-5): jangan buka PTY di sini.
mod daemon;

use serde_json::Value;
use std::sync::{Arc, OnceLock};
use tauri::{Emitter, Manager};

struct Daemon(OnceLock<Result<daemon::Client, String>>);

/// Webview hanya boleh memanggil method pane; aksi manusia lain ditambah per fitur.
const ALLOWED: &[&str] =
    &["pane.list", "pane.create", "pane.write", "pane.resize", "pane.attach", "pane.close", "pane.info", "pane.kill", "daemon.stop"];

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
    ("pane.proc.kill", "Hentikan proses", "Cmd+."),
    ("pane.proc.close", "Tutup pane", "Cmd+W"),
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
                let _ = app2.emit("daemon", v);
            });
            daemon::connect(emit).map_err(|e| e.to_string())
        });
        c.as_ref().map_err(Clone::clone)?.call(&method, params)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
fn quit(app: tauri::AppHandle) {
    app.exit(0);
}

/// Renderer xterm: "webgl" (bawaan) atau "dom".
// ponytail: saklar env untuk eksperimen RAM; jadi pengaturan `[ui]` di M3
#[tauri::command]
fn renderer() -> String {
    std::env::var("GEMBALA_RENDERER").unwrap_or_else(|_| "webgl".into())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(Daemon(OnceLock::new()))
        // menu bawaan Tauri memasang ⌘W = tutup jendela; ⌘W di sini milik "tutup pane" (FR-38, FR-39)
        .menu(|app| {
            use tauri::menu::{MenuBuilder, MenuItemBuilder, SubmenuBuilder};
            let main = SubmenuBuilder::new(app, "Gembala")
                .about(None)
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
            // pintasan pane lewat key equivalent native: WKWebView tidak selalu meneruskan ⌘⇧/⌘⌥/⌃⌘ ke keydown JS
            let mut pane = SubmenuBuilder::new(app, "Pane");
            for (i, (id, label, key)) in PANE_KEYS.iter().enumerate() {
                if i > 0 && PANE_KEYS[i - 1].0.split('.').nth(1) != id.split('.').nth(1) {
                    pane = pane.separator();
                }
                pane = pane.item(&MenuItemBuilder::with_id(*id, *label).accelerator(*key).build(app)?);
            }
            let pane = pane.build()?;
            let mut view = SubmenuBuilder::new(app, "View");
            for (id, label, key) in VIEW_KEYS {
                view = view.item(&MenuItemBuilder::with_id(*id, *label).accelerator(*key).build(app)?);
            }
            let view = view.build()?;
            let window = SubmenuBuilder::new(app, "Window").minimize().fullscreen().build()?;
            MenuBuilder::new(app).items(&[&main, &edit, &view, &pane, &window]).build()
        })
        .on_menu_event(|app, ev| {
            if ["pane.", "app.", "term."].iter().any(|p| ev.id().0.starts_with(p)) {
                let _ = app.emit("shortcut", ev.id().0.as_str());
            }
        })
        .invoke_handler(tauri::generate_handler![daemon_call, renderer, quit, open_url])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod tests {
    #[test]
    fn url_ok_hanya_http() {
        assert!(super::url_ok("https://github.com/AHanif52/gembala"));
        assert!(super::url_ok("http://localhost:1420/"));
        for bad in ["javascript:alert(1)", "file:///etc/passwd", "ssh://x", "https://a b", "https://x\n-a", "HTTPS://x"] {
            assert!(!super::url_ok(bad), "{bad}");
        }
    }
}
