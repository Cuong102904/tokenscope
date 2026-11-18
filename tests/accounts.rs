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

// ---- overview ----

use tokenscope::config::Config;
use tokenscope::pricing::PriceTable;
use tokenscope::query::{Filter, accounts as overview_q};

const NOW: i64 = 1_800_000_000;

fn usage(c: &rusqlite::Connection, key: &str, account: &str, input: i64) {
    c.execute(
        "INSERT INTO usage(msg_key,vendor,account,session_id,project,ts,day,model,input,output,reasoning,
           cache_read,cache_write_5m,cache_write_1h,sidechain) VALUES(?1,'claude',?2,'s','p',?3,?4,'claude-fable-5-1',?5,0,0,0,0,0,0)",
        rusqlite::params![
            key,
            account,
            NOW,
            chrono::Local::now().format("%Y-%m-%d").to_string(),
            input
        ],
    )
    .unwrap();
}

fn reading(c: &rusqlite::Connection, account: &str, scope: &str, pct: f64, at: i64) {
    c.execute(
        "INSERT INTO limit_snapshots(vendor,account,scope,window_minutes,used_percent,resets_at,plan_type,measured_at,source)
         VALUES('claude',?1,?2,300,?3,?4,NULL,?5,'test')",
        rusqlite::params![account, scope, pct, NOW + 3600, at],
    )
    .unwrap();
}

fn rows_of(c: &rusqlite::Connection) -> Vec<overview_q::AccountRow> {
    let paths = Paths {
        home: std::path::PathBuf::from("/nonexistent"),
    };
    let prices = PriceTable::load(&paths).unwrap();
    overview_q::overview(c, &Config::default(), &prices, &Filter::default(), NOW).unwrap()
}

#[test]
fn overview_orders_current_first_then_most_quota_left() {
    let (_d, c) = conn();
    // "cur" has the newest reading (signed in); "low" has plenty left; "full" is at 100%
    reading(&c, "full", "five_hour", 100.0, NOW - 300);
    reading(&c, "low", "five_hour", 7.0, NOW - 200);
    reading(&c, "cur", "five_hour", 50.0, NOW - 10);
    usage(&c, "u1", "idle", 10); // usage only, no reading
    let order: Vec<(String, bool)> = rows_of(&c)
        .into_iter()
        .map(|r| (r.account, r.current))
        .collect();
    assert_eq!(
        order,
        vec![
            ("cur".to_string(), true),
            ("low".to_string(), false),
            ("full".to_string(), false),
            ("idle".to_string(), false),
        ]
    );
}

#[test]
fn overview_label_is_the_registry_email_unless_named() {
    let (_d, c) = conn();
    reading(&c, "k1", "five_hour", 10.0, NOW - 10);
    reading(&c, "k2", "five_hour", 10.0, NOW - 20);
    for (k, e) in [("k1", "one@example.com"), ("k2", "two@example.com")] {
        accounts::upsert(&c, Vendor::Claude, k, &info(e, NOW)).unwrap();
    }
    tokenscope::query::limits::set_label(&c, "claude", "k2", "Work").unwrap();
    let rows = rows_of(&c);
    let get = |k: &str| rows.iter().find(|r| r.account == k).unwrap();
    assert_eq!(get("k1").label, "one@example.com");
    assert_eq!(get("k1").email.as_deref(), Some("one@example.com"));
    assert_eq!(get("k1").org.as_deref(), Some("Acme"));
    assert_eq!(get("k2").label, "Work");
    assert_eq!(
        get("k2").email.as_deref(),
        Some("two@example.com"),
        "email still known"
    );
}

#[test]
fn overview_includes_registered_accounts_with_no_usage_and_windows_and_tokens() {
    let (_d, c) = conn();
    accounts::upsert(&c, Vendor::Claude, "quiet", &info("q@example.com", NOW)).unwrap();
    reading(&c, "busy", "five_hour", 20.0, NOW - 5);
    usage(&c, "u1", "busy", 123);
    let rows = rows_of(&c);
    let busy = rows.iter().find(|r| r.account == "busy").unwrap();
    assert_eq!(busy.tokens, 123);
    assert_eq!(busy.windows.len(), 1);
    assert_eq!(busy.windows[0].scope, "five_hour");
    let quiet = rows.iter().find(|r| r.account == "quiet").unwrap();
    assert_eq!((quiet.tokens, quiet.windows.len()), (0, 0));
    assert_eq!(quiet.cost, Some(0.0));
}
