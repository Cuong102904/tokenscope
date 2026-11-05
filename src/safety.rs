use std::fs::File;
use std::io;
use std::path::Path;

/// Hard-coded never-read list: auth.json, openai-codex-accounts.json*,
/// .credentials.json, oauth*.json, and any path containing "credential" or "token".
pub fn is_forbidden(p: &Path) -> bool {
    let full = p.to_string_lossy().to_lowercase();
    if full.contains("credential") || full.contains("token") {
        return true;
    }
    let name = p
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    name == "auth.json"
        || name.starts_with("openai-codex-accounts.json")
        || name == ".credentials.json"
        || (name.starts_with("oauth") && name.ends_with(".json"))
}

/// Every adapter must open files through this function.
pub fn open_guarded(p: &Path) -> io::Result<File> {
    if is_forbidden(p) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "path is on the never-read list",
        ));
    }
    File::open(p)
}
