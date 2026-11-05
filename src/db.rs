use crate::model::Batch;
use anyhow::Result;
use chrono::{Local, TimeZone};
use rusqlite::{Connection, params};
use std::path::Path;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS files(path TEXT PRIMARY KEY, size INTEGER, mtime INTEGER);
CREATE TABLE IF NOT EXISTS usage(
  msg_key TEXT PRIMARY KEY, vendor TEXT, account TEXT, session_id TEXT, project TEXT,
  ts INTEGER, day TEXT, model TEXT, input INTEGER, output INTEGER, reasoning INTEGER,
  cache_read INTEGER, cache_write_5m INTEGER, cache_write_1h INTEGER,
  sidechain INTEGER, skill TEXT, plugin TEXT);
CREATE INDEX IF NOT EXISTS usage_day ON usage(day);
CREATE INDEX IF NOT EXISTS usage_session ON usage(vendor, session_id);
CREATE TABLE IF NOT EXISTS limit_snapshots(
  vendor TEXT, account TEXT, scope TEXT, window_minutes INTEGER, used_percent REAL,
  resets_at INTEGER, plan_type TEXT, measured_at INTEGER, source TEXT,
  PRIMARY KEY(vendor, account, scope, measured_at));
CREATE TABLE IF NOT EXISTS limit_events(vendor TEXT, account TEXT, ts INTEGER, scope TEXT, resets_at INTEGER,
  PRIMARY KEY(vendor, account, ts, scope));
CREATE TABLE IF NOT EXISTS skill_events(vendor TEXT, session_id TEXT, ts INTEGER, skill TEXT, kind TEXT, key TEXT PRIMARY KEY);
CREATE TABLE IF NOT EXISTS tool_events(vendor TEXT, session_id TEXT, ts INTEGER, tool TEXT, key TEXT PRIMARY KEY);
CREATE TABLE IF NOT EXISTS sessions(vendor TEXT, session_id TEXT, project TEXT, title TEXT, source_path TEXT, last_write INTEGER,
  PRIMARY KEY(vendor, session_id));
CREATE TABLE IF NOT EXISTS kv(key TEXT PRIMARY KEY, value TEXT);
";

pub fn open(path: &Path) -> Result<Connection> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let conn = Connection::open(path)?;
    conn.busy_timeout(std::time::Duration::from_millis(5000))?;
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    let tables: i64 = conn.query_row("SELECT count(*) FROM sqlite_master", [], |r| r.get(0))?;
    if !(version == 1 || (version == 0 && tables == 0)) {
        anyhow::bail!("not an agent-usage database: {}", path.display());
    }
    conn.query_row("PRAGMA journal_mode=WAL", [], |_| Ok(()))?;
    // schema and version in one transaction: no tables-with-version-0 state
    conn.execute_batch(&format!(
        "BEGIN IMMEDIATE;{SCHEMA}PRAGMA user_version = 1;COMMIT;"
    ))?;
    Ok(conn)
}

/// Local-day (machine timezone) as YYYY-MM-DD.
fn local_day(ts: i64) -> String {
    Local
        .timestamp_opt(ts, 0)
        .single()
        .map(|d| d.format("%Y-%m-%d").to_string())
        .unwrap_or_default()
}

pub fn write_batch(conn: &mut Connection, b: &Batch) -> Result<()> {
    let tx = conn.transaction()?;
    {
        let mut u = tx.prepare(
            "INSERT INTO usage(msg_key,vendor,account,session_id,project,ts,day,model,input,output,reasoning,
               cache_read,cache_write_5m,cache_write_1h,sidechain,skill,plugin)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)
             ON CONFLICT(msg_key) DO UPDATE SET
               vendor=excluded.vendor, account=excluded.account, session_id=excluded.session_id,
               project=excluded.project, ts=excluded.ts, day=excluded.day, model=excluded.model,
               input=excluded.input, output=excluded.output, reasoning=excluded.reasoning,
               cache_read=excluded.cache_read, cache_write_5m=excluded.cache_write_5m,
               cache_write_1h=excluded.cache_write_1h, sidechain=excluded.sidechain,
               skill=excluded.skill, plugin=excluded.plugin
             WHERE excluded.output > usage.output",
        )?;
        for r in &b.usage {
            u.execute(params![
                r.msg_key,
                r.vendor.as_str(),
                r.account,
                r.session_id,
                r.project,
                r.ts,
                local_day(r.ts),
                r.model,
                r.input as i64,
                r.output as i64,
                r.reasoning as i64,
                r.cache_read as i64,
                r.cache_write_5m as i64,
                r.cache_write_1h as i64,
                r.sidechain as i64,
                r.skill,
                r.plugin
            ])?;
        }
        let mut l = tx.prepare(
            "INSERT OR IGNORE INTO limit_snapshots(vendor,account,scope,window_minutes,used_percent,resets_at,plan_type,measured_at,source)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
        )?;
        for r in &b.limits {
            l.execute(params![
                r.vendor.as_str(),
                r.account,
                r.scope,
                r.window_minutes,
                r.used_percent,
                r.resets_at,
                r.plan_type,
                r.measured_at,
                r.source
            ])?;
        }
        let mut e = tx.prepare(
            "INSERT OR IGNORE INTO limit_events(vendor,account,ts,scope,resets_at) VALUES(?1,?2,?3,?4,?5)",
        )?;
        for r in &b.limit_events {
            e.execute(params![
                r.vendor.as_str(),
                r.account,
                r.ts,
                r.scope,
                r.resets_at
            ])?;
        }
        let mut s = tx.prepare(
            "INSERT OR IGNORE INTO skill_events(vendor,session_id,ts,skill,kind,key) VALUES(?1,?2,?3,?4,?5,?6)",
        )?;
        for r in &b.skills {
            s.execute(params![
                r.vendor.as_str(),
                r.session_id,
                r.ts,
                r.skill,
                r.kind,
                r.key
            ])?;
        }
        let mut t = tx.prepare(
            "INSERT OR IGNORE INTO tool_events(vendor,session_id,ts,tool,key) VALUES(?1,?2,?3,?4,?5)",
        )?;
        for r in &b.tools {
            t.execute(params![
                r.vendor.as_str(),
                r.session_id,
                r.ts,
                r.tool,
                r.key
            ])?;
        }
        let mut ss = tx.prepare(
            "INSERT INTO sessions(vendor,session_id,project,title,source_path,last_write) VALUES(?1,?2,?3,?4,?5,?6)
             ON CONFLICT(vendor,session_id) DO UPDATE SET project=excluded.project, title=excluded.title,
               source_path=excluded.source_path, last_write=max(coalesce(sessions.last_write,0), coalesce(excluded.last_write,0))",
        )?;
        for r in &b.sessions {
            ss.execute(params![
                r.vendor.as_str(),
                r.session_id,
                r.project,
                r.title,
                r.source_path,
                r.last_write
            ])?;
        }
        let mut k = tx.prepare("INSERT OR REPLACE INTO kv(key,value) VALUES(?1,?2)")?;
        for (key, value) in &b.kv {
            k.execute(params![key, value])?;
        }
    }
    tx.commit()?;
    Ok(())
}

const KV_LAST_DOWNSAMPLE: &str = "limits:last_downsample";
const DOWNSAMPLE_AGE_SECS: i64 = 90 * 86400;
const DOWNSAMPLE_BUCKET_SECS: i64 = 300;

/// Thin out limit snapshots measured before `older_than` (epoch seconds) to at
/// most one per 5-minute bucket per `(vendor, account, scope)`, keeping the
/// earliest of each bucket. Returns the number of rows removed.
pub fn downsample_limits(conn: &Connection, older_than: i64) -> Result<usize> {
    // SQLite: a bare column next to MIN() comes from the row holding the minimum
    Ok(conn.execute(
        "DELETE FROM limit_snapshots WHERE measured_at < ?1 AND rowid NOT IN (
           SELECT keep FROM (
             SELECT rowid AS keep, MIN(measured_at) FROM limit_snapshots WHERE measured_at < ?1
             GROUP BY vendor, account, scope, measured_at / ?2))",
        params![older_than, DOWNSAMPLE_BUCKET_SECS],
    )?)
}

/// [`downsample_limits`] for snapshots older than 90 days, at most once per
/// day (kv `limits:last_downsample`). `Some(removed)` when it ran.
pub fn downsample_daily(conn: &Connection, now: i64) -> Result<Option<usize>> {
    let last: Option<i64> = conn
        .query_row(
            "SELECT value FROM kv WHERE key=?1",
            params![KV_LAST_DOWNSAMPLE],
            |r| r.get::<_, String>(0),
        )
        .ok()
        .and_then(|v| v.parse().ok());
    if last.is_some_and(|l| now - l < 86400) {
        return Ok(None);
    }
    let removed = downsample_limits(conn, now - DOWNSAMPLE_AGE_SECS)?;
    conn.execute(
        "INSERT OR REPLACE INTO kv(key,value) VALUES(?1,?2)",
        params![KV_LAST_DOWNSAMPLE, now.to_string()],
    )?;
    Ok(Some(removed))
}
