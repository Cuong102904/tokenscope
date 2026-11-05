use agent_usage::adapters::{Adapter, CollectCtx, opencode::OpenCodeAdapter};
use agent_usage::{collect, config::Config, db, paths::Paths};
use rusqlite::Connection;
use std::path::Path;

struct Env {
    _d: tempfile::TempDir,
    paths: Paths,
    oc: Connection,
    conn: Connection,
}

fn setup() -> Env {
    let d = tempfile::tempdir().unwrap();
    let paths = Paths { home: d.path().to_path_buf() };
    let ocp = paths.opencode_db();
    std::fs::create_dir_all(ocp.parent().unwrap()).unwrap();
    let oc = Connection::open(&ocp).unwrap();
    oc.execute_batch(&std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/opencode/make_db.sql")).unwrap()).unwrap();
    let conn = db::open(&d.path().join("t.db")).unwrap();
    Env { _d: d, paths, oc, conn }
}

fn run(e: &mut Env) -> usize {
    let cfg = Config::default();
    let st = collect::run_all(&mut e.conn, &e.paths, &cfg, &[Box::new(OpenCodeAdapter)]);
    assert!(st[0].error.is_none(), "{:?}", st[0].error);
    st[0].records
}

fn count(e: &Env) -> i64 {
    e.conn.query_row("SELECT count(*) FROM usage", [], |r| r.get(0)).unwrap()
}

#[test]
fn opencode_tokens() {
    let mut e = setup();
    assert_eq!(run(&mut e), 2, "user row and token-less assistant row are skipped");
    let q = |id: &str| -> (i64, i64, i64, i64, i64, i64, String, String, String) {
        e.conn
            .query_row(
                "SELECT input,output,reasoning,cache_read,cache_write_5m,ts,model,project,session_id FROM usage WHERE msg_key=?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?, r.get(8)?)),
            )
            .unwrap()
    };
    assert_eq!(q("opencode:msg_a1"), (676, 82, 22, 73216, 0, 1700000001, "gpt-test".into(), "/home/u/proj-a".into(), "ses_1".into()));
    assert_eq!(q("opencode:msg_a2").4, 10);
    let (p, t, src, lw): (String, Option<String>, String, i64) = e
        .conn
        .query_row("SELECT project,title,source_path,last_write FROM sessions WHERE vendor='opencode' AND session_id='ses_1'", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .unwrap();
    assert_eq!((p.as_str(), t.as_deref(), lw), ("/home/u/proj-a", Some("Fix the parser"), 1700000100));
    assert_eq!(src, e.paths.opencode_db().to_string_lossy());
}

#[test]
fn incremental_by_time_updated() {
    let mut e = setup();
    run(&mut e);
    assert_eq!(count(&e), 2);
    assert_eq!(run(&mut e), 0, "second run adds nothing");
    assert_eq!(count(&e), 2);
    e.oc.execute(
        "INSERT INTO message VALUES('msg_a4','ses_1',1700000004000,1700000080000,
         '{\"role\":\"assistant\",\"modelID\":\"gpt-test\",\"tokens\":{\"input\":5,\"output\":1,\"reasoning\":0,\"cache\":{\"write\":0,\"read\":0}},\"time\":{\"created\":1700000004000}}')",
        [],
    )
    .unwrap();
    assert_eq!(run(&mut e), 1);
    assert_eq!(count(&e), 3);
    let w: String = e.conn.query_row("SELECT value FROM kv WHERE key='opencode:max_time_updated'", [], |r| r.get(0)).unwrap();
    assert_eq!(w, "1700000080000");
}

#[test]
fn watermark_not_advanced_when_write_does_not_happen() {
    let e = setup();
    let cfg = Config::default();
    let mut ctx = CollectCtx { conn: &e.conn, paths: &e.paths, config: &cfg, done_files: vec![], unreadable: 0, remove_after_write: vec![] };
    let b = OpenCodeAdapter.collect(&mut ctx).unwrap();
    assert_eq!(b.usage.len(), 2);
    // collect alone must not touch kv: only write_batch commits the watermark
    let n: i64 = e.conn.query_row("SELECT count(*) FROM kv WHERE key='opencode:max_time_updated'", [], |r| r.get(0)).unwrap();
    assert_eq!(n, 0);
}

#[test]
fn locked_or_missing_db_is_unavailable_not_panic() {
    // missing
    let d = tempfile::tempdir().unwrap();
    let paths = Paths { home: d.path().to_path_buf() };
    let mut conn = db::open(&d.path().join("t.db")).unwrap();
    let cfg = Config::default();
    let st = collect::run_all(&mut conn, &paths, &cfg, &[Box::new(OpenCodeAdapter)]);
    assert!(!st[0].found);

    // locked: another connection holds an exclusive lock (rollback-journal DB)
    let mut e = setup();
    e.oc.execute_batch("BEGIN EXCLUSIVE").unwrap();
    let cfg = Config::default();
    let st = collect::run_all(&mut e.conn, &e.paths, &cfg, &[Box::new(OpenCodeAdapter)]);
    assert!(st[0].error.is_some(), "locked DB must surface as an adapter error");
    assert_eq!(count(&e), 0);
    e.oc.execute_batch("COMMIT").unwrap();
    assert_eq!(run(&mut e), 2, "recovers once the lock is released");

    // garbage file: error, no panic
    std::fs::write(e.paths.opencode_db(), b"not a database at all, just text").unwrap();
    let cfg = Config::default();
    let st = collect::run_all(&mut e.conn, &e.paths, &cfg, &[Box::new(OpenCodeAdapter)]);
    assert!(st[0].error.is_some());
}
