use super::{
    Filter,
    cost::{Acc, load_rows},
};
use crate::{config::Config, model::Vendor, pricing::PriceTable};
use rusqlite::{Connection, types::Value};
use serde::Serialize;
use std::collections::{BTreeMap, HashMap};

#[derive(Debug, Clone, Serialize)]
pub struct SessionRow {
    pub vendor: String,
    pub session_id: String,
    /// Account label from `Config::label` (configured label, else `vendor:account`).
    pub account: String,
    pub project: String,
    /// From the `sessions` table when present.
    pub title: Option<String>,
    /// Sorted by row count descending (name breaks ties).
    pub models: Vec<String>,
    pub messages: u64,
    pub subagent_messages: u64,
    /// `None` when any row of the session has an unpriced model.
    pub cost: Option<f64>,
    /// Priced part only; with `cost: None` it is a lower bound ("at least").
    pub priced_cost: f64,
    pub first_ts: i64,
    pub last_ts: i64,
}

/// Sessions grouped by (vendor, session_id) over the rows matching `f` (a day
/// window therefore counts only the rows inside it). Sorted by `priced_cost`
/// descending, ties by `last_ts` descending; truncated to `limit`.
pub fn list(
    conn: &Connection,
    prices: &PriceTable,
    cfg: &Config,
    f: &Filter,
    limit: usize,
) -> anyhow::Result<Vec<SessionRow>> {
    let (wh, args) = f.sql_where();
    struct G {
        row: SessionRow,
        acc: Acc,
        models: HashMap<String, u64>,
    }
    let mut groups: BTreeMap<(String, String), G> = BTreeMap::new();
    for r in load_rows(conn, prices, &wh, &args)? {
        let key = (r.vendor.clone(), r.session_id.clone());
        let g = groups.entry(key).or_insert_with(|| G {
            row: SessionRow {
                vendor: r.vendor.clone(),
                session_id: r.session_id.clone(),
                account: match Vendor::parse(&r.vendor) {
                    Some(v) => cfg.label(v, &r.account),
                    None => r.vendor.clone(),
                },
                project: r.project.clone(),
                title: None,
                models: vec![],
                messages: 0,
                subagent_messages: 0,
                cost: None,
                priced_cost: 0.0,
                first_ts: r.ts,
                last_ts: r.ts,
            },
            acc: Acc::default(),
            models: HashMap::new(),
        });
        g.acc.add(r.cost);
        *g.models.entry(r.model).or_default() += 1;
        g.row.messages += 1;
        g.row.subagent_messages += r.sidechain as u64;
        g.row.first_ts = g.row.first_ts.min(r.ts);
        g.row.last_ts = g.row.last_ts.max(r.ts);
    }
    let mut titles: HashMap<(String, String), String> = HashMap::new();
    let mut st =
        conn.prepare("SELECT vendor, session_id, title FROM sessions WHERE title IS NOT NULL")?;
    for t in st.query_map([], |r| {
        Ok((
            (r.get::<_, String>(0)?, r.get::<_, String>(1)?),
            r.get::<_, String>(2)?,
        ))
    })? {
        let (k, v) = t?;
        titles.insert(k, v);
    }
    let mut out: Vec<SessionRow> = groups
        .into_iter()
        .map(|(k, g)| {
            let mut row = g.row;
            row.title = titles.remove(&k);
            row.cost = g.acc.cost();
            row.priced_cost = g.acc.priced;
            let mut m: Vec<(String, u64)> = g.models.into_iter().collect();
            m.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            row.models = m.into_iter().map(|(n, _)| n).collect();
            row
        })
        .collect();
    out.sort_by(|a, b| {
        b.priced_cost
            .total_cmp(&a.priced_cost)
            .then_with(|| b.last_ts.cmp(&a.last_ts))
            .then_with(|| a.session_id.cmp(&b.session_id))
    });
    out.truncate(limit);
    Ok(out)
}

#[derive(Debug, Clone, Serialize)]
pub struct Timeline {
    /// `(ts, cumulative cost)` ordered by ts.
    pub points: Vec<(i64, f64)>,
    /// Models without a price. Their rows add 0 to the curve, so a non-empty
    /// list means the curve is a lower bound.
    pub unpriced_models: Vec<String>,
}

pub fn timeline_detail(
    conn: &Connection,
    prices: &PriceTable,
    vendor: &str,
    session_id: &str,
) -> anyhow::Result<Timeline> {
    let args = [
        Value::Text(vendor.to_string()),
        Value::Text(session_id.to_string()),
    ];
    let mut t = Timeline {
        points: vec![],
        unpriced_models: vec![],
    };
    let mut sum = 0.0;
    for r in load_rows(conn, prices, " WHERE vendor = ? AND session_id = ?", &args)? {
        match r.cost {
            Some(c) => sum += c,
            None if !t.unpriced_models.contains(&r.model) => t.unpriced_models.push(r.model),
            None => {}
        }
        t.points.push((r.ts, sum));
    }
    Ok(t)
}

/// Cumulative cost curve of one session. Unpriced rows add 0; use
/// [`timeline_detail`] to learn whether that happened.
pub fn timeline(
    conn: &Connection,
    prices: &PriceTable,
    vendor: &str,
    session_id: &str,
) -> anyhow::Result<Vec<(i64, f64)>> {
    Ok(timeline_detail(conn, prices, vendor, session_id)?.points)
}
