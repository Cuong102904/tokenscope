use axum::body::Body;
use axum::http::{Request, StatusCode};
use chrono::{Duration, Local};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use std::sync::Arc;
use tokenscope::config::Config;
use tokenscope::server::{self, AppState};
use tokenscope::{db, paths::Paths, pricing::PriceTable};
use tower::ServiceExt;

fn day(ago: i64) -> String {
    (Local::now().date_naive() - Duration::days(ago))
        .format("%Y-%m-%d")
        .to_string()
}

/// Temp HOME with a DB holding: claude s1 (two priced rows, today), codex c1 (one
/// unpriced row, 3 days ago).
fn setup() -> (tempfile::TempDir, AppState) {
    let d = tempfile::tempdir().unwrap();
    let paths = Paths {
        home: d.path().to_path_buf(),
    };
    let c = db::open(&paths.db()).unwrap();
    let rows = [
        (
            "a",
            "claude",
            "s1",
            "/home/u/proj",
            100,
            day(0),
            "claude-fable-5-1",
            1000,
            2000,
        ),
        (
            "b",
            "claude",
            "s1",
            "/home/u/proj",
            200,
            day(0),
            "claude-fable-5-1",
            1000,
            2000,
        ),
        (
            "c",
            "codex",
            "c1",
            "/home/u/other",
            300,
            day(3),
            "no-such-model",
            5,
            6,
        ),
    ];
    for (k, v, s, p, ts, dy, m, i, o) in rows {
        c.execute(
            "INSERT INTO usage(msg_key,vendor,account,session_id,project,ts,day,model,input,output,reasoning,
               cache_read,cache_write_5m,cache_write_1h,sidechain) VALUES(?1,?2,'',?3,?4,?5,?6,?7,?8,?9,0,0,0,0,0)",
            rusqlite::params![k, v, s, p, ts, dy, m, i, o],
        )
        .unwrap();
    }
    let prices = PriceTable::load(&paths).unwrap();
    let st = AppState {
        paths,
        prices: Arc::new(prices),
        cfg: Arc::new(Config::default()),
        port: 8765,
    };
    (d, st)
}

async fn get(st: &AppState, uri: &str) -> (StatusCode, String) {
    get_host(st, uri, Some("127.0.0.1:8765")).await
}

async fn get_host(st: &AppState, uri: &str, host: Option<&str>) -> (StatusCode, String) {
    let mut req = Request::get(uri);
    if let Some(h) = host {
        req = req.header("host", h);
    }
    let res = server::router(st.clone())
        .oneshot(req.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let body = res.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&body).into_owned())
}

async fn json(st: &AppState, uri: &str) -> Value {
    let (s, b) = get(st, uri).await;
    assert_eq!(s, StatusCode::OK, "{uri}: {b}");
    serde_json::from_str(&b).unwrap()
}

#[tokio::test]
async fn api_cost_shape() {
    let (_d, st) = setup();
    let v = json(&st, "/api/cost?days=30").await;
    for k in [
        "totals",
        "daily",
        "by_model",
        "by_project",
        "by_vendor",
        "efficiency",
    ] {
        assert!(v.get(k).is_some(), "missing {k}: {v}");
    }
    assert_eq!(v["daily"].as_array().unwrap().len(), 30);
    assert_eq!(v["totals"]["messages"], 3);
    assert_eq!(v["totals"]["unpriced_models"][0], "no-such-model");
    // (2 * (1000*10 + 2000*50)) / 1e6
    assert!((v["totals"]["cost"].as_f64().unwrap() - 0.22).abs() < 1e-9);
    // unpriced model: cost null, never 0
    let m = v["by_model"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["model"] == "no-such-model")
        .unwrap()
        .clone();
    assert!(m["cost"].is_null());
}

#[tokio::test]
async fn filters_from_query_string() {
    let (_d, st) = setup();
    let v = json(&st, "/api/cost?days=1").await;
    assert_eq!(v["totals"]["messages"], 2);
    let v = json(&st, "/api/cost?days=all&vendor=codex").await;
    assert_eq!(v["totals"]["messages"], 1);
    let v = json(&st, "/api/cost?project=%2Fhome%2Fu%2Fproj&model=").await;
    assert_eq!(v["totals"]["messages"], 2);
    for bad in ["abc", "0", "-1", "3651", "4294967295", "99999999999"] {
        let (s, _) = get(&st, &format!("/api/cost?days={bad}")).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "days={bad}");
    }
    assert_eq!(get(&st, "/api/cost?days=3650").await.0, StatusCode::OK);
    assert_eq!(get(&st, "/api/overview?days=3650").await.0, StatusCode::OK);
}

#[tokio::test]
async fn rejects_foreign_host_header() {
    let (_d, st) = setup();
    for ok in ["127.0.0.1:8765", "localhost:8765"] {
        assert_eq!(
            get_host(&st, "/api/meta", Some(ok)).await.0,
            StatusCode::OK,
            "{ok}"
        );
    }
    for bad in [
        Some("evil.example:8765"),
        Some("127.0.0.1:9999"),
        Some("localhost"),
        Some("127.0.0.1"),
        Some("127.0.0.1.evil:8765"),
        None,
    ] {
        assert_eq!(
            get_host(&st, "/api/meta", bad).await.0,
            StatusCode::FORBIDDEN,
            "{bad:?}"
        );
        assert_eq!(
            get_host(&st, "/", bad).await.0,
            StatusCode::FORBIDDEN,
            "{bad:?}"
        );
    }
}

#[tokio::test]
async fn api_overview_shape() {
    let (_d, st) = setup();
    let v = json(&st, "/api/overview").await;
    assert_eq!(v["totals_today"]["messages"], 2);
    assert_eq!(v["totals_7d"]["messages"], 3);
    assert_eq!(v["totals_30d"]["messages"], 3);
    assert_eq!(
        (&v["active_open"], &v["active_working"]),
        (&json!(0), &json!(0))
    );
    // no limit data: only the Claude "no data" placeholder card
    assert_eq!(v["limits_mini"].as_array().unwrap().len(), 1);
    assert_eq!(v["limits_mini"][0]["no_data"], true);
    assert_eq!(v["daily"].as_array().unwrap().len(), 30);
    // vendor filter still applies to the fixed windows
    let v = json(&st, "/api/overview?vendor=codex").await;
    assert_eq!(v["totals_7d"]["messages"], 1);
}

#[tokio::test]
async fn api_sessions_and_timeline() {
    let (_d, st) = setup();
    let v = json(&st, "/api/sessions?limit=1").await;
    let a = v.as_array().unwrap();
    assert_eq!(a.len(), 1);
    assert_eq!(a[0]["session_id"], "s1");
    let v = json(&st, "/api/sessions?vendor=codex").await;
    assert!(v[0]["cost"].is_null());
    let v = json(&st, "/api/session/claude/s1/timeline").await;
    let pts = v["points"].as_array().unwrap();
    assert_eq!(pts.len(), 2);
    assert_eq!(pts[1][0], 200);
    assert!((pts[1][1].as_f64().unwrap() - 0.22).abs() < 1e-9);
    let v = json(&st, "/api/session/codex/c1/timeline").await;
    assert_eq!(v["unpriced_models"][0], "no-such-model");
}

#[tokio::test]
async fn api_meta_lists_filter_values() {
    let (_d, st) = setup();
    let v = json(&st, "/api/meta").await;
    assert_eq!(v["vendors"], serde_json::json!(["claude", "codex"]));
    assert_eq!(
        v["models"],
        serde_json::json!(["claude-fable-5-1", "no-such-model"])
    );
    assert_eq!(
        v["projects"],
        serde_json::json!(["/home/u/other", "/home/u/proj"])
    );
}

#[tokio::test]
async fn unknown_path_404() {
    let (_d, st) = setup();
    for p in [
        "/nope",
        "/api/nope",
        "/index.html.bak",
        "/api/session/claude/s1",
    ] {
        assert_eq!(get(&st, p).await.0, StatusCode::NOT_FOUND, "{p}");
    }
}

#[tokio::test]
async fn serves_index_without_cdn() {
    let (_d, st) = setup();
    let (s, body) = get(&st, "/").await;
    assert_eq!(s, StatusCode::OK);
    assert!(body.contains("Estimates at API list prices, not your bill."));
    let srcs: Vec<&str> = body
        .split("<script")
        .skip(1)
        .map(|t| t.split('>').next().unwrap())
        .collect();
    assert!(srcs.len() >= 2, "{srcs:?}");
    for s in &srcs {
        assert!(
            !s.contains("http") && !s.contains("cdnjs"),
            "remote script: {s}"
        );
    }
    assert!(!body.contains("cdnjs"));
    let (s, js) = get(&st, "/chart.js").await;
    assert_eq!(s, StatusCode::OK);
    assert!(js.len() > 100_000);
    let req = Request::get("/app.js")
        .header("host", "localhost:8765")
        .body(Body::empty())
        .unwrap();
    let res = server::router(st.clone()).oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert!(
        res.headers()["content-type"]
            .to_str()
            .unwrap()
            .contains("javascript")
    );
}

#[test]
fn binds_loopback_only() {
    assert_eq!(server::bind_addr(8765), "127.0.0.1:8765");
    assert_eq!(server::bind_addr(8799), "127.0.0.1:8799");
}

#[test]
fn collector_retries_failed_open() {
    let d = tempfile::tempdir().unwrap();
    let paths = Paths {
        home: d.path().to_path_buf(),
    };
    // ~/.tokenscope is a plain file: the DB cannot be created
    std::fs::write(paths.data_dir(), "x").unwrap();
    let mut c = tokenscope::collect::Collector::new(paths.clone(), Config::default());
    assert!(!c.tick().ok());
    assert!(!c.tick().ok());
    assert_eq!(c.take_log().len(), 1, "open failure logged once per streak");
    std::fs::remove_file(paths.data_dir()).unwrap();
    assert!(
        c.tick().ok(),
        "collector must recover once the DB can be opened"
    );
    assert!(paths.db().is_file());
}

async fn get_with(st: &AppState, uri: &str, headers: &[(&str, &str)]) -> (StatusCode, String) {
    let mut req = Request::get(uri).header("host", "127.0.0.1:8765");
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    let res = server::router(st.clone())
        .oneshot(req.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let body = res.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&body).into_owned())
}

fn seed_limits(st: &AppState) -> i64 {
    let now = chrono::Utc::now().timestamp();
    let c = db::open(&st.paths.db()).unwrap();
    for (i, used) in [10.0, 20.0, 30.0].iter().enumerate() {
        c.execute(
            "INSERT INTO limit_snapshots(vendor,account,scope,window_minutes,used_percent,resets_at,plan_type,measured_at,source)
             VALUES('claude','a','five_hour',300,?1,?2,NULL,?3,'statusline')",
            rusqlite::params![used, now + 4 * 3600, now - 1200 + 600 * i as i64],
        )
        .unwrap();
    }
    c.execute(
        "INSERT INTO limit_snapshots(vendor,account,scope,window_minutes,used_percent,resets_at,plan_type,measured_at,source)
         VALUES('codex','','weekly',10080,50,?1,'plus',?2,'codex-log')",
        rusqlite::params![now - 3600, now - 3 * 86400],
    )
    .unwrap();
    c.execute("INSERT INTO limit_events(vendor,account,ts,scope,resets_at) VALUES('codex','',?1,'primary',?2)", rusqlite::params![now - 86400, now]).unwrap();
    now
}

#[tokio::test]
async fn api_limits_shape() {
    let (_d, st) = setup();
    seed_limits(&st);
    let v = json(&st, "/api/limits").await;
    let cards = v["cards"].as_array().unwrap();
    assert_eq!(cards.len(), 2, "{v}");
    assert_eq!(cards[0]["vendor"], "claude");
    assert_eq!(cards[0]["used_percent"], 30.0);
    assert_eq!(cards[0]["stale"], false);
    assert!(cards[0]["eta"].is_i64(), "{}", cards[0]);
    assert_eq!(cards[1]["expired"], true);
    assert_eq!(cards[1]["stale"], true);
    assert_eq!(cards[1]["account_label"], "old logs without an account ID");
    assert_eq!(v["events"].as_array().unwrap().len(), 1);
    assert!(v.get("refresh").is_none());
    let o = json(&st, "/api/overview").await;
    assert_eq!(o["limits_mini"], v["cards"]);
}

#[tokio::test]
async fn api_limits_history() {
    let (_d, st) = setup();
    let now = seed_limits(&st);
    let v = json(
        &st,
        "/api/limits/history?vendor=claude&account=a&scope=five_hour&hours=24",
    )
    .await;
    assert_eq!(v["points"].as_array().unwrap().len(), 3);
    assert_eq!(v["points"][2], serde_json::json!([now, 30.0]));
    assert_eq!(v["resets"], serde_json::json!([now + 4 * 3600]));
    // empty account is a valid key (old Codex logs)
    let v = json(
        &st,
        "/api/limits/history?vendor=codex&account=&scope=weekly&hours=168",
    )
    .await;
    assert_eq!(v["points"].as_array().unwrap().len(), 1);
    for bad in ["hours=0", "hours=2161", "hours=x", ""] {
        let (s, _) = get(
            &st,
            &format!("/api/limits/history?vendor=claude&account=a&{bad}"),
        )
        .await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{bad}");
    }
}

#[tokio::test]
async fn api_limits_refresh_agy() {
    let (_d, st) = setup();
    seed_limits(&st);
    // a recent refresh stamp: the real `agy` (if installed) must never run in tests
    let now = chrono::Utc::now().timestamp();
    db::open(&st.paths.db())
        .unwrap()
        .execute(
            "INSERT OR REPLACE INTO kv(key,value) VALUES('agy:last_refresh',?1)",
            [now.to_string()],
        )
        .unwrap();
    // without the same-origin header (e.g. an <img> on another site): refused
    let (s, _) = get(&st, "/api/limits?refresh=agy").await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, b) = get_with(&st, "/api/limits?refresh=agy", &[("x-tokenscope", "1")]).await;
    assert_eq!(s, StatusCode::OK, "{b}");
    let v: Value = serde_json::from_str(&b).unwrap();
    let status = v["refresh"]["status"].as_str().unwrap();
    assert!(["throttled", "not_installed"].contains(&status), "{v}");
    if status == "throttled" {
        assert_eq!(v["refresh"]["next_allowed"], now + 300);
    }
    assert_eq!(v["cards"].as_array().unwrap().len(), 2);
    let (s, _) = get_with(&st, "/api/limits?refresh=other", &[("x-tokenscope", "1")]).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
}

/// Both setup sessions written just now, so both are "active".
fn seed_active(st: &AppState) {
    let now = chrono::Utc::now().timestamp();
    let c = db::open(&st.paths.db()).unwrap();
    for (v, s, p) in [
        ("claude", "s1", "/home/u/proj"),
        ("codex", "c1", "/home/u/other"),
    ] {
        c.execute(
            "INSERT INTO sessions(vendor,session_id,project,title,source_path,last_write) VALUES(?1,?2,?3,NULL,'x',?4)",
            rusqlite::params![v, s, p, now],
        )
        .unwrap();
    }
}

fn vendors_of(v: &Value) -> Vec<String> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|x| x["vendor"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn active_follows_filters_but_sources_do_not() {
    let (_d, st) = setup();
    seed_active(&st);
    let all = json(&st, "/api/active").await;
    assert_eq!(vendors_of(&all["sessions"]).len(), 2, "{all}");
    let v = json(&st, "/api/active?vendor=codex").await;
    assert_eq!(vendors_of(&v["sessions"]), ["codex"]);
    assert_eq!(
        v["sources"], all["sources"],
        "data sources are not filtered"
    );
    let p = json(&st, "/api/active?project=/home/u/proj").await;
    assert_eq!(vendors_of(&p["sessions"]), ["claude"]);
    let m = json(&st, "/api/active?model=claude-fable-5-1").await;
    assert_eq!(vendors_of(&m["sessions"]), ["claude"]);
    let o = json(&st, "/api/overview?vendor=codex").await;
    assert_eq!(
        (&o["active_open"], &o["active_working"]),
        (&json!(1), &json!(1))
    );
}

#[tokio::test]
async fn limits_follow_the_vendor_filter() {
    let (_d, st) = setup();
    seed_limits(&st);
    let v = json(&st, "/api/limits?vendor=codex").await;
    assert_eq!(vendors_of(&v["cards"]), ["codex"]);
    assert_eq!(v["events"].as_array().unwrap().len(), 1);
    let c = json(&st, "/api/limits?vendor=claude").await;
    assert_eq!(vendors_of(&c["cards"]), ["claude"]);
    assert_eq!(c["events"].as_array().unwrap().len(), 0);
    let o = json(&st, "/api/overview?vendor=codex").await;
    assert_eq!(vendors_of(&o["limits_mini"]), ["codex"]);
}

#[tokio::test]
async fn meta_maps_models_to_vendors() {
    let (_d, st) = setup();
    let v = json(&st, "/api/meta").await;
    assert_eq!(
        v["model_vendors"],
        serde_json::json!({"claude-fable-5-1": ["claude"], "no-such-model": ["codex"]})
    );
}

#[tokio::test]
async fn api_limits_refresh_claude() {
    let (_d, st) = setup();
    // a recent refresh stamp: the real `claude` must never run in tests
    let now = chrono::Utc::now().timestamp();
    db::open(&st.paths.db())
        .unwrap()
        .execute(
            "INSERT OR REPLACE INTO kv(key,value) VALUES('claude:last_refresh',?1)",
            [now.to_string()],
        )
        .unwrap();
    let (s, _) = get(&st, "/api/limits?refresh=claude").await;
    assert_eq!(s, StatusCode::FORBIDDEN, "same-origin header required");
    let (s, b) = get_with(&st, "/api/limits?refresh=claude", &[("x-tokenscope", "1")]).await;
    assert_eq!(s, StatusCode::OK, "{b}");
    let v: Value = serde_json::from_str(&b).unwrap();
    let status = v["refresh"]["status"].as_str().unwrap();
    assert!(["throttled", "not_installed"].contains(&status), "{v}");
    if status == "throttled" {
        assert_eq!(v["refresh"]["next_allowed"], now + 300);
    }
}

#[tokio::test]
async fn api_limits_refresh_codex() {
    let (_d, st) = setup();
    // a recent refresh stamp: the real `codex` must never run in tests
    let now = chrono::Utc::now().timestamp();
    db::open(&st.paths.db())
        .unwrap()
        .execute(
            "INSERT OR REPLACE INTO kv(key,value) VALUES('codex:last_refresh',?1)",
            [now.to_string()],
        )
        .unwrap();
    let (s, _) = get(&st, "/api/limits?refresh=codex").await;
    assert_eq!(s, StatusCode::FORBIDDEN, "same-origin header required");
    let (s, b) = get_with(&st, "/api/limits?refresh=codex", &[("x-tokenscope", "1")]).await;
    assert_eq!(s, StatusCode::OK, "{b}");
    let v: Value = serde_json::from_str(&b).unwrap();
    let status = v["refresh"]["status"].as_str().unwrap();
    assert!(["throttled", "not_installed"].contains(&status), "{v}");
}
