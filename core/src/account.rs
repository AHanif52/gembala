//! FR-61, C-9: akun agent = satu folder config agent yang dipakai lewat env resmi (`CLAUDE_CONFIG_DIR`).
//! Gembala tidak menyentuh login atau token (Keychain milik agent); tidak ada jalur yang memilih akun selain manusia.

use serde_json::{Value, json};
use std::path::{Path, PathBuf};

/// Butir folder Utama yang boleh ditautkan ke akun baru (bukan input bebas). `projects` = memori + riwayat, opsional.
pub const SHARE: &[&str] = &["CLAUDE.md", "settings.json", "skills", "commands", "agents", "plugins", "projects"];
pub const SHARE_DEFAULT: &[&str] = &["CLAUDE.md", "settings.json", "skills", "commands", "agents", "plugins"];
const ENV: &str = "CLAUDE_CONFIG_DIR";

#[derive(Clone, Debug, PartialEq)]
pub struct Account {
    pub id: String,
    pub agent: String,
    pub name: String,
    /// seperti ditulis di config (mis. `~/.claude-kantor`)
    pub dir: String,
    pub share: Vec<String>,
}

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| "/".into())
}

/// Folder Utama Claude Code.
pub fn main_dir() -> PathBuf {
    home().join(".claude")
}

/// `~/x` → `$HOME/x`; selain itu apa adanya.
pub fn expand(dir: &str) -> PathBuf {
    match dir.strip_prefix("~/") {
        Some(rest) => home().join(rest),
        None => PathBuf::from(dir),
    }
}

/// Folder akun sah: di bawah `$HOME` (tanpa `..`, tidak menembus lewat symlink), bukan folder Utama atau home sendiri.
pub fn valid_dir(dir: &str) -> Result<PathBuf, String> {
    let p = expand(dir);
    if !p.is_absolute() || p.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
        return Err("folder harus path di bawah home tanpa ..".into());
    }
    let h = home();
    // bagian yang sudah ada diresolusi: symlink yang menunjuk keluar home ditolak
    let real = p.ancestors().find(|a| a.exists()).and_then(|a| a.canonicalize().ok().map(|r| r.join(p.strip_prefix(a).unwrap_or(Path::new("")))));
    let real = real.unwrap_or(p.clone());
    let home_real = h.canonicalize().unwrap_or(h.clone());
    if !real.starts_with(&home_real) || real == home_real {
        return Err("folder harus di bawah folder home".into());
    }
    let main = main_dir().canonicalize().unwrap_or(main_dir());
    if real == main || real.starts_with(&main) {
        return Err("folder tidak boleh folder Utama (~/.claude) atau di dalamnya".into());
    }
    Ok(p)
}

fn valid_id(id: &str) -> bool {
    (1..=32).contains(&id.len()) && id.bytes().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-') && id != "utama"
}

/// `[[account]]` dari config.toml; yang tidak lolos dilewati dengan galat (FR-42).
pub fn from_config(tables: &[&dyn toml_edit::TableLike]) -> (Vec<Account>, Vec<String>) {
    let (mut out, mut errors): (Vec<Account>, Vec<String>) = (Vec::new(), Vec::new());
    for (i, t) in tables.iter().enumerate() {
        let at = |k: &str| format!("account[{i}].{k}");
        let s = |k: &str| t.get(k).and_then(|v| v.as_str()).map(str::to_string);
        let Some(id) = s("id").filter(|x| valid_id(x)) else {
            errors.push(format!("{}: wajib a-z, 0-9, '-', maks 32, bukan \"utama\"; akun dilewati", at("id")));
            continue;
        };
        let agent = s("agent").unwrap_or_else(|| "claude".into());
        if agent != "claude" {
            errors.push(format!("{}: baru \"claude\" yang didukung; akun dilewati", at("agent")));
            continue;
        }
        let name = s("name").filter(|n| (1..=60).contains(&n.chars().count()) && !n.chars().any(char::is_control)).unwrap_or_else(|| id.clone());
        let Some(dir) = s("dir") else {
            errors.push(format!("{}: wajib diisi; akun dilewati", at("dir")));
            continue;
        };
        if let Err(e) = valid_dir(&dir) {
            errors.push(format!("{}: {e}; akun dilewati", at("dir")));
            continue;
        }
        if out.iter().any(|a| a.agent == agent && (a.id == id || expand(&a.dir) == expand(&dir))) {
            errors.push(format!("{}: id atau folder sudah dipakai akun lain; akun dilewati", at("id")));
            continue;
        }
        let share = match t.get("share").and_then(|v| v.as_array()) {
            None => SHARE_DEFAULT.iter().map(|x| x.to_string()).collect(),
            Some(a) => a.iter().filter_map(|v| v.as_str()).filter(|x| SHARE.contains(x)).map(String::from).collect(),
        };
        out.push(Account { id, agent, name, dir, share });
    }
    (out, errors)
}

/// Env yang diekspor peluncur agent (nilai `~/…` dibuka shell, bagian 6).
pub fn env(a: &Account) -> (String, String) {
    (ENV.into(), a.dir.clone())
}

pub fn conflicts(env: &[(String, String)]) -> bool {
    env.iter().any(|(k, _)| k == ENV)
}

/// FR-61, C-9: akun pane = pilihan saat membuka pane (`chosen`, dari UI termasuk bawaan workspace) > akun profil
/// > Utama. Env akun ditambahkan ke profil; hasilnya nama akun untuk label (kosong = Utama).
pub fn apply(p: &mut crate::profile::Profile, chosen: Option<&str>, accounts: &[Account]) -> Result<String, &'static str> {
    let want = chosen.filter(|s| !s.is_empty()).map(String::from).unwrap_or_else(|| p.account.clone());
    if want.is_empty() || want == "utama" {
        return Ok(String::new());
    }
    let a = accounts.iter().find(|a| a.id == want && a.agent == p.agent).ok_or("no_such_account")?;
    if conflicts(&p.env) {
        return Err("account_env_conflict");
    }
    p.env.push(env(a));
    Ok(a.name.clone())
}

/// Buat folder akun (0700) dan tautan butir `share` dari Utama. Butir yang sudah ada tidak ditimpa;
/// dikembalikan sebagai daftar yang dilewati.
pub fn create(a: &Account) -> Result<Vec<String>, String> {
    use std::os::unix::fs::PermissionsExt;
    let dir = valid_dir(&a.dir)?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("buat folder: {e}"))?;
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).map_err(|e| format!("izin folder: {e}"))?;
    let main = main_dir();
    let mut skipped = Vec::new();
    for item in a.share.iter().filter(|x| SHARE.contains(&x.as_str())) {
        let (src, dst) = (main.join(item), dir.join(item));
        if !src.exists() {
            continue;
        }
        if dst.symlink_metadata().is_ok() {
            skipped.push(item.clone());
            continue;
        }
        std::os::unix::fs::symlink(&src, &dst).map_err(|e| format!("tautan {item}: {e}"))?;
    }
    Ok(skipped)
}

/// FR-61 (7): folder akun ikut dihapus. `remove_dir_all` tidak mengikuti symlink, jadi tautan ke Utama hanya dilepas.
/// Hanya folder `.claude…` yang bukan symlink, supaya folder lain yang kebetulan ditunjuk akun tidak terhapus.
pub fn delete_dir(dir: &str) -> Result<(), String> {
    let p = valid_dir(dir)?;
    let Ok(meta) = p.symlink_metadata() else { return Ok(()) }; // folder sudah tidak ada
    let named = p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with(".claude"));
    if !named || !meta.is_dir() {
        return Err(format!("{dir} bukan folder akun Claude; tidak dihapus"));
    }
    std::fs::remove_dir_all(&p).map_err(|e| format!("hapus folder: {e}"))
}

/// Status akun untuk daftar: sudah dipakai (`.claude.json` ada) dan email dari `oauthAccount.emailAddress` saja.
/// Field lain dibuang; file > 5 MB atau rusak = tanpa email. Token tidak ada di file ini (Keychain).
pub fn status(config_json: &Path) -> Value {
    let used = config_json.exists();
    let email = std::fs::metadata(config_json)
        .ok()
        .filter(|m| m.len() <= 5 << 20)
        .and_then(|_| std::fs::read_to_string(config_json).ok())
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| v["oauthAccount"]["emailAddress"].as_str().map(|e| e.chars().filter(|c| !c.is_control()).take(120).collect::<String>()));
    json!({"used": used, "email": email})
}

/// `.claude.json` akun: Utama di home, akun lain di dalam foldernya (terbukti 2 Nov 2026, Claude Code 2.1.309).
pub fn config_json(a: Option<&Account>) -> PathBuf {
    match a {
        None => home().join(".claude.json"),
        Some(a) => expand(&a.dir).join(".claude.json"),
    }
}

/// FR-61: folder yang dipercaya di Utama ikut dipercaya di akun `dir`. Satu arah dan hanya menambah:
/// `.claude.json` Utama hanya dibaca; yang ditulis ke akun hanya `projects.<folder>.hasTrustDialogAccepted`.
/// Akun yang belum pernah dipakai (file belum ada) dilewati. Hasil = jumlah folder yang ditambahkan.
pub fn sync_trust(dir: &str) -> usize {
    let Ok(dir) = valid_dir(dir) else { return 0 };
    let read = |p: &Path| -> Option<Value> {
        std::fs::metadata(p).ok().filter(|m| m.len() <= 32 << 20)?;
        serde_json::from_str(&std::fs::read_to_string(p).ok()?).ok()
    };
    let (Some(main), target_path) = (read(&config_json(None)), dir.join(".claude.json")) else { return 0 };
    let Some(mut target) = read(&target_path) else { return 0 };
    let n = merge_trust(&main, &mut target);
    if n == 0 {
        return 0;
    }
    // salinan sebelum ditulis (satu, ditimpa tiap sinkron) supaya file akun bisa dipulihkan
    let _ = std::fs::copy(&target_path, dir.join(".claude.json.gembala-bak"));
    if crate::config::write_atomic(&target_path, target.to_string().as_bytes()).is_err() {
        return 0;
    }
    n
}

fn merge_trust(main: &Value, target: &mut Value) -> usize {
    let Some(projects) = main["projects"].as_object() else { return 0 };
    if !target.is_object() || !(target["projects"].is_object() || target["projects"].is_null()) {
        return 0;
    }
    let mut n = 0;
    for (folder, p) in projects.iter().filter(|(_, p)| p["hasTrustDialogAccepted"] == true) {
        let t = &mut target["projects"][folder.as_str()];
        if t["hasTrustDialogAccepted"] != true && (t.is_object() || t.is_null()) && p.is_object() {
            t["hasTrustDialogAccepted"] = json!(true);
            n += 1;
        }
    }
    n
}

pub fn to_json(a: &Account) -> Value {
    json!({"id": a.id, "agent": a.agent, "name": a.name, "dir": a.dir, "share": a.share})
}

/// Nama → id: huruf kecil, selain a-z0-9 jadi `-`.
pub fn slug(name: &str) -> String {
    let s: String = name.to_lowercase().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
    s.split('-').filter(|x| !x.is_empty()).collect::<Vec<_>>().join("-").chars().take(32).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(toml: &str) -> (Vec<Account>, Vec<String>) {
        let doc: toml_edit::DocumentMut = toml.parse().unwrap();
        let arr = doc.get("account").and_then(|v| v.as_array_of_tables()).unwrap();
        let tables: Vec<&dyn toml_edit::TableLike> = arr.iter().map(|t| t as &dyn toml_edit::TableLike).collect();
        from_config(&tables)
    }

    #[test]
    fn validasi_akun() {
        let (a, e) = parse(
            "[[account]]\nid = \"kantor\"\nname = \"Kantor\"\ndir = \"~/.claude-kantor-uji\"\n\
             [[account]]\nid = \"utama\"\ndir = \"~/.claude-x\"\n\
             [[account]]\nid = \"luar\"\ndir = \"/tmp/akun\"\n\
             [[account]]\nid = \"naik\"\ndir = \"~/../etc\"\n\
             [[account]]\nid = \"inti\"\ndir = \"~/.claude\"\n\
             [[account]]\nid = \"kantor\"\ndir = \"~/.claude-lain\"\n\
             [[account]]\nid = \"kembar\"\ndir = \"~/.claude-kantor-uji\"\n",
        );
        assert_eq!(a.len(), 1, "{a:?} {e:?}");
        assert_eq!(a[0].share, SHARE_DEFAULT.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert_eq!(e.len(), 6, "{e:?}");
        assert!(e.iter().any(|x| x.contains("Utama")) && e.iter().any(|x| x.contains("di bawah folder home")));
    }

    #[test]
    fn urutan_akun_dan_env() {
        let claude = || crate::profile::builtin().into_iter().find(|p| p.id == "claude").unwrap();
        let acc = |id: &str, name: &str| Account { id: id.into(), agent: "claude".into(), name: name.into(), dir: format!("~/.claude-{id}"), share: vec![] };
        let all = [acc("kantor", "Kantor"), acc("klien", "Klien")];
        // tanpa pilihan, tanpa akun profil = Utama
        let mut p = claude();
        assert_eq!(apply(&mut p, None, &all), Ok(String::new()));
        assert!(!conflicts(&p.env));
        // akun profil dipakai bila tidak dipilih saat membuka pane
        let mut p = crate::profile::Profile { account: "klien".into(), ..claude() };
        assert_eq!(apply(&mut p, Some(""), &all), Ok("Klien".into()));
        assert!(crate::profile::launch(&p, None, None).unwrap().contains("export CLAUDE_CONFIG_DIR=\"$HOME\"/'.claude-klien'"));
        // pilihan saat membuka pane menang atas akun profil; "utama" = tanpa env
        let mut p = crate::profile::Profile { account: "klien".into(), ..claude() };
        assert_eq!(apply(&mut p, Some("kantor"), &all), Ok("Kantor".into()));
        let mut p = crate::profile::Profile { account: "klien".into(), ..claude() };
        assert_eq!(apply(&mut p, Some("utama"), &all), Ok(String::new()));
        assert!(!conflicts(&p.env));
        // akun tak dikenal, atau agent lain, ditolak; env ganda ditolak
        assert_eq!(apply(&mut claude(), Some("hilang"), &all), Err("no_such_account"));
        let mut codex = crate::profile::builtin().into_iter().find(|p| p.id == "codex").unwrap();
        assert_eq!(apply(&mut codex, Some("kantor"), &all), Err("no_such_account"));
        let mut p = crate::profile::Profile { env: vec![(ENV.into(), "~/.x".into())], ..claude() };
        assert_eq!(apply(&mut p, Some("kantor"), &all), Err("account_env_conflict"));
    }

    #[test]
    fn hapus_folder_akun_tautan_utama_aman() {
        let pid = std::process::id();
        let target = std::env::temp_dir().join(format!("gembala-utama-{pid}"));
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("CLAUDE.md"), "x").unwrap();
        let dir = format!("~/.claude-gembala-uji-{pid}");
        let p = expand(&dir);
        std::fs::create_dir_all(p.join("sessions")).unwrap();
        std::os::unix::fs::symlink(&target, p.join("skills")).unwrap();
        assert_eq!(delete_dir(&dir), Ok(()));
        assert!(!p.exists() && target.join("CLAUDE.md").exists(), "tautan dilepas, isi Utama tetap");
        assert_eq!(delete_dir(&dir), Ok(()), "sudah tidak ada = beres");
        // folder bukan .claude… atau symlink ditolak
        let other = format!("~/gembala-uji-{pid}");
        std::fs::create_dir_all(expand(&other)).unwrap();
        assert!(delete_dir(&other).is_err() && expand(&other).exists());
        std::fs::remove_dir(expand(&other)).unwrap();
        std::os::unix::fs::symlink(&target, &p).unwrap();
        assert!(delete_dir(&dir).is_err() && target.exists());
        std::fs::remove_file(&p).unwrap();
        std::fs::remove_dir_all(&target).unwrap();
    }

    #[test]
    fn status_hanya_email() {
        let d = std::env::temp_dir().join(format!("gembala-akun-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let f = d.join(".claude.json");
        assert_eq!(status(&f), json!({"used": false, "email": null}));
        std::fs::write(&f, r#"{"oauthAccount":{"emailAddress":"a@b.c","accountUuid":"rahasia"},"mcpServers":{"x":{"headers":{"Authorization":"Bearer t"}}}}"#).unwrap();
        let s = status(&f);
        assert_eq!(s, json!({"used": true, "email": "a@b.c"}));
        assert!(!s.to_string().contains("rahasia") && !s.to_string().contains("Bearer"));
        std::fs::write(&f, "{rusak").unwrap();
        assert_eq!(status(&f), json!({"used": true, "email": null}));
        let _ = std::fs::remove_dir_all(&d);
        assert_eq!(slug("Kantor Javan!"), "kantor-javan");
    }

    #[test]
    fn trust_utama_ditambahkan_satu_arah() {
        let main = json!({"oauthAccount": {"emailAddress": "a@b"}, "projects": {
            "/r/a": {"hasTrustDialogAccepted": true, "allowedTools": ["x"]},
            "/r/b": {"hasTrustDialogAccepted": false},
            "/r/c": {"hasTrustDialogAccepted": true}}});
        let mut t = json!({"oauthAccount": {"emailAddress": "k@b"}, "projects": {
            "/r/c": {"hasTrustDialogAccepted": false, "history": [1]},
            "/r/k": {"hasTrustDialogAccepted": true}}});
        assert_eq!(merge_trust(&main, &mut t), 2);
        // hanya tanda trust yang disalin; isi lain akun dan login tidak tersentuh
        assert_eq!(t["projects"]["/r/a"], json!({"hasTrustDialogAccepted": true}));
        assert_eq!(t["projects"]["/r/c"], json!({"hasTrustDialogAccepted": true, "history": [1]}));
        assert_eq!(t["projects"]["/r/b"], Value::Null);
        assert_eq!(t["projects"]["/r/k"]["hasTrustDialogAccepted"], true);
        assert_eq!(t["oauthAccount"]["emailAddress"], "k@b");
        assert_eq!(merge_trust(&main, &mut t), 0, "sinkron kedua tidak menulis apa-apa");
        // akun baru tanpa `projects` tetap diisi; format asing dilewati
        let mut fresh = json!({"numStartups": 1});
        assert_eq!(merge_trust(&main, &mut fresh), 2);
        assert_eq!(merge_trust(&main, &mut json!([])), 0);
    }
}
