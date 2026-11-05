//! `agent-usage serve`: read-only JSON API + the embedded single-page dashboard,
//! bound to 127.0.0.1 only. A background thread re-collects every
//! `refresh_seconds`; every request opens its own read-only connection (WAL).

use crate::collect;
use crate::config::Config;
use crate::db;
use crate::paths::Paths;
use crate::pricing::PriceTable;
use crate::query::{Filter, cost, sessions};
use axum::extract::{Path, Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

const INDEX_HTML: &str = include_str!("../assets/web/index.html");
const APP_JS: &str = include_str!("../assets/web/app.js");
const CHART_JS: &str = include_str!("../assets/web/chart.umd.min.js");

/// Everything a request needs; prices and config are loaded once at startup.
#[derive(Clone)]
pub struct AppState {
    pub paths: Paths,
    pub prices: Arc<PriceTable>,
    pub cfg: Arc<Config>,
}

/// Always loopback: the dashboard has no auth, so it must never listen on other interfaces.
pub fn bind_addr(port: u16) -> String {
    format!("127.0.0.1:{port}")
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(|| async { asset("text/html; charset=utf-8", INDEX_HTML) }))
        .route("/app.js", get(|| async { asset("text/javascript; charset=utf-8", APP_JS) }))
        .route("/chart.js", get(|| async { asset("text/javascript; charset=utf-8", CHART_JS) }))
        .route("/api/overview", get(overview))
        .route("/api/cost", get(cost_api))
        .route("/api/sessions", get(sessions_api))
        .route("/api/session/{vendor}/{id}/timeline", get(timeline_api))
        .route("/api/meta", get(meta))
        .with_state(state)
}

fn asset(ctype: &'static str, body: &'static str) -> Response {
    ([(header::CONTENT_TYPE, ctype), (header::CACHE_CONTROL, "no-cache")], body).into_response()
}

/// Query string shared by every API route. Empty values mean "no filter";
/// `days` is a positive number or `all` (absent = all).
#[derive(Debug, Default, Deserialize)]
struct Params {
    days: Option<String>,
    vendor: Option<String>,
    model: Option<String>,
    project: Option<String>,
    limit: Option<usize>,
}

impl Params {
    fn filter(&self) -> Result<Filter, ApiError> {
        let nonempty = |v: &Option<String>| v.as_ref().filter(|s| !s.is_empty()).cloned();
        let days = match self.days.as_deref() {
            None | Some("") | Some("all") => None,
            Some(s) => match s.parse::<u32>() {
                Ok(n) if n > 0 => Some(n),
                _ => return Err(ApiError::BadRequest(format!("bad days: {s}"))),
            },
        };
        Ok(Filter { days, vendor: nonempty(&self.vendor), model: nonempty(&self.model), project: nonempty(&self.project) })
    }
}

enum ApiError {
    BadRequest(String),
    Internal(anyhow::Error),
}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        ApiError::Internal(e)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        match self {
            ApiError::BadRequest(m) => (StatusCode::BAD_REQUEST, m).into_response(),
            ApiError::Internal(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")).into_response(),
        }
    }
}

type ApiResult = Result<Json<serde_json::Value>, ApiError>;

/// Run `f` on the blocking pool with a fresh read-only connection; its result becomes the JSON body.
async fn with_db<T, F>(st: AppState, f: F) -> ApiResult
where
    T: Serialize,
    F: FnOnce(&Connection, &AppState) -> anyhow::Result<T> + Send + 'static,
{
    let out = tokio::task::spawn_blocking(move || -> anyhow::Result<serde_json::Value> {
        let conn = Connection::open_with_flags(st.paths.db(), OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)?;
        conn.busy_timeout(std::time::Duration::from_millis(5000))?;
        Ok(serde_json::to_value(f(&conn, &st)?)?)
    })
    .await
    .map_err(|e| ApiError::Internal(anyhow::anyhow!("worker: {e}")))??;
    Ok(Json(out))
}

/// Fixed windows (today / 7 / 30 days); vendor, model and project filters still apply.
/// `limits_mini` and `active_count` are placeholders until the limits/active tasks.
async fn overview(State(st): State<AppState>, Query(p): Query<Params>) -> ApiResult {
    let f = p.filter()?;
    with_db(st, move |c, st| {
        let w = |days| Filter { days: Some(days), ..f.clone() };
        Ok(serde_json::json!({
            "totals_today": cost::totals(c, &st.prices, &w(1))?,
            "totals_7d": cost::totals(c, &st.prices, &w(7))?,
            "totals_30d": cost::totals(c, &st.prices, &w(30))?,
            "active_count": 0,
            "daily": cost::daily(c, &st.prices, &w(30))?,
            "limits_mini": [],
        }))
    })
    .await
}

async fn cost_api(State(st): State<AppState>, Query(p): Query<Params>) -> ApiResult {
    let f = p.filter()?;
    with_db(st, move |c, st| {
        let pr = &st.prices;
        Ok(serde_json::json!({
            "totals": cost::totals(c, pr, &f)?,
            "daily": cost::daily(c, pr, &f)?,
            "by_model": cost::by_model(c, pr, &f)?,
            "by_project": cost::by_project(c, pr, &f)?,
            "by_vendor": cost::by_vendor(c, pr, &f)?,
            "efficiency": cost::efficiency(c, pr, &f)?,
        }))
    })
    .await
}

async fn sessions_api(State(st): State<AppState>, Query(p): Query<Params>) -> ApiResult {
    let f = p.filter()?;
    let limit = p.limit.unwrap_or(200).min(5000);
    with_db(st, move |c, st| sessions::list(c, &st.prices, &st.cfg, &f, limit)).await
}

async fn timeline_api(State(st): State<AppState>, Path((vendor, id)): Path<(String, String)>) -> ApiResult {
    with_db(st, move |c, st| sessions::timeline_detail(c, &st.prices, &vendor, &id)).await
}

#[derive(Serialize)]
struct Meta {
    vendors: Vec<String>,
    models: Vec<String>,
    projects: Vec<String>,
}

/// Distinct values over all data, not narrowed by the current filter (so a selection can always be undone).
async fn meta(State(st): State<AppState>) -> ApiResult {
    with_db(st, |c, _| {
        let distinct = |col: &str| -> anyhow::Result<Vec<String>> {
            let mut s = c.prepare(&format!(
                "SELECT DISTINCT {col} FROM usage WHERE {col} IS NOT NULL AND {col} <> '' ORDER BY {col}"
            ))?;
            let v = s.query_map([], |r| r.get::<_, String>(0))?.collect::<Result<_, _>>()?;
            Ok(v)
        };
        Ok(Meta { vendors: distinct("vendor")?, models: distinct("model")?, projects: distinct("project")? })
    })
    .await
}

/// Blocking: collect now and then every `refresh_seconds` on a background
/// thread (its own connection), and serve until the process is killed.
pub fn serve(paths: Paths, port: u16) -> anyhow::Result<()> {
    let cfg = Config::load(&paths)?;
    let prices = PriceTable::load(&paths)?;
    // create/migrate the DB before any read-only request arrives
    drop(db::open(&paths.db())?);
    let every = std::time::Duration::from_secs(cfg.refresh_seconds.max(1));
    {
        let (paths, cfg) = (paths.clone(), cfg.clone());
        std::thread::spawn(move || {
            let adapters = collect::default_adapters();
            let mut conn = match db::open(&paths.db()) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("collector: cannot open database: {e:#}");
                    return;
                }
            };
            loop {
                for s in collect::run_all(&mut conn, &paths, &cfg, &adapters) {
                    if let Some(e) = s.error {
                        eprintln!("collector: {}: {e}", s.vendor.as_str());
                    }
                }
                std::thread::sleep(every);
            }
        });
    }
    let state = AppState { paths, prices: Arc::new(prices), cfg: Arc::new(cfg) };
    let addr = bind_addr(port);
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    rt.block_on(async move {
        let listener = tokio::net::TcpListener::bind(&addr).await?;
        println!("agent-usage: http://{addr}");
        axum::serve(listener, router(state)).await?;
        anyhow::Ok(())
    })
}
