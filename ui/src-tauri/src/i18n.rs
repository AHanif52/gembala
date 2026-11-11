// Native text (menu, shortcut labels, `[keys]` problems, quit notification) in the UI language (TECH 20).
// WKWebView reports only one language (`navigator.languages` = ["en-GB"] on an en-ID, id-ID Mac), so the macOS list
// comes from `defaults`. Shortcut labels are sent to Settings > Shortcuts and the palette from here: one source.

/// Preferred languages; read once (macOS restarts apps after a language change).
/// macOS: the system list from `defaults` (`en-ID`, `id-ID`). Linux: `LANGUAGE` (colon separated, the same order the
/// desktop shows), else `LC_ALL`/`LANG` as in `id_ID.UTF-8`.
pub fn system_langs() -> Vec<String> {
    static LANGS: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
    LANGS
        .get_or_init(|| {
            #[cfg(target_os = "macos")]
            {
                let out = std::process::Command::new("/usr/bin/defaults").args(["read", "-g", "AppleLanguages"]).output();
                // output is an old-style plist array: `(\n    "en-ID",\n    "id-ID"\n)`
                out.map(|o| {
                    let text = String::from_utf8_lossy(&o.stdout);
                    text.lines().map(|l| l.trim().trim_end_matches(',').trim_matches('"').to_string()).filter(|l| !l.is_empty() && l != "(" && l != ")").collect()
                })
                .unwrap_or_default()
            }
            #[cfg(not(target_os = "macos"))]
            {
                let env = std::env::var("LANGUAGE").ok().filter(|v| !v.is_empty()).or_else(|| std::env::var("LC_ALL").ok().filter(|v| !v.is_empty())).or_else(|| std::env::var("LANG").ok().filter(|v| !v.is_empty()));
                env.map(|v| v.split(':').map(|l| l.split('.').next().unwrap_or_default().to_string()).filter(|l| !l.is_empty()).collect()).unwrap_or_default()
            }
        })
        .clone()
}

/// `[ui] language`: id/en as is; `system` = first of id/en in the macOS list, anything else English (same as `resolveLang` in ui/src/lib/i18n)
pub fn resolve(pref: &str, system: &[String]) -> &'static str {
    match pref {
        "id" => "id",
        "en" => "en",
        _ => system
            .iter()
            .find_map(|l| match l.get(..2).map(str::to_ascii_lowercase).as_deref() {
                Some("id") => Some("id"),
                Some("en") => Some("en"),
                _ => None,
            })
            .unwrap_or("en"),
    }
}

/// language from `config.get` values
pub fn of_config(values: &serde_json::Value) -> &'static str {
    resolve(values["ui"]["language"].as_str().unwrap_or("system"), &system_langs())
}

/// (key, English, Indonesian). Menu items follow macOS title case in English.
const TEXT: &[(&str, &str, &str)] = &[
    // menu bar
    ("menu.edit", "Edit", "Edit"),
    ("menu.view", "View", "Tampilan"),
    ("menu.tab", "Tab", "Tab"),
    ("menu.pane", "Pane", "Pane"),
    ("menu.msg", "Messages", "Pesan"),
    ("menu.window", "Window", "Jendela"),
    ("menu.about", "About Gembala", "Tentang Gembala"),
    ("menu.settings", "Settings…", "Pengaturan…"),
    ("menu.hide", "Hide Gembala", "Sembunyikan Gembala"),
    ("menu.hide_others", "Hide Others", "Sembunyikan yang Lain"),
    ("menu.show_all", "Show All", "Tampilkan Semua"),
    ("menu.quit_all", "Quit and Stop All Agents…", "Keluar dan Hentikan Semua Agent…"),
    ("menu.quit", "Quit Gembala", "Keluar dari Gembala"),
    ("menu.undo", "Undo", "Urungkan"),
    ("menu.redo", "Redo", "Ulangi"),
    ("menu.cut", "Cut", "Potong"),
    ("menu.copy", "Copy", "Salin"),
    ("menu.paste", "Paste", "Tempel"),
    ("menu.select_all", "Select All", "Pilih Semua"),
    ("menu.minimize", "Minimize", "Kecilkan"),
    ("menu.fullscreen", "Toggle Full Screen", "Layar Penuh"),
    // shortcut labels, keyed by action id (lib.rs *_KEYS)
    ("pane.split.right", "Split Right", "Split ke Kanan"),
    ("pane.split.down", "Split Down", "Split ke Bawah"),
    ("pane.focus.left", "Focus Left", "Fokus ke Kiri"),
    ("pane.focus.right", "Focus Right", "Fokus ke Kanan"),
    ("pane.focus.up", "Focus Up", "Fokus ke Atas"),
    ("pane.focus.down", "Focus Down", "Fokus ke Bawah"),
    ("pane.resize.left", "Move Divider Left", "Geser Pemisah ke Kiri"),
    ("pane.resize.right", "Move Divider Right", "Geser Pemisah ke Kanan"),
    ("pane.resize.up", "Move Divider Up", "Geser Pemisah ke Atas"),
    ("pane.resize.down", "Move Divider Down", "Geser Pemisah ke Bawah"),
    ("pane.swap.left", "Swap with Left Pane", "Tukar dengan Pane Kiri"),
    ("pane.swap.right", "Swap with Right Pane", "Tukar dengan Pane Kanan"),
    ("pane.swap.up", "Swap with Pane Above", "Tukar dengan Pane Atas"),
    ("pane.swap.down", "Swap with Pane Below", "Tukar dengan Pane Bawah"),
    ("pane.zoom.toggle", "Maximize Pane", "Maksimalkan Pane"),
    ("pane.move.newtab", "Move Pane to New Tab", "Pindahkan Pane ke Tab Baru"),
    ("pane.input.rich", "Rich Input", "Rich Input"),
    ("pane.diff.toggle", "Changes Panel", "Panel Perubahan"),
    ("pane.proc.kill", "Stop Process", "Hentikan Proses"),
    ("pane.proc.close", "Close Pane", "Tutup Pane"),
    ("tab.new", "New Tab from Profile…", "Tab Baru dari Profil…"),
    ("task.new", "Ask an Agent…", "Suruh Agent…"),
    ("tab.prev", "Previous Tab", "Tab Sebelumnya"),
    ("tab.next", "Next Tab", "Tab Berikutnya"),
    ("tab.helper.owner", "Go to Helper's Owner", "Buka Pemilik Pembantu"),
    ("tab.helper.next", "Next Helper", "Pembantu Berikutnya"),
    ("tab.helper.prev", "Previous Helper", "Pembantu Sebelumnya"),
    ("tab.helper.compare", "Compare with Owner", "Sandingkan sama Pemilik"),
    ("tab.goto.1", "Tab 1", "Tab 1"),
    ("tab.goto.2", "Tab 2", "Tab 2"),
    ("tab.goto.3", "Tab 3", "Tab 3"),
    ("tab.goto.4", "Tab 4", "Tab 4"),
    ("tab.goto.5", "Tab 5", "Tab 5"),
    ("tab.goto.6", "Tab 6", "Tab 6"),
    ("tab.goto.7", "Tab 7", "Tab 7"),
    ("tab.goto.8", "Tab 8", "Tab 8"),
    ("tab.goto.9", "Tab 9", "Tab 9"),
    ("tab.sidebar", "Show/Hide Sidebar", "Tampilkan/Sembunyikan Sidebar"),
    ("tab.search", "Command Palette", "Palet Perintah"),
    ("pane.attention.next", "Next Pane Needing Permission", "Pane Berikutnya yang Butuh Izin"),
    ("msg.compose", "Send to Another Agent…", "Kirim ke Agent Lain…"),
    ("msg.drawer", "Message Drawer", "Laci Pesan"),
    ("msg.stop", "Pause / Resume All Messages", "Henti / Lanjutkan Semua Pesan"),
    ("term.find", "Find…", "Cari…"),
    ("term.clear", "Clear Screen", "Bersihkan Layar"),
    ("term.block.prev", "Previous Command Block", "Blok Perintah Sebelumnya"),
    ("term.block.next", "Next Command Block", "Blok Perintah Berikutnya"),
    ("term.zoom.in", "Zoom In", "Perbesar"),
    ("term.zoom.out", "Zoom Out", "Perkecil"),
    ("term.zoom.reset", "Actual Size", "Ukuran Asli"),
    ("term.zoom.pane_in", "Zoom In This Pane", "Perbesar Pane Ini"),
    ("term.zoom.pane_out", "Zoom Out This Pane", "Perkecil Pane Ini"),
    ("term.zoom.pane_reset", "Actual Size for This Pane", "Ukuran Asli Pane Ini"),
    // `[keys]` problems (Settings > Shortcuts)
    ("key.empty", "empty", "kosong"),
    ("key.bad_mod", "unknown modifier in \"{raw}\"", "modifier tidak dikenal di \"{raw}\""),
    ("key.need_cmd", "\"{raw}\" has to use the app key ({app}); other keys belong to the terminal", "\"{raw}\" harus memakai tombol aplikasi ({app}); tombol lain milik terminal"),
    ("key.bad_key", "unknown key in \"{raw}\"", "tombol tidak dikenal di \"{raw}\""),
    ("key.bad_action", "keys.\"{raw}\": unknown action \"{action}\"", "keys.\"{raw}\": aksi \"{action}\" tidak dikenal"),
    ("key.clash", "clash: {key} is used by {owner} and {id}; {id} gets no shortcut", "bentrok: {key} dipakai {owner} dan {id}; {id} tanpa pintasan"),
    // quit notification (FR-60)
    ("quit.title", "Gembala is still running", "Gembala masih berjalan"),
    (
        "quit.body",
        "Gembala keeps running in the background because {n} pane(s) are still running something ({names}). Open Gembala to see or stop them.",
        "Gembala tetap berjalan di latar karena {n} pane masih menjalankan proses ({names}). Buka Gembala untuk melihat atau menghentikannya.",
    ),
    ("quit.process", "process", "proses"),
];

/// text for `key` in `lang`; unknown key = empty (caught by `table_complete` and `shortcut_labels_in_both_languages`)
pub fn tr(lang: &str, key: &str) -> &'static str {
    TEXT.iter().find(|(k, _, _)| *k == key).map_or("", |(_, en, id)| if lang == "id" { id } else { en })
}

/// `tr` with `{name}` filled in
pub fn fmt(lang: &str, key: &str, args: &[(&str, &str)]) -> String {
    args.iter().fold(tr(lang, key).to_string(), |s, (k, v)| s.replace(&format!("{{{k}}}"), v))
}

#[cfg(test)]
mod tests {
    #[test]
    fn resolve_first_supported() {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert_eq!(super::resolve("system", &s(&["fr-FR", "id-ID", "en-US"])), "id");
        assert_eq!(super::resolve("system", &s(&["en-ID", "id-ID"])), "en");
        assert_eq!(super::resolve("system", &s(&["fr-FR"])), "en");
        assert_eq!(super::resolve("id", &s(&["en-US"])), "id");
    }

    /// every row has both languages, the same {params}, and no key twice
    #[test]
    fn table_complete() {
        fn params(s: &str) -> Vec<&str> {
            let mut v: Vec<&str> = s.split('{').skip(1).filter_map(|x| x.split_once('}').map(|p| p.0)).collect();
            v.sort();
            v
        }
        for (i, (k, en, id)) in super::TEXT.iter().enumerate() {
            assert!(!en.is_empty() && !id.is_empty(), "{k}");
            assert_eq!(params(en), params(id), "{k}");
            assert!(!super::TEXT[..i].iter().any(|(x, _, _)| x == k), "{k} twice");
        }
        assert_eq!(super::fmt("id", "key.bad_key", &[("raw", "cmd+foo")]), "tombol tidak dikenal di \"cmd+foo\"");
    }
}
