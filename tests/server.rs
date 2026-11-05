use agent_usage::config::Config;
use agent_usage::server::{self, AppState};
use agent_usage::{db, paths::Paths, pricing::PriceTable};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use chrono::{Duration, Local};
use http_body_util::BodyExt;
use serde_json::Value;
use std::sync::Arc;
use tower::ServiceExt;

fn day(ago: i64) -> String {
    (Local::now().date_naive() - Duration::days(ago)).format("%Y-%m-%d").to_string()
}

/// Temp HOME with a DB holding: claude s1 (two priced rows, today), codex c1 (one
/// unpriced row, 3 days ago).
fn setup() -> (tempfile::TempDir, AppState) {
    let d = tempfile::tempdir().unwrap();
    let paths = Paths { home: d.path().to_path_buf() };
    let c = db::open(&paths.db()).unwrap();
    let rows = [
        ("a", "claude", "s1", "/home/u/proj", 100, day(0), "claude-fable-5-1", 1000, 2000),
        ("b", "claude", "s1", "/home/u/proj", 200, day(0), "claude-fable-5-1", 1000, 2000),
        ("c", "codex", "c1", "/home/u/other", 300, day(3), "no-such-model", 5, 6),
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
    let st = AppState { paths, prices: Arc::new(prices), cfg: Arc::new(Config::default()) };
    (d, st)
}

async fn get(st: &AppState, uri: &str) -> (StatusCode, String) {
    let res = server::router(st.clone()).oneshot(Request::get(uri).body(Body::empty()).unwrap()).await.unwrap();
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
    for k in ["totals", "daily", "by_model", "by_project", "by_vendor", "efficiency"] {
        assert!(v.get(k).is_some(), "missing {k}: {v}");
    }
    assert_eq!(v["daily"].as_array().unwrap().len(), 30);
    assert_eq!(v["totals"]["messages"], 3);
    assert_eq!(v["totals"]["unpriced_models"][0], "no-such-model");
    // (2 * (1000*10 + 2000*50)) / 1e6
    assert!((v["totals"]["cost"].as_f64().unwrap() - 0.22).abs() < 1e-9);
    // unpriced model: cost null, never 0
    let m = v["by_model"].as_array().unwrap().iter().find(|m| m["model"] == "no-such-model").unwrap().clone();
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
    let (s, _) = get(&st, "/api/cost?days=abc").await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn api_overview_shape() {
    let (_d, st) = setup();
    let v = json(&st, "/api/overview").await;
    assert_eq!(v["totals_today"]["messages"], 2);
    assert_eq!(v["totals_7d"]["messages"], 3);
    assert_eq!(v["totals_30d"]["messages"], 3);
    assert_eq!(v["active_count"], 0);
    assert_eq!(v["limits_mini"], serde_json::json!([]));
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
    assert_eq!(v["models"], serde_json::json!(["claude-fable-5-1", "no-such-model"]));
    assert_eq!(v["projects"], serde_json::json!(["/home/u/other", "/home/u/proj"]));
}

#[tokio::test]
async fn unknown_path_404() {
    let (_d, st) = setup();
    for p in ["/nope", "/api/nope", "/index.html.bak", "/api/session/claude/s1"] {
        assert_eq!(get(&st, p).await.0, StatusCode::NOT_FOUND, "{p}");
    }
}

#[tokio::test]
async fn serves_index_without_cdn() {
    let (_d, st) = setup();
    let (s, body) = get(&st, "/").await;
    assert_eq!(s, StatusCode::OK);
    assert!(body.contains("Ước tính theo giá API, không phải hoá đơn."));
    let srcs: Vec<&str> = body.split("<script").skip(1).map(|t| t.split('>').next().unwrap()).collect();
    assert!(srcs.len() >= 2, "{srcs:?}");
    for s in &srcs {
        assert!(!s.contains("http") && !s.contains("cdnjs"), "remote script: {s}");
    }
    assert!(!body.contains("cdnjs"));
    let (s, js) = get(&st, "/chart.js").await;
    assert_eq!(s, StatusCode::OK);
    assert!(js.len() > 100_000);
    let res = server::router(st.clone()).oneshot(Request::get("/app.js").body(Body::empty()).unwrap()).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert!(res.headers()["content-type"].to_str().unwrap().contains("javascript"));
}

#[test]
fn binds_loopback_only() {
    assert_eq!(server::bind_addr(8765), "127.0.0.1:8765");
    assert_eq!(server::bind_addr(8799), "127.0.0.1:8799");
}
