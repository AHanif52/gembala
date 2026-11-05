// UI language for native text (TECH 20). WKWebView reports only one language (`navigator.languages` = ["en-GB"]
// on an en-ID, id-ID Mac), so the macOS list comes from `defaults`.

/// macOS preferred languages, e.g. ["en-ID", "id-ID"]
pub fn system_langs() -> Vec<String> {
    let out = std::process::Command::new("/usr/bin/defaults").args(["read", "-g", "AppleLanguages"]).output();
    // output is an old-style plist array: `(\n    "en-ID",\n    "id-ID"\n)`
    out.map(|o| String::from_utf8_lossy(&o.stdout).lines().map(|l| l.trim().trim_end_matches(',').trim_matches('"').to_string()).filter(|l| !l.is_empty() && l != "(" && l != ")").collect())
        .unwrap_or_default()
}
