use serde_json::json;
use std::path::Path;
use tokenscope::adapters::{
    Adapter, CollectCtx,
    claude::{ClaudeAdapter, parse_line},
};
use tokenscope::{config::Config, db, model::Batch, paths::Paths, scan};

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
fn claude_fixture() {
    let d = tempfile::tempdir().unwrap();
    copy_dir(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/claude-home"),
        d.path(),
    );
    let paths = Paths {
        home: d.path().to_path_buf(),
    };
    let cfg = Config::default();
    let mut conn = db::open(&d.path().join("t.db")).unwrap();
    let ad = ClaudeAdapter;
    let disc = ad.discover(&paths);
    assert!(disc.found);
    assert_eq!(disc.detail, "2 jsonl files");

    let mut ctx = CollectCtx {
        conn: &conn,
        paths: &paths,
        config: &cfg,
        done_files: vec![],
        unreadable: 0,
        remove_after_write: vec![],
    };
    let batch = ad.collect(&mut ctx).unwrap();
    let done = std::mem::take(&mut ctx.done_files);
    assert_eq!(done.len(), 2);
    db::write_batch(&mut conn, &batch).unwrap();
    scan::mark_done(&conn, &done).unwrap();

    let n = |sql: &str| -> i64 { conn.query_row(sql, [], |r| r.get(0)).unwrap() };
    assert_eq!(n("SELECT count(*) FROM usage"), 3);
    assert_eq!(
        n("SELECT output FROM usage WHERE msg_key='claude:msg_1|req_1'"),
        50
    );
    assert_eq!(n("SELECT count(*) FROM usage WHERE sidechain=1"), 1);
    assert_eq!(n("SELECT count(*) FROM usage WHERE model LIKE '<%'"), 0);
    let (scope, resets): (String, i64) = conn
        .query_row("SELECT scope, resets_at FROM limit_events", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    assert_eq!((scope.as_str(), resets), ("five_hour", 1792996200));
    assert_eq!(n("SELECT count(*) FROM limit_events"), 1);
    assert_eq!(
        n("SELECT count(*) FROM skill_events WHERE kind='invoked'"),
        1
    );
    assert_eq!(
        n("SELECT count(*) FROM usage WHERE skill='superpowers:brainstorming'"),
        1
    );
    assert_eq!(n("SELECT count(*) FROM tool_events"), 2);
    assert_eq!(n("SELECT count(*) FROM sessions WHERE session_id='s1'"), 1);
    let src: String = conn
        .query_row(
            "SELECT source_path FROM sessions WHERE session_id='s1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(src.ends_with("s1.jsonl"), "{src}");
    let (w5, w1): (i64, i64) = conn
        .query_row(
            "SELECT cache_write_5m, cache_write_1h FROM usage WHERE msg_key='claude:msg_1|req_1'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((w5, w1), (20, 10));

    // second scan: nothing changed
    let mut ctx = CollectCtx {
        conn: &conn,
        paths: &paths,
        config: &cfg,
        done_files: vec![],
        unreadable: 0,
        remove_after_write: vec![],
    };
    let b2 = ad.collect(&mut ctx).unwrap();
    assert!(b2.usage.is_empty() && ctx.done_files.is_empty());
}

#[test]
fn cache_creation_fallback() {
    let v = json!({"type":"assistant","sessionId":"s","requestId":"r","timestamp":"2026-11-05T01:00:00Z",
        "message":{"id":"m","model":"claude-sonnet-4-5","usage":{"input_tokens":1,"output_tokens":2,"cache_creation_input_tokens":42},"content":[]}});
    let mut b = Batch::default();
    parse_line(&v, "default", &mut b);
    assert_eq!(b.usage[0].cache_write_5m, 42);
    assert_eq!(b.usage[0].cache_write_1h, 0);
}

#[test]
fn missing_ids_skipped() {
    let v = json!({"type":"assistant","sessionId":"s","timestamp":"2026-11-05T01:00:00Z",
        "message":{"id":"m","model":"claude-sonnet-4-5","usage":{"input_tokens":1,"output_tokens":2},"content":[]}});
    let mut b = Batch::default();
    parse_line(&v, "default", &mut b);
    assert!(b.usage.is_empty());
}
