use super::{Adapter, CollectCtx, Discovery};
use crate::model::*;
use crate::paths::Paths;
use crate::scan;
use serde_json::Value;

pub struct CodexAdapter;

fn u(v: &Value) -> u64 {
    v.as_u64().unwrap_or(0)
}

fn s(v: &Value) -> Option<String> {
    v.as_str().filter(|x| !x.is_empty()).map(String::from)
}

fn ts_of(v: &Value) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(v["timestamp"].as_str()?).ok().map(|d| d.timestamp())
}

pub fn scope_for(window_minutes: i64) -> String {
    match window_minutes {
        300 => "five_hour".into(),
        10080 => "weekly".into(),
        43200 => "monthly".into(),
        n => format!("window_{n}m"),
    }
}

/// Parent-directory names of every `/SKILL.md` mentioned in `text`.
fn skills_in(text: &str) -> Vec<String> {
    let mut out = vec![];
    for (i, _) in text.match_indices("/SKILL.md") {
        let head = &text[..i];
        let start = head.rfind(['/', ' ', '"', '\'', '=', ':', '\\']).map_or(0, |p| p + 1);
        let name = &head[start..];
        if !name.is_empty() {
            out.push(name.to_string());
        }
    }
    out
}

/// (session_id, project, account) from a session_meta payload; the account id is stored only as sha8.
fn meta(p: &Value) -> (String, String, String) {
    (
        s(&p["id"]).unwrap_or_default(),
        s(&p["cwd"]).unwrap_or_default(),
        s(&p["creator_account_id"]).map(|a| sha8(&a)).unwrap_or_default(),
    )
}

/// Parse one rollout file. Returns `(session_id, project)`; both are empty when the file has no
/// session_meta. Without a session id token usage is skipped (its key needs the id), while rate
/// limits, tools and skills are still kept (with session "" and account "").
pub fn parse_rollout(lines: &[Value], out: &mut Batch) -> (String, String) {
    let (mut session_id, mut project, mut account) = (String::new(), String::new(), String::new());
    let mut model = String::new();
    let mut last_total: Option<u64> = None;
    for v in lines {
        let p = &v["payload"];
        let Some(ts) = ts_of(v) else {
            if v["type"] == "session_meta" {
                (session_id, project, account) = meta(p);
            }
            continue;
        };
        match v["type"].as_str() {
            Some("session_meta") => (session_id, project, account) = meta(p),
            Some("turn_context") => {
                if let Some(m) = s(&p["model"]) {
                    model = m;
                }
            }
            Some("event_msg") if p["type"] == "token_count" => {
                let info = &p["info"];
                if info.is_object() && !session_id.is_empty() {
                    let total = u(&info["total_token_usage"]["total_tokens"]);
                    if last_total != Some(total) {
                        last_total = Some(total);
                        let l = &info["last_token_usage"];
                        let cached = u(&l["cached_input_tokens"]);
                        out.usage.push(UsageRecord {
                            msg_key: format!("codex:{session_id}:{total}"),
                            vendor: Vendor::Codex,
                            account: account.clone(),
                            session_id: session_id.clone(),
                            project: project.clone(),
                            ts,
                            model: model.clone(),
                            input: u(&l["input_tokens"]).saturating_sub(cached),
                            output: u(&l["output_tokens"]),
                            reasoning: u(&l["reasoning_output_tokens"]),
                            cache_read: cached,
                            cache_write_5m: u(&l["cache_write_input_tokens"]),
                            cache_write_1h: 0,
                            sidechain: false,
                            skill: None,
                            plugin: None,
                        });
                    }
                }
                let rl = &p["rate_limits"];
                if rl.is_object() {
                    let prefix = match s(&rl["limit_id"]) {
                        Some(id) if id != "codex" => format!("{id}:"),
                        _ => String::new(),
                    };
                    for w in ["primary", "secondary"] {
                        let (Some(wm), Some(used)) = (rl[w]["window_minutes"].as_i64(), rl[w]["used_percent"].as_f64()) else { continue };
                        out.limits.push(LimitSnapshot {
                            vendor: Vendor::Codex,
                            account: account.clone(),
                            scope: format!("{prefix}{}", scope_for(wm)),
                            window_minutes: Some(wm),
                            used_percent: used,
                            resets_at: rl[w]["resets_at"].as_i64(),
                            plan_type: s(&rl["plan_type"]),
                            measured_at: ts,
                            source: "codex-log".into(),
                        });
                    }
                    if let Some(kind) = s(&rl["rate_limit_reached_type"]) {
                        out.limit_events.push(LimitEvent {
                            vendor: Vendor::Codex,
                            account: account.clone(),
                            ts,
                            scope: format!("{prefix}{kind}"),
                            resets_at: rl["primary"]["resets_at"].as_i64(),
                        });
                    }
                }
            }
            Some("response_item") if matches!(p["type"].as_str(), Some("function_call" | "custom_tool_call")) => {
                let (Some(name), Some(call_id)) = (s(&p["name"]), s(&p["call_id"])) else { continue };
                out.tools.push(ToolEvent {
                    key: format!("codex:{call_id}"),
                    vendor: Vendor::Codex,
                    session_id: session_id.clone(),
                    ts,
                    tool: name,
                });
                // function_call carries its text in `arguments`, custom_tool_call in `input`
                let text = match (&p["arguments"], &p["input"]) {
                    (Value::String(a), _) => a.clone(),
                    (_, Value::String(i)) => i.clone(),
                    _ => String::new(),
                };
                for skill in skills_in(&text) {
                    out.skills.push(SkillEvent {
                        key: format!("codex:{call_id}:{skill}"),
                        vendor: Vendor::Codex,
                        session_id: session_id.clone(),
                        ts,
                        skill,
                        kind: "file_read",
                    });
                }
            }
            _ => {}
        }
    }
    (session_id, project)
}

impl Adapter for CodexAdapter {
    fn vendor(&self) -> Vendor {
        Vendor::Codex
    }

    fn discover(&self, paths: &Paths) -> Discovery {
        let root = paths.codex_sessions();
        if !root.exists() {
            return Discovery { found: false, detail: "not found".into() };
        }
        let n = walkdir::WalkDir::new(&root)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file() && e.path().extension().is_some_and(|x| x == "jsonl"))
            .count();
        Discovery { found: true, detail: format!("{n} jsonl files") }
    }

    fn collect(&self, ctx: &mut CollectCtx) -> anyhow::Result<Batch> {
        let mut batch = Batch::default();
        for stamp in scan::changed_files(ctx.conn, &ctx.paths.codex_sessions(), "jsonl")? {
            let lines = match scan::read_jsonl(&stamp.path) {
                Ok(l) => l,
                Err(_) => {
                    ctx.unreadable += 1;
                    continue;
                }
            };
            let (session_id, project) = parse_rollout(&lines, &mut batch);
            if !session_id.is_empty() {
                batch.sessions.push(SessionInfo {
                    vendor: Vendor::Codex,
                    session_id,
                    project,
                    title: None,
                    source_path: stamp.path.to_string_lossy().into_owned(),
                    last_write: stamp.mtime / 1_000_000_000,
                });
            }
            ctx.done_files.push(stamp);
        }
        Ok(batch)
    }
}
