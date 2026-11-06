use axum::body::Body;
use axum::http::Request;
use http_body_util::BodyExt;
use rusqlite::{Connection, params};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};
use tokenscope::adapters::{claude::ClaudeAdapter, codex::CodexAdapter};
use tokenscope::config::Config;
use tokenscope::model::Vendor;
use tokenscope::open_sessions::{OpenSession, OpenSessions};
use tokenscope::query::active;
use tokenscope::server::{self, AppState};
use tokenscope::{collect, db, paths::Paths, pricing::PriceTable};
use tower::ServiceExt;

const NOW: i64 = 1_800_000_000;

fn session(c: &Connection, vendor: &str, sid: &str, project: &str, last_write: i64) {
    c.execute(
        "INSERT INTO sessions(vendor,session_id,project,title,source_path,last_write) VALUES(?1,?2,?3,'T','/x',?4)",
        params![vendor, sid, project, last_write],
    )
    .unwrap();
}

fn usage(c: &Connection, key: &str, vendor: &str, sid: &str, ts: i64, model: &str, out: i64) {
    c.execute(
        "INSERT INTO usage(msg_key,vendor,account,session_id,project,ts,day,model,input,output,reasoning,
           cache_read,cache_write_5m,cache_write_1h,sidechain) VALUES(?1,?2,'',?3,'/p',?4,'2026-11-05',?5,0,?6,0,0,0,0,0)",
        params![key, vendor, sid, ts, model, out],
    )
    .unwrap();
}

fn setup() -> (tempfile::TempDir, Connection, PriceTable, Paths) {
    let d = tempfile::tempdir().unwrap();
    let paths = Paths {
        home: d.path().to_path_buf(),
    };
    let conn = db::open(&paths.db()).unwrap();
    let prices = PriceTable::load(&paths).unwrap();
    (d, conn, prices, paths)
}

#[test]
fn window_boundaries_and_costs() {
    let (_d, c, prices, _p) = setup();
    let cfg = Config::default(); // 5 minutes
    session(&c, "claude", "in", "/home/u/proj", NOW - 120);
    session(&c, "claude", "out", "/home/u/proj", NOW - 600);
    session(&c, "claude", "edge", "/home/u/proj", NOW - 300);
    session(&c, "claude", "edge2", "/home/u/proj", NOW - 301);
    // 'in': an old row (outside the last hour) and a recent one, different models
    usage(
        &c,
        "a",
        "claude",
        "in",
        NOW - 7200,
        "claude-fable-5-1",
        1_000_000,
    );
    usage(
        &c,
        "b",
        "claude",
        "in",
        NOW - 60,
        "claude-sonnet-5-5",
        1_000_000,
    );
    let list = active::list(&c, &prices, &cfg, &none(), NOW).unwrap();
    let ids: Vec<_> = list.iter().map(|s| s.session_id.as_str()).collect();
    assert_eq!(
        ids,
        vec!["in", "edge"],
        "sorted by last_write desc; 300 s is inside, 301 s is not"
    );
    let s = &list[0];
    assert_eq!(
        (s.vendor.as_str(), s.project.as_str(), s.last_write),
        ("claude", "/home/u/proj", NOW - 120)
    );
    assert_eq!(
        s.model.as_deref(),
        Some("claude-sonnet-5-5"),
        "most recent row's model"
    );
    let cost = s.cost_so_far.expect("priced");
    let last_hour = s.cost_last_hour.expect("priced");
    assert!(cost > last_hour && last_hour > 0.0, "{cost} {last_hour}");
    // no usage rows at all: still listed, nothing spent
    assert_eq!(
        (
            list[1].cost_so_far,
            list[1].cost_last_hour,
            list[1].model.clone()
        ),
        (Some(0.0), Some(0.0), None)
    );
}

#[test]
fn unpriced_row_gives_none_but_keeps_lower_bound() {
    let (_d, c, prices, _p) = setup();
    session(&c, "codex", "u", "/p", NOW - 10);
    usage(
        &c,
        "a",
        "codex",
        "u",
        NOW - 20,
        "claude-fable-5-1",
        1_000_000,
    );
    usage(&c, "b", "codex", "u", NOW - 10, "no-such-model", 5);
    let s = &active::list(&c, &prices, &Config::default(), &none(), NOW).unwrap()[0];
    assert_eq!((s.cost_so_far, s.cost_last_hour), (None, None));
    assert!(s.priced_so_far > 0.0 && s.priced_last_hour > 0.0);
    assert_eq!(s.model.as_deref(), Some("no-such-model"));
}

#[test]
fn active_minutes_comes_from_config() {
    let (_d, c, prices, _p) = setup();
    session(&c, "claude", "s", "/p", NOW - 600);
    assert!(
        active::list(&c, &prices, &Config::default(), &none(), NOW)
            .unwrap()
            .is_empty()
    );
    let cfg = Config {
        active_minutes: 15,
        ..Config::default()
    };
    assert_eq!(
        active::list(&c, &prices, &cfg, &none(), NOW).unwrap().len(),
        1
    );
}

#[test]
fn older_write_never_lowers_last_write() {
    let (_d, mut c, _pr, _p) = setup();
    use tokenscope::model::{Batch, SessionInfo, Vendor};
    let mk = |lw| SessionInfo {
        vendor: Vendor::Claude,
        session_id: "s".into(),
        project: "/p".into(),
        title: None,
        source_path: "/x".into(),
        last_write: lw,
    };
    for lw in [NOW, NOW - 1000] {
        db::write_batch(
            &mut c,
            &Batch {
                sessions: vec![mk(lw)],
                ..Default::default()
            },
        )
        .unwrap();
    }
    let lw: i64 = c
        .query_row("SELECT last_write FROM sessions", [], |r| r.get(0))
        .unwrap();
    assert_eq!(lw, NOW);
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let t = to.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_dir(&e.path(), &t)
        } else {
            std::fs::copy(e.path(), t).unwrap();
        }
    }
}

fn set_mtime(p: &Path, secs: i64) {
    let f = std::fs::File::options().write(true).open(p).unwrap();
    f.set_modified(UNIX_EPOCH + Duration::from_secs(secs as u64))
        .unwrap();
}

/// Claude/Codex write the file mtime in SECONDS (not ns), and a rewrite that
/// only moves the mtime-visible size/time refreshes it on the next collect.
#[test]
fn claude_and_codex_last_write_is_file_mtime_in_seconds() {
    let (d, mut c, _pr, paths) = setup();
    for h in ["claude-home", "codex-home"] {
        copy_dir(
            &Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures")
                .join(h),
            d.path(),
        );
    }
    let cl = d.path().join(".claude/projects/-home-u-proj/s1.jsonl");
    let sub = d
        .path()
        .join(".claude/projects/-home-u-proj/s1/subagents/agent-x.jsonl");
    let cx = d.path().join(".codex/sessions/2026/11/02/rollout-a.jsonl");
    let cfg = Config::default();
    let run = |c: &mut Connection| {
        collect::run_all(
            c,
            &paths,
            &cfg,
            &[Box::new(ClaudeAdapter), Box::new(CodexAdapter)],
        );
    };
    let lw = |c: &Connection, v: &str| -> Vec<i64> {
        let mut s = c
            .prepare("SELECT last_write FROM sessions WHERE vendor=?1")
            .unwrap();
        s.query_map([v], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect()
    };
    set_mtime(&cl, NOW - 100);
    set_mtime(&sub, NOW - 50);
    set_mtime(&cx, NOW - 70);
    run(&mut c);
    assert_eq!(
        lw(&c, "claude"),
        vec![NOW - 50],
        "one row per sessionId, latest of parent and subagent file"
    );
    assert_eq!(lw(&c, "codex"), vec![NOW - 70]);
    // only the (older) parent file changes again: last_write must not go backwards
    let mut text = std::fs::read_to_string(&cl).unwrap();
    text.push('\n');
    std::fs::write(&cl, text).unwrap();
    set_mtime(&cl, NOW - 20);
    run(&mut c);
    assert_eq!(lw(&c, "claude"), vec![NOW - 20]);
    set_mtime(&sub, NOW - 80); // touched but older than the stored value
    run(&mut c);
    assert_eq!(lw(&c, "claude"), vec![NOW - 20]);
}

async fn api(st: &AppState, uri: &str) -> serde_json::Value {
    let req = Request::get(uri)
        .header("host", "127.0.0.1:8765")
        .body(Body::empty())
        .unwrap();
    let res = server::router(st.clone()).oneshot(req).await.unwrap();
    assert_eq!(res.status(), 200);
    serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes()).unwrap()
}

#[tokio::test]
async fn api_active_and_overview_count() {
    let (_d, c, prices, paths) = setup();
    let now = chrono::Utc::now().timestamp();
    session(&c, "claude", "live", "/home/u/proj", now - 30);
    session(&c, "claude", "dead", "/home/u/proj", now - 3600);
    usage(
        &c,
        "a",
        "claude",
        "live",
        now - 30,
        "claude-fable-5-1",
        1000,
    );
    c.execute(
        "INSERT INTO kv(key,value) VALUES('status:claude', ?1)",
        [r#"{"vendor":"claude","found":true,"detail":"3 jsonl files","records":7,"error":null,"last_run":1}"#],
    )
    .unwrap();
    drop(c);
    let st = AppState {
        paths,
        prices: Arc::new(prices),
        cfg: Arc::new(Config::default()),
        port: 8765,
    };
    let v = api(&st, "/api/active").await;
    assert_eq!(v["sessions"].as_array().unwrap().len(), 1);
    assert_eq!(v["sessions"][0]["session_id"], "live");
    assert_eq!(v["active_minutes"], 5);
    let src = v["sources"].as_array().unwrap();
    assert_eq!(src.len(), 1);
    assert_eq!(
        (
            src[0]["vendor"].as_str(),
            src[0]["records"].as_u64(),
            src[0]["found"].as_bool()
        ),
        (Some("claude"), Some(7), Some(true))
    );
    // stored totals, not just the last run's new rows
    assert_eq!(src[0]["total_records"].as_i64(), Some(1));
    assert_eq!(src[0]["last_limit_at"], serde_json::Value::Null);
    let c = db::open(&st.paths.db()).unwrap();
    c.execute(
        "INSERT INTO limit_snapshots(vendor,account,scope,window_minutes,used_percent,resets_at,plan_type,measured_at,source) VALUES('claude','','five_hour',300,10,NULL,NULL,?1,'t'),('claude','','weekly',10080,10,NULL,NULL,?2,'t')",
        [now - 100, now - 50],
    )
    .unwrap();
    drop(c);
    let v = api(&st, "/api/active").await;
    assert_eq!(v["sources"][0]["last_limit_at"].as_i64(), Some(now - 50));
    assert_eq!(api(&st, "/api/overview").await["active_open"], 1);
}

fn none() -> OpenSessions {
    OpenSessions::default()
}

fn open(sid: &str, pid: u32, busy: bool, since: i64) -> OpenSession {
    OpenSession {
        vendor: Vendor::Claude,
        session_id: sid.into(),
        cwd: format!("/home/u/{sid}-cwd"),
        pid,
        busy,
        since: Some(since),
    }
}

#[test]
fn open_sessions_join_the_list_with_their_state() {
    let (_d, c, prices, _p) = setup();
    session(&c, "claude", "busy", "/home/u/a", NOW - 30);
    session(&c, "claude", "idle-old", "/home/u/b", NOW - 3600); // open but quiet for an hour
    session(&c, "claude", "closed", "/home/u/c", NOW - 60); // written lately, no process
    session(&c, "codex", "cx", "/home/u/d", NOW - 10); // no process info for codex
    let reg = OpenSessions {
        claude: Some(vec![
            open("busy", 11, true, NOW - 5),
            open("idle-old", 12, false, NOW - 900),
            open("fresh", 13, false, NOW - 20), // open, nothing logged yet
        ]),
    };
    let list = active::list(&c, &prices, &Config::default(), &reg, NOW).unwrap();
    let rows: Vec<_> = list
        .iter()
        .map(|s| (s.session_id.as_str(), s.state, s.pid, s.project.as_str()))
        .collect();
    assert_eq!(
        rows,
        vec![
            // working first (most recently written first), then idle
            ("cx", "working", None, "/home/u/d"),
            ("busy", "working", Some(11), "/home/u/a"),
            ("closed", "working", None, "/home/u/c"),
            ("fresh", "idle", Some(13), "/home/u/fresh-cwd"),
            ("idle-old", "idle", Some(12), "/home/u/b"),
        ]
    );
    let idle_old = list.iter().find(|s| s.session_id == "idle-old").unwrap();
    assert_eq!(
        (idle_old.state_since, idle_old.last_write),
        (Some(NOW - 900), NOW - 3600)
    );
    let fresh = list.iter().find(|s| s.session_id == "fresh").unwrap();
    assert_eq!(
        (fresh.last_write, fresh.model.clone(), fresh.cost_so_far),
        (NOW - 20, None, Some(0.0))
    );
    assert_eq!(active::counts(&list), (5, 3));
}

#[test]
fn without_a_registry_the_log_window_decides_as_before() {
    let (_d, c, prices, _p) = setup();
    session(&c, "claude", "recent", "/p", NOW - 30);
    session(&c, "claude", "old", "/p", NOW - 3600);
    let list = active::list(&c, &prices, &Config::default(), &none(), NOW).unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(
        (list[0].state, list[0].pid, list[0].state_since),
        ("working", None, None)
    );
}

#[tokio::test]
async fn api_counts_open_and_working_from_the_claude_registry() {
    let (_d, c, prices, paths) = setup();
    let now = chrono::Utc::now().timestamp();
    session(&c, "claude", "mine", "/home/u/proj", now - 7200);
    session(&c, "claude", "recent", "/home/u/proj", now - 30);
    drop(c);
    // this test process stands in for an open, idle `claude`
    let me = std::process::id();
    let stat = std::fs::read_to_string(format!("/proc/{me}/stat")).unwrap();
    let start = tokenscope::open_sessions::proc_start_time(&stat)
        .unwrap()
        .to_string();
    let reg = paths.claude_session_registry();
    std::fs::create_dir_all(&reg).unwrap();
    std::fs::write(
        reg.join(format!("{me}.json")),
        format!(r#"{{"pid":{me},"sessionId":"mine","cwd":"/home/u/proj","procStart":"{start}","status":"idle","statusUpdatedAt":{}}}"#, (now - 60) * 1000),
    )
    .unwrap();
    // a closed one: same shape, a pid that does not run
    std::fs::write(
        reg.join("4194000.json"),
        r#"{"pid":4194000,"sessionId":"gone","cwd":"/x","procStart":"1","status":"busy"}"#,
    )
    .unwrap();
    let st = AppState {
        paths,
        prices: Arc::new(prices),
        cfg: Arc::new(Config::default()),
        port: 8765,
    };
    let v = api(&st, "/api/active").await;
    let rows: Vec<_> = v["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| {
            (
                s["session_id"].as_str().unwrap(),
                s["state"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(rows, vec![("recent", "working"), ("mine", "idle")]);
    assert_eq!(v["sessions"][1]["pid"].as_u64(), Some(me as u64));
    let o = api(&st, "/api/overview").await;
    assert_eq!(
        (o["active_open"].as_u64(), o["active_working"].as_u64()),
        (Some(2), Some(1))
    );
    // filters apply to open sessions too
    let o = api(&st, "/api/overview?project=/elsewhere").await;
    assert_eq!(o["active_open"].as_u64(), Some(0));
}
