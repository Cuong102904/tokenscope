use agent_usage::adapters::{Adapter, CollectCtx, codex::{CodexAdapter, parse_rollout}};
use agent_usage::{config::Config, db, model::{Batch, sha8}, paths::Paths, scan};
use serde_json::json;
use std::path::Path;

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let t = to.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_dir(&e.path(), &t);
        } else {
            std::fs::copy(e.path(), t).unwrap();
        }
    }
}

#[test]
fn codex_fixture() {
    let d = tempfile::tempdir().unwrap();
    copy_dir(&Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/codex-home"), d.path());
    let paths = Paths { home: d.path().to_path_buf() };
    let cfg = Config::default();
    let mut conn = db::open(&d.path().join("t.db")).unwrap();
    let ad = CodexAdapter;
    assert!(ad.discover(&paths).found);
    let mut ctx = CollectCtx { conn: &conn, paths: &paths, config: &cfg, done_files: vec![], unreadable: 0, remove_after_write: vec![] };
    let batch = ad.collect(&mut ctx).unwrap();
    assert_eq!(batch.usage.len(), 2, "repeated token_count must be dropped by the adapter, not only by the DB key");
    let done = std::mem::take(&mut ctx.done_files);
    db::write_batch(&mut conn, &batch).unwrap();
    scan::mark_done(&conn, &done).unwrap();

    let n = |sql: &str| -> i64 { conn.query_row(sql, [], |r| r.get(0)).unwrap() };
    assert_eq!(n("SELECT count(*) FROM usage"), 2);
    let (i, c, o, r, m): (i64, i64, i64, i64, String) = conn
        .query_row("SELECT input,cache_read,output,reasoning,model FROM usage WHERE msg_key='codex:sess-codex-1:19771'", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))
        .unwrap();
    assert_eq!((i, c, o, r, m.as_str()), (9622, 9984, 165, 64, "gpt-5.6-terra"));
    assert_eq!(n("SELECT cache_write_5m FROM usage WHERE msg_key='codex:sess-codex-1:59761'"), 5);
    let acct: String = conn.query_row("SELECT DISTINCT account FROM usage", [], |r| r.get(0)).unwrap();
    assert_eq!(acct, sha8("acct-test"));

    // raw account id must be in no table
    let tables: Vec<String> = conn
        .prepare("SELECT name FROM sqlite_master WHERE type='table'").unwrap()
        .query_map([], |r| r.get(0)).unwrap().map(|r| r.unwrap()).collect();
    for t in tables {
        let cols: Vec<String> = conn.prepare(&format!("PRAGMA table_info({t})")).unwrap()
            .query_map([], |r| r.get(1)).unwrap().map(|r| r.unwrap()).collect();
        for col in cols {
            let hits: i64 = conn.query_row(&format!("SELECT count(*) FROM {t} WHERE CAST({col} AS TEXT) LIKE '%acct-test%'"), [], |r| r.get(0)).unwrap();
            assert_eq!(hits, 0, "{t}.{col}");
        }
    }

    let lim = |scope: &str| -> f64 {
        conn.query_row("SELECT used_percent FROM limit_snapshots WHERE scope=?1 ORDER BY measured_at DESC LIMIT 1", [scope], |r| r.get(0)).unwrap()
    };
    assert_eq!(lim("five_hour"), 40.0); // latest, from the info:null event
    assert_eq!(lim("weekly"), 12.0);
    assert_eq!(n("SELECT count(*) FROM limit_snapshots WHERE scope='five_hour' AND used_percent=37.0"), 1);
    let (ev_scope, ev_reset): (String, i64) = conn.query_row("SELECT scope, resets_at FROM limit_events", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
    assert_eq!((ev_scope.as_str(), ev_reset), ("primary", 1793000000));
    let (sk, kind): (String, String) = conn.query_row("SELECT skill, kind FROM skill_events", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
    assert_eq!((sk.as_str(), kind.as_str()), ("foo", "file_read"));
    assert_eq!(n("SELECT count(*) FROM tool_events WHERE tool='exec_command'"), 2);
    let (proj, sid): (String, String) = conn.query_row("SELECT project, session_id FROM sessions", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
    assert_eq!((proj.as_str(), sid.as_str()), ("/home/u/proj", "sess-codex-1"));

    // second scan: nothing changed
    let mut ctx = CollectCtx { conn: &conn, paths: &paths, config: &cfg, done_files: vec![], unreadable: 0, remove_after_write: vec![] };
    assert!(ad.collect(&mut ctx).unwrap().usage.is_empty());
}

#[test]
fn free_plan_monthly_window() {
    let lines = vec![
        json!({"type":"session_meta","timestamp":"2026-11-02T01:00:00Z","payload":{"id":"s","cwd":"/x"}}),
        json!({"type":"event_msg","timestamp":"2026-11-02T01:00:10Z","payload":{"type":"token_count","info":null,
            "rate_limits":{"limit_id":"codex","primary":{"used_percent":5.0,"window_minutes":43200,"resets_at":1},"secondary":null,"plan_type":"free","rate_limit_reached_type":null}}}),
        json!({"type":"event_msg","timestamp":"2026-11-02T01:00:11Z","payload":{"type":"token_count","info":null,
            "rate_limits":{"limit_id":"other","primary":{"used_percent":7.0,"window_minutes":123,"resets_at":1},"secondary":null,"plan_type":"free","rate_limit_reached_type":null}}}),
    ];
    let mut b = Batch::default();
    parse_rollout(&lines, &mut b);
    let scopes: Vec<_> = b.limits.iter().map(|l| l.scope.as_str()).collect();
    assert_eq!(scopes, ["monthly", "other:window_123m"]);
    assert_eq!(b.limits[0].account, "");
}

fn call(t: &str, name: &str, field: &str, text: &str, id: &str) -> serde_json::Value {
    json!({"type":"response_item","timestamp":"2026-11-02T01:00:20Z","payload":{"type":t,"name":name,field:text,"call_id":id}})
}

#[test]
fn custom_tool_calls_and_skill_reads() {
    let lines = vec![
        json!({"type":"session_meta","timestamp":"2026-11-02T01:00:00Z","payload":{"id":"s","cwd":"/x",
            "base_instructions":{"text":"see /home/u/.codex/skills/ignored/SKILL.md"}}}),
        call("custom_tool_call", "exec", "input", "cat /home/u/.codex/plugins/cache/k/k/1.0/skills/verify-implementation/SKILL.md", "c1"),
        call("custom_tool_call", "apply_patch", "input", "*** Begin Patch\n*** End Patch", "c2"),
        call("function_call", "exec_command", "arguments", "{\"cmd\":\"cat /a/skills/one/SKILL.md /b/skills/two/SKILL.md\"}", "c3"),
    ];
    let mut b = Batch::default();
    parse_rollout(&lines, &mut b);
    let tools: Vec<_> = b.tools.iter().map(|t| (t.tool.as_str(), t.key.as_str())).collect();
    assert_eq!(tools, [("exec", "codex:c1"), ("apply_patch", "codex:c2"), ("exec_command", "codex:c3")]);
    let skills: Vec<_> = b.skills.iter().map(|s| (s.skill.as_str(), s.key.as_str(), s.kind)).collect();
    assert_eq!(skills, [
        ("verify-implementation", "codex:c1:verify-implementation", "file_read"),
        ("one", "codex:c3:one", "file_read"),
        ("two", "codex:c3:two", "file_read"),
    ]);
}

#[test]
fn missing_session_meta_keeps_limits() {
    let lines = vec![json!({"type":"event_msg","timestamp":"2026-11-02T01:00:10Z","payload":{"type":"token_count",
        "info":{"total_token_usage":{"total_tokens":5},"last_token_usage":{"input_tokens":5}},
        "rate_limits":{"limit_id":"codex","primary":{"used_percent":5.0,"window_minutes":300,"resets_at":1},"secondary":null}}})];
    let mut b = Batch::default();
    let (sid, _) = parse_rollout(&lines, &mut b);
    assert_eq!(sid, "");
    assert!(b.usage.is_empty());
    assert_eq!(b.limits.len(), 1);
}
