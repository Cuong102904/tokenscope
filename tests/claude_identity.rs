use tokenscope::accounts::claude_key;
use tokenscope::adapters::claude_identity::{migrate_email_key, signed_in};
use tokenscope::db;
use tokenscope::model::sha8;
use tokenscope::paths::Paths;

fn home_with(claude_json: Option<&str>) -> (tempfile::TempDir, Paths) {
    let d = tempfile::tempdir().unwrap();
    let paths = Paths {
        home: d.path().to_path_buf(),
    };
    if let Some(body) = claude_json {
        std::fs::write(paths.claude_json(), body).unwrap();
    }
    (d, paths)
}

const OK: &str = r#"{"numStartups":3,"oauthAccount":{"accountUuid":"u-1","emailAddress":"me@example.com","organizationUuid":"o-1","organizationName":"Acme","organizationRateLimitTier":"default_claude_max_5x"}}"#;

#[test]
fn reads_oauth_account_fields() {
    let (_d, paths) = home_with(Some(OK));
    let s = signed_in(&paths, 42).unwrap();
    assert_eq!(s.key, claude_key("u-1", Some("o-1")));
    assert_eq!(s.info.email, "me@example.com");
    assert_eq!(s.info.org.as_deref(), Some("Acme"));
    assert_eq!(s.info.plan.as_deref(), Some("default_claude_max_5x"));
    assert_eq!(s.info.last_seen, 42);
}

#[test]
fn returns_none_when_file_missing() {
    let (_d, paths) = home_with(None);
    assert!(signed_in(&paths, 1).is_none());
}

#[test]
fn returns_none_for_invalid_json() {
    let (_d, paths) = home_with(Some("{not json"));
    assert!(signed_in(&paths, 1).is_none());
}

#[test]
fn returns_none_without_oauth_account_or_with_empty_fields() {
    let (_d, p1) = home_with(Some(r#"{"numStartups":1}"#));
    assert!(signed_in(&p1, 1).is_none());
    let (_d, p2) = home_with(Some(
        r#"{"oauthAccount":{"accountUuid":"","emailAddress":"me@example.com"}}"#,
    ));
    assert!(signed_in(&p2, 1).is_none());
    let (_d, p3) = home_with(Some(
        r#"{"oauthAccount":{"accountUuid":"u","emailAddress":"  "}}"#,
    ));
    assert!(signed_in(&p3, 1).is_none());
}

#[test]
fn key_includes_org() {
    let (_d, a) = home_with(Some(OK));
    let (_d2, b) = home_with(Some(&OK.replace("o-1", "o-2")));
    assert_ne!(signed_in(&a, 1).unwrap().key, signed_in(&b, 1).unwrap().key);
}

#[test]
fn migrates_email_keyed_rows_once() {
    let (_d, paths) = home_with(None);
    let c = db::open(&paths.db()).unwrap();
    let old = sha8("me@example.com");
    c.execute(
        "INSERT INTO limit_snapshots(vendor,account,scope,window_minutes,used_percent,resets_at,plan_type,measured_at,source) VALUES('claude',?1,'weekly',10080,5,NULL,NULL,10,'x')",
        [&old],
    )
    .unwrap();
    c.execute(
        "INSERT INTO limit_events(vendor,account,ts,scope,resets_at) VALUES('claude',?1,10,'weekly',20)",
        [&old],
    )
    .unwrap();
    c.execute(
        "INSERT INTO kv(key,value) VALUES(?1,'Work')",
        [format!("label:claude:{old}")],
    )
    .unwrap();
    migrate_email_key(&c, "me@example.com", "newkey01").unwrap();
    migrate_email_key(&c, "me@example.com", "newkey01").unwrap(); // idempotent
    let acct: String = c
        .query_row("SELECT account FROM limit_snapshots", [], |r| r.get(0))
        .unwrap();
    let ev: String = c
        .query_row("SELECT account FROM limit_events", [], |r| r.get(0))
        .unwrap();
    assert_eq!((acct.as_str(), ev.as_str()), ("newkey01", "newkey01"));
    let label: String = c
        .query_row(
            "SELECT value FROM kv WHERE key='label:claude:newkey01'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(label, "Work");
    let left: i64 = c
        .query_row(
            "SELECT count(*) FROM kv WHERE key=?1",
            [format!("label:claude:{old}")],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(left, 0);
}
