//! Antigravity (`agy`): weekly limits only, and only on demand via `refresh`
//! (running `agy -p "/usage"` may consume quota, so `collect` never runs it).
use super::{Adapter, CollectCtx, Discovery};
use crate::config::Config;
use crate::model::{Batch, LimitSnapshot, Vendor, sha8};
use crate::paths::Paths;
use crate::safety;
use rusqlite::{Connection, OptionalExtension, params};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

const KV_LAST_REFRESH: &str = "agy:last_refresh";
const MIN_INTERVAL_SECS: i64 = 300;
const REFRESH_TIMEOUT: Duration = Duration::from_secs(30);
const SOURCE: &str = "agy /usage";
const AUTH_MARKER: &str = "applyAuthResult: email=";
const LOG_TAIL_BYTES: u64 = 4 * 1024 * 1024;

pub struct AgyAdapter;

#[derive(Debug, Clone, PartialEq)]
pub enum RefreshOutcome {
    Ran,
    Throttled { next_allowed: i64 },
    NotInstalled,
}

/// Resolve `bin` (a path, or a name looked up on `$PATH`) to an existing file.
fn resolve(bin: &Path) -> Option<PathBuf> {
    if bin.components().count() > 1 {
        return bin.is_file().then(|| bin.to_path_buf());
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join(bin)).find(|p| p.is_file())
}

impl Adapter for AgyAdapter {
    fn vendor(&self) -> Vendor {
        Vendor::Agy
    }

    fn discover(&self, paths: &Paths) -> Discovery {
        let found = resolve(Path::new("agy")).is_some();
        let logs = if paths.agy_log_dir().is_dir() { "log dir present" } else { "no log dir" };
        let detail = if found { format!("agy on PATH, {logs}") } else { format!("agy not on PATH, {logs}") };
        Discovery { found, detail }
    }

    fn collect(&self, _ctx: &mut CollectCtx) -> anyhow::Result<Batch> {
        Ok(Batch::default())
    }
}

/// Parse `agy -p "/usage"` output; lines that do not match are dropped.
pub fn parse_usage(text: &str, measured_at: i64, account: &str) -> Vec<LimitSnapshot> {
    text.lines()
        .filter_map(|line| {
            let f: Vec<&str> = line.split('\t').map(str::trim).collect();
            let [group, label, pct, reset] = f[..] else { return None };
            if label != "Weekly Limit Remaining" || group.is_empty() {
                return None;
            }
            let remaining: f64 = pct.strip_suffix('%')?.trim().parse().ok()?;
            let resets_at = chrono::DateTime::parse_from_rfc3339(reset).ok()?.timestamp();
            Some(LimitSnapshot {
                vendor: Vendor::Agy,
                account: account.to_string(),
                scope: format!("weekly:{group}"),
                window_minutes: Some(10080),
                used_percent: 100.0 - remaining,
                resets_at: Some(resets_at),
                plan_type: None,
                measured_at,
                source: SOURCE.to_string(),
            })
        })
        .collect()
}

fn newest_log(dir: &Path) -> Option<PathBuf> {
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "log"))
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .max()
        .map(|(_, p)| p)
}

/// sha8 of the email on the last `applyAuthResult: email=` line of the newest
/// log; "" if none. The raw email never leaves this function.
pub fn account_hint(log_dir: &Path) -> String {
    let Some(path) = newest_log(log_dir) else { return String::new() };
    let Ok(mut f) = safety::open_guarded(&path) else { return String::new() };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let mut buf = vec![];
    if f.seek(SeekFrom::Start(len.saturating_sub(LOG_TAIL_BYTES))).is_err() || f.read_to_end(&mut buf).is_err() {
        return String::new();
    }
    let text = String::from_utf8_lossy(&buf);
    let Some(i) = text.rfind(AUTH_MARKER) else { return String::new() };
    let email: String = text[i + AUTH_MARKER.len()..].chars().take_while(|c| !c.is_whitespace() && !matches!(c, ',' | ';' | '"' | '\'' | ')')).collect();
    if email.is_empty() { String::new() } else { sha8(&email) }
}

/// Run `agy -p "/usage"` (binary from `$PATH`) unless one ran within 5 minutes.
pub fn refresh(paths: &Paths, cfg: &Config, conn: &mut Connection) -> anyhow::Result<RefreshOutcome> {
    refresh_with(paths, cfg, conn, Path::new("agy"), chrono::Utc::now().timestamp())
}

/// `refresh` with an injectable binary and clock (for tests).
pub fn refresh_with(paths: &Paths, cfg: &Config, conn: &mut Connection, bin: &Path, now: i64) -> anyhow::Result<RefreshOutcome> {
    refresh_with_timeout(paths, cfg, conn, bin, now, REFRESH_TIMEOUT)
}

pub fn refresh_with_timeout(paths: &Paths, _cfg: &Config, conn: &mut Connection, bin: &Path, now: i64, timeout: Duration) -> anyhow::Result<RefreshOutcome> {
    let Some(bin) = resolve(bin) else { return Ok(RefreshOutcome::NotInstalled) };
    let last: Option<i64> = conn
        .query_row("SELECT value FROM kv WHERE key=?1", params![KV_LAST_REFRESH], |r| r.get::<_, String>(0))
        .optional()?
        .and_then(|v| v.parse().ok());
    if let Some(last) = last
        && now - last < MIN_INTERVAL_SECS
    {
        return Ok(RefreshOutcome::Throttled { next_allowed: last + MIN_INTERVAL_SECS });
    }
    // stamp before running: a failed or timed-out run must not be retried in a tight loop
    conn.execute("INSERT OR REPLACE INTO kv(key,value) VALUES(?1,?2)", params![KV_LAST_REFRESH, now.to_string()])?;
    let mut cmd = Command::new(&bin);
    cmd.args(["-p", "/usage"]);
    let out = crate::statusline::run_with_timeout(cmd, b"", timeout).ok_or_else(|| anyhow::anyhow!("agy /usage failed to start or timed out after {}s", timeout.as_secs()))?;
    let snaps = parse_usage(&String::from_utf8_lossy(&out), now, &account_hint(&paths.agy_log_dir()));
    crate::db::write_batch(conn, &Batch { limits: snaps, ..Default::default() })?;
    Ok(RefreshOutcome::Ran)
}
