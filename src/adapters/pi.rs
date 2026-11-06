//! Pi coding agent (`pi`): session transcripts under `~/.pi/agent/sessions/<--cwd-->/*.jsonl`
//! (or `$PI_CODING_AGENT_SESSION_DIR`, or `$PI_CODING_AGENT_DIR/sessions`).
//!
//! Each file starts with a `{"type":"session","id",cwd,…}` header. Assistant replies are
//! `{"type":"message","message":{"role":"assistant",model,usage,content}}` entries, and
//! side calls such as cache warming are `{"type":"usage",model,usage}` entries; both are
//! billed, so both become usage rows. Pi's `usage.output` already includes reasoning
//! tokens and `cacheWrite1h` is the 1-hour part of `cacheWrite`, which matches the
//! conventions here. Pi talks to many providers and keeps no account id in the log, so
//! rows have no account. Only `*.jsonl` under the sessions directory is read; the
//! `auth.json` next to it is on the never-read list.

use super::{Adapter, CollectCtx, Discovery};
use crate::model::*;
use crate::paths::Paths;
use crate::scan;
use serde_json::Value;
use std::path::PathBuf;

pub struct PiAdapter;

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

/// Where Pi keeps its sessions, from its two env overrides (given here for testing).
pub fn sessions_dir_from(
    paths: &Paths,
    agent_dir: Option<&str>,
    session_dir: Option<&str>,
) -> PathBuf {
    let expand = |p: &str| match p.strip_prefix("~/") {
        Some(rest) => paths.home.join(rest),
        None => PathBuf::from(p),
    };
    match (
        session_dir.filter(|s| !s.is_empty()),
        agent_dir.filter(|s| !s.is_empty()),
    ) {
        (Some(s), _) => expand(s),
        (None, Some(a)) => expand(a).join("sessions"),
        (None, None) => paths.home.join(".pi/agent/sessions"),
    }
}

fn sessions_dir(paths: &Paths) -> PathBuf {
    let var = |k| std::env::var(k).ok();
    sessions_dir_from(
        paths,
        var("PI_CODING_AGENT_DIR").as_deref(),
        var("PI_CODING_AGENT_SESSION_DIR").as_deref(),
    )
}

/// Usage rows, tool calls and the session record of one transcript.
pub fn parse_session(lines: &[Value]) -> Batch {
    let mut out = Batch::default();
    let Some(header) = lines.iter().find(|v| v["type"] == "session") else {
        return out;
    };
    let Some(session_id) = s(&header["id"]) else {
        return out;
    };
    let project = s(&header["cwd"]).unwrap_or_default();
    let mut title = None;
    let mut last = ts_of(header).unwrap_or(0);
    for v in lines {
        let Some(ts) = ts_of(v) else { continue };
        last = last.max(ts);
        let (model, usage) = match v["type"].as_str() {
            Some("message") if v["message"]["role"] == "assistant" => {
                (&v["message"]["model"], &v["message"]["usage"])
            }
            Some("usage") => (&v["model"], &v["usage"]),
            Some("session_info") => {
                if let Some(n) = s(&v["name"]) {
                    title = Some(n);
                }
                continue;
            }
            _ => continue,
        };
        let (input, output, cache_read, cache_write) = (
            u(&usage["input"]),
            u(&usage["output"]),
            u(&usage["cacheRead"]),
            u(&usage["cacheWrite"]),
        );
        let Some(entry_id) = s(&v["id"]) else {
            continue;
        };
        if input + output + cache_read + cache_write > 0 {
            let w1 = u(&usage["cacheWrite1h"]).min(cache_write);
            out.usage.push(UsageRecord {
                msg_key: format!("pi:{session_id}:{entry_id}"),
                vendor: Vendor::Pi,
                account: String::new(),
                session_id: session_id.clone(),
                project: project.clone(),
                ts,
                model: s(model).unwrap_or_default(),
                input,
                output,
                reasoning: u(&usage["reasoning"]),
                cache_read,
                cache_write_5m: cache_write - w1,
                cache_write_1h: w1,
                sidechain: false,
                skill: None,
                plugin: None,
            });
        }
        for c in v["message"]["content"].as_array().into_iter().flatten() {
            if c["type"] != "toolCall" {
                continue;
            }
            if let (Some(id), Some(name)) = (s(&c["id"]), s(&c["name"])) {
                out.tools.push(ToolEvent {
                    key: format!("pi:{id}"),
                    vendor: Vendor::Pi,
                    session_id: session_id.clone(),
                    ts,
                    tool: name,
                });
            }
        }
    }
    out.sessions.push(SessionInfo {
        vendor: Vendor::Pi,
        session_id,
        project,
        title,
        source_path: String::new(),
        last_write: last,
    });
    out
}

impl Adapter for PiAdapter {
    fn vendor(&self) -> Vendor {
        Vendor::Pi
    }

    fn discover(&self, paths: &Paths) -> Discovery {
        let root = sessions_dir(paths);
        if !root.is_dir() {
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
        let mut batch = Batch::default();
        for stamp in scan::changed_files(ctx.conn, &sessions_dir(ctx.paths), "jsonl")? {
            let lines = match scan::read_jsonl(&stamp.path) {
                Ok(l) => l,
                Err(_) => {
                    ctx.unreadable += 1;
                    continue;
                }
            };
            let mut b = parse_session(&lines);
            for s in &mut b.sessions {
                s.source_path = stamp.path.to_string_lossy().into_owned();
                // the file's own write time is when the session was last active
                s.last_write = s.last_write.max(stamp.mtime / 1_000_000_000);
            }
            batch.usage.append(&mut b.usage);
            batch.tools.append(&mut b.tools);
            batch.sessions.append(&mut b.sessions);
            ctx.done_files.push(stamp);
        }
        Ok(batch)
    }
}
