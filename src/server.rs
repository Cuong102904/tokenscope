//! `agent-usage serve`: read-only JSON API + the embedded single-page dashboard,
//! bound to 127.0.0.1 only. A background thread re-collects every
//! `refresh_seconds`; every request opens its own read-only connection (WAL).

use crate::collect;
use crate::config::Config;
use crate::db;
use crate::paths::Paths;
use crate::pricing::PriceTable;
use crate::query::{Filter, active, cost, limits, sessions, skills};
use axum::extract::{Path, Query, Request, State};
use axum::http::{StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use std::panic::{AssertUnwindSafe, catch_unwind};
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
    /// The listening port; only `Host: 127.0.0.1:<port>` / `localhost:<port>` is served.
    pub port: u16,
}

/// Always loopback: the dashboard has no auth, so it must never listen on other interfaces.
pub fn bind_addr(port: u16) -> String {
    format!("127.0.0.1:{port}")
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route(
            "/",
            get(|| async { asset("text/html; charset=utf-8", INDEX_HTML) }),
        )
        .route(
            "/app.js",
            get(|| async { asset("text/javascript; charset=utf-8", APP_JS) }),
        )
        .route(
            "/chart.js",
            get(|| async { asset("text/javascript; charset=utf-8", CHART_JS) }),
        )
        .route("/api/overview", get(overview))
        .route("/api/cost", get(cost_api))
        .route("/api/sessions", get(sessions_api))
        .route("/api/session/{vendor}/{id}/timeline", get(timeline_api))
        .route("/api/skills", get(skills_api))
        .route("/api/active", get(active_api))
        .route("/api/meta", get(meta))
        .route("/api/limits", get(limits_api))
        .route("/api/limits/history", get(limits_history_api))
        .layer(middleware::from_fn_with_state(state.clone(), host_guard))
        .with_state(state)
}

/// DNS-rebinding guard: a page on another origin that resolves to 127.0.0.1
/// still sends its own Host header, so only our exact loopback hosts are served.
async fn host_guard(State(st): State<AppState>, req: Request, next: Next) -> Response {
    let ok = req
        .headers()
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .is_some_and(|h| {
            h == format!("127.0.0.1:{}", st.port) || h == format!("localhost:{}", st.port)
        });
    if ok {
        next.run(req).await
    } else {
        (StatusCode::FORBIDDEN, "forbidden host").into_response()
    }
}

fn asset(ctype: &'static str, body: &'static str) -> Response {
    (
        [
            (header::CONTENT_TYPE, ctype),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        body,
    )
        .into_response()
}

/// Query string shared by every API route. Empty values mean "no filter";
/// `days` is 1..=[`MAX_DAYS`] or `all` (absent = all).
const MAX_DAYS: u32 = 3650;

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
                Ok(n) if (1..=MAX_DAYS).contains(&n) => Some(n),
                _ => return Err(ApiError::BadRequest(format!("bad days: {s}"))),
            },
        };
        Ok(Filter {
            days,
            vendor: nonempty(&self.vendor),
            model: nonempty(&self.model),
            project: nonempty(&self.project),
        })
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
            ApiError::Internal(e) => {
                (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")).into_response()
            }
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
        let conn = Connection::open_with_flags(
            st.paths.db(),
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        conn.busy_timeout(std::time::Duration::from_millis(5000))?;
        Ok(serde_json::to_value(f(&conn, &st)?)?)
    })
    .await
    .map_err(|e| ApiError::Internal(anyhow::anyhow!("worker: {e}")))??;
    Ok(Json(out))
}

/// Fixed windows (today / 7 / 30 days); vendor, model and project filters still apply.
/// `limits_mini` is the same card list as `/api/limits`; `active_count` counts `/api/active` (unfiltered).
async fn overview(State(st): State<AppState>, Query(p): Query<Params>) -> ApiResult {
    let f = p.filter()?;
    with_db(st, move |c, st| {
        let w = |days| Filter { days: Some(days), ..f.clone() };
        Ok(serde_json::json!({
            "totals_today": cost::totals(c, &st.prices, &w(1))?,
            "totals_7d": cost::totals(c, &st.prices, &w(7))?,
            "totals_30d": cost::totals(c, &st.prices, &w(30))?,
            "active_count": active::list(c, &st.prices, &st.cfg, chrono::Utc::now().timestamp())?.len(),
            "daily": cost::daily(c, &st.prices, &w(30))?,
            "limits_mini": limits::current(c, &st.cfg, chrono::Utc::now().timestamp())?,
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
    with_db(st, move |c, st| {
        sessions::list(c, &st.prices, &st.cfg, &f, limit)
    })
    .await
}

/// Cost by skill/plugin follows every filter; event counts follow days and vendor, and
/// model/project only through sessions that have a matching usage row.
async fn skills_api(State(st): State<AppState>, Query(p): Query<Params>) -> ApiResult {
    let f = p.filter()?;
    with_db(st, move |c, st| {
        Ok(serde_json::json!({
            "by_cost": skills::by_cost(c, &st.prices, &f)?,
            "invocations": skills::invocations(c, &f)?,
            "by_plugin": skills::by_plugin(c, &st.prices, &f)?,
            "tools": skills::tools(c, &f)?,
        }))
    })
    .await
}

/// `{ active_minutes, sessions, sources }`; not narrowed by the filters (it is "what runs now").
async fn active_api(State(st): State<AppState>) -> ApiResult {
    with_db(st, |c, st| {
        Ok(serde_json::json!({
            "active_minutes": st.cfg.active_minutes,
            "sessions": active::list(c, &st.prices, &st.cfg, chrono::Utc::now().timestamp())?,
            "sources": active::sources(c),
        }))
    })
    .await
}

async fn timeline_api(
    State(st): State<AppState>,
    Path((vendor, id)): Path<(String, String)>,
) -> ApiResult {
    with_db(st, move |c, st| {
        sessions::timeline_detail(c, &st.prices, &vendor, &id)
    })
    .await
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

/// Limit-reached events shown in the limits tab cover this many days.
const LIMIT_EVENT_DAYS: i64 = 30;
/// `refresh=agy` runs a process and writes the DB, so it needs this header:
/// a cross-site `<img>`/form cannot set it, and without CORS a script cannot either.
const REFRESH_HEADER: &str = "x-agent-usage";

#[derive(Debug, Default, Deserialize)]
struct LimitsParams {
    refresh: Option<String>,
}

/// `{ cards, events }`, plus `refresh: {status, next_allowed?, message?}` with
/// `?refresh=agy`. A failed refresh is reported there; the cards still load.
async fn limits_api(
    State(st): State<AppState>,
    headers: axum::http::HeaderMap,
    Query(p): Query<LimitsParams>,
) -> Response {
    let refresh = match p.refresh.as_deref() {
        None | Some("") => None,
        Some("agy") if !headers.contains_key(REFRESH_HEADER) => {
            return (
                StatusCode::FORBIDDEN,
                format!("refresh needs the {REFRESH_HEADER} header"),
            )
                .into_response();
        }
        Some("agy") => Some(run_agy_refresh(st.clone()).await),
        Some(other) => {
            return ApiError::BadRequest(format!("unknown refresh: {other}")).into_response();
        }
    };
    let res = with_db(st, move |c, st| {
        let now = chrono::Utc::now().timestamp();
        let mut v = serde_json::json!({
            "cards": limits::current(c, &st.cfg, now)?,
            "events": limits::events(c, &st.cfg, now - LIMIT_EVENT_DAYS * 86400)?,
        });
        if let Some(r) = refresh {
            v["refresh"] = r;
        }
        Ok(v)
    })
    .await;
    res.into_response()
}

/// `agy::refresh` on the blocking pool with its own writable connection; never fails the request.
async fn run_agy_refresh(st: AppState) -> serde_json::Value {
    use crate::adapters::agy::{self, RefreshOutcome};
    let out = tokio::task::spawn_blocking(move || -> anyhow::Result<RefreshOutcome> {
        let mut conn = db::open(&st.paths.db())?;
        agy::refresh(&st.paths, &st.cfg, &mut conn)
    })
    .await;
    match out {
        Ok(Ok(RefreshOutcome::Ran)) => serde_json::json!({"status": "ran"}),
        Ok(Ok(RefreshOutcome::Throttled { next_allowed })) => {
            serde_json::json!({"status": "throttled", "next_allowed": next_allowed})
        }
        Ok(Ok(RefreshOutcome::NotInstalled)) => serde_json::json!({"status": "not_installed"}),
        Ok(Err(e)) => serde_json::json!({"status": "error", "message": format!("{e:#}")}),
        Err(e) => serde_json::json!({"status": "error", "message": format!("worker: {e}")}),
    }
}

/// History window cap for `/api/limits/history` (90 days).
const MAX_HISTORY_HOURS: u32 = 2160;

#[derive(Debug, Default, Deserialize)]
struct HistoryParams {
    vendor: Option<String>,
    account: Option<String>,
    scope: Option<String>,
    hours: Option<String>,
}

/// `{ points: [[measured_at, used %]], resets: [resets_at] }` for one key over the last `hours` (default 24).
async fn limits_history_api(
    State(st): State<AppState>,
    Query(p): Query<HistoryParams>,
) -> ApiResult {
    let (Some(vendor), Some(scope)) = (
        p.vendor.filter(|s| !s.is_empty()),
        p.scope.filter(|s| !s.is_empty()),
    ) else {
        return Err(ApiError::BadRequest("vendor and scope are required".into()));
    };
    let account = p.account.unwrap_or_default();
    let hours = match p.hours.as_deref() {
        None | Some("") => 24,
        Some(s) => match s.parse::<u32>() {
            Ok(n) if (1..=MAX_HISTORY_HOURS).contains(&n) => n,
            _ => return Err(ApiError::BadRequest(format!("bad hours: {s}"))),
        },
    };
    with_db(st, move |c, _| {
        let since = chrono::Utc::now().timestamp() - i64::from(hours) * 3600;
        Ok(serde_json::json!({
            "points": limits::history(c, &vendor, &account, &scope, since)?,
            "resets": limits::resets(c, &vendor, &account, &scope, since)?,
        }))
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
            // built on this thread: adapters are not Send
            let mut c = Collector::new(paths, cfg);
            loop {
                c.tick();
                std::thread::sleep(every);
            }
        });
    }
    let state = AppState {
        paths,
        prices: Arc::new(prices),
        cfg: Arc::new(cfg),
        port,
    };
    let addr = bind_addr(port);
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    rt.block_on(async move {
        let listener = tokio::net::TcpListener::bind(&addr).await?;
        println!("agent-usage: http://{addr}");
        axum::serve(listener, router(state)).await?;
        anyhow::Ok(())
    })
}

/// Background collection state. [`Collector::tick`] never panics and never
/// gives up: a failed DB open is retried on the next tick, and a panic inside
/// `run_all` drops the connection so the next tick starts fresh.
pub struct Collector {
    paths: Paths,
    cfg: Config,
    adapters: Vec<Box<dyn crate::adapters::Adapter>>,
    conn: Option<Connection>,
    open_failing: bool,
}

impl Collector {
    pub fn new(paths: Paths, cfg: Config) -> Collector {
        Collector {
            paths,
            cfg,
            adapters: collect::default_adapters(),
            conn: None,
            open_failing: false,
        }
    }

    /// One collection pass; `true` when `run_all` completed.
    pub fn tick(&mut self) -> bool {
        if self.conn.is_none() {
            match db::open(&self.paths.db()) {
                Ok(c) => {
                    if self.open_failing {
                        eprintln!("collector: database open again");
                    }
                    self.conn = Some(c);
                    self.open_failing = false;
                }
                Err(e) => {
                    // log once per failure streak
                    if !self.open_failing {
                        eprintln!("collector: cannot open database (will retry): {e:#}");
                    }
                    self.open_failing = true;
                    return false;
                }
            }
        }
        let Some(conn) = self.conn.as_mut() else {
            return false;
        };
        let (paths, cfg, adapters) = (&self.paths, &self.cfg, &self.adapters);
        match catch_unwind(AssertUnwindSafe(|| {
            collect::run_all(conn, paths, cfg, adapters)
        })) {
            Ok(statuses) => {
                for s in statuses {
                    if let Some(e) = s.error {
                        eprintln!("collector: {}: {e}", s.vendor.as_str());
                    }
                }
                true
            }
            Err(_) => {
                eprintln!("collector: panic during collection; reconnecting next time");
                self.conn = None;
                false
            }
        }
    }
}
