use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;
use tokenscope::adapters::agy::{self, AgyAdapter, RefreshOutcome};
use tokenscope::adapters::{Adapter, CollectCtx};
use tokenscope::{config::Config, db, paths::Paths};

const FIXTURE: &str = include_str!("fixtures/agy/usage.txt");
const RESET: i64 = 1_794_182_400; // 2026-11-09T00:00:00Z

fn fake_agy(dir: &Path, body: &str) -> std::path::PathBuf {
    let p = dir.join("agy");
    std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    p
}

#[test]
fn parse_usage_fixture() {
    let v = agy::parse_usage(FIXTURE, 1000, "acc");
    assert_eq!(v.len(), 2);
    assert_eq!(v[0].scope, "weekly:Gemini Models");
    assert_eq!(v[0].used_percent, 20.0);
    assert_eq!(v[1].scope, "weekly:Claude and GPT models");
    assert_eq!(v[1].used_percent, 65.0);
    for s in &v {
        assert_eq!(s.resets_at, Some(RESET));
        assert_eq!(s.window_minutes, Some(10080));
        assert_eq!(s.source, "agy /usage");
        assert_eq!(s.measured_at, 1000);
        assert_eq!(s.account, "acc");
    }
}

#[test]
fn refresh_runs_then_throttles() {
    let home = tempfile::tempdir().unwrap();
    let paths = Paths {
        home: home.path().to_path_buf(),
    };
    let bin = fake_agy(
        home.path(),
        "cat <<'EOT'\nGemini Models\tWeekly Limit Remaining\t80%\t2026-11-09T00:00:00Z\nEOT",
    );
    let mut conn = db::open(&paths.db()).unwrap();
    let cfg = Config::default();
    let out = agy::refresh_with(&paths, &cfg, &mut conn, &bin, 10_000).unwrap();
    assert!(matches!(out, RefreshOutcome::Ran));
    let n: i64 = conn
        .query_row(
            "SELECT count(*) FROM limit_snapshots WHERE vendor='agy' AND used_percent=20.0",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 1);
    let out = agy::refresh_with(&paths, &cfg, &mut conn, &bin, 10_299).unwrap();
    assert!(
        matches!(
            out,
            RefreshOutcome::Throttled {
                next_allowed: 10_300
            }
        ),
        "{out:?}"
    );
    let out = agy::refresh_with(&paths, &cfg, &mut conn, &bin, 10_300).unwrap();
    assert!(matches!(out, RefreshOutcome::Ran));
}

#[test]
fn refresh_not_installed() {
    let home = tempfile::tempdir().unwrap();
    let paths = Paths {
        home: home.path().to_path_buf(),
    };
    let mut conn = db::open(&paths.db()).unwrap();
    let out = agy::refresh_with(
        &paths,
        &Config::default(),
        &mut conn,
        &home.path().join("nope"),
        1,
    )
    .unwrap();
    assert!(matches!(out, RefreshOutcome::NotInstalled));
}

#[test]
fn refresh_times_out_and_kills() {
    let home = tempfile::tempdir().unwrap();
    let paths = Paths {
        home: home.path().to_path_buf(),
    };
    let bin = fake_agy(home.path(), "sleep 30");
    let mut conn = db::open(&paths.db()).unwrap();
    let t = std::time::Instant::now();
    let r = agy::refresh_with_timeout(
        &paths,
        &Config::default(),
        &mut conn,
        &bin,
        1,
        std::time::Duration::from_millis(300),
    );
    assert!(r.is_err());
    assert!(t.elapsed().as_secs() < 5);
}

#[test]
fn email_never_stored_raw() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(
        d.path().join("old.log"),
        "applyAuthResult: email=old@example.com\n",
    )
    .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(30));
    std::fs::write(
        d.path().join("new.log"),
        "x applyAuthResult: email=first@example.com ok\nmid\n2026 applyAuthResult: email=me@example.com, more\n",
    )
    .unwrap();
    let h = agy::account_hint(d.path());
    assert_eq!(h.len(), 8);
    assert!(h.chars().all(|c| c.is_ascii_hexdigit()));
    assert_eq!(h, tokenscope::model::sha8("me@example.com"));
    assert_eq!(agy::account_hint(&d.path().join("missing")), "");
}

#[test]
fn collect_does_not_run_command_and_discover_uses_path() {
    let home = tempfile::tempdir().unwrap();
    let paths = Paths {
        home: home.path().to_path_buf(),
    };
    let conn = db::open(&paths.db()).unwrap();
    let cfg = Config::default();
    let mut ctx = CollectCtx {
        conn: &conn,
        paths: &paths,
        config: &cfg,
        done_files: vec![],
        unreadable: 0,
        remove_after_write: vec![],
    };
    let b = AgyAdapter.collect(&mut ctx).unwrap();
    assert!(b.limits.is_empty() && b.usage.is_empty());
    let _ = AgyAdapter.discover(&paths);
}

#[test]
fn status_refresh_prints_agy_lines() {
    let home = tempfile::tempdir().unwrap();
    let bindir = tempfile::tempdir().unwrap();
    fake_agy(
        bindir.path(),
        "cat <<'EOT'\nGemini Models\tWeekly Limit Remaining\t80%\t2026-11-09T00:00:00Z\nEOT",
    );
    // a fake `claude` too: `status --refresh` asks it as well, and the real one must never run in tests
    fake_claude(
        bindir.path(),
        "echo 'Current session: 49% used · resets 11:39am (UTC)'",
    );
    let path = format!(
        "{}:{}",
        bindir.path().display(),
        std::env::var("PATH").unwrap()
    );
    let o = Command::new(env!("CARGO_BIN_EXE_tokenscope"))
        .args(["status", "--refresh"])
        .env("HOME", home.path())
        .env("PATH", path)
        .env("TZ", "Asia/Ho_Chi_Minh")
        .output()
        .unwrap();
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(o.status.success(), "{out}");
    assert!(
        out.contains("Gemini Models") && out.contains("20%"),
        "{out}"
    );
    // 2026-11-09T00:00Z is 07:00 in Vietnam: reset times are shown in local time
    assert!(out.contains("resets 2026-11-09 07:00"), "{out}");
    assert!(out.contains("claude five_hour: 49% used"), "{out}");
    assert!(!out.contains("UTC"), "{out}");
}

fn fake_claude(dir: &Path, body: &str) {
    let p = dir.join("claude");
    std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn stamped(conn: &rusqlite::Connection) -> bool {
    conn.query_row(
        "SELECT count(*) FROM kv WHERE key='agy:last_refresh'",
        [],
        |r| r.get::<_, i64>(0),
    )
    .unwrap()
        == 1
}

#[test]
fn refresh_junk_or_failing_output_is_error_but_stamps_throttle() {
    for body in [
        "echo 'please log in me@example.com email=me@example.com'",
        "exit 1",
    ] {
        let home = tempfile::tempdir().unwrap();
        let paths = Paths {
            home: home.path().to_path_buf(),
        };
        let bin = fake_agy(home.path(), body);
        let mut conn = db::open(&paths.db()).unwrap();
        let e = agy::refresh_with(&paths, &Config::default(), &mut conn, &bin, 500)
            .unwrap_err()
            .to_string();
        assert!(e.contains("no weekly limit lines"), "{e}");
        assert!(!e.contains("email="), "{e}");
        assert!(stamped(&conn));
        let out = agy::refresh_with(&paths, &Config::default(), &mut conn, &bin, 501).unwrap();
        assert!(matches!(out, RefreshOutcome::Throttled { .. }));
    }
}

#[test]
fn parse_usage_clamps_percent() {
    let v = agy::parse_usage(
        "G\tWeekly Limit Remaining\t120%\t2026-11-09T00:00:00Z\nH\tWeekly Limit Remaining\t-5%\t2026-11-09T00:00:00Z",
        1,
        "",
    );
    assert_eq!((v[0].used_percent, v[1].used_percent), (0.0, 100.0));
}

#[test]
fn status_refresh_json_has_agy_key() {
    let home = tempfile::tempdir().unwrap();
    let bindir = tempfile::tempdir().unwrap();
    fake_agy(
        bindir.path(),
        "cat <<'EOT'\nGemini Models\tWeekly Limit Remaining\t80%\t2026-11-09T00:00:00Z\nEOT",
    );
    // a fake `claude` too: `status --refresh` asks it as well, and the real one must never run in tests
    fake_claude(
        bindir.path(),
        "echo 'Current session: 49% used · resets 11:39am (UTC)'",
    );
    let path = format!(
        "{}:{}",
        bindir.path().display(),
        std::env::var("PATH").unwrap()
    );
    let o = Command::new(env!("CARGO_BIN_EXE_tokenscope"))
        .args(["status", "--refresh", "--json"])
        .env("HOME", home.path())
        .env("PATH", path)
        .output()
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v["agy"]["snapshots"][0]["scope"], "weekly:Gemini Models");
    assert_eq!(v["agy"]["snapshots"][0]["used_percent"], 20.0);
    assert_eq!(v["claude"]["snapshots"][0]["scope"], "five_hour");
    assert_eq!(v["claude"]["snapshots"][0]["used_percent"], 49.0);
}
