pub mod claude;

use crate::config::Config;
use crate::model::{Batch, Vendor};
use crate::paths::Paths;
use crate::scan::FileStamp;
use rusqlite::Connection;

#[derive(Debug, Clone)]
pub struct Discovery {
    pub found: bool,
    pub detail: String,
}

pub struct CollectCtx<'a> {
    pub conn: &'a Connection,
    pub paths: &'a Paths,
    pub config: &'a Config,
    /// Files fully processed by the adapter; the collector calls `scan::mark_done` after writing.
    pub done_files: Vec<FileStamp>,
    /// Files the adapter found but could not read; surfaced in the adapter's status detail.
    pub unreadable: usize,
}

pub trait Adapter {
    fn vendor(&self) -> Vendor;
    fn discover(&self, paths: &Paths) -> Discovery;
    fn collect(&self, ctx: &mut CollectCtx) -> anyhow::Result<Batch>;
}
