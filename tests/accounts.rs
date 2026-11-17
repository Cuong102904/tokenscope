use tokenscope::accounts::{self, AccountInfo};
use tokenscope::db;
use tokenscope::model::Vendor;
use tokenscope::paths::Paths;

fn conn() -> (tempfile::TempDir, rusqlite::Connection) {
    let d = tempfile::tempdir().unwrap();
    let paths = Paths {
        home: d.path().to_path_buf(),
    };
    let c = db::open(&paths.db()).unwrap();
    (d, c)
}

fn info(email: &str, last_seen: i64) -> AccountInfo {
    AccountInfo {
        email: email.into(),
        org: Some("Acme".into()),
        plan: None,
        last_seen,
    }
}

#[test]
fn claude_key_differs_by_org() {
    let a = accounts::claude_key("u1", Some("a"));
    let b = accounts::claude_key("u1", Some("b"));
    let none = accounts::claude_key("u1", None);
    assert_ne!(a, b);
    assert_ne!(a, none);
    assert_ne!(b, none);
    assert_eq!(a, accounts::claude_key("u1", Some("a")));
    assert_eq!(a.len(), 8);
    assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
}

#[test]
fn upsert_then_get_round_trips() {
    let (_d, c) = conn();
    let i = info("me@example.com", 100);
    accounts::upsert(&c, Vendor::Claude, "k1", &i).unwrap();
    assert_eq!(accounts::get(&c, Vendor::Claude, "k1").unwrap(), Some(i));
    assert_eq!(accounts::get(&c, Vendor::Claude, "other").unwrap(), None);
}

#[test]
fn upsert_keeps_latest_last_seen() {
    let (_d, c) = conn();
    accounts::upsert(&c, Vendor::Claude, "k1", &info("old@example.com", 500)).unwrap();
    accounts::upsert(&c, Vendor::Claude, "k1", &info("new@example.com", 100)).unwrap();
    let got = accounts::get(&c, Vendor::Claude, "k1").unwrap().unwrap();
    assert_eq!(got.last_seen, 500);
    assert_eq!(got.email, "new@example.com");
}

#[test]
fn all_lists_only_that_vendor() {
    let (_d, c) = conn();
    accounts::upsert(&c, Vendor::Claude, "k1", &info("a@example.com", 1)).unwrap();
    accounts::upsert(&c, Vendor::Claude, "k2", &info("b@example.com", 2)).unwrap();
    accounts::upsert(&c, Vendor::Codex, "k3", &info("c@example.com", 3)).unwrap();
    let mut keys: Vec<String> = accounts::all(&c, Vendor::Claude)
        .unwrap()
        .into_iter()
        .map(|(k, _)| k)
        .collect();
    keys.sort();
    assert_eq!(keys, vec!["k1", "k2"]);
}
