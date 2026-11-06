use super::cost::{Acc, load_rows};
use crate::collect::{self, AdapterStatus};
use crate::{config::Config, model::Vendor, pricing::PriceTable};
use rusqlite::{Connection, types::Value};
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct ActiveSession {
    pub vendor: String,
    pub session_id: String,
    pub project: String,
    pub title: Option<String>,
    /// Model of the most recent usage row (`None` when the session has no rows yet).
    pub model: Option<String>,
    /// Epoch seconds: file mtime (Claude, Codex) or `time_updated` (OpenCode).
    pub last_write: i64,
    /// Whole-session cost; `None` when any row is unpriced (see `priced_so_far`).
    pub cost_so_far: Option<f64>,
    /// Priced part of `cost_so_far`; with `cost_so_far: None` it is a lower bound.
    pub priced_so_far: f64,
    /// Cost of the rows with `ts >= now - 3600`; `None` when any of them is unpriced.
    pub cost_last_hour: Option<f64>,
    pub priced_last_hour: f64,
}

/// Sessions written within `cfg.active_minutes` of `now` (epoch seconds), most
/// recently written first. One row per (vendor, session_id): Claude subagent
/// files share the parent's id and are folded into it by the adapter.
pub fn list(
    conn: &Connection,
    prices: &PriceTable,
    cfg: &Config,
    now: i64,
) -> anyhow::Result<Vec<ActiveSession>> {
    let since = now - cfg.active_minutes.max(0) * 60;
    let mut st = conn.prepare(
        "SELECT vendor, session_id, project, title, last_write FROM sessions
         WHERE last_write >= ?1 ORDER BY last_write DESC, vendor, session_id",
    )?;
    let sessions = st
        .query_map([since], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, i64>(4)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let hour_ago = now - 3600;
    let mut out = vec![];
    for (vendor, session_id, project, title, last_write) in sessions {
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
            cost_so_far: all.cost(),
            priced_so_far: all.priced,
            cost_last_hour: hour.cost(),
            priced_last_hour: hour.priced,
        });
    }
    Ok(out)
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
