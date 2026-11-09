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

fn auth_json(email: &str) -> String {
    format!(
        r#"{{"loggedIn":true,"authMethod":"claude.ai","email":"{email}","orgName":"Acme","subscriptionType":"team"}}"#
    )
}

#[test]
fn parses_the_signed_in_identity() {
    let id = claude_usage::parse_auth_status(&auth_json("me@example.com")).unwrap();
    assert_eq!(
        (id.email.as_str(), id.org.as_deref(), id.plan.as_deref()),
        ("me@example.com", Some("Acme"), Some("team"))
    );
    assert_eq!(
        claude_usage::parse_auth_status(r#"{"loggedIn":false}"#),
        None
    );
    assert_eq!(
        claude_usage::parse_auth_status(r#"{"loggedIn":true,"authMethod":"api_key"}"#),
        None,
        "no email, nothing to name the account by"
    );
    assert_eq!(claude_usage::parse_auth_status("not json"), None);
    assert_eq!(claude_usage::parse_auth_status(&auth_json("  ")), None);
}

/// A fake `claude`: `auth status` answers `auth` (and counts calls), anything else prints the usage fixture.
fn identity_claude(dir: &Path, auth: &str) -> std::path::PathBuf {
    let fx = dir.join("out.txt");
    std::fs::write(&fx, FIXTURE).unwrap();
    let au = dir.join("auth.txt");
    std::fs::write(&au, auth).unwrap();
    fake_claude(
        dir,
        &format!(
            "case \"$1\" in auth) echo x >> {0}/auth_calls; cat {1};; *) cat {2};; esac",
            dir.display(),
            au.display(),
            fx.display()
        ),
    )
}

fn accounts(conn: &rusqlite::Connection) -> Vec<String> {
    let mut st = conn
        .prepare("SELECT DISTINCT account FROM limit_snapshots WHERE vendor='claude' ORDER BY 1")
        .unwrap();
    st.query_map([], |r| r.get(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect()
}

fn setup_home() -> (tempfile::TempDir, Paths, rusqlite::Connection) {
    let h = tempfile::tempdir().unwrap();
    let paths = Paths {
        home: h.path().to_path_buf(),
    };
    let conn = db::open(&paths.db()).unwrap();
    (h, paths, conn)
}

#[test]
fn readings_are_keyed_and_named_by_the_signed_in_account() {
    use tokenscope::model::sha8;
    let (h, paths, mut conn) = setup_home();
    let cfg = Config::default(); // claude_account = "default": not chosen by the user
    let bin = identity_claude(h.path(), &auth_json("me@example.com"));
    // readings from before the identity was known belong to the one account that existed
    conn.execute(
        "INSERT INTO limit_snapshots(vendor,account,scope,window_minutes,used_percent,resets_at,plan_type,measured_at,source) VALUES('claude','default','weekly',10080,5,NULL,NULL,10,'statusline')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO limit_events(vendor,account,ts,scope,resets_at) VALUES('claude','default',10,'weekly',20)",
        [],
    )
    .unwrap();
    let t0 = local(2026, 11, 6, 11, 0);
    claude_usage::refresh_with(&paths, &cfg, &mut conn, &bin, t0).unwrap();
    let key = sha8("me@example.com");
    assert_eq!(
        accounts(&conn),
        std::slice::from_ref(&key),
        "old default rows moved over"
    );
    let ev: String = conn
        .query_row("SELECT account FROM limit_events", [], |r| r.get(0))
        .unwrap();
    assert_eq!(ev, key);
    let label: String = conn
        .query_row(
            "SELECT value FROM kv WHERE key = ?1",
            [format!("label:claude:{key}")],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(label, "me@example.com", "named by its email, locally");
    // a name the user chose (the dashboard stores it in the same kv row) is never overwritten
    conn.execute(
        "UPDATE kv SET value = 'Work' WHERE key = ?1",
        [format!("label:claude:{key}")],
    )
    .unwrap();
    claude_usage::refresh_with(&paths, &cfg, &mut conn, &bin, t0 + 400).unwrap();
    let label: String = conn
        .query_row(
            "SELECT value FROM kv WHERE key = ?1",
            [format!("label:claude:{key}")],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(label, "Work");
}

#[test]
fn switching_claude_account_adds_a_row_and_keeps_the_old_one() {
    use tokenscope::model::sha8;
    let (h, paths, mut conn) = setup_home();
    let cfg = Config::default();
    let bin = identity_claude(h.path(), &auth_json("a@example.com"));
    let t0 = local(2026, 11, 6, 11, 0);
    claude_usage::refresh_with(&paths, &cfg, &mut conn, &bin, t0).unwrap();
    std::fs::write(h.path().join("auth.txt"), auth_json("b@example.com")).unwrap();
    claude_usage::refresh_with(&paths, &cfg, &mut conn, &bin, t0 + 400).unwrap();
    let mut want = vec![sha8("a@example.com"), sha8("b@example.com")];
    want.sort();
    assert_eq!(accounts(&conn), want);
    let name = |email: &str| -> String {
        conn.query_row(
            "SELECT value FROM kv WHERE key = ?1",
            [format!("label:claude:{}", sha8(email))],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert_eq!(
        (name("a@example.com"), name("b@example.com")),
        ("a@example.com".into(), "b@example.com".into())
    );
}

#[test]
fn an_account_chosen_in_config_is_left_alone() {
    let (h, paths, mut conn) = setup_home();
    let cfg = Config {
        claude_account: "work".into(),
        ..Config::default()
    };
    let bin = identity_claude(h.path(), &auth_json("me@example.com"));
    claude_usage::refresh_with(&paths, &cfg, &mut conn, &bin, 1000).unwrap();
    assert_eq!(accounts(&conn), ["work"]);
    assert!(
        !h.path().join("auth_calls").exists(),
        "no identity lookup when the user chose the account"
    );
}

#[test]
fn without_an_identity_the_old_key_is_used() {
    let (h, paths, mut conn) = setup_home();
    let cfg = Config::default();
    let bin = identity_claude(h.path(), r#"{"loggedIn":false}"#);
    assert_eq!(
        claude_usage::refresh_with(&paths, &cfg, &mut conn, &bin, 1000).unwrap(),
        RefreshOutcome::Ran
    );
    assert_eq!(accounts(&conn), ["default"]);
    let n: i64 = conn
        .query_row(
            "SELECT count(*) FROM kv WHERE key LIKE 'label:%'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 0);
}

#[test]
fn statusline_snapshots_follow_the_identified_account() {
    use tokenscope::adapters::{Adapter, claude_snapshots::ClaudeSnapshotsAdapter};
    use tokenscope::collect::run_all;
    use tokenscope::model::sha8;
    let (h, paths, mut conn) = setup_home();
    let cfg = Config::default();
    let bin = identity_claude(h.path(), &auth_json("me@example.com"));
    claude_usage::refresh_with(&paths, &cfg, &mut conn, &bin, 1000).unwrap();
    let dir = paths.snapshots_dir();
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("claude-x-202611061100.json"),
        r#"{"account":"default","measured_at":2000,"five_hour":{"used_percent":44,"resets_at":99999},"seven_day":null}"#,
    )
    .unwrap();
    let ads: Vec<Box<dyn Adapter>> = vec![Box::new(ClaudeSnapshotsAdapter)];
    let st = run_all(&mut conn, &paths, &cfg, &ads);
    assert!(st[0].error.is_none(), "{:?}", st[0].error);
    assert_eq!(accounts(&conn), [sha8("me@example.com")]);
}
