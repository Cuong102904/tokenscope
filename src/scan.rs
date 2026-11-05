use crate::safety::open_guarded;
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::Value;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;
use walkdir::WalkDir;

/// (size, mtime in nanoseconds since epoch) of an already-opened file.
fn stat(f: &std::fs::File) -> Result<(i64, i64)> {
    let m = f.metadata()?;
    let mtime = m
        .modified()?
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0);
    Ok((m.len() as i64, mtime))
}

/// Files under `root` with extension `ext` whose (size, mtime) differ from the
/// `files` table. Never-read files and unreadable files are skipped. Does NOT
/// record anything; call `mark_done` after the data was written successfully.
pub fn changed_files(conn: &Connection, root: &Path, ext: &str) -> Result<Vec<PathBuf>> {
    if !root.exists() {
        return Ok(vec![]);
    }
    let mut out = vec![];
    for entry in WalkDir::new(root).sort_by_file_name().into_iter().filter_map(|e| e.ok()) {
        let p = entry.path();
        if !entry.file_type().is_file() || p.extension().is_none_or(|e| e != ext) {
            continue;
        }
        let Ok(f) = open_guarded(p) else { continue };
        let Ok((size, mtime)) = stat(&f) else { continue };
        let known: Option<(i64, i64)> = conn
            .query_row(
                "SELECT size, mtime FROM files WHERE path=?1",
                params![p.to_string_lossy()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        if known != Some((size, mtime)) {
            out.push(p.to_path_buf());
        }
    }
    Ok(out)
}

/// Record the current (size, mtime) of each path. Files that vanished or are
/// forbidden are ignored.
pub fn mark_done(conn: &Connection, paths: &[PathBuf]) -> Result<()> {
    let mut st = conn.prepare(
        "INSERT INTO files(path,size,mtime) VALUES(?1,?2,?3)
         ON CONFLICT(path) DO UPDATE SET size=excluded.size, mtime=excluded.mtime",
    )?;
    for p in paths {
        let Ok(f) = open_guarded(p) else { continue };
        let Ok((size, mtime)) = stat(&f) else { continue };
        st.execute(params![p.to_string_lossy(), size, mtime])?;
    }
    Ok(())
}

/// Parse a JSONL file. Blank lines are ignored. A malformed line is skipped
/// and counted; a malformed LAST line of a file not ending in '\n' is a
/// partially-written line and is skipped without being counted (it will be
/// read on a later scan).
pub fn read_jsonl_counted(p: &Path) -> Result<(Vec<Value>, usize)> {
    let mut bytes = Vec::new();
    open_guarded(p)?.read_to_end(&mut bytes)?;
    let text = String::from_utf8_lossy(&bytes);
    let ends_nl = text.ends_with('\n');
    let lines: Vec<&str> = text.lines().collect();
    let (mut vals, mut skipped) = (vec![], 0);
    for (i, line) in lines.iter().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<Value>(line) {
            Ok(v) => vals.push(v),
            Err(_) if i + 1 == lines.len() && !ends_nl => {}
            Err(_) => skipped += 1,
        }
    }
    Ok((vals, skipped))
}

pub fn read_jsonl(p: &Path) -> Result<Vec<Value>> {
    Ok(read_jsonl_counted(p)?.0)
}
