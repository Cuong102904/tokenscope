use agent_usage::model::{Batch, UsageRecord, Vendor};
use agent_usage::{db, safety};
use std::path::Path;

fn rec(output: u64) -> UsageRecord {
    UsageRecord {
        msg_key: "k1".into(),
        vendor: Vendor::Claude,
        account: String::new(),
        session_id: "s".into(),
        project: "p".into(),
        ts: 1_700_000_000,
        model: "m".into(),
        input: 1,
        output,
        reasoning: 0,
        cache_read: 0,
        cache_write_5m: 0,
        cache_write_1h: 0,
        sidechain: false,
        skill: None,
        plugin: None,
    }
}

#[test]
fn forbidden_paths() {
    for p in [
        "/h/.codex/auth.json",
        "/h/.codex/multi-auth/openai-codex-accounts.json.bak",
        "/h/.claude/.credentials.json",
        "/h/x/oauth_tokens.json",
        "/h/.local/share/opencode/auth.json",
    ] {
        assert!(safety::is_forbidden(Path::new(p)), "{p}");
    }
    assert!(!safety::is_forbidden(Path::new(
        "/h/.claude/projects/a/b.jsonl"
    )));
    assert!(!safety::is_forbidden(Path::new(
        "/h/.codex/multi-auth/quota-cache.json"
    )));
    assert!(!safety::is_forbidden(Path::new(
        "/h/.claude/projects/-home-u-token-counter/s1.jsonl"
    )));
    assert!(safety::is_forbidden(Path::new("/h/x/oauth_creds.json")));
}

#[test]
fn open_guarded_refuses_symlink_to_forbidden() {
    let d = tempfile::tempdir().unwrap();
    let target = d.path().join("auth.json");
    std::fs::write(&target, "x").unwrap();
    let link = d.path().join("ok.json");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    let e = safety::open_guarded(&link).unwrap_err();
    assert_eq!(e.kind(), std::io::ErrorKind::PermissionDenied);
}

#[test]
fn open_guarded_denies() {
    let e = safety::open_guarded(Path::new("/h/.codex/auth.json")).unwrap_err();
    assert_eq!(e.kind(), std::io::ErrorKind::PermissionDenied);
}

#[test]
fn upsert_keeps_max_output() {
    let d = tempfile::tempdir().unwrap();
    let mut c = db::open(&d.path().join("x/u.db")).unwrap();
    let out = |c: &rusqlite::Connection| -> (i64, i64) {
        c.query_row("SELECT count(*), max(output) FROM usage", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap()
    };
    for o in [10, 5] {
        db::write_batch(
            &mut c,
            &Batch {
                usage: vec![rec(o)],
                ..Default::default()
            },
        )
        .unwrap();
    }
    assert_eq!(out(&c), (1, 10), "smaller output must not replace");
    db::write_batch(
        &mut c,
        &Batch {
            usage: vec![rec(20)],
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(out(&c), (1, 20));
}

#[test]
fn open_creates_wal_db() {
    let d = tempfile::tempdir().unwrap();
    let c = db::open(&d.path().join("sub/u.db")).unwrap();
    let m: String = c
        .query_row("PRAGMA journal_mode", [], |r| r.get(0))
        .unwrap();
    assert_eq!(m, "wal");
    c.execute("INSERT INTO kv(key,value) VALUES('a','b')", [])
        .unwrap();
}

#[test]
fn sha8_known() {
    assert_eq!(agent_usage::model::sha8("abc"), "ba7816bf");
}

#[test]
fn open_checks_user_version() {
    let d = tempfile::tempdir().unwrap();
    let fresh = d.path().join("a.db");
    let c = agent_usage::db::open(&fresh).unwrap();
    assert_eq!(
        c.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
    drop(c);
    agent_usage::db::open(&fresh).unwrap();
    let other = d.path().join("b.db");
    let c = rusqlite::Connection::open(&other).unwrap();
    c.execute_batch("PRAGMA user_version = 7; CREATE TABLE t(x);")
        .unwrap();
    drop(c);
    let e = agent_usage::db::open(&other).unwrap_err().to_string();
    assert!(e.contains("not an agent-usage database"), "{e}");
    // foreign db with user_version 0 but tables (e.g. the python prototype)
    let proto = d.path().join("c.db");
    let c = rusqlite::Connection::open(&proto).unwrap();
    c.execute_batch("CREATE TABLE messages(x);").unwrap();
    drop(c);
    assert!(agent_usage::db::open(&proto).is_err());
}
