use super::cost::{Acc, load_rows};
use crate::collect::{self, AdapterStatus};
use crate::open_sessions::OpenSessions;
use crate::{config::Config, model::Vendor, pricing::PriceTable};
use rusqlite::{Connection, OptionalExtension, types::Value};
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct ActiveSession {
    pub vendor: String,
    pub session_id: String,
    pub project: String,
    pub title: Option<String>,
    /// Model of the most recent usage row (`None` when the session has no rows yet).
    pub model: Option<String>,
    /// Epoch seconds: file mtime (Claude, Codex) or `time_updated` (OpenCode); for an open
    /// session with nothing logged yet, its last busy/idle change.
    pub last_write: i64,
    /// [`WORKING`] or [`IDLE`] (open, waiting for input). A session whose process cannot be
    /// seen (`pid: None`) is listed because its log was written in the last `active_minutes`,
    /// and counts as working.
    pub state: &'static str,
    /// Epoch seconds of the last busy/idle change, when the agent reports it.
    pub state_since: Option<i64>,
    /// The agent's process, when its open sessions can be seen (Claude Code).
    pub pid: Option<u32>,
    /// Whole-session cost; `None` when any row is unpriced (see `priced_so_far`).
    pub cost_so_far: Option<f64>,
    /// Priced part of `cost_so_far`; with `cost_so_far: None` it is a lower bound.
    pub priced_so_far: f64,
    /// Cost of the rows with `ts >= now - 3600`; `None` when any of them is unpriced.
    pub cost_last_hour: Option<f64>,
    pub priced_last_hour: f64,
}

pub const WORKING: &str = "working";
pub const IDLE: &str = "idle";

/// `(open, working)` for a [`list`] result: every listed session is open or was just written.
pub fn counts(sessions: &[ActiveSession]) -> (usize, usize) {
    let working = sessions.iter().filter(|s| s.state == WORKING).count();
    (sessions.len(), working)
}

/// Sessions open now (see [`OpenSessions`]) plus sessions written within `cfg.active_minutes`
/// of `now` (epoch seconds): working first, then idle, each most recently written first.
/// One row per (vendor, session_id): Claude subagent files share the parent's id and are
/// folded into it by the adapter.
pub fn list(
    conn: &Connection,
    prices: &PriceTable,
    cfg: &Config,
    open: &OpenSessions,
    now: i64,
) -> anyhow::Result<Vec<ActiveSession>> {
    let since = now - cfg.active_minutes.max(0) * 60;
    let hour_ago = now - 3600;
    let mut out = vec![];
    for Candidate {
        vendor,
        session_id,
        project,
        title,
        last_write,
    } in candidates(conn, open, since, now)?
    {
        let (state, state_since, pid) = state_of(open, &vendor, &session_id);
        let args = [Value::Text(vendor.clone()), Value::Text(session_id.clone())];
        let (mut all, mut hour) = (Acc::default(), Acc::default());
        let mut model: Option<String> = None;
        // rows come ordered by ts, so the last one is the most recent
        let mut project = project.unwrap_or_default();
        for r in load_rows(conn, prices, " WHERE vendor = ? AND session_id = ?", &args)? {
            all.add(r.cost);
            if r.ts >= hour_ago {
                hour.add(r.cost);
            }
            if project.is_empty() {
                project = r.project.clone();
            }
            model = Some(r.model);
        }
        out.push(ActiveSession {
            vendor,
            session_id,
            project,
            title,
            model,
            last_write,
            state,
            state_since,
            pid,
            cost_so_far: all.cost(),
            priced_so_far: all.priced,
            cost_last_hour: hour.cost(),
            priced_last_hour: hour.priced,
        });
    }
    // stable: equal times keep the query's vendor, session_id order
    out.sort_by_key(|s| (s.state != WORKING, std::cmp::Reverse(s.last_write)));
    Ok(out)
}

struct Candidate {
    vendor: String,
    session_id: String,
    project: Option<String>,
    title: Option<String>,
    last_write: i64,
}

/// Sessions written since `since`, then open sessions not among them: quiet
/// for longer than the window, or with nothing logged yet (project = the process's cwd).
fn candidates(
    conn: &Connection,
    open: &OpenSessions,
    since: i64,
    now: i64,
) -> anyhow::Result<Vec<Candidate>> {
    let mut st = conn.prepare(
        "SELECT vendor, session_id, project, title, last_write FROM sessions
         WHERE last_write >= ?1 ORDER BY last_write DESC, vendor, session_id",
    )?;
    let mut out = st
        .query_map([since], |r| {
            Ok(Candidate {
                vendor: r.get(0)?,
                session_id: r.get(1)?,
                project: r.get(2)?,
                title: r.get(3)?,
                last_write: r.get(4)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let mut one = conn.prepare(
        "SELECT project, title, last_write FROM sessions WHERE vendor = ?1 AND session_id = ?2",
    )?;
    let mut opened: Vec<Candidate> = vec![];
    for o in open.all() {
        let vendor = o.vendor.as_str();
        if out
            .iter()
            .any(|c| c.vendor == vendor && c.session_id == o.session_id)
        {
            continue;
        }
        let stored = one
            .query_row([vendor, o.session_id.as_str()], |r| {
                Ok((
                    r.get::<_, Option<String>>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            })
            .optional()?;
        let (project, title, last_write) = stored.unwrap_or((None, None, o.since.unwrap_or(now)));
        opened.push(Candidate {
            vendor: vendor.to_string(),
            session_id: o.session_id.clone(),
            project: project.filter(|p| !p.is_empty()).or(Some(o.cwd.clone())),
            title,
            last_write,
        });
    }
    out.extend(opened);
    Ok(out)
}

/// `(state, state_since, pid)`: from the session's process when it is open, otherwise
/// working (it is listed only because its log was written recently: a `claude -p` run, an
/// agent whose processes cannot be seen, or a session closed a moment ago).
fn state_of(
    open: &OpenSessions,
    vendor: &str,
    session_id: &str,
) -> (&'static str, Option<i64>, Option<u32>) {
    match open.find(vendor, session_id) {
        Some(o) => (if o.busy { WORKING } else { IDLE }, o.since, Some(o.pid)),
        None => (WORKING, None, None),
    }
}

/// One row of "Data sources": the last run's status (`records` = new rows of that run) plus
/// what is stored in total.
#[derive(Debug, Clone, Serialize)]
pub struct SourceRow {
    #[serde(flatten)]
    pub status: AdapterStatus,
    /// All `usage` rows stored for this vendor.
    pub total_records: i64,
    /// Newest `limit_snapshots.measured_at` for this vendor (epoch seconds).
    pub last_limit_at: Option<i64>,
}

/// `(stored usage rows, newest limit measurement)` for one vendor; zeros/`None` on any error.
pub fn stored_counts(conn: &Connection, vendor: Vendor) -> (i64, Option<i64>) {
    let total = conn
        .query_row(
            "SELECT count(*) FROM usage WHERE vendor=?1",
            [vendor.as_str()],
            |r| r.get(0),
        )
        .unwrap_or(0);
    let last = conn
        .query_row(
            "SELECT max(measured_at) FROM limit_snapshots WHERE vendor=?1",
            [vendor.as_str()],
            |r| r.get(0),
        )
        .unwrap_or(None);
    (total, last)
}

/// Stored status of every vendor that has run (kv `status:<vendor>`), in a fixed vendor order.
pub fn sources(conn: &Connection) -> Vec<SourceRow> {
    [
        Vendor::Claude,
        Vendor::Codex,
        Vendor::OpenCode,
        Vendor::Agy,
        Vendor::Pi,
    ]
    .into_iter()
    .filter_map(|v| {
        let status = collect::load_status(conn, v)?;
        let (total_records, last_limit_at) = stored_counts(conn, v);
        Some(SourceRow {
            status,
            total_records,
            last_limit_at,
        })
    })
    .collect()
}
