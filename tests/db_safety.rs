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
    assert!(!safety::is_forbidden(Path::new("/h/.claude/projects/a/b.jsonl")));
    assert!(!safety::is_forbidden(Path::new("/h/.codex/multi-auth/quota-cache.json")));
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
    for o in [10, 5, 20] {
        let b = Batch { usage: vec![rec(o)], ..Default::default() };
        db::write_batch(&mut c, &b).unwrap();
    }
    let (n, out): (i64, i64) = c
        .query_row("SELECT count(*), max(output) FROM usage", [], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap();
    assert_eq!((n, out), (1, 20));
}

#[test]
fn open_creates_wal_db() {
    let d = tempfile::tempdir().unwrap();
    let c = db::open(&d.path().join("sub/u.db")).unwrap();
    let m: String = c.query_row("PRAGMA journal_mode", [], |r| r.get(0)).unwrap();
    assert_eq!(m, "wal");
    c.execute("INSERT INTO kv(key,value) VALUES('a','b')", []).unwrap();
}

#[test]
fn sha8_known() {
    assert_eq!(agent_usage::model::sha8("abc"), "ba7816bf");
}
