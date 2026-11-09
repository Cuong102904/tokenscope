use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::Duration;
use tokenscope::adapters::codex_usage::{self, RefreshOutcome};
use tokenscope::config::Config;
use tokenscope::model::{Vendor, sha8};
use tokenscope::{db, paths::Paths};

const FIXTURE: &str = include_str!("fixtures/codex_usage/rate_limits.json");
const ACCOUNT_ID: &str = "00000000-0000-4000-8000-000000000001";

#[test]
fn parses_each_limit_and_window_with_the_hashed_account() {
    let v: serde_json::Value = serde_json::from_str(FIXTURE).unwrap();
    let s = codex_usage::parse_rate_limits(&v["result"], 1000);
    let got: Vec<_> = s
        .iter()
        .map(|x| {
            (
                x.scope.as_str(),
                x.window_minutes,
                x.used_percent,
                x.resets_at,
            )
        })
        .collect();
    assert_eq!(
        got,
        [
            ("five_hour", Some(300), 39.0, Some(1793948751)),
            ("weekly", Some(10080), 100.0, Some(1794378292)),
            ("five_hour:GPT-6 Astra", Some(300), 12.5, Some(1793948751)),
        ]
    );
    assert!(
        s.iter()
            .all(|x| x.vendor == Vendor::Codex && x.account == sha8(ACCOUNT_ID)),
        "same account key as the session logs"
    );
    assert!(s.iter().all(|x| x.plan_type.as_deref() == Some("plus")
        && x.measured_at == 1000
        && x.source == "codex app-server"));
}

#[test]
fn falls_back_to_the_single_rate_limits_object() {
    let v = serde_json::json!({"rateLimits": {"limitId": "codex", "primary": {"usedPercent": 5, "windowDurationMins": 300, "resetsAt": 10}, "secondary": null}});
    let s = codex_usage::parse_rate_limits(&v, 1);
    assert_eq!(s.len(), 1);
    assert_eq!(
        (s[0].scope.as_str(), s[0].account.as_str()),
        ("five_hour", "")
    );
}

/// A fake `codex` that answers JSON-RPC lines on stdin like `codex app-server`.
fn fake_codex(dir: &Path, on_read: &str) -> std::path::PathBuf {
    let p = dir.join("codex");
    let body = format!(
        "#!/bin/sh\n[ \"$1\" = app-server ] || exit 3\npwd > {dir}/cwd\nwhile read -r l; do\n  case \"$l\" in\n    *'\"initialize\"'*) echo '{{\"id\":1,\"result\":{{}}}}' ;;\n    *rateLimits/read*) {on_read} ;;\n  esac\ndone\n",
        dir = dir.display()
    );
    std::fs::write(&p, body).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    p
}

#[test]
fn refresh_asks_app_server_and_throttles() {
    let h = tempfile::tempdir().unwrap();
    let paths = Paths {
        home: h.path().to_path_buf(),
    };
    let mut conn = db::open(&paths.db()).unwrap();
    let fx = h.path().join("fx.json");
    std::fs::write(&fx, format!("{}\n", FIXTURE.trim())).unwrap(); // one JSON message per line
    let bin = fake_codex(
        h.path(),
        &format!(
            "echo '{{\"method\":\"account/updated\"}}'; cat {}",
            fx.display()
        ),
    );
    let cfg = Config::default();
    assert_eq!(
        codex_usage::refresh_with(&paths, &cfg, &mut conn, &bin, 5000).unwrap(),
        RefreshOutcome::Ran
    );
    let cwd = std::fs::read_to_string(h.path().join("cwd")).unwrap();
    assert_eq!(
        Path::new(cwd.trim()),
        paths.data_dir().canonicalize().unwrap(),
        "runs inside ~/.tokenscope"
    );
    let n: i64 = conn
        .query_row(
            "SELECT count(*) FROM limit_snapshots WHERE vendor='codex' AND account=?1",
            [sha8(ACCOUNT_ID)],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 3);
    assert_eq!(
        codex_usage::refresh_with(&paths, &cfg, &mut conn, &bin, 5299).unwrap(),
        RefreshOutcome::Throttled { next_allowed: 5300 }
    );
}

#[test]
fn refresh_reports_errors_and_never_hangs() {
    let h = tempfile::tempdir().unwrap();
    let paths = Paths {
        home: h.path().to_path_buf(),
    };
    let mut conn = db::open(&paths.db()).unwrap();
    let cfg = Config::default();
    assert_eq!(
        codex_usage::refresh_with(&paths, &cfg, &mut conn, &h.path().join("nope/codex"), 1)
            .unwrap(),
        RefreshOutcome::NotInstalled
    );
    let bin = fake_codex(
        h.path(),
        r#"echo '{"id":2,"error":{"code":-32600,"message":"not logged in"}}'"#,
    );
    let e = codex_usage::refresh_with(&paths, &cfg, &mut conn, &bin, 1000)
        .unwrap_err()
        .to_string();
    assert!(e.contains("not logged in"), "{e}");
    // never answers: gives up at the timeout instead of hanging the dashboard
    let bin = fake_codex(h.path(), "sleep 30");
    let t = std::time::Instant::now();
    let e = codex_usage::refresh_with_timeout(
        &paths,
        &cfg,
        &mut conn,
        &bin,
        2000,
        Duration::from_millis(500),
    )
    .unwrap_err()
    .to_string();
    assert!(
        e.contains("timed out") && t.elapsed() < Duration::from_secs(5),
        "{e}"
    );
}

/// Stand-in for the Codex login another tool (codex-multi-auth, `codex login`) rewrites on a
/// switch. Mode 000: tokenscope may look at its metadata but must never need to open it.
fn login(paths: &Paths, content: &str) {
    let dir = paths.home.join(".codex");
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("auth.json");
    if f.exists() {
        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    std::fs::write(&f, content).unwrap();
    std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o000)).unwrap();
}

fn codex_rows(conn: &rusqlite::Connection) -> i64 {
    conn.query_row(
        "SELECT count(*) FROM limit_snapshots WHERE vendor='codex'",
        [],
        |r| r.get(0),
    )
    .unwrap()
}

/// A fake app-server answering with the fixture; each run appends a line to `runs`.
fn counting_codex(dir: &Path) -> std::path::PathBuf {
    let fx = dir.join("fx.json");
    std::fs::write(&fx, format!("{}\n", FIXTURE.trim())).unwrap();
    fake_codex(
        dir,
        &format!("echo run >> {}/runs; cat {}", dir.display(), fx.display()),
    )
}

fn runs(dir: &Path) -> usize {
    std::fs::read_to_string(dir.join("runs"))
        .map(|s| s.lines().count())
        .unwrap_or(0)
}

#[test]
fn follows_an_account_switch_by_the_login_files_metadata() {
    let h = tempfile::tempdir().unwrap();
    let paths = Paths {
        home: h.path().to_path_buf(),
    };
    let mut conn = db::open(&paths.db()).unwrap();
    let cfg = Config::default();
    let bin = counting_codex(h.path());
    let follow = |conn: &mut rusqlite::Connection, now| {
        codex_usage::follow_switch_with(&paths, &cfg, conn, &bin, now).unwrap()
    };
    // not logged in: nothing to follow
    assert_eq!(follow(&mut conn, 1000), None);
    // first look only remembers the login; unchanged, nothing happens
    login(&paths, "account A");
    assert_eq!(follow(&mut conn, 1030), None);
    assert_eq!(follow(&mut conn, 1060), None);
    assert_eq!((runs(h.path()), codex_rows(&conn)), (0, 0));
    // a refresh just ran, so the 5-minute throttle is closed...
    assert_eq!(
        codex_usage::refresh_with(&paths, &cfg, &mut conn, &bin, 1100).unwrap(),
        RefreshOutcome::Ran
    );
    assert_eq!(runs(h.path()), 1);
    // ...but a switch is a new account: asked right away, and the throttle is restamped
    login(&paths, "account B, a longer login");
    assert_eq!(follow(&mut conn, 1200), Some(RefreshOutcome::Ran));
    assert_eq!(runs(h.path()), 2);
    assert_eq!(
        codex_usage::refresh_with(&paths, &cfg, &mut conn, &bin, 1210).unwrap(),
        RefreshOutcome::Throttled { next_allowed: 1500 }
    );
    assert_eq!(follow(&mut conn, 1230), None, "same login, nothing new");
}

#[test]
fn quick_switches_are_followed_at_most_once_a_minute() {
    let h = tempfile::tempdir().unwrap();
    let paths = Paths {
        home: h.path().to_path_buf(),
    };
    let mut conn = db::open(&paths.db()).unwrap();
    let cfg = Config::default();
    let bin = counting_codex(h.path());
    let mut follow =
        |now| codex_usage::follow_switch_with(&paths, &cfg, &mut conn, &bin, now).unwrap();
    login(&paths, "A");
    assert_eq!(follow(1000), None);
    login(&paths, "BB");
    assert_eq!(follow(1010), Some(RefreshOutcome::Ran));
    // switched again 20 s later: waits for the minute to pass, then asks once
    login(&paths, "CCC");
    assert_eq!(follow(1030), None);
    assert_eq!(follow(1060), None);
    assert_eq!(follow(1075), Some(RefreshOutcome::Ran));
    assert_eq!(follow(1200), None);
    assert_eq!(runs(h.path()), 2);
}

#[test]
fn a_failed_follow_is_reported_once_not_retried_every_tick() {
    let h = tempfile::tempdir().unwrap();
    let paths = Paths {
        home: h.path().to_path_buf(),
    };
    let mut conn = db::open(&paths.db()).unwrap();
    let cfg = Config::default();
    let bin = fake_codex(
        h.path(),
        r#"echo run >> runs; echo '{"id":2,"error":{"code":-32600,"message":"not logged in"}}'"#,
    );
    login(&paths, "A");
    codex_usage::follow_switch_with(&paths, &cfg, &mut conn, &bin, 1000).unwrap();
    login(&paths, "BB");
    let e = codex_usage::follow_switch_with(&paths, &cfg, &mut conn, &bin, 1100)
        .unwrap_err()
        .to_string();
    assert!(e.contains("not logged in"), "{e}");
    assert_eq!(
        codex_usage::follow_switch_with(&paths, &cfg, &mut conn, &bin, 1200).unwrap(),
        None
    );
}

#[test]
fn the_background_collector_follows_a_switch() {
    use tokenscope::collect::Collector;
    let h = tempfile::tempdir().unwrap();
    let paths = Paths {
        home: h.path().to_path_buf(),
    };
    let bin = counting_codex(h.path());
    let mut c = Collector::with_adapters(paths.clone(), Config::default(), vec![]).codex_bin(bin);
    login(&paths, "A");
    assert!(c.tick().ok());
    login(&paths, "BB");
    assert!(c.tick().ok());
    assert_eq!(runs(h.path()), 1);
    let conn = db::open(&paths.db()).unwrap();
    assert_eq!(codex_rows(&conn), 3);
    assert!(c.take_log().is_empty());
}
