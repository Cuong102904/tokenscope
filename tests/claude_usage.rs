use chrono::{Local, TimeZone};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use tokenscope::adapters::claude_usage::{self, RefreshOutcome};
use tokenscope::config::Config;
use tokenscope::db;
use tokenscope::paths::Paths;

const FIXTURE: &str = include_str!("fixtures/claude_usage/usage.txt");

fn local(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> i64 {
    Local
        .with_ymd_and_hms(y, mo, d, h, mi, 0)
        .unwrap()
        .timestamp()
}

#[test]
fn parses_session_week_and_per_model_lines() {
    let now = local(2026, 11, 6, 11, 0);
    let s = claude_usage::parse_usage(FIXTURE, now, "me");
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
            (
                "five_hour",
                Some(300),
                49.0,
                Some(local(2026, 11, 6, 11, 39))
            ),
            (
                "weekly",
                Some(10080),
                13.0,
                Some(local(2026, 11, 13, 1, 59))
            ),
            (
                "weekly:Fable",
                Some(10080),
                3.0,
                Some(local(2026, 11, 13, 1, 59))
            ),
        ]
    );
    assert!(
        s.iter()
            .all(|x| x.account == "me" && x.measured_at == now && x.source == "claude /usage")
    );
    assert!(
        s.iter()
            .all(|x| x.vendor == tokenscope::model::Vendor::Claude)
    );
}

#[test]
fn reset_dates_roll_into_next_year_and_accept_other_time_forms() {
    let now = local(2026, 12, 30, 22, 0);
    let t = "Current week (all models): 80% used · resets Jan 2, 2am (Europe/Paris)\n\
             Current session: 5.5% used · resets 1:15pm (Asia/Ho_Chi_Minh)\n\
             Current week (Opus): 0% used · resets Jan 2 (UTC)";
    let s = claude_usage::parse_usage(t, now, "a");
    assert_eq!(
        s[0].resets_at,
        Some(local(2027, 1, 2, 2, 0)),
        "next January"
    );
    assert_eq!(s[1].used_percent, 5.5);
    assert_eq!(
        s[1].resets_at,
        Some(local(2026, 12, 31, 13, 15)),
        "time only, already past today -> tomorrow"
    );
    assert_eq!(s[2].scope, "weekly:Opus");
    assert_eq!(
        s[2].resets_at,
        Some(local(2027, 1, 2, 0, 0)),
        "date only -> midnight"
    );
}

#[test]
fn ignores_unrelated_lines_and_keeps_a_reading_without_reset() {
    let s = claude_usage::parse_usage(
        "Current session: 12% used\nCurrent mood: great\n  78%  Start fresh · run /clear\nCurrent week (all models): x% used · resets Nov 13",
        0,
        "a",
    );
    assert_eq!(s.len(), 1);
    assert_eq!(
        (s[0].scope.as_str(), s[0].used_percent, s[0].resets_at),
        ("five_hour", 12.0, None)
    );
}

fn fake_claude(dir: &Path, body: &str) -> std::path::PathBuf {
    let p = dir.join("claude");
    std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    p
}

#[test]
fn refresh_runs_without_saving_a_session_and_throttles() {
    let h = tempfile::tempdir().unwrap();
    let paths = Paths {
        home: h.path().to_path_buf(),
    };
    let mut conn = db::open(&paths.db()).unwrap();
    let cfg = Config {
        claude_account: "work".into(),
        ..Config::default()
    };
    let fx = h.path().join("out.txt");
    std::fs::write(&fx, FIXTURE).unwrap();
    // record args and cwd, then print the fixture
    let bin = fake_claude(
        h.path(),
        &format!(
            "echo \"$@\" > {0}/args; pwd > {0}/cwd; cat {1}",
            h.path().display(),
            fx.display()
        ),
    );
    let now = local(2026, 11, 6, 11, 0);
    assert_eq!(
        claude_usage::refresh_with(&paths, &cfg, &mut conn, &bin, now).unwrap(),
        RefreshOutcome::Ran
    );
    let args = std::fs::read_to_string(h.path().join("args")).unwrap();
    assert_eq!(args.trim(), "-p --no-session-persistence /usage");
    let cwd = std::fs::read_to_string(h.path().join("cwd")).unwrap();
    assert_eq!(
        Path::new(cwd.trim()),
        paths.data_dir().canonicalize().unwrap(),
        "runs inside ~/.tokenscope"
    );
    let n: i64 = conn
        .query_row("SELECT count(*) FROM limit_snapshots WHERE vendor='claude' AND account='work' AND source='claude /usage'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 3);
    assert_eq!(
        claude_usage::refresh_with(&paths, &cfg, &mut conn, &bin, now + 299).unwrap(),
        RefreshOutcome::Throttled {
            next_allowed: now + 300
        }
    );
    assert_eq!(
        claude_usage::refresh_with(&paths, &cfg, &mut conn, &bin, now + 300).unwrap(),
        RefreshOutcome::Ran
    );
}

#[test]
fn refresh_reports_missing_binary_and_unusable_output() {
    let h = tempfile::tempdir().unwrap();
    let paths = Paths {
        home: h.path().to_path_buf(),
    };
    let mut conn = db::open(&paths.db()).unwrap();
    let cfg = Config::default();
    let out =
        claude_usage::refresh_with(&paths, &cfg, &mut conn, &h.path().join("nope/claude"), 1000)
            .unwrap();
    assert_eq!(out, RefreshOutcome::NotInstalled);
    let bin = fake_claude(h.path(), "echo 'Invalid API key · Please run /login'");
    let e = claude_usage::refresh_with(&paths, &cfg, &mut conn, &bin, 2000)
        .unwrap_err()
        .to_string();
    assert!(
        e.contains("no usage lines") && e.contains("Invalid API key"),
        "{e}"
    );
}
