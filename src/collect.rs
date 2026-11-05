use crate::adapters::{Adapter, CollectCtx, claude::ClaudeAdapter, claude_snapshots::ClaudeSnapshotsAdapter};
use crate::config::Config;
use crate::db;
use crate::model::Vendor;
use crate::paths::Paths;
use crate::pricing::PriceTable;
use crate::query::{Filter, cost};
use crate::scan;
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use std::panic::{AssertUnwindSafe, catch_unwind};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdapterStatus {
    pub vendor: Vendor,
    pub found: bool,
    pub detail: String,
    pub records: usize,
    pub error: Option<String>,
    pub last_run: i64,
}

/// The single registration point for adapters; later tasks append here.
pub fn default_adapters() -> Vec<Box<dyn Adapter>> {
    vec![Box::new(ClaudeAdapter), Box::new(ClaudeSnapshotsAdapter)]
}

fn panic_message(p: Box<dyn std::any::Any + Send>) -> String {
    p.downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| p.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "adapter panicked".to_string())
}

fn run_one(conn: &mut Connection, paths: &Paths, cfg: &Config, ad: &dyn Adapter, st: &mut AdapterStatus) -> anyhow::Result<()> {
    let disc = ad.discover(paths);
    st.found = disc.found;
    st.detail = disc.detail;
    if !disc.found {
        return Ok(());
    }
    let (batch, done, unreadable, remove) = {
        let mut ctx = CollectCtx { conn, paths, config: cfg, done_files: vec![], unreadable: 0, remove_after_write: vec![] };
        let batch = ad.collect(&mut ctx)?;
        (batch, std::mem::take(&mut ctx.done_files), ctx.unreadable, std::mem::take(&mut ctx.remove_after_write))
    };
    if unreadable > 0 {
        st.detail = format!("{}, {unreadable} unreadable", st.detail);
    }
    db::write_batch(conn, &batch)?;
    st.records = batch.usage.len();
    // only after the data is safely written
    scan::mark_done(conn, &done)?;
    for p in remove {
        let _ = std::fs::remove_file(p);
    }
    Ok(())
}

/// Run every adapter (a failure or panic in one never stops the others),
/// persist each status under `kv` key `status:<vendor>`, and refresh `today.txt`.
pub fn run_all(conn: &mut Connection, paths: &Paths, cfg: &Config, adapters: &[Box<dyn Adapter>]) -> Vec<AdapterStatus> {
    let mut out = vec![];
    for ad in adapters {
        let mut st = AdapterStatus {
            vendor: ad.vendor(),
            found: false,
            detail: String::new(),
            records: 0,
            error: None,
            last_run: chrono::Utc::now().timestamp(),
        };
        match catch_unwind(AssertUnwindSafe(|| run_one(conn, paths, cfg, ad.as_ref(), &mut st))) {
            Ok(Ok(())) => {}
            Ok(Err(e)) => st.error = Some(format!("{e:#}")),
            Err(p) => st.error = Some(format!("panic: {}", panic_message(p))),
        }
        if ad.is_auxiliary() {
            out.push(st);
            continue;
        }
        if let Ok(json) = serde_json::to_string(&st) {
            let _ = conn.execute(
                "INSERT OR REPLACE INTO kv(key,value) VALUES(?1,?2)",
                params![format!("status:{}", st.vendor.as_str()), json],
            );
        }
        out.push(st);
    }
    if let Err(e) = write_today(conn, paths) {
        eprintln!("warning: could not write today.txt: {e:#}");
    }
    out
}

/// `~/.agent-usage/today.txt` holds today's cost (`<local date> $12.00`, so a reader can ignore a stale day) for the statusline.
fn write_today(conn: &Connection, paths: &Paths) -> anyhow::Result<()> {
    let prices = PriceTable::load(paths)?;
    let t = cost::totals(conn, &prices, &Filter::days(1))?;
    let dir = paths.data_dir();
    std::fs::create_dir_all(&dir)?;
    let tmp = dir.join(format!("today.txt.{}.tmp", std::process::id()));
    let date = chrono::Local::now().format("%Y-%m-%d");
    std::fs::write(&tmp, format!("{date} ${:.2}", t.cost))?;
    std::fs::rename(&tmp, dir.join("today.txt"))?;
    Ok(())
}

/// Last stored status per vendor (for `doctor` / `serve`).
pub fn load_status(conn: &Connection, vendor: Vendor) -> Option<AdapterStatus> {
    let v: String = conn
        .query_row("SELECT value FROM kv WHERE key=?1", params![format!("status:{}", vendor.as_str())], |r| r.get(0))
        .ok()?;
    serde_json::from_str(&v).ok()
}
