//! Shared plumbing for limits read by running a CLI on demand (`agy -p "/usage"`,
//! `claude -p "/usage"`): finding the binary and a per-tool 5-minute throttle
//! stamped in the `kv` table.

use rusqlite::{Connection, OptionalExtension, params};
use std::path::{Path, PathBuf};

/// At most one run per tool in this many seconds.
pub const MIN_INTERVAL_SECS: i64 = 300;

#[derive(Debug, Clone, PartialEq)]
pub enum RefreshOutcome {
    Ran,
    Throttled { next_allowed: i64 },
    NotInstalled,
}

/// Resolve `bin` (a path, or a name looked up on `$PATH`) to an existing file.
pub fn resolve(bin: &Path) -> Option<PathBuf> {
    if bin.components().count() > 1 {
        return bin.is_file().then(|| bin.to_path_buf());
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|d| d.join(bin))
        .find(|p| p.is_file())
}

/// `Some(Throttled)` when `key` was stamped less than [`MIN_INTERVAL_SECS`] before
/// `now`. Otherwise stamps `now` and returns `None`: the stamp goes in before the
/// run, so a failing or hanging tool is not retried in a tight loop.
pub fn throttle(conn: &Connection, key: &str, now: i64) -> anyhow::Result<Option<RefreshOutcome>> {
    let last: Option<i64> = conn
        .query_row("SELECT value FROM kv WHERE key=?1", params![key], |r| {
            r.get::<_, String>(0)
        })
        .optional()?
        .and_then(|v| v.parse().ok());
    if let Some(last) = last
        && now - last < MIN_INTERVAL_SECS
    {
        return Ok(Some(RefreshOutcome::Throttled {
            next_allowed: last + MIN_INTERVAL_SECS,
        }));
    }
    conn.execute(
        "INSERT OR REPLACE INTO kv(key,value) VALUES(?1,?2)",
        params![key, now.to_string()],
    )?;
    Ok(None)
}
