use agent_usage::adapters::{claude::ClaudeAdapter, codex::CodexAdapter};
use agent_usage::config::Config;
use agent_usage::query::active;
use agent_usage::server::{self, AppState};
use agent_usage::{collect, db, paths::Paths, pricing::PriceTable};
use axum::body::Body;
use axum::http::Request;
use http_body_util::BodyExt;
use rusqlite::{Connection, params};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};
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
    let list = active::list(&c, &prices, &cfg, NOW).unwrap();
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
    let s = &active::list(&c, &prices, &Config::default(), NOW).unwrap()[0];
    assert_eq!((s.cost_so_far, s.cost_last_hour), (None, None));
    assert!(s.priced_so_far > 0.0 && s.priced_last_hour > 0.0);
    assert_eq!(s.model.as_deref(), Some("no-such-model"));
}

#[test]
fn active_minutes_comes_from_config() {
    let (_d, c, prices, _p) = setup();
    session(&c, "claude", "s", "/p", NOW - 600);
    assert!(
        active::list(&c, &prices, &Config::default(), NOW)
            .unwrap()
            .is_empty()
    );
    let cfg = Config {
        active_minutes: 15,
        ..Config::default()
    };
    assert_eq!(active::list(&c, &prices, &cfg, NOW).unwrap().len(), 1);
}

#[test]
fn older_write_never_lowers_last_write() {
    let (_d, mut c, _pr, _p) = setup();
    use agent_usage::model::{Batch, SessionInfo, Vendor};
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
    assert_eq!(api(&st, "/api/overview").await["active_count"], 1);
}
