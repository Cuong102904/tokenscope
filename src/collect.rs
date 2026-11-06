use crate::adapters::{
    Adapter, CollectCtx, agy::AgyAdapter, claude::ClaudeAdapter,
    claude_snapshots::ClaudeSnapshotsAdapter, codex::CodexAdapter, opencode::OpenCodeAdapter,
    pi::PiAdapter,
};
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
    vec![
        Box::new(ClaudeAdapter),
        Box::new(ClaudeSnapshotsAdapter),
        Box::new(CodexAdapter),
        Box::new(OpenCodeAdapter),
        Box::new(AgyAdapter),
        Box::new(PiAdapter),
    ]
}

fn panic_message(p: Box<dyn std::any::Any + Send>) -> String {
    p.downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| p.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "adapter panicked".to_string())
}

fn run_one(
    conn: &mut Connection,
    paths: &Paths,
    cfg: &Config,
    ad: &dyn Adapter,
    st: &mut AdapterStatus,
) -> anyhow::Result<()> {
    let disc = ad.discover(paths);
    st.found = disc.found;
    st.detail = disc.detail;
    if !disc.found {
        return Ok(());
    }
    let (batch, done, unreadable, remove) = {
        let mut ctx = CollectCtx {
            conn,
            paths,
            config: cfg,
            done_files: vec![],
            unreadable: 0,
            remove_after_write: vec![],
        };
        let batch = ad.collect(&mut ctx)?;
        (
            batch,
            std::mem::take(&mut ctx.done_files),
            ctx.unreadable,
            std::mem::take(&mut ctx.remove_after_write),
        )
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
/// persist each status under `kv` key `status:<vendor>`, thin out old limit
/// snapshots (once a day), and refresh `today.txt`.
pub fn run_all(
    conn: &mut Connection,
    paths: &Paths,
    cfg: &Config,
    adapters: &[Box<dyn Adapter>],
) -> Vec<AdapterStatus> {
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
        match catch_unwind(AssertUnwindSafe(|| {
            run_one(conn, paths, cfg, ad.as_ref(), &mut st)
        })) {
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
    // limit-snapshot retention (no-op unless a day has passed since the last run)
    if let Err(e) = crate::db::downsample_daily(conn, chrono::Utc::now().timestamp()) {
        eprintln!("warning: downsampling limit snapshots: {e:#}");
    }
    if let Err(e) = write_today(conn, paths) {
        eprintln!("warning: could not write today.txt: {e:#}");
    }
    out
}

/// `~/.tokenscope/today.txt` holds today's cost (`<local date> <Totals::display>`, e.g.
/// `2026-11-05 ≥ $1.00`; the date lets a reader ignore a stale day) for the statusline.
fn write_today(conn: &Connection, paths: &Paths) -> anyhow::Result<()> {
    let prices = PriceTable::load(paths)?;
    let t = cost::totals(conn, &prices, &Filter::days(1))?;
    let dir = paths.data_dir();
    std::fs::create_dir_all(&dir)?;
    let tmp = dir.join(format!("today.txt.{}.tmp", std::process::id()));
    let date = chrono::Local::now().format("%Y-%m-%d");
    std::fs::write(&tmp, format!("{date} {}", t.display()))?;
    std::fs::rename(&tmp, dir.join("today.txt"))?;
    Ok(())
}

/// Last stored status per vendor (for `doctor` / `serve`).
pub fn load_status(conn: &Connection, vendor: Vendor) -> Option<AdapterStatus> {
    let v: String = conn
        .query_row(
            "SELECT value FROM kv WHERE key=?1",
            params![format!("status:{}", vendor.as_str())],
            |r| r.get(0),
        )
        .ok()?;
    serde_json::from_str(&v).ok()
}

/// Result of one [`Collector::tick`].
#[derive(Debug)]
pub enum Tick {
    /// `run_all` completed (individual adapters may still have failed).
    Collected(Vec<AdapterStatus>),
    /// The DB could not be opened; retried on the next tick.
    OpenFailed(String),
    /// `run_all` panicked; the connection is dropped and reopened next tick.
    Panicked(String),
}

impl Tick {
    pub fn ok(&self) -> bool {
        matches!(self, Tick::Collected(_))
    }
}

/// Background collection shared by `serve`, `tui` and `collect --watch`. [`Collector::tick`]
/// never panics and never gives up: a failed DB open is retried on the next tick, and a panic
/// inside `run_all` drops the connection so the next tick starts fresh. Persistent problems are
/// logged once per streak into [`Collector::take_log`] (the caller decides where lines go).
pub struct Collector {
    paths: Paths,
    cfg: Config,
    adapters: Vec<Box<dyn Adapter>>,
    conn: Option<Connection>,
    open_failing: bool,
    panicking: bool,
    /// Per adapter index: did its last run end with an error?
    failing: Vec<bool>,
    log: Vec<String>,
}

impl Collector {
    pub fn new(paths: Paths, cfg: Config) -> Collector {
        Collector::with_adapters(paths, cfg, default_adapters())
    }

    pub fn with_adapters(paths: Paths, cfg: Config, adapters: Vec<Box<dyn Adapter>>) -> Collector {
        Collector {
            failing: vec![false; adapters.len()],
            paths,
            cfg,
            adapters,
            conn: None,
            open_failing: false,
            panicking: false,
            log: vec![],
        }
    }

    /// The open connection (e.g. for `agy::refresh` after a tick), if any.
    pub fn conn_mut(&mut self) -> Option<&mut Connection> {
        self.conn.as_mut()
    }

    /// Lines logged since the last call: the start of a failure streak and the recovery.
    pub fn take_log(&mut self) -> Vec<String> {
        std::mem::take(&mut self.log)
    }

    /// One collection pass.
    pub fn tick(&mut self) -> Tick {
        if self.conn.is_none() {
            match db::open(&self.paths.db()) {
                Ok(c) => {
                    if self.open_failing {
                        self.log.push("database open again".into());
                    }
                    self.conn = Some(c);
                    self.open_failing = false;
                }
                Err(e) => {
                    let msg = format!("{e:#}");
                    if !self.open_failing {
                        self.log
                            .push(format!("cannot open database (will retry): {msg}"));
                    }
                    self.open_failing = true;
                    return Tick::OpenFailed(msg);
                }
            }
        }
        let Some(conn) = self.conn.as_mut() else {
            return Tick::OpenFailed("no connection".into());
        };
        let (paths, cfg, adapters) = (&self.paths, &self.cfg, &self.adapters);
        match catch_unwind(AssertUnwindSafe(|| run_all(conn, paths, cfg, adapters))) {
            Ok(statuses) => {
                if self.panicking {
                    self.log.push("collection works again".into());
                }
                self.panicking = false;
                for (i, s) in statuses.iter().enumerate() {
                    let was = std::mem::replace(&mut self.failing[i], s.error.is_some());
                    match (&s.error, was) {
                        (Some(e), false) => self.log.push(format!("{}: {e}", s.vendor.as_str())),
                        (None, true) => self.log.push(format!("{}: ok again", s.vendor.as_str())),
                        _ => {}
                    }
                }
                Tick::Collected(statuses)
            }
            Err(p) => {
                let msg = panic_message(p);
                if !self.panicking {
                    self.log.push(format!(
                        "panic during collection ({msg}); reconnecting next time"
                    ));
                }
                self.panicking = true;
                self.conn = None;
                Tick::Panicked(msg)
            }
        }
    }

    /// `tick` and print the new log lines to stderr with `prefix`.
    pub fn tick_logged(&mut self, prefix: &str) -> Tick {
        let t = self.tick();
        for line in self.take_log() {
            eprintln!("{prefix}: {line}");
        }
        t
    }
}
