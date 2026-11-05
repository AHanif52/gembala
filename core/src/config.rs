//! `config.toml` (TECH bagian 5, FR-42): validasi per kunci, nilai bawaan untuk yang rusak,
//! file tidak pernah ditimpa otomatis. Tulis dari Pengaturan lewat `toml_edit` supaya komentar tetap.

use serde_json::{Map, Value, json};
use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use toml_edit::DocumentMut;

use crate::diag::Diag;

enum Kind {
    Int(i64, i64, i64), // min, max, bawaan
    Float(f64, f64, f64),
    Bool(bool),
    Enum(&'static [&'static str], &'static str),
    Str(&'static str),
    /// Daftar id dari pilihan tetap, urutan = tampilan (bawaan)
    List(&'static [&'static str], &'static [&'static str]),
}

use Kind::*;

// ponytail: hanya bagian yang sudah dipakai app; notify/app/toolbar/workspace/profile ditambah di M4–M7
const UI: &[(&str, Kind)] = &[
    // BRD 11: system = first of id/en in the macOS language list, else English (TECH 20)
    ("language", Enum(&["system", "id", "en"], "system")),
    ("theme", Enum(&["system", "light", "dark"], "system")),
    ("font_family", Str("Hack")),
    ("font_size", Int(8, 32, 13)),
    ("line_height", Float(1.0, 2.0, 1.2)),
    ("cursor", Enum(&["bar", "block", "underline"], "bar")),
    ("cursor_blink", Bool(true)),
    ("option_as_meta", Enum(&["none", "left", "right", "both"], "left")),
    // xterm sekitar 12 byte per sel: 5.000 baris × 200 kolom ≈ 12 MB per pane bila penuh
    ("scrollback_lines", Int(0, 100_000, 5_000)),
    ("screen_reader", Bool(false)),
    // FR-59: input editor di prompt zsh; prompt shell disembunyikan (berlaku untuk pane baru)
    ("input_editor", Bool(true)),
    // FR-58: linimasa blok perintah di pane shell (garis, titik, kepala blok, tombol Ulangi/Ubah/Salin);
    // mati = terminal polos, penanda OSC 133 tetap dilacak untuk lompat blok
    ("command_timeline", Bool(true)),
    // FR-20: Cmd+Q = app ditutup, agent tetap jalan (keep) atau semua pane dihentikan (stop)
    ("on_quit", Enum(&["keep", "stop"], "keep")),
];
const CHIPS: &[&str] = &["attach", "diff", "rich_input", "cwd", "branch", "settings"];
/// FR-55: isi dan urutan chip toolbar agent
const TOOLBAR: &[(&str, Kind)] = &[("left", List(CHIPS, &["attach", "diff", "rich_input"])), ("right", List(CHIPS, &["cwd", "branch", "settings"]))];
const LIMITS: &[(&str, Kind)] = &[("max_panes", Int(1, 256, 32))];
/// FR-40: banner macOS + badge Dock + Dock melompat
const NOTIFY: &[(&str, Kind)] =
    &[("on_blocked", Bool(true)), ("on_done", Bool(true)), ("long_command_secs", Int(5, 3600, 30)), ("sound", Bool(true))];
const SECTIONS: &[(&str, &[(&str, Kind)])] = &[("ui", UI), ("limits", LIMITS), ("notify", NOTIFY), ("toolbar", TOOLBAR)];

pub struct Config {
    /// Profil bawaan + `[[profile]]` yang lolos validasi (FR-29). Rujukan rahasia ikut, nilainya tidak.
    pub profiles: Vec<crate::profile::Profile>,
    /// `{ui, limits, keys}` yang berlaku (nilai bawaan untuk yang rusak atau tidak ada).
    pub values: Value,
    pub errors: Vec<Diag>,
    /// File tidak bisa diurai sama sekali: pemanggil memakai nilai valid terakhir.
    pub broken: bool,
    /// FR-61: `[[account]]` yang lolos validasi
    pub accounts: Vec<crate::account::Account>,
    /// FR-78: plugin and per-workspace MCP add-ons that passed validation
    pub addons: crate::addon::Addons,
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
        List(_, d) => json!(d),
    }
}

fn check(kind: &Kind, v: &Value) -> Result<Value, Diag> {
    match kind {
        Int(lo, hi, _) => v.as_i64().filter(|n| (lo..=hi).contains(&n)).map(|n| json!(n)).ok_or_else(|| Diag::new("int_range").arg("lo", lo).arg("hi", hi)),
        Float(lo, hi, _) => v.as_f64().filter(|n| (*lo..=*hi).contains(n)).map(|n| json!(n)).ok_or_else(|| Diag::new("num_range").arg("lo", lo).arg("hi", hi)),
        Bool(_) => v.as_bool().map(|b| json!(b)).ok_or("bool".into()),
        Enum(opts, _) => v.as_str().filter(|s| opts.contains(s)).map(|s| json!(s)).ok_or_else(|| Diag::new("one_of").arg("opts", opts.join(", "))),
        List(opts, _) => v
            .as_array()
            .filter(|a| a.len() <= opts.len() && a.iter().all(|x| x.as_str().is_some_and(|s| opts.contains(&s))))
            .map(|a| Value::Array(a.clone()))
            .ok_or_else(|| Diag::new("list_of").arg("opts", opts.join(", "))),
        Str(_) => v
            .as_str()
            .filter(|s| !s.is_empty() && s.len() <= 200 && !s.chars().any(char::is_control))
            .map(|s| json!(s))
            .ok_or("text_len".into()),
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
    } else if let Some(a) = item.as_array() {
        Value::Array(a.iter().map(|v| v.as_str().map_or(Value::Null, |s| json!(s))).collect())
    } else {
        Value::Null
    }
}

/// `None` = file belum ada (semua nilai bawaan, tanpa galat).
pub fn load(text: Option<&str>) -> Config {
    let mut errors = Vec::new();
    let doc = match text.map(str::parse::<DocumentMut>) {
        Some(Err(e)) => {
            errors.push(Diag::new("toml_parse").arg("why", e.to_string().trim()));
            return Config { values: defaults(), errors, broken: true, profiles: crate::profile::builtin(), accounts: Vec::new(), addons: Default::default() };
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
                    errors.push(why.at(format!("{section}.{key}")).then("default").arg("default", default(kind)));
                    default(kind)
                }),
            };
            out.insert(key.to_string(), v);
        }
        if let Some(t) = table.and_then(|t| t.as_table_like()) {
            for (k, _) in t.iter().filter(|(k, _)| !spec.iter().any(|(s, _)| s == k)) {
                errors.push(Diag::new("unknown_key").at(format!("{section}.{k}")).then("ignored"));
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
                None => errors.push(Diag::new("key_not_action").at(format!("keys.\"{k}\"")).then("ignored")),
            }
        }
    }
    values.insert("keys".into(), Value::Object(keys));
    let item = doc.as_ref().and_then(|d| d.get("profile"));
    let arr = item.and_then(|v| v.as_array_of_tables());
    if item.is_some() && arr.is_none() {
        errors.push(Diag::new("not_array_table").at("profile").then("ignored"));
    }
    let tables: Vec<&dyn toml_edit::TableLike> = arr.map(|a| a.iter().map(|t| t as &dyn toml_edit::TableLike).collect()).unwrap_or_default();
    let (mut profiles, perr) = crate::profile::from_config(&tables);
    errors.extend(perr);
    // FR-61: akun, lalu rujukan akun di profil (akun tak dikenal atau env ganda = profil memakai Utama)
    let item = doc.as_ref().and_then(|d| d.get("account"));
    let arr = item.and_then(|v| v.as_array_of_tables());
    if item.is_some() && arr.is_none() {
        errors.push(Diag::new("not_array_table").at("account").then("ignored"));
    }
    let tables: Vec<&dyn toml_edit::TableLike> = arr.map(|a| a.iter().map(|t| t as &dyn toml_edit::TableLike).collect()).unwrap_or_default();
    let (accounts, aerr) = crate::account::from_config(&tables);
    errors.extend(aerr);
    for p in profiles.iter_mut().filter(|p| !p.account.is_empty()) {
        let why = if !accounts.iter().any(|a| a.id == p.account && a.agent == p.agent) {
            Some("no_such_account")
        } else if crate::account::conflicts(&p.env) {
            Some("account_env_conflict")
        } else {
            None
        };
        if let Some(why) = why {
            errors.push(Diag::new(why).at(format!("profile {}.account", p.id)).then("main_account"));
            p.account.clear();
        }
    }
    values.insert("profiles".into(), profiles.iter().map(crate::profile::to_json).collect());
    values.insert("accounts".into(), accounts.iter().map(crate::account::to_json).collect());
    let (addons, aerr) = crate::addon::from_config(doc.as_ref(), path().parent().unwrap_or(Path::new("/")));
    errors.extend(aerr);
    values.insert("addons".into(), crate::addon::to_json(&addons));
    Config { values: Value::Object(values), errors, broken: false, profiles, accounts, addons }
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
pub fn set(path: &Path, section: &str, key: &str, value: &Value) -> Result<(), Diag> {
    let kind = SECTIONS
        .iter()
        .find(|(s, _)| *s == section)
        .and_then(|(_, spec)| spec.iter().find(|(k, _)| *k == key))
        .map(|(_, kind)| kind)
        .ok_or_else(|| Diag::new("unknown_key").at(format!("{section}.{key}")))?;
    let v = check(kind, value).map_err(|why| why.at(format!("{section}.{key}")))?;
    let text = match fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(io_err("io_read", e)),
    };
    let mut doc: DocumentMut = text.parse().map_err(|_| Diag::new("config_broken"))?;
    if doc.get(section).is_none() {
        doc[section] = toml_edit::table();
    }
    doc[section][key] = match v {
        Value::Bool(b) => toml_edit::value(b),
        Value::Number(n) if n.is_i64() => toml_edit::value(n.as_i64().unwrap()),
        Value::Number(n) => toml_edit::value(n.as_f64().unwrap_or_default()),
        Value::String(s) => toml_edit::value(s),
        Value::Array(a) => toml_edit::value(a.iter().filter_map(Value::as_str).collect::<toml_edit::Array>()),
        _ => unreachable!("check hanya menghasilkan skalar atau daftar teks"),
    };
    write_atomic(path, doc.to_string().as_bytes()).map_err(|e| io_err("io_write", e))
}

/// OS error while reading/writing config: the OS text stays as is
pub fn io_err(code: &'static str, e: io::Error) -> Diag {
    Diag::new(code).arg("os", e)
}

fn read_doc(path: &Path) -> Result<(DocumentMut, Vec<Diag>), Diag> {
    let text = match fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(io_err("io_read", e)),
    };
    let doc: DocumentMut = text.parse().map_err(|_| Diag::new("config_broken"))?;
    Ok((doc, load(Some(&text)).errors))
}

/// Tulis hanya bila tidak menambah galat baru; galat baru pertama dikembalikan ke Pengaturan, file tidak disentuh.
fn commit(path: &Path, doc: &DocumentMut, before: &[Diag]) -> Result<(), Diag> {
    let text = doc.to_string();
    if let Some(fresh) = load(Some(&text)).errors.into_iter().find(|e| !before.contains(e)) {
        return Err(fresh);
    }
    write_atomic(path, text.as_bytes()).map_err(|e| io_err("io_write", e))
}

/// FR-29: simpan profil dari Pengaturan ke `[[profile]]` dengan id yang sama (atau tambah baru).
pub fn save_profile(path: &Path, p: &Value) -> Result<(), Diag> {
    let id = p["id"].as_str().ok_or_else(|| Diag::new("required").at("id"))?;
    let (mut doc, before) = read_doc(path)?;
    // gabung dengan entri lama: kunci yang tidak dikirim (mis. env yang nilainya tidak ditampilkan) tetap
    let old = doc
        .get("profile")
        .and_then(|v| v.as_array_of_tables())
        .and_then(|a| a.iter().find(|x| x.get("id").and_then(|v| v.as_str()) == Some(id)).cloned());
    let mut t = old.unwrap_or_default();
    t["id"] = toml_edit::value(id);
    for k in ["name", "command", "cwd", "policy", "permission", "account"] {
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
    let arr = doc["profile"].as_array_of_tables_mut().ok_or_else(|| Diag::new("not_array_table").at("profile"))?;
    let found = arr.iter().position(|x| x.get("id").and_then(|v| v.as_str()) == Some(id));
    match found.and_then(|i| arr.get_mut(i)) {
        Some(existing) => *existing = t,
        None => arr.push(t),
    }
    commit(path, &doc, &before)
}

/// FR-39: pintasan dari Pengaturan. Kombinasi lama untuk aksi itu dibuang; `combo` kosong = kembali ke bawaan.
/// Kombinasi dan nama aksi diperiksa lagi (wajib cmd, bentrok) oleh sisi Rust app yang membangun menu.
pub fn set_key(path: &Path, action: &str, combo: &str) -> Result<(), Diag> {
    let ok = |s: &str, extra: &str| s.len() <= 64 && s.chars().all(|c| c.is_ascii_alphanumeric() || extra.contains(c));
    if action.is_empty() || !ok(action, "._") || !ok(combo, "+.,=-;'/[]\\`") {
        return Err("bad_combo".into());
    }
    let (mut doc, before) = read_doc(path)?;
    if doc.get("keys").is_none() {
        doc["keys"] = toml_edit::table();
    }
    let keys = doc["keys"].as_table_like_mut().ok_or_else(|| Diag::new("not_table").at("keys"))?;
    let old: Vec<String> = keys.iter().filter(|(_, v)| v.as_str() == Some(action)).map(|(k, _)| k.to_string()).collect();
    for k in old {
        keys.remove(&k);
    }
    if !combo.is_empty() {
        keys.insert(&combo.to_lowercase(), toml_edit::value(action));
    }
    commit(path, &doc, &before)
}

/// FR-61: tambah `[[account]]` (id unik per agent; validasi penuh oleh `load` lewat `commit`).
pub fn save_account(path: &Path, a: &crate::account::Account) -> Result<(), Diag> {
    let (mut doc, before) = read_doc(path)?;
    if !doc.contains_key("account") {
        doc["account"] = toml_edit::Item::ArrayOfTables(toml_edit::ArrayOfTables::new());
    }
    let arr = doc["account"].as_array_of_tables_mut().ok_or_else(|| Diag::new("not_array_table").at("account"))?;
    if arr.iter().any(|x| x.get("id").and_then(|v| v.as_str()) == Some(a.id.as_str())) {
        return Err(Diag::new("account_exists").arg("id", &a.id));
    }
    let mut t = toml_edit::Table::new();
    t["id"] = toml_edit::value(a.id.as_str());
    t["agent"] = toml_edit::value(a.agent.as_str());
    t["name"] = toml_edit::value(a.name.as_str());
    t["dir"] = toml_edit::value(a.dir.as_str());
    t["share"] = toml_edit::value(a.share.iter().map(String::as_str).collect::<toml_edit::Array>());
    arr.push(t);
    commit(path, &doc, &before)
}

/// FR-61 (8): ubah setelan akun yang sudah ada (share, model, effort, trust); validasi penuh oleh `load` lewat `commit`.
pub fn update_account(path: &Path, a: &crate::account::Account) -> Result<(), Diag> {
    let (mut doc, before) = read_doc(path)?;
    let t = doc
        .get_mut("account")
        .and_then(|v| v.as_array_of_tables_mut())
        .and_then(|arr| arr.iter_mut().find(|x| x.get("id").and_then(|v| v.as_str()) == Some(a.id.as_str())))
        .ok_or_else(|| Diag::new("no_such_account").arg("id", &a.id))?;
    t["share"] = toml_edit::value(a.share.iter().map(String::as_str).collect::<toml_edit::Array>());
    for (k, v) in [("model", &a.model), ("effort", &a.effort)] {
        if v.is_empty() {
            t.remove(k);
        } else {
            t[k] = toml_edit::value(v.as_str());
        }
    }
    if a.trust {
        t.remove("trust");
    } else {
        t["trust"] = toml_edit::value(false);
    }
    commit(path, &doc, &before)
}

/// FR-61: hapus `[[account]]`; folder, tautan, dan Keychain tidak disentuh.
pub fn remove_account(path: &Path, id: &str) -> Result<(), Diag> {
    let (mut doc, before) = read_doc(path)?;
    if let Some(arr) = doc.get_mut("account").and_then(|v| v.as_array_of_tables_mut()) {
        arr.retain(|x| x.get("id").and_then(|v| v.as_str()) != Some(id));
    }
    // profil yang merujuk akun ini kembali ke Utama (tanpa ini commit menolak karena rujukan jadi galat)
    if let Some(arr) = doc.get_mut("profile").and_then(|v| v.as_array_of_tables_mut()) {
        for t in arr.iter_mut().filter(|t| t.get("account").and_then(|v| v.as_str()) == Some(id)) {
            t.remove("account");
        }
    }
    commit(path, &doc, &before)
}

/// Implicit parent table (`[a.b]` without a bare `[a]` header), created when missing.
fn implicit<'a>(t: &'a mut toml_edit::Table, key: &str) -> Result<&'a mut toml_edit::Table, Diag> {
    if !t.contains_key(key) {
        let mut n = toml_edit::Table::new();
        n.set_implicit(true);
        t.insert(key, toml_edit::Item::Table(n));
    }
    t[key].as_table_mut().ok_or_else(|| Diag::new("not_table").at(key))
}

/// FR-78: add or change `[addons.plugin.<id>]` (only the given fields), or remove it; validated by `load` via `commit`.
pub fn set_plugin(path: &Path, id: &str, dir: Option<&str>, scope: Option<&str>, workspaces: Option<&[String]>, remove: bool) -> Result<(), Diag> {
    let (mut doc, before) = read_doc(path)?;
    let plugins = implicit(implicit(doc.as_table_mut(), "addons")?, "plugin")?;
    if remove {
        plugins.remove(id);
        return commit(path, &doc, &before);
    }
    if !plugins.contains_key(id) && dir.is_none() {
        return Err(Diag::new("no_such_plugin").arg("id", id));
    }
    let t = implicit(plugins, id)?;
    t.set_implicit(false);
    if let Some(d) = dir {
        t["path"] = toml_edit::value(d);
    }
    if let Some(s) = scope {
        t["scope"] = toml_edit::value(s);
    }
    if let Some(w) = workspaces {
        t["workspaces"] = toml_edit::value(w.iter().map(String::as_str).collect::<toml_edit::Array>());
    }
    commit(path, &doc, &before)
}

/// FR-78: `[workspace.<id>.mcp]` = file + strict, or removed (`None`).
pub fn set_mcp(path: &Path, ws: &str, mcp: Option<(&str, bool)>) -> Result<(), Diag> {
    let (mut doc, before) = read_doc(path)?;
    let w = implicit(implicit(doc.as_table_mut(), "workspace")?, ws)?;
    match mcp {
        Some((file, strict)) => {
            let t = implicit(w, "mcp")?;
            t.set_implicit(false);
            t["file"] = toml_edit::value(file);
            t["strict"] = toml_edit::value(strict);
        }
        None => drop(w.remove("mcp")),
    }
    commit(path, &doc, &before)
}

/// FR-29: profil bawaan kembali ke asal / profil tambahan dihapus = semua `[[profile]]` dengan id itu dibuang.
pub fn reset_profile(path: &Path, id: &str) -> Result<(), Diag> {
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
        let all = c.errors.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n");
        for want in ["ui.font_size", "ui.theme", "ui.warna", "cmd+y"] {
            assert!(all.contains(want), "{want} tidak ditandai: {all}");
        }
    }

    #[test]
    fn language_enum() {
        assert_eq!(load(None).values["ui"]["language"], "system");
        assert_eq!(load(Some("[ui]\nlanguage = \"en\"\n")).values["ui"]["language"], "en");
        let c = load(Some("[ui]\nlanguage = \"fr\"\n"));
        assert_eq!(c.values["ui"]["language"], "system");
        assert!(c.errors.iter().map(ToString::to_string).collect::<Vec<_>>().join("").contains("ui.language"), "{:?}", c.errors);
    }

    #[test]
    fn linimasa_perintah_dari_config() {
        // FR-58: bawaan nyala; false dari config bertahan, nilai bukan bool jatuh ke bawaan dan ditandai
        assert_eq!(load(None).values["ui"]["command_timeline"], true);
        let c = load(Some("[ui]\ncommand_timeline = false\n"));
        assert_eq!(c.values["ui"]["command_timeline"], false);
        assert!(c.errors.is_empty(), "{:?}", c.errors);
        let c = load(Some("[ui]\ncommand_timeline = \"mati\"\n"));
        assert_eq!(c.values["ui"]["command_timeline"], true);
        assert!(c.errors.iter().map(ToString::to_string).collect::<Vec<_>>().join("").contains("ui.command_timeline"), "{:?}", c.errors);
        // ditulis lewat config.set, bukan hanya dibaca
        let p = scratch("linimasa");
        set(&p, "ui", "command_timeline", &json!(false)).unwrap();
        assert_eq!(load(Some(&fs::read_to_string(&p).unwrap())).values["ui"]["command_timeline"], false);
    }

    #[test]
    fn toolbar_dari_config() {
        let c = load(Some("[toolbar]\nleft = [\"rich_input\", \"diff\"]\nright = [\"branch\", \"jam\"]\n"));
        assert_eq!(c.values["toolbar"]["left"], json!(["rich_input", "diff"]));
        assert_eq!(c.values["toolbar"]["right"], json!(["cwd", "branch", "settings"]), "rusak = bawaan");
        assert!(c.errors.iter().map(ToString::to_string).collect::<Vec<_>>().join("").contains("toolbar.right"));
        let p = scratch("toolbar");
        set(&p, "toolbar", "left", &json!(["diff"])).unwrap();
        assert_eq!(load(Some(&fs::read_to_string(&p).unwrap())).values["toolbar"]["left"], json!(["diff"]));
    }

    #[test]
    fn pintasan_dari_pengaturan() {
        let p = scratch("keys");
        set_key(&p, "pane.split.right", "Cmd+T").unwrap();
        set_key(&p, "pane.split.right", "cmd+e").unwrap(); // menggantikan, bukan menambah
        let c = load(Some(&fs::read_to_string(&p).unwrap()));
        assert_eq!(c.values["keys"], json!({"cmd+e": "pane.split.right"}));
        set_key(&p, "pane.split.right", "").unwrap();
        assert_eq!(load(Some(&fs::read_to_string(&p).unwrap())).values["keys"], json!({}));
        assert!(set_key(&p, "x\ny", "cmd+a").is_err());
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
        assert_eq!(bad.code, "env_secret", "{bad}");
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
