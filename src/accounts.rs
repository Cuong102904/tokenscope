//! Account registry: which real account (email, org, plan) sits behind an internal account key.
//!
//! The key is `sha8` of the identity and is never shown; names shown to the user come from here.
//! Everything lives in `kv` (`account:<vendor>:<key>` -> JSON), so there is no schema change.
//! Real emails stay in the local database and are never sent anywhere.

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::model::{Vendor, sha8};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AccountInfo {
    pub email: String,
    pub org: Option<String>,
    pub plan: Option<String>,
    pub last_seen: i64,
}

fn kv_key(vendor: Vendor, key: &str) -> String {
    format!("account:{}:{key}", vendor.as_str())
}

/// Internal key of a Claude account: the org is part of it, so one email in two orgs stays two
/// accounts.
pub fn claude_key(account_uuid: &str, org_uuid: Option<&str>) -> String {
    sha8(&format!("{account_uuid}:{}", org_uuid.unwrap_or("")))
}

/// Store or refresh an account; the newest `last_seen` wins, everything else takes the new value.
pub fn upsert(conn: &Connection, vendor: Vendor, key: &str, info: &AccountInfo) -> Result<()> {
    let mut merged = info.clone();
    if let Some(old) = get(conn, vendor, key)? {
        merged.last_seen = merged.last_seen.max(old.last_seen);
    }
    conn.execute(
        "INSERT OR REPLACE INTO kv(key,value) VALUES(?1,?2)",
        params![kv_key(vendor, key), serde_json::to_string(&merged)?],
    )?;
    Ok(())
}

pub fn get(conn: &Connection, vendor: Vendor, key: &str) -> Result<Option<AccountInfo>> {
    let raw: Option<String> = conn
        .query_row(
            "SELECT value FROM kv WHERE key=?1",
            [kv_key(vendor, key)],
            |r| r.get(0),
        )
        .optional()?;
    Ok(raw.and_then(|s| serde_json::from_str(&s).ok()))
}

/// Every registered account of a vendor as `(key, info)`.
pub fn all(conn: &Connection, vendor: Vendor) -> Result<Vec<(String, AccountInfo)>> {
    let prefix = kv_key(vendor, "");
    let mut stmt = conn.prepare("SELECT key, value FROM kv WHERE substr(key,1,?1)=?2")?;
    let rows = stmt.query_map(params![prefix.len() as i64, prefix], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (k, v) = row?;
        if let Ok(info) = serde_json::from_str(&v) {
            out.push((k[prefix.len()..].to_string(), info));
        }
    }
    Ok(out)
}
