//! The signed-in Claude account, read from `~/.claude.json` (`oauthAccount`).
//!
//! Only metadata is read (uuid, email, organisation, rate-limit tier), never a token. The key is
//! the same one the transcripts' `session-owner` line yields (`accounts::claude_key`), so usage
//! rows and limit readings of one account sit under one key.

use crate::accounts::{self, AccountInfo, claude_key};
use crate::config::Config;
use crate::model::{Vendor, sha8};
use crate::paths::Paths;
use rusqlite::Connection;

use super::claude_usage::{DEFAULT_ACCOUNT, adopt_default_rows};

const KV_IDENTITY_KEY: &str = "claude:identity_key";
/// Registry entries are only rewritten when something changed or this many seconds passed.
const TOUCH_EVERY: i64 = 600;

#[derive(Debug, Clone, PartialEq)]
pub struct SignedIn {
    pub key: String,
    pub info: AccountInfo,
}

/// `None` when the file is missing, not JSON, has no `oauthAccount`, or lacks a uuid or email.
pub fn signed_in(paths: &Paths, now: i64) -> Option<SignedIn> {
    let text = crate::safety::read_to_string_guarded(&paths.claude_json()).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    let o = v.get("oauthAccount")?;
    let get = |k: &str| {
        o.get(k)
            .and_then(|x| x.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    let uuid = get("accountUuid")?;
    let email = get("emailAddress")?;
    let org_uuid = get("organizationUuid");
    Some(SignedIn {
        key: claude_key(&uuid, org_uuid.as_deref()),
        info: AccountInfo {
            email,
            org: get("organizationName"),
            // `subscriptionType` is not in this file; the rate-limit tier is the closest plan name
            plan: get("organizationRateLimitTier"),
            last_seen: now,
        },
    })
}

/// Move the readings and the name recorded under the old `sha8(email)` key to `new_key`.
/// Idempotent: nothing under the old key means nothing to do.
pub fn migrate_email_key(conn: &Connection, email: &str, new_key: &str) -> anyhow::Result<()> {
    let old = sha8(email);
    if old == new_key {
        return Ok(());
    }
    for table in ["limit_snapshots", "limit_events"] {
        conn.execute(
            &format!(
                "UPDATE OR IGNORE {table} SET account = ?1 WHERE vendor = 'claude' AND account = ?2"
            ),
            [new_key, old.as_str()],
        )?;
    }
    let (old_label, new_label) = (
        format!("label:claude:{old}"),
        format!("label:claude:{new_key}"),
    );
    conn.execute(
        "UPDATE OR IGNORE kv SET key = ?1 WHERE key = ?2",
        [&new_label, &old_label],
    )?;
    conn.execute("DELETE FROM kv WHERE key = ?1", [&old_label])?;
    Ok(())
}

/// Register the signed-in account (always) and, unless the user chose `claude_account` in
/// `config.toml`, make its key the one new readings use. Returns that key.
pub fn sync_signed_in(
    conn: &Connection,
    paths: &Paths,
    cfg: &Config,
    now: i64,
) -> anyhow::Result<Option<String>> {
    let Some(s) = signed_in(paths, now) else {
        return Ok(None);
    };
    let known = accounts::get(conn, Vendor::Claude, &s.key)?;
    let fresh = known.as_ref().is_none_or(|k| {
        (k.email.as_str(), &k.org, &k.plan) != (s.info.email.as_str(), &s.info.org, &s.info.plan)
            || now - k.last_seen >= TOUCH_EVERY
    });
    if fresh {
        accounts::upsert(conn, Vendor::Claude, &s.key, &s.info)?;
    }
    if cfg.claude_account != DEFAULT_ACCOUNT {
        return Ok(None);
    }
    migrate_email_key(conn, &s.info.email, &s.key)?;
    adopt_default_rows(conn, &s.key)?;
    conn.execute(
        "INSERT OR REPLACE INTO kv(key,value) VALUES(?1,?2)",
        [KV_IDENTITY_KEY, s.key.as_str()],
    )?;
    Ok(Some(s.key))
}
