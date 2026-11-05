use super::Filter;
use crate::pricing::PriceTable;
use rusqlite::{Connection, params_from_iter};
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
