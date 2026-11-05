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
