// UI hanya klien daemon (BRD R-5): jangan buka PTY di sini.
mod daemon;

use serde_json::Value;
use std::sync::{Arc, OnceLock};
use tauri::{Emitter, Manager};

struct Daemon(OnceLock<Result<daemon::Client, String>>);

/// Webview hanya boleh memanggil method pane; aksi manusia lain ditambah per fitur.
const ALLOWED: &[&str] =
    &["pane.list", "pane.create", "pane.write", "pane.resize", "pane.attach", "pane.close", "pane.info", "pane.kill"];

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

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(Daemon(OnceLock::new()))
        // menu bawaan Tauri memasang ⌘W = tutup jendela; ⌘W di sini milik "tutup pane" (FR-38, FR-39)
        .menu(|app| {
            use tauri::menu::{MenuBuilder, SubmenuBuilder};
            let main = SubmenuBuilder::new(app, "Gembala")
                .about(None)
                .separator()
                .hide()
                .hide_others()
                .show_all()
                .separator()
                .quit()
                .build()?;
            let edit = SubmenuBuilder::new(app, "Edit").undo().redo().separator().cut().copy().paste().select_all().build()?;
            let window = SubmenuBuilder::new(app, "Window").minimize().fullscreen().build()?;
            MenuBuilder::new(app).items(&[&main, &edit, &window]).build()
        })
        .invoke_handler(tauri::generate_handler![daemon_call])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
