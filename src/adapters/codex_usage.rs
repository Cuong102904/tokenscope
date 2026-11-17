//! Codex rate limits on demand through `codex app-server` (JSON-RPC, one message
//! per line on stdio): `initialize`, then `account/rateLimits/read`.
//!
//! Codex does the auth with its own login and no model is called. The answer is
//! structured JSON, and it carries the account id, which is hashed (sha8) exactly
//! like the `creator_account_id` of the session logs, so these readings land on the
//! same cards. Only the logged-in account can be asked; other accounts' cards keep
//! waiting for their logs. When another tool switches the login (codex-multi-auth,
//! `codex login`), [`follow_switch`] asks for the new account's limits right away.

pub use super::refresh::RefreshOutcome;
use super::refresh::{resolve, throttle};
use crate::adapters::codex::scope_for;
use crate::config::Config;
use crate::model::{Batch, LimitSnapshot, Vendor, sha8};
use crate::paths::Paths;
use rusqlite::Connection;
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

const KV_LAST_REFRESH: &str = "codex:last_refresh";
const REFRESH_TIMEOUT: Duration = Duration::from_secs(20);
const SOURCE: &str = "codex app-server";
const READ_ID: i64 = 2;

/// Snapshots from an `account/rateLimits/read` result: every limit in
/// `rateLimitsByLimitId` (or the single `rateLimits`), primary and secondary window.
/// The main `codex` limit uses the plain scope (`five_hour`, `weekly`, …); others get
/// `:<limit name>` appended.
pub fn parse_rate_limits(result: &Value, measured_at: i64) -> Vec<LimitSnapshot> {
    let account = result["accountId"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(sha8)
        .unwrap_or_default();
    let limits: Vec<&Value> = match result["rateLimitsByLimitId"].as_object() {
        Some(m) if !m.is_empty() => m.values().collect(),
        _ => vec![&result["rateLimits"]],
    };
    let mut out = vec![];
    for l in limits {
        let id = l["limitId"].as_str().unwrap_or("codex");
        let name = l["limitName"].as_str().unwrap_or(id);
        for w in ["primary", "secondary"] {
            let (Some(used), Some(mins)) = (
                l[w]["usedPercent"].as_f64(),
                l[w]["windowDurationMins"].as_i64(),
            ) else {
                continue;
            };
            let scope = if id == "codex" {
                scope_for(mins)
            } else {
                format!("{}:{name}", scope_for(mins))
            };
            out.push(LimitSnapshot {
                vendor: Vendor::Codex,
                account: account.clone(),
                scope,
                window_minutes: Some(mins),
                used_percent: used.clamp(0.0, 100.0),
                resets_at: l[w]["resetsAt"].as_i64(),
                plan_type: l["planType"].as_str().map(str::to_string),
                measured_at,
                source: SOURCE.to_string(),
            });
        }
    }
    out
}

/// Ask `codex app-server` for the rate limits and return the `result` object. The
/// process is killed as soon as the answer (or the timeout) arrives.
fn ask(bin: &Path, dir: &Path, timeout: Duration) -> anyhow::Result<Value> {
    let mut child = Command::new(bin)
        .arg("app-server")
        .current_dir(dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let mut stdin = child.stdin.take().expect("piped stdin");
    let stdout = child.stdout.take().expect("piped stdout");
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    let result = (|| {
        // stdin stays open until the answer: app-server stops at EOF without replying
        for msg in [
            json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
                   "params": {"clientInfo": {"name": "tokenscope", "version": env!("CARGO_PKG_VERSION")}}}),
            json!({"jsonrpc": "2.0", "method": "initialized"}),
            json!({"jsonrpc": "2.0", "id": READ_ID, "method": "account/rateLimits/read"}),
        ] {
            writeln!(stdin, "{msg}")?;
        }
        stdin.flush()?;
        let deadline = Instant::now() + timeout;
        loop {
            let line = match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                Ok(l) => l,
                Err(RecvTimeoutError::Timeout) => anyhow::bail!(
                    "codex app-server timed out after {}s",
                    timeout.as_secs_f32()
                ),
                Err(RecvTimeoutError::Disconnected) => {
                    anyhow::bail!("codex app-server exited without answering")
                }
            };
            let Ok(v) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if v["id"].as_i64() != Some(READ_ID) {
                continue; // notifications and the initialize reply
            }
            if let Some(m) = v["error"]["message"].as_str() {
                anyhow::bail!("codex app-server: {m}");
            }
            return Ok(v["result"].clone());
        }
    })();
    drop(stdin);
    let _ = child.kill();
    let _ = child.wait();
    result
}

/// Ask `codex app-server` (binary from `$PATH`) unless one ran within 5 minutes.
pub fn refresh(
    paths: &Paths,
    cfg: &Config,
    conn: &mut Connection,
) -> anyhow::Result<RefreshOutcome> {
    refresh_with(
        paths,
        cfg,
        conn,
        Path::new("codex"),
        chrono::Utc::now().timestamp(),
    )
}

/// `refresh` with an injectable binary and clock (for tests).
pub fn refresh_with(
    paths: &Paths,
    cfg: &Config,
    conn: &mut Connection,
    bin: &Path,
    now: i64,
) -> anyhow::Result<RefreshOutcome> {
    refresh_with_timeout(paths, cfg, conn, bin, now, REFRESH_TIMEOUT)
}

pub fn refresh_with_timeout(
    paths: &Paths,
    _cfg: &Config,
    conn: &mut Connection,
    bin: &Path,
    now: i64,
    timeout: Duration,
) -> anyhow::Result<RefreshOutcome> {
    let Some(bin) = resolve(bin) else {
        return Ok(RefreshOutcome::NotInstalled);
    };
    if let Some(throttled) = throttle(conn, KV_LAST_REFRESH, now)? {
        return Ok(throttled);
    }
    ask_and_store(paths, conn, &bin, now, timeout)
}

/// After an account switch, wait at least this long since the last ask before asking again.
const SWITCH_MIN_GAP_SECS: i64 = 60;
const KV_LOGIN_SIG: &str = "codex:login_sig";

/// Identity of `~/.codex/auth.json` from its metadata only (modification time, size, inode).
/// The file holds the login, so it is never opened; `None` when it does not exist.
pub fn login_signature(path: &Path) -> Option<String> {
    use std::os::unix::fs::MetadataExt;
    let m = std::fs::metadata(path).ok()?;
    Some(format!(
        "{}.{}:{}:{}",
        m.mtime(),
        m.mtime_nsec(),
        m.len(),
        m.ino()
    ))
}

/// Follow an account switch made elsewhere (codex-multi-auth, `codex login`): when the Codex
/// login file changed since the last look, ask `codex app-server` for the limits of the account
/// now logged in, without waiting for the 5-minute throttle. The first look only remembers the
/// file. Codex renewing its own token also changes it; that just asks once more. `Some` when
/// it asked.
pub fn follow_switch(
    paths: &Paths,
    cfg: &Config,
    conn: &mut Connection,
) -> anyhow::Result<Option<RefreshOutcome>> {
    follow_switch_with(
        paths,
        cfg,
        conn,
        Path::new("codex"),
        chrono::Utc::now().timestamp(),
    )
}

/// `follow_switch` with an injectable binary and clock (for tests).
pub fn follow_switch_with(
    paths: &Paths,
    _cfg: &Config,
    conn: &mut Connection,
    bin: &Path,
    now: i64,
) -> anyhow::Result<Option<RefreshOutcome>> {
    let Some(sig) = login_signature(&paths.codex_login()) else {
        return Ok(None);
    };
    let stored = kv_get(conn, KV_LOGIN_SIG)?;
    if stored.as_deref() == Some(sig.as_str()) {
        return Ok(None);
    }
    if stored.is_some() {
        let last = kv_get(conn, KV_LAST_REFRESH)?.and_then(|v| v.parse::<i64>().ok());
        if last.is_some_and(|l| now - l < SWITCH_MIN_GAP_SECS) {
            return Ok(None); // switched again just now: the next tick asks
        }
    }
    // remembered before asking, so a failing ask is reported once, not on every tick
    kv_set(conn, KV_LOGIN_SIG, &sig)?;
    if stored.is_none() {
        return Ok(None);
    }
    let Some(bin) = resolve(bin) else {
        return Ok(None);
    };
    kv_set(conn, KV_LAST_REFRESH, &now.to_string())?;
    ask_and_store(paths, conn, &bin, now, REFRESH_TIMEOUT).map(Some)
}

fn kv_get(conn: &Connection, key: &str) -> rusqlite::Result<Option<String>> {
    use rusqlite::OptionalExtension;
    conn.query_row("SELECT value FROM kv WHERE key=?1", [key], |r| r.get(0))
        .optional()
}

fn kv_set(conn: &Connection, key: &str, value: &str) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO kv(key,value) VALUES(?1,?2)",
        [key, value],
    )
    .map(|_| ())
}

/// Run `codex app-server` and store its limits (no throttle check here).
fn ask_and_store(
    paths: &Paths,
    conn: &mut Connection,
    bin: &Path,
    now: i64,
    timeout: Duration,
) -> anyhow::Result<RefreshOutcome> {
    // inside ~/.tokenscope, never next to the user's projects
    let dir = paths.data_dir();
    std::fs::create_dir_all(&dir)?;
    let snaps = parse_rate_limits(&ask(bin, &dir, timeout)?, now);
    if snaps.is_empty() {
        anyhow::bail!("codex app-server returned no rate limits");
    }
    crate::db::write_batch(
        conn,
        &Batch {
            limits: snaps,
            ..Default::default()
        },
    )?;
    Ok(RefreshOutcome::Ran)
}
