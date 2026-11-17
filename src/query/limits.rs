//! Rate-limit views: the newest measurement per `(vendor, account, scope)`
//! with stale/expired flags and an ETA to 100%, history for the chart, and
//! the "limit reached" events.

use crate::config::Config;
use crate::model::Vendor;
use rusqlite::{Connection, params};
use serde::Serialize;
use std::collections::HashMap;

/// Two measurements belong to the same window when their `resets_at` differ
/// by at most this much (Codex reports it with ±1 s jitter).
const SAME_RESET_SECS: i64 = 60;
const ETA_LOOKBACK_SECS: i64 = 3600;
const ETA_MIN_POINTS: usize = 3;
const ETA_MIN_SPAN_SECS: i64 = 600;

/// An account with no reading for this many days is no longer shown.
pub const FORGET_AFTER_DAYS: i64 = 30;
pub const EXPIRED_NOTE: &str = "reset, waiting for a new reading";
pub const NO_DATA_NOTE: &str = "No rate-limit data yet. Click Refresh Claude on the Limits tab, or add the status line: `tokenscope statusline --print-snippet`";
pub const UNKNOWN_ACCOUNT_LABEL: &str = "old logs without an account ID";
const STALE_ETA_NOTE: &str = "reading is stale, no ETA";
const MIXED_REASON: &str = "readings mix accounts or are noisy";
const MIXED_WARNING: &str =
    "% used went down within one window: readings mix accounts or are noisy, no ETA";

#[derive(Debug, Clone, Serialize)]
pub struct LimitCard {
    pub vendor: String,
    /// Raw account key (sha8 or ""), needed to ask for the history.
    pub account: String,
    pub account_label: String,
    /// The name can be changed from the dashboard (it does not come from `config.toml`).
    pub label_editable: bool,
    /// This account has the newest reading of its vendor: the one that is logged in, when
    /// only one account at a time can be (Codex, Claude Code).
    pub current: bool,
    pub scope: String,
    pub window_minutes: Option<i64>,
    /// Of the newest measurement. Not current when `expired` (the window has reset since).
    pub used_percent: f64,
    pub resets_at: Option<i64>,
    pub measured_at: i64,
    pub source: String,
    pub plan_type: Option<String>,
    pub stale: bool,
    /// `resets_at` is in the past: the shown number belongs to the previous window.
    pub expired: bool,
    /// Placeholder card: no measurement exists for this vendor yet.
    pub no_data: bool,
    pub eta: Option<i64>,
    /// Why there is no ETA (empty when there is one).
    pub eta_note: String,
    pub warning: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LimitEventRow {
    pub vendor: String,
    pub account: String,
    pub account_label: String,
    pub ts: i64,
    pub scope: String,
    pub resets_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum EtaResult {
    /// Epoch second at which the linear trend reaches 100%.
    At(i64),
    Unavailable(String),
}

/// Measurements older than this are flagged stale.
pub fn stale_after_secs(window_minutes: Option<i64>) -> i64 {
    match window_minutes {
        Some(w) if w <= 300 => 30 * 60,
        _ => 6 * 3600,
    }
}

/// Longest account name, in characters.
pub const MAX_LABEL_CHARS: usize = 60;
const LABEL_PREFIX: &str = "label:";

/// Names set from the dashboard, keyed `<vendor>:<account>` (kv rows `label:<vendor>:<account>`).
pub fn stored_labels(conn: &Connection) -> anyhow::Result<HashMap<String, String>> {
    let mut st = conn.prepare("SELECT key, value FROM kv WHERE key LIKE 'label:%'")?;
    let rows = st
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows
        .into_iter()
        .filter_map(|(k, v)| Some((k.strip_prefix(LABEL_PREFIX)?.to_string(), v)))
        .collect())
}

/// Name an account from the dashboard. The name is trimmed; a blank one removes it. Accounts
/// named in `config.toml` keep that name.
pub fn set_label(
    conn: &Connection,
    vendor: &str,
    account: &str,
    label: &str,
) -> anyhow::Result<()> {
    anyhow::ensure!(Vendor::parse(vendor).is_some(), "unknown vendor: {vendor}");
    anyhow::ensure!(
        !account.is_empty()
            && account.len() <= 64
            && account
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')),
        "bad account key"
    );
    let label = label.trim();
    anyhow::ensure!(
        label.chars().count() <= MAX_LABEL_CHARS && !label.chars().any(char::is_control),
        "a name is at most {MAX_LABEL_CHARS} characters, on one line"
    );
    let key = format!("{LABEL_PREFIX}{vendor}:{account}");
    if label.is_empty() {
        conn.execute("DELETE FROM kv WHERE key = ?1", [key])?;
    } else {
        conn.execute(
            "INSERT OR REPLACE INTO kv(key, value) VALUES(?1, ?2)",
            [key, label.to_string()],
        )?;
    }
    Ok(())
}

/// Name shown for an account: `config.toml`, else the dashboard name, else `vendor:account`.
/// The flag says whether the dashboard may rename it.
pub fn account_label(
    cfg: &Config,
    stored: &HashMap<String, String>,
    vendor: &str,
    account: &str,
) -> (String, bool) {
    let key = format!("{vendor}:{account}");
    if cfg.account_labels.contains_key(&key) {
        let v = Vendor::parse(vendor).unwrap_or(Vendor::Claude);
        return (cfg.label(v, account), false);
    }
    if let Some(l) = stored.get(&key) {
        return (l.clone(), true);
    }
    let label = match Vendor::parse(vendor) {
        Some(Vendor::Codex) if account.is_empty() => UNKNOWN_ACCOUNT_LABEL.to_string(),
        Some(v) => cfg.label(v, account),
        None if account.is_empty() => vendor.to_string(),
        None => key,
    };
    (label, !account.is_empty())
}

fn vendor_rank(v: &str) -> usize {
    ["claude", "codex", "opencode", "agy"]
        .iter()
        .position(|x| *x == v)
        .unwrap_or(9)
}

/// The newest measurement per `(vendor, account, scope)`, Claude first. A
/// placeholder Claude card (`no_data`) is added when Claude has none at all.
pub fn current(conn: &Connection, cfg: &Config, now: i64) -> anyhow::Result<Vec<LimitCard>> {
    let mut st = conn.prepare(
        "SELECT vendor, account, scope, window_minutes, used_percent, resets_at, measured_at, source, plan_type
         FROM limit_snapshots s
         WHERE measured_at = (SELECT MAX(measured_at) FROM limit_snapshots m
                              WHERE m.vendor = s.vendor AND m.account = s.account AND m.scope = s.scope)",
    )?;
    let rows = st.query_map([], |r| {
        Ok(LimitCard {
            vendor: r.get(0)?,
            account: r.get(1)?,
            account_label: String::new(),
            label_editable: true,
            current: false,
            scope: r.get(2)?,
            window_minutes: r.get(3)?,
            used_percent: r.get(4)?,
            resets_at: r.get(5)?,
            measured_at: r.get(6)?,
            source: r.get(7)?,
            plan_type: r.get(8)?,
            stale: false,
            expired: false,
            no_data: false,
            eta: None,
            eta_note: String::new(),
            warning: None,
        })
    })?;
    let rows: Vec<LimitCard> = rows.collect::<Result<_, _>>()?;
    // An account's plan is the one on its newest reading. Windows last seen under another
    // plan (a monthly window from before a free -> plus upgrade, say) no longer apply.
    let mut plan_now: HashMap<(&str, &str), (i64, Option<&str>)> = HashMap::new();
    for c in &rows {
        let e = plan_now
            .entry((c.vendor.as_str(), c.account.as_str()))
            .or_insert((i64::MIN, None));
        if c.measured_at > e.0 && c.plan_type.is_some() {
            *e = (c.measured_at, c.plan_type.as_deref());
        }
    }
    let current_plan = |c: &LimitCard| match (
        c.plan_type.as_deref(),
        plan_now.get(&(c.vendor.as_str(), c.account.as_str())),
    ) {
        (Some(p), Some((_, Some(now_p)))) => p == *now_p,
        _ => true,
    };
    // Accounts not read for FORGET_AFTER_DAYS are gone from view (old account-less Codex
    // logs, an account no longer used); their history stays in the database.
    let mut newest: HashMap<(&str, &str), i64> = HashMap::new();
    for c in &rows {
        let e = newest
            .entry((c.vendor.as_str(), c.account.as_str()))
            .or_insert(i64::MIN);
        *e = (*e).max(c.measured_at);
    }
    let recent = |c: &LimitCard| {
        newest[&(c.vendor.as_str(), c.account.as_str())] >= now - FORGET_AFTER_DAYS * 86400
    };
    let rows: Vec<LimitCard> = rows
        .iter()
        .filter(|c| current_plan(c) && recent(c))
        .cloned()
        .collect();
    let stored = stored_labels(conn)?;
    // per vendor, the account of its newest reading
    let mut latest: HashMap<&str, (i64, &str)> = HashMap::new();
    for c in &rows {
        let e = latest.entry(c.vendor.as_str()).or_insert((i64::MIN, ""));
        if (c.measured_at, c.account.as_str()) > *e {
            *e = (c.measured_at, c.account.as_str());
        }
    }
    let latest: HashMap<String, String> = latest
        .into_iter()
        .map(|(v, (_, a))| (v.to_string(), a.to_string()))
        .collect();
    let mut cards = vec![];
    for c in rows {
        let mut c = c;
        (c.account_label, c.label_editable) = account_label(cfg, &stored, &c.vendor, &c.account);
        c.current = !c.account.is_empty() && latest.get(&c.vendor) == Some(&c.account);
        c.stale = now - c.measured_at > stale_after_secs(c.window_minutes);
        c.expired = c.resets_at.is_some_and(|r| r < now);
        if c.expired {
            c.eta_note = EXPIRED_NOTE.to_string();
        } else if let Some(r) = c.resets_at {
            let points = window_points(conn, &c, r)?;
            match eta(&points, r) {
                // an old trend is not a forecast (it may even point into the past)
                EtaResult::At(_) if c.stale => c.eta_note = STALE_ETA_NOTE.to_string(),
                EtaResult::At(t) if t < now => {
                    c.eta_note = "projected time has passed, waiting for a new reading".to_string()
                }
                EtaResult::At(t) => c.eta = Some(t),
                EtaResult::Unavailable(why) => {
                    if why == MIXED_REASON {
                        c.warning = Some(MIXED_WARNING.to_string());
                    }
                    c.eta_note = why;
                }
            }
        } else {
            c.eta_note = "no reset time".to_string();
        }
        cards.push(c);
    }
    if !cards.iter().any(|c| c.vendor == "claude") {
        cards.push(LimitCard {
            vendor: "claude".into(),
            account: String::new(),
            account_label: "claude".into(),
            label_editable: false,
            current: false,
            scope: String::new(),
            window_minutes: None,
            used_percent: 0.0,
            resets_at: None,
            measured_at: 0,
            source: String::new(),
            plan_type: None,
            stale: false,
            expired: false,
            no_data: true,
            eta: None,
            eta_note: NO_DATA_NOTE.to_string(),
            warning: None,
        });
    }
    cards.sort_by(|a, b| {
        (
            vendor_rank(&a.vendor),
            &a.account_label,
            a.window_minutes,
            &a.scope,
        )
            .cmp(&(
                vendor_rank(&b.vendor),
                &b.account_label,
                b.window_minutes,
                &b.scope,
            ))
    });
    Ok(cards)
}

/// Measurements of `c`'s key in the same reset window, within the ETA lookback before its newest one.
fn window_points(
    conn: &Connection,
    c: &LimitCard,
    resets_at: i64,
) -> anyhow::Result<Vec<(i64, f64)>> {
    let mut st = conn.prepare(
        "SELECT measured_at, used_percent FROM limit_snapshots
         WHERE vendor=?1 AND account=?2 AND scope=?3 AND measured_at >= ?4 AND ABS(resets_at - ?5) <= ?6
         ORDER BY measured_at",
    )?;
    let v = st
        .query_map(
            params![
                c.vendor,
                c.account,
                c.scope,
                c.measured_at - ETA_LOOKBACK_SECS,
                resets_at,
                SAME_RESET_SECS
            ],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?
        .collect::<Result<_, _>>()?;
    Ok(v)
}

/// When the linear trend of `points` (`(measured_at, used %)`, one reset
/// window) reaches 100%. Uses only the last 60 minutes (relative to the newest
/// point); needs ≥ 3 points spanning ≥ 10 minutes; any decrease means mixed
/// accounts or noise.
pub fn eta(points: &[(i64, f64)], resets_at: i64) -> EtaResult {
    let no = |s: &str| EtaResult::Unavailable(s.to_string());
    let mut pts = points.to_vec();
    pts.sort_by_key(|p| p.0);
    let Some(&(last_t, last_y)) = pts.last() else {
        return no("no readings yet");
    };
    pts.retain(|p| p.0 >= last_t - ETA_LOOKBACK_SECS);
    if pts.windows(2).any(|w| w[1].1 < w[0].1) {
        return no(MIXED_REASON);
    }
    if pts.len() < ETA_MIN_POINTS {
        return no("not enough readings (need ≥ 3 in 60 minutes)");
    }
    if last_t - pts[0].0 < ETA_MIN_SPAN_SECS {
        return no("not enough readings (they must span ≥ 10 minutes)");
    }
    if last_y >= 100.0 {
        return no("limit already reached");
    }
    // least squares on times relative to the first point (keeps f64 precise)
    let t0 = pts[0].0;
    let n = pts.len() as f64;
    let mt = pts.iter().map(|p| (p.0 - t0) as f64).sum::<f64>() / n;
    let my = pts.iter().map(|p| p.1).sum::<f64>() / n;
    let cov: f64 = pts
        .iter()
        .map(|p| ((p.0 - t0) as f64 - mt) * (p.1 - my))
        .sum();
    let var: f64 = pts.iter().map(|p| ((p.0 - t0) as f64 - mt).powi(2)).sum();
    let slope = cov / var;
    if slope <= 0.0 || !slope.is_finite() {
        return no("no increase in the last 60 minutes");
    }
    let at = (t0 as f64 + mt + (100.0 - my) / slope).round() as i64;
    if at > resets_at {
        return no("won't hit the limit before reset");
    }
    EtaResult::At(at.max(last_t))
}

/// `(measured_at, used %)` for one key since `since`, oldest first.
pub fn history(
    conn: &Connection,
    vendor: &str,
    account: &str,
    scope: &str,
    since: i64,
) -> anyhow::Result<Vec<(i64, f64)>> {
    let mut st = conn.prepare(
        "SELECT measured_at, used_percent FROM limit_snapshots
         WHERE vendor=?1 AND account=?2 AND scope=?3 AND measured_at >= ?4 ORDER BY measured_at",
    )?;
    let v = st
        .query_map(params![vendor, account, scope, since], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?
        .collect::<Result<_, _>>()?;
    Ok(v)
}

/// Distinct reset moments reported by measurements since `since` (jitter collapsed), ascending.
pub fn resets(
    conn: &Connection,
    vendor: &str,
    account: &str,
    scope: &str,
    since: i64,
) -> anyhow::Result<Vec<i64>> {
    let mut st = conn.prepare(
        "SELECT DISTINCT resets_at FROM limit_snapshots
         WHERE vendor=?1 AND account=?2 AND scope=?3 AND measured_at >= ?4 AND resets_at IS NOT NULL ORDER BY resets_at",
    )?;
    let all: Vec<i64> = st
        .query_map(params![vendor, account, scope, since], |r| r.get(0))?
        .collect::<Result<_, _>>()?;
    let mut out: Vec<i64> = vec![];
    for r in all {
        if out.last().is_none_or(|l| r - l > SAME_RESET_SECS) {
            out.push(r);
        }
    }
    Ok(out)
}

/// Limit-reached events since `since`, newest first.
pub fn events(conn: &Connection, cfg: &Config, since: i64) -> anyhow::Result<Vec<LimitEventRow>> {
    let mut st = conn.prepare(
        "SELECT vendor, account, ts, scope, resets_at FROM limit_events WHERE ts >= ?1 ORDER BY ts DESC, vendor, scope",
    )?;
    let stored = stored_labels(conn)?;
    let v = st
        .query_map([since], |r| {
            let (vendor, account): (String, String) = (r.get(0)?, r.get(1)?);
            Ok(LimitEventRow {
                account_label: account_label(cfg, &stored, &vendor, &account).0,
                vendor,
                account,
                ts: r.get(2)?,
                scope: r.get(3)?,
                resets_at: r.get(4)?,
            })
        })?
        .collect::<Result<_, _>>()?;
    Ok(v)
}
