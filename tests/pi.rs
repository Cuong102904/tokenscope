use std::path::Path;
use tokenscope::adapters::Adapter;
use tokenscope::adapters::pi::{self, PiAdapter};
use tokenscope::model::{Batch, Vendor};
use tokenscope::{config::Config, db, paths::Paths};

const FIXTURE: &str =
    "tests/fixtures/pi/sessions/--home-u-proj--/2026-11-06T05-00-00-000Z_a1b2c3.jsonl";

fn parse_fixture() -> Batch {
    let text = std::fs::read_to_string(FIXTURE).unwrap();
    let lines: Vec<serde_json::Value> = text
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    pi::parse_session(&lines)
}

#[test]
fn assistant_messages_and_usage_entries_become_usage_rows() {
    let b = parse_fixture();
    let got: Vec<_> = b
        .usage
        .iter()
        .map(|r| {
            (
                r.msg_key.as_str(),
                r.model.as_str(),
                r.input,
                r.output,
                r.reasoning,
                r.cache_read,
                r.cache_write_5m,
                r.cache_write_1h,
            )
        })
        .collect();
    assert_eq!(
        got,
        [
            // output already includes reasoning; cacheWrite1h is the 1-hour part of cacheWrite
            (
                "pi:a1b2c3:e4",
                "claude-sonnet-5-5",
                1200,
                800,
                300,
                30000,
                3000,
                2000
            ),
            ("pi:a1b2c3:e6", "gpt-6-astra", 900, 150, 0, 0, 0, 0),
            // the aborted, zero-usage reply is skipped; the cache-warming call is real spend
            ("pi:a1b2c3:e8", "claude-sonnet-5-5", 10, 1, 0, 31000, 0, 0),
        ]
    );
    let r = &b.usage[0];
    assert_eq!(
        (
            r.vendor,
            r.session_id.as_str(),
            r.project.as_str(),
            r.account.as_str()
        ),
        (Vendor::Pi, "a1b2c3", "/home/u/proj", "")
    );
    assert_eq!(
        r.ts,
        chrono::DateTime::parse_from_rfc3339("2026-11-06T05:00:05Z")
            .unwrap()
            .timestamp()
    );
}

#[test]
fn tool_calls_and_session_title() {
    let b = parse_fixture();
    let tools: Vec<_> = b
        .tools
        .iter()
        .map(|t| (t.key.as_str(), t.tool.as_str()))
        .collect();
    assert_eq!(tools, [("pi:call_1", "read"), ("pi:call_2", "bash")]);
    assert_eq!(b.sessions.len(), 1);
    let s = &b.sessions[0];
    assert_eq!(
        (
            s.session_id.as_str(),
            s.project.as_str(),
            s.title.as_deref()
        ),
        ("a1b2c3", "/home/u/proj", Some("Fix the login flow"))
    );
}

fn home_with_fixture() -> (tempfile::TempDir, Paths) {
    let h = tempfile::tempdir().unwrap();
    let agent = h.path().join(".pi/agent");
    let dir = agent.join("sessions/--home-u-proj--");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::copy(FIXTURE, dir.join("s.jsonl")).unwrap();
    // credentials next to the sessions must never be read
    std::fs::copy("tests/fixtures/pi/auth.json", agent.join("auth.json")).unwrap();
    let paths = Paths {
        home: h.path().to_path_buf(),
    };
    (h, paths)
}

#[test]
fn collect_reads_sessions_and_is_incremental() {
    let (_h, paths) = home_with_fixture();
    assert!(PiAdapter.discover(&paths).found);
    let mut conn = db::open(&paths.db()).unwrap();
    let cfg = Config::default();
    let run = |conn: &mut rusqlite::Connection| {
        tokenscope::collect::run_all(conn, &paths, &cfg, &[Box::new(PiAdapter)])
    };
    let st = run(&mut conn);
    assert!(st[0].error.is_none(), "{:?}", st[0].error);
    let n: i64 = conn
        .query_row("SELECT count(*) FROM usage WHERE vendor='pi'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(n, 3);
    let st = run(&mut conn);
    assert_eq!(st[0].records, 0, "unchanged files are not read again");
    let leaked: i64 = conn
        .query_row(
            "SELECT count(*) FROM usage WHERE model LIKE '%SECRET%' OR session_id LIKE '%SECRET%'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(leaked, 0);
}

#[test]
fn pi_credentials_are_on_the_never_read_list() {
    let (h, _paths) = home_with_fixture();
    let auth = h.path().join(".pi/agent/auth.json");
    assert!(auth.exists());
    assert!(tokenscope::safety::is_forbidden(&auth));
    assert!(tokenscope::safety::check_path(&auth).is_err());
}

#[test]
fn session_dir_follows_pi_env_vars() {
    let paths = Paths {
        home: Path::new("/h").to_path_buf(),
    };
    assert_eq!(
        pi::sessions_dir_from(&paths, None, None),
        Path::new("/h/.pi/agent/sessions")
    );
    assert_eq!(
        pi::sessions_dir_from(&paths, Some("/a"), None),
        Path::new("/a/sessions")
    );
    assert_eq!(
        pi::sessions_dir_from(&paths, Some("/a"), Some("/s")),
        Path::new("/s")
    );
    assert_eq!(
        pi::sessions_dir_from(&paths, Some("~/x"), None),
        Path::new("/h/x/sessions")
    );
}

#[test]
fn not_found_without_pi() {
    let h = tempfile::tempdir().unwrap();
    let paths = Paths {
        home: h.path().to_path_buf(),
    };
    assert!(!PiAdapter.discover(&paths).found);
}
