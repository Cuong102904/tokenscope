//! Claude Code rate limits on demand: `claude -p --no-session-persistence "/usage"`.
//!
//! Claude Code keeps its 5-hour and weekly usage on Anthropic's side; no local log
//! has it. `/usage` asks for it with Claude's own login, so tokenscope never reads
//! the credentials. The output is text for people, parsed line by line:
//!
//! ```text
//! Current session: 49% used · resets Nov 6, 11:39am (Asia/Ho_Chi_Minh)
//! Current week (all models): 13% used · resets Nov 13, 1:59am (Asia/Ho_Chi_Minh)
//! Current week (Fable): 3% used · resets Nov 13, 1:59am (Asia/Ho_Chi_Minh)
//! ```
//!
//! Scopes match the status line snapshots (`five_hour`, `weekly`) so both sources
//! feed the same cards; per-model weeks become `weekly:<model>`. Reset times are read
//! in the machine's time zone, which is the one Claude prints them in.

pub use super::refresh::RefreshOutcome;
use super::refresh::{resolve, throttle};
use crate::config::Config;
use crate::model::{Batch, LimitSnapshot, Vendor};
use crate::paths::Paths;
use chrono::{Datelike, Local, NaiveDate, NaiveTime, TimeZone};
use rusqlite::Connection;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

const KV_LAST_REFRESH: &str = "claude:last_refresh";
const REFRESH_TIMEOUT: Duration = Duration::from_secs(30);
const SOURCE: &str = "claude /usage";
const MONTHS: [&str; 12] = [
    "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
];

/// Parse `claude /usage` output; lines that do not match are dropped.
pub fn parse_usage(text: &str, measured_at: i64, account: &str) -> Vec<LimitSnapshot> {
    text.lines()
        .filter_map(|l| parse_line(l.trim(), measured_at, account))
        .collect()
}

fn parse_line(line: &str, now: i64, account: &str) -> Option<LimitSnapshot> {
    let (label, rest) = line.strip_prefix("Current ")?.split_once(": ")?;
    let (scope, window) = if label == "session" {
        ("five_hour".to_string(), 300)
    } else {
        let model = label.strip_prefix("week (")?.strip_suffix(')')?;
        let scope = if model == "all models" {
            "weekly".to_string()
        } else {
            format!("weekly:{model}")
        };
        (scope, 10080)
    };
    let (pct, rest) = rest.split_once("% used")?;
    let used: f64 = pct.trim().parse().ok()?;
    Some(LimitSnapshot {
        vendor: Vendor::Claude,
        account: account.to_string(),
        scope,
        window_minutes: Some(window),
        used_percent: used.clamp(0.0, 100.0),
        resets_at: rest
            .split_once("resets ")
            .and_then(|(_, r)| reset_time(r, now)),
        plan_type: None,
        measured_at: now,
        source: SOURCE.to_string(),
    })
}

/// `Nov 6, 11:39am (Asia/Ho_Chi_Minh)`, `Jan 2, 2am`, `1:15pm` or `Jan 2` as local
/// epoch seconds. No year is printed: a date more than two days in the past is
/// next year's ("Jan 2" read on Dec 30). A time alone is the next such time.
fn reset_time(s: &str, now: i64) -> Option<i64> {
    let s = match s.rfind(" (") {
        Some(i) if s.ends_with(')') => &s[..i],
        _ => s,
    }
    .trim();
    let (date, time) = match s.split_once(',') {
        Some((d, t)) => (Some(d.trim()), Some(t.trim())),
        None if s.starts_with(|c: char| c.is_ascii_digit()) => (None, Some(s)),
        None => (Some(s), None),
    };
    let t = match time {
        Some(t) => parse_clock(t)?,
        None => NaiveTime::MIN,
    };
    let today = Local.timestamp_opt(now, 0).single()?.date_naive();
    let at = |d: NaiveDate| {
        Local
            .from_local_datetime(&d.and_time(t))
            .earliest()
            .map(|x| x.timestamp())
    };
    match date {
        Some(d) => {
            let (mon, day) = d.split_once(' ')?;
            let m = month(mon)?;
            let day: u32 = day.trim().parse().ok()?;
            let this_year = at(NaiveDate::from_ymd_opt(today.year(), m, day)?)?;
            if this_year < now - 2 * 86400 {
                at(NaiveDate::from_ymd_opt(today.year() + 1, m, day)?)
            } else {
                Some(this_year)
            }
        }
        None => {
            let at_today = at(today)?;
            if at_today < now {
                at(today.succ_opt()?)
            } else {
                Some(at_today)
            }
        }
    }
}

/// `11:39am`, `2am`, `12:05pm`, `14:00`.
fn parse_clock(t: &str) -> Option<NaiveTime> {
    let t = t.trim().to_ascii_lowercase();
    let (body, pm) = match (t.strip_suffix("pm"), t.strip_suffix("am")) {
        (Some(b), _) => (b, Some(true)),
        (_, Some(b)) => (b, Some(false)),
        _ => (t.as_str(), None),
    };
    let (h, m) = match body.trim().split_once(':') {
        Some((h, m)) => (h.parse::<u32>().ok()?, m.parse::<u32>().ok()?),
        None => (body.trim().parse::<u32>().ok()?, 0),
    };
    let h = match pm {
        Some(true) if h < 12 => h + 12,
        Some(false) if h == 12 => 0,
        _ => h,
    };
    NaiveTime::from_hms_opt(h, m, 0)
}

fn month(s: &str) -> Option<u32> {
    let k = s.get(..3)?.to_ascii_lowercase();
    MONTHS.iter().position(|m| *m == k).map(|i| i as u32 + 1)
}

/// Run `claude /usage` (binary from `$PATH`) unless one ran within 5 minutes.
pub fn refresh(
    paths: &Paths,
    cfg: &Config,
    conn: &mut Connection,
) -> anyhow::Result<RefreshOutcome> {
    refresh_with(
        paths,
        cfg,
        conn,
        Path::new("claude"),
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
    let Some(bin) = resolve(bin) else {
        return Ok(RefreshOutcome::NotInstalled);
    };
    if let Some(throttled) = throttle(conn, KV_LAST_REFRESH, now)? {
        return Ok(throttled);
    }
    // --no-session-persistence keeps this run out of Claude's session logs (and so out
    // of tokenscope's own numbers). Running inside ~/.tokenscope means that even a
    // Claude version that ignored the flag would never log next to the user's projects.
    let dir = paths.data_dir();
    std::fs::create_dir_all(&dir)?;
    let mut cmd = Command::new(&bin);
    cmd.args(["-p", "--no-session-persistence", "/usage"])
        .current_dir(&dir);
    let out = crate::statusline::run_with_timeout(cmd, b"", REFRESH_TIMEOUT).ok_or_else(|| {
        anyhow::anyhow!(
            "claude /usage failed to start or timed out after {}s",
            REFRESH_TIMEOUT.as_secs()
        )
    })?;
    let text = String::from_utf8_lossy(&out);
    let snaps = parse_usage(&text, now, &cfg.claude_account);
    if snaps.is_empty() {
        let head: String = text.chars().take(200).collect();
        anyhow::bail!("claude /usage produced no usage lines: {}", head.trim());
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
