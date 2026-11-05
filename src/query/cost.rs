use super::Filter;
use crate::pricing::PriceTable;
use chrono::{Duration, Local, NaiveDate};
use rusqlite::types::Value;
use rusqlite::{Connection, params_from_iter};
use std::collections::{BTreeMap, BTreeSet};
use serde::Serialize;

#[derive(Debug, Clone, Default, Serialize)]
pub struct Totals {
    /// Sum over priced models only; models without a price are listed in `unpriced_models`.
    pub cost: f64,
    pub unpriced_models: Vec<String>,
    pub sessions: u64,
    pub messages: u64,
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    /// 5m + 1h
    pub cache_write: u64,
}

pub fn totals(conn: &Connection, prices: &PriceTable, f: &Filter) -> anyhow::Result<Totals> {
    let (wh, args) = f.sql_where();
    let mut t = Totals::default();
    let sql = format!(
        "SELECT model, count(*), coalesce(sum(input),0), coalesce(sum(output),0), coalesce(sum(cache_read),0),
                coalesce(sum(cache_write_5m),0), coalesce(sum(cache_write_1h),0)
         FROM usage{wh} GROUP BY model ORDER BY model"
    );
    let mut st = conn.prepare(&sql)?;
    let rows = st.query_map(params_from_iter(args.iter()), |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?, r.get::<_, i64>(3)?,
            r.get::<_, i64>(4)?, r.get::<_, i64>(5)?, r.get::<_, i64>(6)?))
    })?;
    for row in rows {
        let (model, n, i, o, cr, w5, w1) = row?;
        let (i, o, cr, w5, w1) = (i as u64, o as u64, cr as u64, w5 as u64, w1 as u64);
        match prices.cost(&model, i, o, cr, w5, w1) {
            Some(c) => t.cost += c,
            None => t.unpriced_models.push(model),
        }
        t.messages += n as u64;
        t.input += i;
        t.output += o;
        t.cache_read += cr;
        t.cache_write += w5 + w1;
    }
    let sql = format!("SELECT count(DISTINCT vendor || '|' || session_id) FROM usage{wh}");
    t.sessions = conn.query_row(&sql, params_from_iter(args.iter()), |r| r.get::<_, i64>(0))? as u64;
    Ok(t)
}

/// One `usage` row with its cost already computed (prices are prefix-based, so
/// cost is always aggregated in Rust, never in SQL).
pub(crate) struct Row {
    pub vendor: String,
    pub account: String,
    pub session_id: String,
    pub project: String,
    pub ts: i64,
    pub day: String,
    pub model: String,
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub sidechain: bool,
    /// `None` when the model has no price.
    pub cost: Option<f64>,
}

pub(crate) fn load_rows(conn: &Connection, prices: &PriceTable, wh: &str, args: &[Value]) -> anyhow::Result<Vec<Row>> {
    let sql = format!(
        "SELECT vendor, account, session_id, project, ts, day, model, input, output, cache_read,
                cache_write_5m, cache_write_1h, sidechain
         FROM usage{wh} ORDER BY ts, msg_key"
    );
    let mut st = conn.prepare(&sql)?;
    let rows = st.query_map(params_from_iter(args.iter()), |r| {
        let n = |i: usize| r.get::<_, Option<i64>>(i).map(|v| v.unwrap_or(0).max(0) as u64);
        let text = |i: usize| r.get::<_, Option<String>>(i).map(Option::unwrap_or_default);
        let model = text(6)?;
        let (input, output, cache_read, w5, w1) = (n(7)?, n(8)?, n(9)?, n(10)?, n(11)?);
        Ok(Row {
            vendor: text(0)?,
            account: text(1)?,
            session_id: text(2)?,
            project: text(3)?,
            ts: r.get::<_, Option<i64>>(4)?.unwrap_or(0),
            day: text(5)?,
            cost: prices.cost(&model, input, output, cache_read, w5, w1),
            model,
            input,
            output,
            cache_read,
            cache_write: w5 + w1,
            sidechain: n(12)? != 0,
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Cost of a group of rows: the priced part plus whether anything was unpriced.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Acc {
    pub priced: f64,
    pub unpriced: bool,
}

impl Acc {
    pub fn add(&mut self, c: Option<f64>) {
        match c {
            Some(c) => self.priced += c,
            None => self.unpriced = true,
        }
    }

    /// `None` if any row was unpriced: never a silent partial sum.
    pub fn cost(&self) -> Option<f64> {
        (!self.unpriced).then_some(self.priced)
    }
}

/// Sort key: priced part descending (so rows with `cost: None` still rank by what is known).
fn by_priced_desc(a: f64, b: f64) -> std::cmp::Ordering {
    b.total_cmp(&a)
}

fn load(conn: &Connection, prices: &PriceTable, f: &Filter) -> anyhow::Result<Vec<Row>> {
    let (wh, args) = f.sql_where();
    load_rows(conn, prices, &wh, &args)
}

#[derive(Debug, Clone, Serialize)]
pub struct DayRow {
    pub day: String,
    /// Priced models only; empty for a day without usage.
    pub by_model: BTreeMap<String, f64>,
    /// Models used that day that have no price (absent from `by_model`, never shown as $0).
    pub unpriced: Vec<String>,
}

/// Every local day in the window, empty days included. `days: None` spans the
/// first to the last day present in the data.
pub fn daily(conn: &Connection, prices: &PriceTable, f: &Filter) -> anyhow::Result<Vec<DayRow>> {
    let rows = load(conn, prices, f)?;
    let mut by_day: BTreeMap<String, DayRow> = BTreeMap::new();
    for r in &rows {
        let d = by_day.entry(r.day.clone()).or_insert_with(|| DayRow {
            day: r.day.clone(),
            by_model: BTreeMap::new(),
            unpriced: vec![],
        });
        match r.cost {
            Some(c) => *d.by_model.entry(r.model.clone()).or_insert(0.0) += c,
            None if !d.unpriced.contains(&r.model) => d.unpriced.push(r.model.clone()),
            None => {}
        }
    }
    let parse = |s: &str| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok();
    let (from, to) = match f.days {
        Some(n) => {
            let today = Local::now().date_naive();
            (today - Duration::days(n.max(1) as i64 - 1), today)
        }
        None => {
            let days: Vec<NaiveDate> = by_day.keys().filter_map(|k| parse(k)).collect();
            match (days.iter().min(), days.iter().max()) {
                (Some(a), Some(b)) => (*a, *b),
                _ => return Ok(vec![]),
            }
        }
    };
    let mut out = vec![];
    let mut d = from;
    while d <= to {
        let key = d.format("%Y-%m-%d").to_string();
        out.push(by_day.remove(&key).unwrap_or_else(|| DayRow {
            day: key,
            by_model: BTreeMap::new(),
            unpriced: vec![],
        }));
        d += Duration::days(1);
    }
    Ok(out)
}

#[derive(Debug, Clone, Serialize)]
pub struct ModelRow {
    pub model: String,
    pub vendor: String,
    /// `None` when the model has no price.
    pub cost: Option<f64>,
    pub messages: u64,
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
}

/// Grouped by (model, vendor); sorted by cost descending, unpriced last.
pub fn by_model(conn: &Connection, prices: &PriceTable, f: &Filter) -> anyhow::Result<Vec<ModelRow>> {
    let mut m: BTreeMap<(String, String), (ModelRow, Acc)> = BTreeMap::new();
    for r in load(conn, prices, f)? {
        let (row, acc) = m.entry((r.model.clone(), r.vendor.clone())).or_insert_with(|| {
            (
                ModelRow {
                    model: r.model.clone(),
                    vendor: r.vendor.clone(),
                    cost: None,
                    messages: 0,
                    input: 0,
                    output: 0,
                    cache_read: 0,
                    cache_write: 0,
                },
                Acc::default(),
            )
        });
        acc.add(r.cost);
        row.messages += 1;
        row.input += r.input;
        row.output += r.output;
        row.cache_read += r.cache_read;
        row.cache_write += r.cache_write;
    }
    let mut v: Vec<(ModelRow, Acc)> = m.into_values().collect();
    for (row, acc) in &mut v {
        row.cost = acc.cost();
    }
    v.sort_by(|a, b| by_priced_desc(a.1.priced, b.1.priced).then_with(|| a.0.model.cmp(&b.0.model)));
    Ok(v.into_iter().map(|(r, _)| r).collect())
}

#[derive(Debug, Clone, Serialize)]
pub struct ProjectRow {
    pub project: String,
    /// `None` when any model in the project has no price.
    pub cost: Option<f64>,
    /// Sum over the priced part; with `cost: None` it is a lower bound ("at least").
    pub priced_cost: f64,
    pub sessions: u64,
    pub messages: u64,
}

/// Sorted by `priced_cost` descending (project name breaks ties).
pub fn by_project(conn: &Connection, prices: &PriceTable, f: &Filter) -> anyhow::Result<Vec<ProjectRow>> {
    type Group = (Acc, BTreeSet<(String, String)>, u64);
    let mut m: BTreeMap<String, Group> = BTreeMap::new();
    for r in load(conn, prices, f)? {
        let e = m.entry(r.project).or_default();
        e.0.add(r.cost);
        e.1.insert((r.vendor, r.session_id));
        e.2 += 1;
    }
    let mut v: Vec<ProjectRow> = m
        .into_iter()
        .map(|(project, (acc, s, n))| ProjectRow {
            project,
            cost: acc.cost(),
            priced_cost: acc.priced,
            sessions: s.len() as u64,
            messages: n,
        })
        .collect();
    v.sort_by(|a, b| by_priced_desc(a.priced_cost, b.priced_cost).then_with(|| a.project.cmp(&b.project)));
    Ok(v)
}

#[derive(Debug, Clone, Serialize)]
pub struct Efficiency {
    /// cache_read / (input + cache_read); 0 when the denominator is 0.
    pub cache_hit_ratio: f64,
    /// Cost of sidechain (subagent) rows / total priced cost; 0 when that total is 0.
    /// Unpriced rows are excluded from both sides.
    pub subagent_cost_share: f64,
}

pub fn efficiency(conn: &Connection, prices: &PriceTable, f: &Filter) -> anyhow::Result<Efficiency> {
    let (mut input, mut cache_read, mut total, mut side) = (0u64, 0u64, 0.0, 0.0);
    for r in load(conn, prices, f)? {
        input += r.input;
        cache_read += r.cache_read;
        let c = r.cost.unwrap_or(0.0);
        total += c;
        if r.sidechain {
            side += c;
        }
    }
    let denom = input + cache_read;
    Ok(Efficiency {
        cache_hit_ratio: if denom == 0 { 0.0 } else { cache_read as f64 / denom as f64 },
        subagent_cost_share: if total == 0.0 { 0.0 } else { side / total },
    })
}

#[derive(Debug, Clone, Serialize)]
pub struct VendorRow {
    pub vendor: String,
    /// `None` when any model of the vendor has no price.
    pub cost: Option<f64>,
    /// Priced part only; with `cost: None` it is a lower bound ("at least").
    pub priced_cost: f64,
    pub messages: u64,
}

/// Sorted by `priced_cost` descending.
pub fn by_vendor(conn: &Connection, prices: &PriceTable, f: &Filter) -> anyhow::Result<Vec<VendorRow>> {
    let mut m: BTreeMap<String, (Acc, u64)> = BTreeMap::new();
    for r in load(conn, prices, f)? {
        let e = m.entry(r.vendor).or_default();
        e.0.add(r.cost);
        e.1 += 1;
    }
    let mut v: Vec<VendorRow> = m
        .into_iter()
        .map(|(vendor, (acc, messages))| VendorRow { vendor, cost: acc.cost(), priced_cost: acc.priced, messages })
        .collect();
    v.sort_by(|a, b| by_priced_desc(a.priced_cost, b.priced_cost).then_with(|| a.vendor.cmp(&b.vendor)));
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{db, paths::Paths};
    use chrono::{Duration, Local};

    fn day(ago: i64) -> String {
        (Local::now().date_naive() - Duration::days(ago)).format("%Y-%m-%d").to_string()
    }

    fn ins(c: &Connection, key: &str, vendor: &str, sess: &str, model: &str, day: &str, v: [i64; 5]) {
        c.execute(
            "INSERT INTO usage(msg_key,vendor,account,session_id,project,ts,day,model,input,output,reasoning,
               cache_read,cache_write_5m,cache_write_1h,sidechain) VALUES(?1,?2,'',?3,'p',0,?4,?5,?6,?7,0,?8,?9,?10,0)",
            rusqlite::params![key, vendor, sess, day, model, v[0], v[1], v[2], v[3], v[4]],
        )
        .unwrap();
    }

    #[test]
    fn totals_cost_unpriced_and_day_window() {
        let d = tempfile::tempdir().unwrap();
        let prices = PriceTable::load(&Paths { home: d.path().to_path_buf() }).unwrap();
        let c = db::open(&d.path().join("t.db")).unwrap();
        // claude-fable-5-1: in $10, out $50, cache_read $0.25 per 1M; write5m x1.25, write1h x2
        // (1000*10 + 2000*50 + 4000*0.25 + 100*10*1.25 + 10*10*2) / 1e6 = 0.11245
        ins(&c, "a", "claude", "s1", "claude-fable-5-1", &day(0), [1000, 2000, 4000, 100, 10]);
        ins(&c, "b", "claude", "s1", "no-such-model", &day(0), [5, 6, 7, 8, 9]);
        ins(&c, "c", "claude", "s2", "claude-fable-5-1", &day(10), [1000, 2000, 4000, 100, 10]);

        let today = totals(&c, &prices, &Filter::days(1)).unwrap();
        assert!((today.cost - 0.11245).abs() < 1e-9, "{}", today.cost);
        assert_eq!(today.unpriced_models, vec!["no-such-model".to_string()]);
        assert_eq!((today.messages, today.sessions), (2, 1));
        assert_eq!((today.input, today.output, today.cache_read, today.cache_write), (1005, 2006, 4007, 127));

        let d7 = totals(&c, &prices, &Filter::days(7)).unwrap();
        assert_eq!(d7.messages, 2);
        let all = totals(&c, &prices, &Filter::default()).unwrap();
        assert!((all.cost - 0.2249).abs() < 1e-9);
        assert_eq!((all.messages, all.sessions), (3, 2));

        let one = Filter { model: Some("claude-fable-5-1".into()), ..Default::default() };
        assert_eq!(totals(&c, &prices, &one).unwrap().messages, 2);
        let none = Filter { vendor: Some("codex".into()), ..Default::default() };
        let z = totals(&c, &prices, &none).unwrap();
        assert_eq!((z.messages, z.cost), (0, 0.0));
    }
}
