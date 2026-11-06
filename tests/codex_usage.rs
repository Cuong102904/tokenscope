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
