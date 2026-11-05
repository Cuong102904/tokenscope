//! OpenCode: reads `~/.local/share/opencode/opencode.db` strictly read-only.
//! Incremental via kv `opencode:max_time_updated`; the new watermark travels in `Batch.kv`
//! so it is committed in the same transaction as the rows (never advanced before a successful write).
use super::{Adapter, CollectCtx, Discovery};
use crate::model::{Batch, SessionInfo, UsageRecord, Vendor};
use crate::paths::Paths;
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use serde_json::Value;
use std::collections::BTreeSet;
use std::path::Path;
use std::time::Duration;

const KV_WATERMARK: &str = "opencode:max_time_updated";
const BUSY_TIMEOUT: Duration = Duration::from_secs(1);

pub struct OpenCodeAdapter;

/// Percent-escape the characters that are special inside a SQLite URI path.
fn uri_for(path: &Path) -> String {
    let mut out = String::from("file:");
    for c in path.to_string_lossy().chars() {
        match c {
            '%' => out.push_str("%25"),
            '?' => out.push_str("%3f"),
            '#' => out.push_str("%23"),
            c => out.push(c),
        }
    }
    out.push_str("?mode=ro");
    out
}

fn open_ro(path: &Path) -> anyhow::Result<Connection> {
    let conn = Connection::open_with_flags(
        uri_for(path),
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_URI
            | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    conn.busy_timeout(BUSY_TIMEOUT)?;
    Ok(conn)
}

fn n(v: &Value) -> u64 {
    v.as_u64().unwrap_or(0)
}

/// `data` JSON of an assistant message -> usage record; `None` for non-assistant rows and rows
/// without tokens. A missing `modelID` is recorded as model `unknown` (unpriced, shows n/a).
fn parse_message(
    id: &str,
    session_id: &str,
    time_created: i64,
    data: &Value,
) -> Option<UsageRecord> {
    if data["role"].as_str() != Some("assistant") {
        return None;
    }
    let model = data["modelID"]
        .as_str()
        .filter(|m| !m.is_empty())
        .unwrap_or("unknown");
    let t = data.get("tokens").filter(|t| t.is_object())?;
    let (input, output, reasoning) = (n(&t["input"]), n(&t["output"]), n(&t["reasoning"]));
    let (cache_read, cache_write) = (n(&t["cache"]["read"]), n(&t["cache"]["write"]));
    if input + output + reasoning + cache_read + cache_write == 0 {
        return None;
    }
    // data.time.created is ms; message.time_created (fallback) is ms as well.
    let ms = data["time"]["created"].as_i64().unwrap_or(time_created);
    Some(UsageRecord {
        msg_key: format!("opencode:{id}"),
        vendor: Vendor::OpenCode,
        account: String::new(),
        session_id: session_id.to_string(),
        project: String::new(),
        ts: ms / 1000,
        model: model.to_string(),
        input,
        output: output + reasoning,
        reasoning,
        cache_read,
        cache_write_5m: cache_write,
        cache_write_1h: 0,
        sidechain: false,
        skill: None,
        plugin: None,
    })
}

/// Read every assistant message with `last < time_updated <= max` (max is sampled first so the
/// window is closed even if OpenCode writes concurrently). The role is checked in Rust, not with
/// `json_extract` in SQL: one row with invalid JSON would fail the whole statement and the
/// watermark would never advance. Returns the batch and the number of rows whose `data` is not JSON.
pub fn read_batch(db_path: &Path, last: i64) -> anyhow::Result<(Batch, usize)> {
    let conn = open_ro(db_path)?;
    let max: Option<i64> = conn.query_row(
        "SELECT max(time_updated) FROM message WHERE time_updated > ?1",
        [last],
        |r| r.get(0),
    )?;
    let Some(max) = max else {
        return Ok((Batch::default(), 0));
    };
    let mut batch = Batch::default();
    let mut malformed = 0;
    let mut touched = BTreeSet::new();
    let mut st = conn.prepare(
        "SELECT id, session_id, time_created, data FROM message
         WHERE time_updated > ?1 AND time_updated <= ?2
         ORDER BY time_updated, id",
    )?;
    let mut rows = st.query(params![last, max])?;
    while let Some(r) = rows.next()? {
        let (id, sid, created, data): (String, String, i64, Option<String>) =
            (r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?);
        let Some(v) = data.and_then(|d| serde_json::from_str::<Value>(&d).ok()) else {
            malformed += 1;
            continue;
        };
        if let Some(rec) = parse_message(&id, &sid, created, &v) {
            touched.insert(sid);
            batch.usage.push(rec);
        }
    }
    for sid in touched {
        let row: Option<(Option<String>, Option<String>, Option<i64>)> = conn
            .query_row(
                "SELECT directory, title, time_updated FROM session WHERE id=?1",
                [&sid],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        let (dir, title, upd) = row.unwrap_or_default();
        let dir = dir.unwrap_or_default();
        let last_write = upd
            .map(|t| t / 1000)
            .or_else(|| {
                batch
                    .usage
                    .iter()
                    .filter(|u| u.session_id == sid)
                    .map(|u| u.ts)
                    .max()
            })
            .unwrap_or(0);
        for u in batch.usage.iter_mut().filter(|u| u.session_id == sid) {
            u.project = dir.clone();
        }
        batch.sessions.push(SessionInfo {
            vendor: Vendor::OpenCode,
            session_id: sid,
            project: dir,
            title,
            source_path: db_path.to_string_lossy().into_owned(),
            last_write,
        });
    }
    batch.kv.push((KV_WATERMARK.to_string(), max.to_string()));
    Ok((batch, malformed))
}

impl Adapter for OpenCodeAdapter {
    fn vendor(&self) -> Vendor {
        Vendor::OpenCode
    }

    fn discover(&self, paths: &Paths) -> Discovery {
        let p = paths.opencode_db();
        let found = p.is_file();
        Discovery {
            found,
            detail: if found {
                "opencode.db present".into()
            } else {
                "opencode.db not found".into()
            },
        }
    }

    fn collect(&self, ctx: &mut CollectCtx) -> anyhow::Result<Batch> {
        let last: i64 = ctx
            .conn
            .query_row("SELECT value FROM kv WHERE key=?1", [KV_WATERMARK], |r| {
                r.get::<_, String>(0)
            })
            .optional()?
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        let (batch, malformed) = read_batch(&ctx.paths.opencode_db(), last)?;
        ctx.unreadable += malformed;
        Ok(batch)
    }
}
