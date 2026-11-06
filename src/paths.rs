use std::path::PathBuf;

/// Data directory and DB file name used before the rename to tokenscope.
const LEGACY_DIR: &str = ".agent-usage";
const LEGACY_DB: &str = "agent-usage.db";

#[derive(Debug, Clone)]
pub struct Paths {
    pub home: PathBuf,
}

impl Paths {
    pub fn from_env() -> anyhow::Result<Paths> {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| anyhow::anyhow!("HOME is not set"))?;
        Ok(Paths { home })
    }
    pub fn data_dir(&self) -> PathBuf {
        self.home.join(".tokenscope")
    }
    pub fn db(&self) -> PathBuf {
        self.data_dir().join("tokenscope.db")
    }
    /// One-time move of the pre-rename `~/.agent-usage` to `~/.tokenscope`,
    /// renaming its `agent-usage.db` (+ `-wal`/`-shm`) to `tokenscope.db`.
    /// Returns true when this call did the move. Never overwrites: when
    /// `~/.tokenscope` already exists the legacy directory is left alone.
    pub fn migrate_legacy(&self) -> std::io::Result<bool> {
        let old = self.home.join(LEGACY_DIR);
        let new = self.data_dir();
        if new.exists() || !old.is_dir() {
            return Ok(false);
        }
        // DB files first, so the directory never shows up under its new name
        // still holding the old DB name (a concurrent run would create an empty DB).
        for suffix in ["", "-wal", "-shm"] {
            let from = old.join(format!("{LEGACY_DB}{suffix}"));
            let to = old.join(format!("tokenscope.db{suffix}"));
            if from.exists() && !to.exists() {
                std::fs::rename(&from, &to)?;
            }
        }
        match std::fs::rename(&old, &new) {
            Ok(()) => Ok(true),
            // another process moved it first
            Err(_) if new.exists() => Ok(false),
            Err(e) => Err(e),
        }
    }
    pub fn snapshots_dir(&self) -> PathBuf {
        self.data_dir().join("snapshots")
    }
    pub fn config(&self) -> PathBuf {
        self.data_dir().join("config.toml")
    }
    pub fn claude_projects(&self) -> PathBuf {
        self.home.join(".claude/projects")
    }
    /// `<pid>.json` per open interactive `claude` (see `open_sessions`).
    pub fn claude_session_registry(&self) -> PathBuf {
        self.home.join(".claude/sessions")
    }
    pub fn codex_sessions(&self) -> PathBuf {
        self.home.join(".codex/sessions")
    }
    pub fn opencode_db(&self) -> PathBuf {
        self.home.join(".local/share/opencode/opencode.db")
    }
    pub fn agy_log_dir(&self) -> PathBuf {
        self.home.join(".gemini/antigravity-cli/log")
    }
}
