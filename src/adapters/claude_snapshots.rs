use super::{Adapter, CollectCtx, Discovery};
use crate::model::*;
use crate::paths::Paths;
use serde_json::Value;
use std::path::PathBuf;

/// Ingests the rate-limit snapshots `agent-usage statusline` drops into
/// `~/.agent-usage/snapshots/claude-*.json`, then deletes those files.
pub struct ClaudeSnapshotsAdapter;

fn files(paths: &Paths) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(paths.snapshots_dir()) else { return vec![] };
    let mut v: Vec<PathBuf> = rd
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("claude-") && n.ends_with(".json"))
        })
        .collect();
    v.sort();
    v
}

fn window(v: &Value, scope: &str, minutes: i64, account: &str, measured_at: i64, out: &mut Batch) {
    let Some(used) = v["used_percent"].as_f64() else { return };
    out.limits.push(LimitSnapshot {
        vendor: Vendor::Claude,
        account: account.to_string(),
        scope: scope.to_string(),
        window_minutes: Some(minutes),
        used_percent: used,
        resets_at: v["resets_at"].as_i64(),
        plan_type: None,
        measured_at,
        source: "statusline".to_string(),
    });
}

impl Adapter for ClaudeSnapshotsAdapter {
    fn vendor(&self) -> Vendor {
        Vendor::Claude
    }

    fn is_auxiliary(&self) -> bool {
        true
    }

    fn discover(&self, paths: &Paths) -> Discovery {
        let n = files(paths).len();
        Discovery { found: n > 0, detail: format!("{n} statusline snapshots") }
    }

    fn collect(&self, ctx: &mut CollectCtx) -> anyhow::Result<Batch> {
        let mut batch = Batch::default();
        for p in files(ctx.paths) {
            let parsed = std::fs::read_to_string(&p).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok());
            // our own files: a corrupt one is counted once, then deleted
            let Some(v) = parsed else {
                ctx.unreadable += 1;
                ctx.remove_after_write.push(p);
                continue;
            };
            let account = v["account"].as_str().unwrap_or(&ctx.config.claude_account);
            let Some(at) = v["measured_at"].as_i64() else {
                ctx.unreadable += 1;
                ctx.remove_after_write.push(p);
                continue;
            };
            window(&v["five_hour"], "five_hour", 300, account, at, &mut batch);
            window(&v["seven_day"], "weekly", 10080, account, at, &mut batch);
            ctx.remove_after_write.push(p);
        }
        Ok(batch)
    }
}
