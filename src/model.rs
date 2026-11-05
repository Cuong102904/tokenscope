use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Vendor {
    Claude,
    Codex,
    OpenCode,
    Agy,
}

impl Vendor {
    pub fn as_str(&self) -> &'static str {
        match self {
            Vendor::Claude => "claude",
            Vendor::Codex => "codex",
            Vendor::OpenCode => "opencode",
            Vendor::Agy => "agy",
        }
    }
}

#[derive(Debug, Clone)]
pub struct UsageRecord {
    pub msg_key: String,
    pub vendor: Vendor,
    pub account: String,
    pub session_id: String,
    pub project: String,
    pub ts: i64,
    pub model: String,
    pub input: u64,
    pub output: u64,
    pub reasoning: u64,
    pub cache_read: u64,
    pub cache_write_5m: u64,
    pub cache_write_1h: u64,
    pub sidechain: bool,
    pub skill: Option<String>,
    pub plugin: Option<String>,
}

#[derive(Debug, Clone)]
pub struct LimitSnapshot {
    pub vendor: Vendor,
    pub account: String,
    pub scope: String,
    pub window_minutes: Option<i64>,
    pub used_percent: f64,
    pub resets_at: Option<i64>,
    pub plan_type: Option<String>,
    pub measured_at: i64,
    pub source: String,
}

#[derive(Debug, Clone)]
pub struct LimitEvent {
    pub vendor: Vendor,
    pub account: String,
    pub ts: i64,
    pub scope: String,
    pub resets_at: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct SkillEvent {
    pub key: String,
    pub vendor: Vendor,
    pub session_id: String,
    pub ts: i64,
    pub skill: String,
    /// "invoked" or "file_read"
    pub kind: &'static str,
}

#[derive(Debug, Clone)]
pub struct ToolEvent {
    pub key: String,
    pub vendor: Vendor,
    pub session_id: String,
    pub ts: i64,
    pub tool: String,
}

#[derive(Debug, Clone)]
pub struct SessionInfo {
    pub vendor: Vendor,
    pub session_id: String,
    pub project: String,
    pub title: Option<String>,
    pub source_path: String,
    pub last_write: i64,
}

#[derive(Debug, Clone, Default)]
pub struct Batch {
    pub usage: Vec<UsageRecord>,
    pub limits: Vec<LimitSnapshot>,
    pub limit_events: Vec<LimitEvent>,
    pub skills: Vec<SkillEvent>,
    pub tools: Vec<ToolEvent>,
    pub sessions: Vec<SessionInfo>,
}

/// First 8 hex chars of sha256(s); used for every account identifier.
pub fn sha8(s: &str) -> String {
    let d = Sha256::digest(s.as_bytes());
    d.iter().take(4).map(|b| format!("{b:02x}")).collect()
}
