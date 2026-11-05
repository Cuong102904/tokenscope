pub mod cost;
pub mod limits;
pub mod sessions;
pub mod skills;

use chrono::{Duration, Local};
use rusqlite::types::Value;

#[derive(Debug, Clone, Default)]
pub struct Filter {
    /// Last N local days including today.
    pub days: Option<u32>,
    pub vendor: Option<String>,
    pub model: Option<String>,
    pub project: Option<String>,
}

impl Filter {
    pub fn days(n: u32) -> Filter {
        Filter { days: Some(n), ..Default::default() }
    }

    /// `(" WHERE ...", params)`, or an empty clause when nothing is filtered.
    pub fn sql_where(&self) -> (String, Vec<Value>) {
        let mut conds = vec![];
        let mut args = vec![];
        if let Some(n) = self.days {
            let from = Local::now().date_naive() - Duration::days(n.max(1) as i64 - 1);
            conds.push("day >= ?");
            args.push(Value::Text(from.format("%Y-%m-%d").to_string()));
        }
        for (col, v) in [("vendor", &self.vendor), ("model", &self.model), ("project", &self.project)] {
            if let Some(v) = v {
                conds.push(match col {
                    "vendor" => "vendor = ?",
                    "model" => "model = ?",
                    _ => "project = ?",
                });
                args.push(Value::Text(v.clone()));
            }
        }
        if conds.is_empty() {
            (String::new(), args)
        } else {
            (format!(" WHERE {}", conds.join(" AND ")), args)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sql_where_combines_filters() {
        assert_eq!(Filter::default().sql_where().0, "");
        let f = Filter {
            days: Some(7),
            vendor: Some("claude".into()),
            model: Some("m".into()),
            project: Some("/p".into()),
        };
        let (sql, args) = f.sql_where();
        assert_eq!(sql, " WHERE day >= ? AND vendor = ? AND model = ? AND project = ?");
        let from = (Local::now().date_naive() - Duration::days(6)).format("%Y-%m-%d").to_string();
        let texts: Vec<_> = args.iter().map(|v| match v { Value::Text(s) => s.clone(), _ => panic!() }).collect();
        assert_eq!(texts, vec![from, "claude".into(), "m".into(), "/p".into()]);
        let (sql, args) = Filter { model: Some("m".into()), ..Default::default() }.sql_where();
        assert_eq!((sql.as_str(), args.len()), (" WHERE model = ?", 1));
    }
}
