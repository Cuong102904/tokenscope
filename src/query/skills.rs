//! Skills and tools. Cost per skill comes from `usage.skill` / `usage.plugin` (Claude's
//! attributionSkill/attributionPlugin: messages produced while the skill was active).
//! Event counts come from `skill_events` / `tool_events`, which have no model/project
//! column: days and vendor apply directly, model and project narrow to sessions that
//! have a matching `usage` row (EXISTS on the (vendor, session_id) index).

use super::Filter;
use super::cost::Acc;
use crate::pricing::PriceTable;
use chrono::{Duration, Local, TimeZone};
use rusqlite::types::Value;
use rusqlite::{Connection, params_from_iter};
use serde::Serialize;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize)]
pub struct SkillCost {
    pub skill: String,
    pub plugin: Option<String>,
    /// `None` when any message of the skill has no price.
    pub cost: Option<f64>,
    /// Priced part only; with `cost: None` it is a lower bound.
    pub priced_cost: f64,
    pub messages: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct SkillCount {
    pub vendor: String,
    pub skill: String,
    /// `invoked` (Claude Skill tool) or `file_read` (Codex read of a SKILL.md).
    pub kind: String,
    pub count: u64,
    pub sessions: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct PluginCost {
    pub plugin: String,
    pub cost: Option<f64>,
    pub priced_cost: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolCount {
    pub vendor: String,
    pub tool: String,
    pub count: u64,
}

const TOOLS_LIMIT: usize = 20;

/// Usage rows that carry a skill, priced in Rust (prices are prefix-based).
struct SkillRow {
    skill: String,
    plugin: Option<String>,
    cost: Option<f64>,
}

fn load_skill_rows(conn: &Connection, prices: &PriceTable, f: &Filter) -> anyhow::Result<Vec<SkillRow>> {
    let (wh, args) = f.sql_where();
    let wh = if wh.is_empty() { " WHERE skill IS NOT NULL AND skill <> ''".to_string() } else { format!("{wh} AND skill IS NOT NULL AND skill <> ''") };
    let sql = format!(
        "SELECT skill, plugin, model, input, output, cache_read, cache_write_5m, cache_write_1h FROM usage{wh}"
    );
    let mut st = conn.prepare(&sql)?;
    let rows = st.query_map(params_from_iter(args.iter()), |r| {
        let n = |i: usize| r.get::<_, Option<i64>>(i).map(|v| v.unwrap_or(0).max(0) as u64);
        let model = r.get::<_, Option<String>>(2)?.unwrap_or_default();
        Ok(SkillRow {
            skill: r.get(0)?,
            plugin: r.get::<_, Option<String>>(1)?.filter(|p| !p.is_empty()),
            cost: prices.cost(&model, n(3)?, n(4)?, n(5)?, n(6)?, n(7)?),
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Sorted by priced cost descending (skill name breaks ties).
pub fn by_cost(conn: &Connection, prices: &PriceTable, f: &Filter) -> anyhow::Result<Vec<SkillCost>> {
    let mut m: BTreeMap<String, (Option<String>, Acc, u64)> = BTreeMap::new();
    for r in load_skill_rows(conn, prices, f)? {
        let e = m.entry(r.skill).or_default();
        if e.0.is_none() {
            e.0 = r.plugin;
        }
        e.1.add(r.cost);
        e.2 += 1;
    }
    let mut v: Vec<SkillCost> = m
        .into_iter()
        .map(|(skill, (plugin, acc, messages))| SkillCost { skill, plugin, cost: acc.cost(), priced_cost: acc.priced, messages })
        .collect();
    v.sort_by(|a, b| b.priced_cost.total_cmp(&a.priced_cost).then_with(|| a.skill.cmp(&b.skill)));
    Ok(v)
}

/// Cost by plugin; messages with a skill but no plugin are grouped under "(không có plugin)".
pub fn by_plugin(conn: &Connection, prices: &PriceTable, f: &Filter) -> anyhow::Result<Vec<PluginCost>> {
    let mut m: BTreeMap<String, Acc> = BTreeMap::new();
    for r in load_skill_rows(conn, prices, f)? {
        m.entry(r.plugin.unwrap_or_else(|| NO_PLUGIN.to_string())).or_default().add(r.cost);
    }
    let mut v: Vec<PluginCost> = m
        .into_iter()
        .map(|(plugin, acc)| PluginCost { plugin, cost: acc.cost(), priced_cost: acc.priced })
        .collect();
    v.sort_by(|a, b| b.priced_cost.total_cmp(&a.priced_cost).then_with(|| a.plugin.cmp(&b.plugin)));
    Ok(v)
}

const NO_PLUGIN: &str = "(không có plugin)";

/// `(" WHERE ...", params)` for an events table (`ts` epoch, no day/model/project column).
fn events_where(f: &Filter) -> (String, Vec<Value>) {
    let mut conds: Vec<String> = vec![];
    let mut args: Vec<Value> = vec![];
    if let Some(n) = f.days {
        let from = Local::now().date_naive() - Duration::days(n.max(1) as i64 - 1);
        let ts = from
            .and_hms_opt(0, 0, 0)
            .and_then(|t| Local.from_local_datetime(&t).earliest())
            .map(|t| t.timestamp())
            .unwrap_or(0);
        conds.push("e.ts >= ?".into());
        args.push(Value::Integer(ts));
    }
    if let Some(v) = &f.vendor {
        conds.push("e.vendor = ?".into());
        args.push(Value::Text(v.clone()));
    }
    for (col, v) in [("model", &f.model), ("project", &f.project)] {
        if let Some(v) = v {
            conds.push(format!("EXISTS (SELECT 1 FROM usage u WHERE u.vendor = e.vendor AND u.session_id = e.session_id AND u.{col} = ?)"));
            args.push(Value::Text(v.clone()));
        }
    }
    if conds.is_empty() { (String::new(), args) } else { (format!(" WHERE {}", conds.join(" AND ")), args) }
}

/// Per (vendor, skill, kind), most used first.
pub fn invocations(conn: &Connection, f: &Filter) -> anyhow::Result<Vec<SkillCount>> {
    let (wh, args) = events_where(f);
    let sql = format!(
        "SELECT e.vendor, e.skill, e.kind, count(*), count(DISTINCT e.session_id) FROM skill_events e{wh}
         GROUP BY e.vendor, e.skill, e.kind ORDER BY count(*) DESC, e.skill, e.vendor"
    );
    let mut st = conn.prepare(&sql)?;
    let rows = st.query_map(params_from_iter(args.iter()), |r| {
        Ok(SkillCount {
            vendor: r.get(0)?,
            skill: r.get(1)?,
            kind: r.get(2)?,
            count: r.get::<_, i64>(3)? as u64,
            sessions: r.get::<_, i64>(4)? as u64,
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Top 20 tools by call count, per vendor.
pub fn tools(conn: &Connection, f: &Filter) -> anyhow::Result<Vec<ToolCount>> {
    let (wh, args) = events_where(f);
    let sql = format!(
        "SELECT e.vendor, e.tool, count(*) FROM tool_events e{wh} GROUP BY e.vendor, e.tool
         ORDER BY count(*) DESC, e.tool, e.vendor LIMIT {TOOLS_LIMIT}"
    );
    let mut st = conn.prepare(&sql)?;
    let rows = st.query_map(params_from_iter(args.iter()), |r| {
        Ok(ToolCount { vendor: r.get(0)?, tool: r.get(1)?, count: r.get::<_, i64>(2)? as u64 })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}
