use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct Paths {
    pub home: PathBuf,
}

impl Paths {
    pub fn from_env() -> Paths {
        let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
        Paths { home }
    }
    pub fn data_dir(&self) -> PathBuf {
        self.home.join(".agent-usage")
    }
    pub fn db(&self) -> PathBuf {
        self.data_dir().join("usage.db")
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
