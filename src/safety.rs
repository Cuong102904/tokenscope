use std::fs::File;
use std::io;
use std::path::Path;

/// Hard-coded never-read list, matched on the FILE NAME only: auth.json,
/// openai-codex-accounts.json*, .credentials.json, oauth*.json, and any name
/// containing "credential" or "token".
pub fn is_forbidden(p: &Path) -> bool {
    let name = p
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    name.contains("credential")
        || name.contains("token")
        || name == "auth.json"
        || name.starts_with("openai-codex-accounts.json")
        || (name.starts_with("oauth") && name.ends_with(".json"))
}

/// Every adapter must open files through this function.
pub fn open_guarded(p: &Path) -> io::Result<File> {
    let resolved = std::fs::canonicalize(p).ok();
    if is_forbidden(p) || resolved.as_deref().is_some_and(is_forbidden) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "path is on the never-read list",
        ));
    }
    File::open(p)
}
