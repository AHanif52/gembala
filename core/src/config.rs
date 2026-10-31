//! `config.toml` (TECH bagian 5, FR-42): validasi per kunci, nilai bawaan untuk yang rusak,
//! file tidak pernah ditimpa otomatis. Tulis dari Pengaturan lewat `toml_edit` supaya komentar tetap.

use serde_json::{Map, Value, json};
use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use toml_edit::DocumentMut;

enum Kind {
    Int(i64, i64, i64), // min, max, bawaan
    Float(f64, f64, f64),
    Bool(bool),
    Enum(&'static [&'static str], &'static str),
    Str(&'static str),
}

use Kind::*;

// ponytail: hanya bagian yang sudah dipakai app; notify/app/toolbar/workspace/profile ditambah di M4–M7
const UI: &[(&str, Kind)] = &[
    ("theme", Enum(&["system", "light", "dark"], "system")),
    ("font_family", Str("SF Mono")),
    ("font_size", Int(8, 32, 13)),
    ("line_height", Float(1.0, 2.0, 1.2)),
    ("cursor", Enum(&["bar", "block", "underline"], "bar")),
    ("cursor_blink", Bool(true)),
    ("option_as_meta", Enum(&["none", "left", "right", "both"], "left")),
    // xterm sekitar 12 byte per sel: 5.000 baris × 200 kolom ≈ 12 MB per pane bila penuh
    ("scrollback_lines", Int(0, 100_000, 5_000)),
    ("screen_reader", Bool(false)),
];
const LIMITS: &[(&str, Kind)] = &[("max_panes", Int(1, 256, 32))];
/// FR-40: banner macOS + badge Dock + Dock melompat
const NOTIFY: &[(&str, Kind)] =
    &[("on_blocked", Bool(true)), ("on_done", Bool(true)), ("long_command_secs", Int(5, 3600, 30)), ("sound", Bool(true))];
const SECTIONS: &[(&str, &[(&str, Kind)])] = &[("ui", UI), ("limits", LIMITS), ("notify", NOTIFY)];

pub struct Config {
    /// Profil bawaan + `[[profile]]` yang lolos validasi (FR-29). Rujukan rahasia ikut, nilainya tidak.
    pub profiles: Vec<crate::profile::Profile>,
    /// `{ui, limits, keys}` yang berlaku (nilai bawaan untuk yang rusak atau tidak ada).
    pub values: Value,
    pub errors: Vec<String>,
    /// File tidak bisa diurai sama sekali: pemanggil memakai nilai valid terakhir.
    pub broken: bool,
}

pub fn path() -> PathBuf {
    if let Some(p) = std::env::var_os("GEMBALA_CONFIG") {
        return p.into();
    }
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| "/".into());
    home.join("Library/Application Support/Gembala/config.toml")
}

/// Tata letak (FR-45) di sebelah config: struktur workspace, tab, split, profil, folder. Tanpa isi layar (SEC-13).
pub fn layout_path() -> PathBuf {
    path().with_file_name("layout.json")
}

fn default(kind: &Kind) -> Value {
    match kind {
        Int(_, _, d) => json!(d),
        Float(_, _, d) => json!(d),
        Bool(d) => json!(d),
        Enum(_, d) | Str(d) => json!(d),
    }
}

fn check(kind: &Kind, v: &Value) -> Result<Value, String> {
    match kind {
        Int(lo, hi, _) => v.as_i64().filter(|n| (lo..=hi).contains(&n)).map(|n| json!(n)).ok_or(format!("harus bilangan bulat {lo}–{hi}")),
        Float(lo, hi, _) => v.as_f64().filter(|n| (*lo..=*hi).contains(n)).map(|n| json!(n)).ok_or(format!("harus angka {lo}–{hi}")),
        Bool(_) => v.as_bool().map(|b| json!(b)).ok_or("harus true atau false".into()),
        Enum(opts, _) => v.as_str().filter(|s| opts.contains(s)).map(|s| json!(s)).ok_or(format!("harus salah satu dari {}", opts.join(", "))),
        Str(_) => v
            .as_str()
            .filter(|s| !s.is_empty() && s.len() <= 200 && !s.chars().any(char::is_control))
            .map(|s| json!(s))
            .ok_or("harus teks 1–200 karakter".into()),
    }
}

fn to_json(item: &toml_edit::Item) -> Value {
    if let Some(n) = item.as_integer() {
        json!(n)
    } else if let Some(f) = item.as_float() {
        json!(f)
    } else if let Some(b) = item.as_bool() {
        json!(b)
    } else if let Some(s) = item.as_str() {
        json!(s)
    } else {
        Value::Null
    }
}

/// `None` = file belum ada (semua nilai bawaan, tanpa galat).
pub fn load(text: Option<&str>) -> Config {
    let mut errors = Vec::new();
    let doc = match text.map(str::parse::<DocumentMut>) {
        Some(Err(e)) => {
            errors.push(format!("config.toml tidak bisa diurai: {}", e.to_string().trim()));
            return Config { values: defaults(), errors, broken: true, profiles: crate::profile::builtin() };
        }
        Some(Ok(d)) => Some(d),
        None => None,
    };
    let mut values = Map::new();
    for (section, spec) in SECTIONS {
        let table = doc.as_ref().and_then(|d| d.get(section));
        let mut out = Map::new();
        for (key, kind) in *spec {
            let v = match table.and_then(|t| t.get(key)) {
                None => default(kind),
                Some(item) => check(kind, &to_json(item)).unwrap_or_else(|why| {
                    errors.push(format!("{section}.{key}: {why}; memakai bawaan {}", default(kind)));
                    default(kind)
                }),
            };
            out.insert(key.to_string(), v);
        }
        if let Some(t) = table.and_then(|t| t.as_table_like()) {
            for (k, _) in t.iter().filter(|(k, _)| !spec.iter().any(|(s, _)| s == k)) {
                errors.push(format!("{section}.{k}: kunci tidak dikenal, diabaikan"));
            }
        }
        values.insert(section.to_string(), Value::Object(out));
    }
    // [keys] diteruskan apa adanya (teks → teks); UI yang tahu daftar aksi dan menandai bentrok
    let mut keys = Map::new();
    if let Some(t) = doc.as_ref().and_then(|d| d.get("keys")).and_then(|t| t.as_table_like()) {
        for (k, v) in t.iter() {
            match v.as_str() {
                Some(a) => drop(keys.insert(k.to_string(), json!(a))),
                None => errors.push(format!("keys.\"{k}\": nilai harus nama aksi (teks), diabaikan")),
            }
        }
    }
    values.insert("keys".into(), Value::Object(keys));
    let item = doc.as_ref().and_then(|d| d.get("profile"));
    let arr = item.and_then(|v| v.as_array_of_tables());
    if item.is_some() && arr.is_none() {
        errors.push("profile: harus ditulis sebagai [[profile]], diabaikan".into());
    }
    let tables: Vec<&dyn toml_edit::TableLike> = arr.map(|a| a.iter().map(|t| t as &dyn toml_edit::TableLike).collect()).unwrap_or_default();
    let (profiles, perr) = crate::profile::from_config(&tables);
    errors.extend(perr);
    values.insert("profiles".into(), profiles.iter().map(crate::profile::to_json).collect());
    Config { values: Value::Object(values), errors, broken: false, profiles }
}

fn defaults() -> Value {
    let mut values = Map::new();
    for (section, spec) in SECTIONS {
        let out: Map<String, Value> = spec.iter().map(|(k, kind)| (k.to_string(), default(kind))).collect();
        values.insert(section.to_string(), Value::Object(out));
    }
    values.insert("keys".into(), json!({}));
    values.insert("profiles".into(), crate::profile::builtin().iter().map(crate::profile::to_json).collect());
    Value::Object(values)
}

/// Ubah satu kunci dari Pengaturan. Komentar dan urutan file tetap; file rusak tidak disentuh.
pub fn set(path: &Path, section: &str, key: &str, value: &Value) -> Result<(), String> {
    let kind = SECTIONS
        .iter()
        .find(|(s, _)| *s == section)
        .and_then(|(_, spec)| spec.iter().find(|(k, _)| *k == key))
        .map(|(_, kind)| kind)
        .ok_or_else(|| format!("{section}.{key}: kunci tidak dikenal"))?;
    let v = check(kind, value).map_err(|why| format!("{section}.{key}: {why}"))?;
    let text = match fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e.to_string()),
    };
    let mut doc: DocumentMut = text.parse().map_err(|_| "config.toml rusak; perbaiki filenya dulu, Pengaturan tidak menimpanya".to_string())?;
    if doc.get(section).is_none() {
        doc[section] = toml_edit::table();
    }
    doc[section][key] = match v {
        Value::Bool(b) => toml_edit::value(b),
        Value::Number(n) if n.is_i64() => toml_edit::value(n.as_i64().unwrap()),
        Value::Number(n) => toml_edit::value(n.as_f64().unwrap_or_default()),
        Value::String(s) => toml_edit::value(s),
        _ => unreachable!("check hanya menghasilkan skalar"),
    };
    write_atomic(path, doc.to_string().as_bytes()).map_err(|e| e.to_string())
}

fn read_doc(path: &Path) -> Result<(DocumentMut, Vec<String>), String> {
    let text = match fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e.to_string()),
    };
    let doc: DocumentMut = text.parse().map_err(|_| "config.toml rusak; perbaiki filenya dulu, Pengaturan tidak menimpanya".to_string())?;
    Ok((doc, load(Some(&text)).errors))
}

/// Tulis hanya bila tidak menambah galat baru; galat baru dikembalikan ke Pengaturan, file tidak disentuh.
fn commit(path: &Path, doc: &DocumentMut, before: &[String]) -> Result<(), String> {
    let text = doc.to_string();
    let fresh: Vec<String> = load(Some(&text)).errors.into_iter().filter(|e| !before.contains(e)).collect();
    if !fresh.is_empty() {
        return Err(fresh.join("; "));
    }
    write_atomic(path, text.as_bytes()).map_err(|e| e.to_string())
}

/// FR-29: simpan profil dari Pengaturan ke `[[profile]]` dengan id yang sama (atau tambah baru).
pub fn save_profile(path: &Path, p: &Value) -> Result<(), String> {
    let id = p["id"].as_str().ok_or("id wajib diisi")?;
    let (mut doc, before) = read_doc(path)?;
    // gabung dengan entri lama: kunci yang tidak dikirim (mis. env yang nilainya tidak ditampilkan) tetap
    let old = doc
        .get("profile")
        .and_then(|v| v.as_array_of_tables())
        .and_then(|a| a.iter().find(|x| x.get("id").and_then(|v| v.as_str()) == Some(id)).cloned());
    let mut t = old.unwrap_or_default();
    t["id"] = toml_edit::value(id);
    for k in ["name", "command", "cwd"] {
        if let Some(v) = p[k].as_str() {
            t[k] = toml_edit::value(v);
        }
    }
    if let Some(args) = p["args"].as_array() {
        t["args"] = toml_edit::value(args.iter().filter_map(Value::as_str).collect::<toml_edit::Array>());
    }
    for k in ["summarize", "notify"] {
        if let Some(b) = p[k].as_bool() {
            t[k] = toml_edit::value(b);
        }
    }
    // env digabung per kunci (nilai lama tidak dikirim ke UI); secret_env diganti utuh (rujukannya tampil di form)
    for (k, merge) in [("env", true), ("secret_env", false)] {
        if let Some(m) = p[k].as_object() {
            let mut sub = match (merge, t.get(k).and_then(|v| v.as_table())) {
                (true, Some(old)) => old.clone(),
                _ => toml_edit::Table::new(),
            };
            for (name, v) in m {
                sub[name.as_str()] = toml_edit::value(v.as_str().unwrap_or_default());
            }
            t[k] = toml_edit::Item::Table(sub);
        }
    }
    if doc.get("profile").is_none() {
        doc["profile"] = toml_edit::Item::ArrayOfTables(toml_edit::ArrayOfTables::new());
    }
    let arr = doc["profile"].as_array_of_tables_mut().ok_or("profile: harus ditulis sebagai [[profile]]")?;
    let found = arr.iter().position(|x| x.get("id").and_then(|v| v.as_str()) == Some(id));
    match found.and_then(|i| arr.get_mut(i)) {
        Some(existing) => *existing = t,
        None => arr.push(t),
    }
    commit(path, &doc, &before)
}

/// FR-29: profil bawaan kembali ke asal / profil tambahan dihapus = semua `[[profile]]` dengan id itu dibuang.
pub fn reset_profile(path: &Path, id: &str) -> Result<(), String> {
    let (mut doc, before) = read_doc(path)?;
    if let Some(arr) = doc.get_mut("profile").and_then(|v| v.as_array_of_tables_mut()) {
        arr.retain(|x| x.get("id").and_then(|v| v.as_str()) != Some(id));
    }
    commit(path, &doc, &before)
}

/// Tulis ke file sementara di folder yang sama lalu `rename` (TECH bagian 5). Folder `0700` (SEC-25).
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let dir = path.parent().ok_or_else(|| io::Error::other("path tanpa folder"))?;
    // folder Gembala dan subfoldernya (hooks/) dibuat 0700; satu tingkat di atasnya cukup
    if let Some(up) = dir.parent().filter(|u| !u.exists()) {
        crate::rundir::ensure_private_dir(up)?;
    }
    crate::rundir::ensure_private_dir(dir)?;
    let name = path.file_name().map(|n| n.to_string_lossy()).unwrap_or_default();
    let tmp = dir.join(format!(".{name}.{}", std::process::id()));
    let mut f = fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tanpa_file_semua_bawaan() {
        let c = load(None);
        assert!(c.errors.is_empty() && !c.broken);
        assert_eq!(c.values["ui"]["font_size"], 13);
        assert_eq!(c.values["limits"]["max_panes"], 32);
    }

    #[test]
    fn kunci_rusak_pakai_bawaan_sisanya_tetap() {
        let c = load(Some("[ui]\nfont_size = 99\ncursor = \"block\"\ntheme = 3\nwarna = 1\n[keys]\n\"cmd+t\" = \"pane.split.right\"\n\"cmd+y\" = 5\n"));
        assert!(!c.broken);
        assert_eq!(c.values["ui"]["font_size"], 13);
        assert_eq!(c.values["ui"]["cursor"], "block");
        assert_eq!(c.values["ui"]["theme"], "system");
        assert_eq!(c.values["keys"]["cmd+t"], "pane.split.right");
        let all = c.errors.join("\n");
        for want in ["ui.font_size", "ui.theme", "ui.warna", "cmd+y"] {
            assert!(all.contains(want), "{want} tidak ditandai: {all}");
        }
    }

    #[test]
    fn file_tidak_terurai_ditandai_broken() {
        let c = load(Some("[ui\nfont_size = "));
        assert!(c.broken);
        assert_eq!(c.values["ui"]["font_size"], 13);
    }

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("gembala-cfg-{}-{tag}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        d.join("config.toml")
    }

    #[test]
    fn set_menjaga_komentar_dan_menolak_nilai_salah() {
        let p = scratch("set");
        set(&p, "ui", "font_size", &json!(15)).unwrap(); // file dan folder belum ada
        let mut text = fs::read_to_string(&p).unwrap();
        text = format!("# punya pengguna\n{text}[keys]\n\"cmd+t\" = \"pane.split.right\" # komentar\n");
        fs::write(&p, &text).unwrap();
        set(&p, "ui", "cursor", &json!("block")).unwrap();
        let after = fs::read_to_string(&p).unwrap();
        assert!(after.contains("# punya pengguna") && after.contains("# komentar"), "{after}");
        assert_eq!(load(Some(&after)).values["ui"]["cursor"], "block");
        assert!(set(&p, "ui", "font_size", &json!(99)).is_err());
        assert!(set(&p, "ui", "warna", &json!(1)).is_err());
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(fs::metadata(p.parent().unwrap()).unwrap().permissions().mode() & 0o777, 0o700);
    }

    #[test]
    fn profil_disimpan_dan_dikembalikan() {
        let p = scratch("profil");
        set(&p, "ui", "font_size", &json!(14)).unwrap();
        let pr = json!({"id": "claude", "args": ["--model", "opus"], "env": {"CLAUDE_CONFIG_DIR": "~/.claude"}, "summarize": false});
        save_profile(&p, &pr).unwrap();
        save_profile(&p, &json!({"id": "lazygit", "name": "Lazygit", "command": "lazygit"})).unwrap();
        let c = load(Some(&fs::read_to_string(&p).unwrap()));
        let claude = c.profiles.iter().find(|x| x.id == "claude").unwrap();
        assert_eq!((claude.args.len(), claude.summarize), (2, false));
        assert!(c.profiles.iter().any(|x| x.id == "lazygit"));
        assert_eq!(c.values["ui"]["font_size"], 14, "bagian lain tetap");
        // ubah tanpa mengirim env: env lama tetap
        save_profile(&p, &json!({"id": "claude", "name": "Claude"})).unwrap();
        let c = load(Some(&fs::read_to_string(&p).unwrap()));
        assert_eq!(c.profiles.iter().find(|x| x.id == "claude").unwrap().env.len(), 1);
        // rahasia di env biasa ditolak, file tidak berubah
        let before = fs::read_to_string(&p).unwrap();
        let bad = save_profile(&p, &json!({"id": "claude", "env": {"KEY": "sk-ant-123"}})).unwrap_err();
        assert!(bad.contains("rahasia"), "{bad}");
        assert_eq!(fs::read_to_string(&p).unwrap(), before);
        reset_profile(&p, "claude").unwrap();
        let c = load(Some(&fs::read_to_string(&p).unwrap()));
        assert_eq!(c.profiles.iter().find(|x| x.id == "claude").unwrap().args.len(), 0);
    }

    #[test]
    fn set_tidak_menimpa_file_rusak() {
        let p = scratch("rusak");
        crate::rundir::ensure_private_dir(p.parent().unwrap()).unwrap();
        fs::write(&p, "[ui\n").unwrap();
        assert!(set(&p, "ui", "font_size", &json!(14)).is_err());
        assert_eq!(fs::read_to_string(&p).unwrap(), "[ui\n");
    }
}
