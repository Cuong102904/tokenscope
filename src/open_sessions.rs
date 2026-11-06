//! Agent sessions open right now, as opposed to sessions whose log was written recently.
//!
//! Only Claude Code publishes this: every running `claude` keeps
//! `~/.claude/sessions/<pid>.json` with its session id, working directory and whether it is
//! busy or waiting for input (`claude -p` runs too, marked `entrypoint: "sdk-cli"`; those are
//! skipped). Only `<digits>.json` files are read; the `.key` files next to
//! them are never opened. A file counts only while its process is alive, checked against the
//! start time in `/proc/<pid>/stat` so a reused pid does not revive a closed session.

use crate::{model::Vendor, paths::Paths, safety};
use serde_json::Value;
use std::path::Path;

#[derive(Debug, Clone, PartialEq)]
pub struct OpenSession {
    pub vendor: Vendor,
    pub session_id: String,
    pub cwd: String,
    pub pid: u32,
    /// The agent is producing output (`false`: open and waiting for input).
    pub busy: bool,
    /// Epoch seconds of the last busy/idle change, when the agent reports it.
    pub since: Option<i64>,
}

/// Open sessions per vendor; `None` when that vendor's open sessions cannot be seen, in which
/// case "active" falls back to "log written in the last `active_minutes`".
#[derive(Debug, Clone, Default)]
pub struct OpenSessions {
    pub claude: Option<Vec<OpenSession>>,
}

impl OpenSessions {
    /// Every open session of every vendor that can be seen.
    pub fn all(&self) -> impl Iterator<Item = &OpenSession> {
        self.claude.iter().flatten()
    }

    pub fn find(&self, vendor: &str, session_id: &str) -> Option<&OpenSession> {
        self.all()
            .find(|o| o.vendor.as_str() == vendor && o.session_id == session_id)
    }
}

/// Larger files are not Claude's session records; skip them instead of reading.
const MAX_FILE: u64 = 64 * 1024;

pub fn scan(paths: &Paths) -> OpenSessions {
    OpenSessions {
        claude: claude(&paths.claude_session_registry(), &proc_alive),
    }
}

/// Live entries of Claude's session registry, or `None` when it does not exist.
/// `alive(pid, start)` decides whether the process is still the one that wrote the file.
pub fn claude(dir: &Path, alive: &dyn Fn(u32, Option<&str>) -> bool) -> Option<Vec<OpenSession>> {
    let entries = std::fs::read_dir(dir).ok()?;
    let mut out: Vec<OpenSession> = vec![];
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let Some(stem) = name.strip_suffix(".json") else {
            continue;
        };
        let Ok(pid) = stem.parse::<u32>() else {
            continue;
        };
        if e.metadata()
            .map_or(true, |m| !m.is_file() || m.len() > MAX_FILE)
        {
            continue;
        }
        let Ok(text) = safety::read_to_string_guarded(&e.path()) else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        let session_id = v["sessionId"].as_str().unwrap_or_default();
        if session_id.is_empty() || v["pid"].as_u64() != Some(pid as u64) {
            continue;
        }
        // `claude -p` (scripts, tokenscope's own `/usage` refresh) registers as "sdk-cli";
        // it is a run, not a session someone has open. Its log, if any, still lists it.
        if v["entrypoint"].as_str() == Some("sdk-cli") {
            continue;
        }
        let start = match &v["procStart"] {
            Value::String(s) => Some(s.clone()),
            Value::Number(n) => Some(n.to_string()),
            _ => None,
        };
        if !alive(pid, start.as_deref()) {
            continue;
        }
        let s = OpenSession {
            vendor: Vendor::Claude,
            session_id: session_id.to_string(),
            cwd: v["cwd"].as_str().unwrap_or_default().to_string(),
            pid,
            busy: v["status"].as_str() == Some("busy"),
            since: v["statusUpdatedAt"]
                .as_i64()
                .or_else(|| v["updatedAt"].as_i64())
                .map(|ms| ms / 1000),
        };
        // two processes on one session (rare): keep the busy one
        match out.iter_mut().find(|o| o.session_id == s.session_id) {
            Some(o) if s.busy && !o.busy => *o = s,
            Some(_) => {}
            None => out.push(s),
        }
    }
    Some(out)
}

/// The process exists and, when `start` is known, started at that time (field 22 of
/// `/proc/<pid>/stat`, clock ticks since boot).
pub fn proc_alive(pid: u32, start: Option<&str>) -> bool {
    let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
        return false;
    };
    match start {
        Some(s) => proc_start_time(&stat) == Some(s),
        None => true,
    }
}

/// Field 22 of a `/proc/<pid>/stat` line; the command name (field 2) may contain spaces and
/// parentheses, so fields are counted after the last `)`.
pub fn proc_start_time(stat: &str) -> Option<&str> {
    let rest = &stat[stat.rfind(')')? + 1..];
    rest.split_whitespace().nth(19)
}
