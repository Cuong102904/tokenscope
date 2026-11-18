use super::{Adapter, CollectCtx, Discovery};
use crate::accounts::claude_key;
use crate::model::*;
use crate::paths::Paths;
use crate::scan;
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub struct ClaudeAdapter;

fn u(v: &Value) -> u64 {
    v.as_u64().unwrap_or(0)
}

fn s(v: &Value) -> Option<String> {
    v.as_str().filter(|x| !x.is_empty()).map(String::from)
}

fn ts_of(v: &Value) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(v["timestamp"].as_str()?)
        .ok()
        .map(|d| d.timestamp())
}

const KV_OWNER_SCAN: &str = "claude:owner_scan_v1";

/// `(accountUuid, organizationUuid)` of a `session-owner` line. The uuid must be a non-empty
/// string; the organisation is optional.
pub fn parse_owner(v: &Value) -> Option<(String, Option<String>)> {
    if v["type"] != "session-owner" {
        return None;
    }
    Some((s(&v["accountUuid"])?, s(&v["organizationUuid"])))
}

/// The account key of the session a transcript belongs to: its own `session-owner` line (not
/// necessarily the first), else, for a subagent file, the owner of the parent session file
/// `<dir>/<sessionId>.jsonl`. `None` for sessions that predate the line.
fn owner_of_file(
    path: &Path,
    lines: &[Value],
    parents: &mut HashMap<PathBuf, Option<String>>,
) -> Option<String> {
    let key_of = |lines: &[Value]| {
        lines
            .iter()
            .find_map(parse_owner)
            .map(|(u, o)| claude_key(&u, o.as_deref()))
    };
    if let Some(k) = key_of(lines) {
        return Some(k);
    }
    // .../<sessionId>/subagents/agent-x.jsonl -> .../<sessionId>.jsonl
    let mut dir = path.parent()?;
    while dir.file_name().is_some_and(|n| n != "subagents") {
        dir = dir.parent()?;
    }
    let session_dir = dir.parent()?;
    let parent = session_dir.with_extension("jsonl");
    parents
        .entry(parent.clone())
        .or_insert_with(|| scan::read_jsonl(&parent).ok().and_then(|l| key_of(&l)))
        .clone()
}

/// Turn one transcript line into batch entries (usage, tools, skills, limit events).
pub fn parse_line(v: &Value, account: &str, out: &mut Batch) {
    if v["type"] != "assistant" {
        return;
    }
    let Some(ts) = ts_of(v) else { return };
    let session_id = s(&v["sessionId"]).unwrap_or_default();

    if v["apiError"] == "usage_limit_reached" {
        let info = &v["apiErrorParams"]["rate_limit_info"];
        if let Some(scope) = s(&info["rateLimitType"]) {
            out.limit_events.push(LimitEvent {
                vendor: Vendor::Claude,
                account: account.to_string(),
                ts,
                scope,
                resets_at: info["resetsAt"].as_i64(),
            });
        }
        return;
    }

    let msg = &v["message"];
    let usage = &msg["usage"];
    if !usage.is_object() {
        return;
    }
    if msg["model"]
        .as_str()
        .is_none_or(|m| m.is_empty() || m.starts_with('<'))
    {
        return;
    }
    let (Some(mid), Some(rid)) = (s(&msg["id"]), s(&v["requestId"])) else {
        return;
    };
    let model = msg["model"].as_str().unwrap_or_default().to_string();

    let cc = &usage["cache_creation"];
    let (w5, w1) = if cc.is_object() {
        (
            u(&cc["ephemeral_5m_input_tokens"]),
            u(&cc["ephemeral_1h_input_tokens"]),
        )
    } else {
        (u(&usage["cache_creation_input_tokens"]), 0)
    };

    out.usage.push(UsageRecord {
        msg_key: format!("claude:{mid}|{rid}"),
        vendor: Vendor::Claude,
        account: account.to_string(),
        session_id: session_id.clone(),
        project: s(&v["cwd"]).unwrap_or_default(),
        ts,
        model,
        input: u(&usage["input_tokens"]),
        output: u(&usage["output_tokens"]),
        reasoning: 0,
        cache_read: u(&usage["cache_read_input_tokens"]),
        cache_write_5m: w5,
        cache_write_1h: w1,
        sidechain: v["isSidechain"].as_bool().unwrap_or(false),
        skill: s(&v["attributionSkill"]),
        plugin: s(&v["attributionPlugin"]),
    });

    for c in msg["content"].as_array().into_iter().flatten() {
        if c["type"] != "tool_use" {
            continue;
        }
        let (Some(id), Some(name)) = (s(&c["id"]), s(&c["name"])) else {
            continue;
        };
        let key = format!("claude:{id}");
        out.tools.push(ToolEvent {
            key: key.clone(),
            vendor: Vendor::Claude,
            session_id: session_id.clone(),
            ts,
            tool: name.clone(),
        });
        if name == "Skill"
            && let Some(skill) = s(&c["input"]["skill"])
        {
            out.skills.push(SkillEvent {
                key,
                vendor: Vendor::Claude,
                session_id: session_id.clone(),
                ts,
                skill,
                kind: "invoked",
            });
        }
    }
}

impl Adapter for ClaudeAdapter {
    fn vendor(&self) -> Vendor {
        Vendor::Claude
    }

    fn discover(&self, paths: &Paths) -> Discovery {
        let root = paths.claude_projects();
        if !root.exists() {
            return Discovery {
                found: false,
                detail: "not found".into(),
            };
        }
        let n = walkdir::WalkDir::new(&root)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_type().is_file() && e.path().extension().is_some_and(|x| x == "jsonl")
            })
            .count();
        Discovery {
            found: true,
            detail: format!("{n} jsonl files"),
        }
    }

    fn collect(&self, ctx: &mut CollectCtx) -> anyhow::Result<Batch> {
        let fallback = ctx.config.claude_account.clone();
        let mut batch = Batch::default();
        let mut parents: HashMap<PathBuf, Option<String>> = HashMap::new();
        // Rows written before attribution sit under the configured account and their files are
        // marked done: forget the stamps once so every transcript is read again. The marker is
        // saved with the batch, so a failed write retries.
        let root = ctx.paths.claude_projects();
        let owner_scan_done: Option<String> = {
            use rusqlite::OptionalExtension;
            ctx.conn
                .query_row(
                    "SELECT value FROM kv WHERE key = ?1",
                    [KV_OWNER_SCAN],
                    |r| r.get(0),
                )
                .optional()?
        };
        if owner_scan_done.is_none() {
            let prefix = format!("{}/", root.to_string_lossy());
            ctx.conn.execute(
                "DELETE FROM files WHERE substr(path, 1, ?1) = ?2",
                rusqlite::params![prefix.chars().count() as i64, prefix],
            )?;
            batch.kv.push((KV_OWNER_SCAN.to_string(), "1".to_string()));
        }
        let mut sessions: HashMap<String, SessionInfo> = HashMap::new();
        for stamp in scan::changed_files(ctx.conn, &ctx.paths.claude_projects(), "jsonl")? {
            let lines = match scan::read_jsonl(&stamp.path) {
                Ok(l) => l,
                Err(_) => {
                    ctx.unreadable += 1;
                    continue;
                }
            };
            let account = owner_of_file(&stamp.path, &lines, &mut parents)
                .unwrap_or_else(|| fallback.clone());
            let mut project = String::new();
            let mut session_id = String::new();
            for v in &lines {
                if session_id.is_empty() {
                    session_id = s(&v["sessionId"]).unwrap_or_default();
                }
                if project.is_empty() {
                    project = s(&v["cwd"]).unwrap_or_default();
                }
                parse_line(v, &account, &mut batch);
            }
            if !session_id.is_empty() {
                let last_write = stamp.mtime / 1_000_000_000;
                let e = sessions
                    .entry(session_id.clone())
                    .or_insert_with(|| SessionInfo {
                        vendor: Vendor::Claude,
                        session_id,
                        project: project.clone(),
                        title: None,
                        source_path: stamp.path.to_string_lossy().into_owned(),
                        last_write,
                    });
                // subagent files share the parent's sessionId; keep the latest write
                if last_write > e.last_write {
                    e.last_write = last_write;
                }
                if e.project.is_empty() {
                    e.project = project;
                }
                if !stamp
                    .path
                    .components()
                    .any(|c| c.as_os_str() == "subagents")
                {
                    e.source_path = stamp.path.to_string_lossy().into_owned();
                }
            }
            ctx.done_files.push(stamp);
        }
        batch.sessions.extend(sessions.into_values());
        Ok(batch)
    }
}
