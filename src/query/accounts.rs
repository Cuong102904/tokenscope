//! The accounts overview: one row per account on the machine, with its name, whether it is the
//! signed-in one, the quota left in each window and what it used over the filter's range.
//!
//! Accounts come from three places that are merged by `(vendor, account key)`: the usage rows,
//! the newest limit readings, and the account registry (`accounts`), so an account shows up even
//! when it has no usage in the range or no reading yet.

use super::{Filter, cost, limits};
use crate::accounts::{self, AccountInfo};
use crate::config::Config;
use crate::model::Vendor;
use crate::pricing::PriceTable;
use rusqlite::Connection;
use serde::Serialize;
use std::collections::BTreeMap;

const ALL_VENDORS: [Vendor; 5] = [
    Vendor::Claude,
    Vendor::Codex,
    Vendor::OpenCode,
    Vendor::Agy,
    Vendor::Pi,
];
/// Shown for usage recorded before sessions carried an owner.
pub const UNKNOWN_ACCOUNT: &str = "unknown account";

#[derive(Debug, Clone, Serialize)]
pub struct AccountWindow {
    pub scope: String,
    pub used_percent: f64,
    pub resets_at: Option<i64>,
    pub eta: Option<i64>,
    /// The window has reset since this reading: the number is from the previous window.
    pub expired: bool,
    pub stale: bool,
    /// When the reading was taken (the dashboard shows its age when it is stale).
    pub measured_at: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct AccountRow {
    pub vendor: String,
    /// Raw account key; the dashboard needs it for names and history, never as a name.
    pub account: String,
    pub label: String,
    pub email: Option<String>,
    pub org: Option<String>,
    pub plan: Option<String>,
    /// The signed-in account of its vendor (newest reading).
    pub current: bool,
    pub windows: Vec<AccountWindow>,
    pub tokens: u64,
    /// `None` when any model of the account has no price.
    pub cost: Option<f64>,
    pub priced_cost: f64,
}

#[derive(Default)]
struct Build {
    info: Option<AccountInfo>,
    plan_type: Option<String>,
    current: bool,
    windows: Vec<AccountWindow>,
    tokens: u64,
    cost: Option<f64>,
    priced_cost: f64,
}

pub fn overview(
    conn: &Connection,
    cfg: &Config,
    prices: &PriceTable,
    f: &Filter,
    now: i64,
) -> anyhow::Result<Vec<AccountRow>> {
    let wanted = |vendor: &str| f.vendor.as_deref().is_none_or(|v| v == vendor);
    let mut m: BTreeMap<(String, String), Build> = BTreeMap::new();

    for v in ALL_VENDORS.into_iter().filter(|v| wanted(v.as_str())) {
        for (key, info) in accounts::all(conn, v)? {
            let b = m.entry((v.as_str().to_string(), key)).or_default();
            b.cost = Some(0.0);
            b.info = Some(info);
        }
    }
    for u in cost::by_account(conn, prices, f)? {
        let b = m.entry((u.vendor, u.account)).or_default();
        b.tokens = u.tokens;
        b.cost = u.cost;
        b.priced_cost = u.priced_cost;
    }
    for c in limits::current(conn, cfg, now)? {
        if c.no_data || c.account.is_empty() || !wanted(&c.vendor) {
            continue;
        }
        let b = m
            .entry((c.vendor.clone(), c.account.clone()))
            .or_insert_with(|| Build {
                cost: Some(0.0),
                ..Default::default()
            });
        b.current |= c.current;
        if b.plan_type.is_none() {
            b.plan_type = c.plan_type.clone();
        }
        b.windows.push(AccountWindow {
            scope: c.scope,
            used_percent: c.used_percent,
            resets_at: c.resets_at,
            eta: c.eta,
            expired: c.expired,
            stale: c.stale,
            measured_at: c.measured_at,
        });
    }

    let stored = limits::stored_labels(conn)?;
    let mut rows: Vec<AccountRow> = m
        .into_iter()
        .map(|((vendor, account), mut b)| {
            b.windows.sort_by(|a, c| a.scope.cmp(&c.scope));
            let key = format!("{vendor}:{account}");
            let named = stored.contains_key(&key) || cfg.account_labels.contains_key(&key);
            let mut label = limits::account_label(cfg, &stored, &vendor, &account).0;
            if !named && vendor == "claude" && (account.is_empty() || account == "default") {
                label = UNKNOWN_ACCOUNT.to_string();
            }
            let info = b.info;
            AccountRow {
                vendor,
                account,
                label,
                email: info.as_ref().map(|i| i.email.clone()),
                org: info.as_ref().and_then(|i| i.org.clone()),
                plan: info.as_ref().and_then(|i| i.plan.clone()).or(b.plan_type),
                current: b.current,
                windows: b.windows,
                tokens: b.tokens,
                cost: b.cost,
                priced_cost: b.priced_cost,
            }
        })
        .collect();
    // signed-in first, then the most quota left; accounts without a reading go last
    let worst = |r: &AccountRow| {
        r.windows
            .iter()
            .filter(|w| !w.expired)
            .map(|w| w.used_percent)
            .reduce(f64::max)
    };
    rows.sort_by(|a, b| {
        b.current
            .cmp(&a.current)
            .then_with(|| match (worst(a), worst(b)) {
                (Some(x), Some(y)) => x.total_cmp(&y),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => std::cmp::Ordering::Equal,
            })
            .then_with(|| (&a.vendor, &a.account).cmp(&(&b.vendor, &b.account)))
    });
    Ok(rows)
}
