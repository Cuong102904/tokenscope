//! `agent-usage tui`: one screen with cost totals, one gauge per rate-limit
//! card and the active sessions. Split in three: [`build_state`] (DB -> plain
//! data, injectable `now`), [`render`] (pure drawing, testable with
//! `TestBackend`) and [`run`] (terminal + event loop + background collector).

use crate::adapters::agy;
use crate::collect;
use crate::config::Config;
use crate::db;
use crate::paths::Paths;
use crate::pricing::PriceTable;
use crate::query::Filter;
use crate::query::active::{self, ActiveSession};
use crate::query::cost::{self, Totals};
use crate::query::limits::{self, EXPIRED_NOTE, LimitCard};
use chrono::{DateTime, Local};
use ratatui::Frame;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Gauge, Paragraph, Row, Table};
use rusqlite::{Connection, OpenFlags};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::mpsc;
use std::time::{Duration, Instant};

const DISCLAIMER: &str = "Ước tính theo giá API, không phải hoá đơn.";
const REDRAW_EVERY: Duration = Duration::from_secs(5);
const MAX_GAUGES: usize = 8;

/// Everything `render` needs; built by [`build_state`] or by hand in tests.
#[derive(Debug, Clone, Default)]
pub struct TuiState {
    /// Epoch seconds the numbers are relative to ("3p trước").
    pub now: i64,
    pub today: Totals,
    pub d7: Totals,
    pub d30: Totals,
    /// Cards to draw (expired-and-stale ones already removed).
    pub limits: Vec<LimitCard>,
    /// Number of expired-and-stale cards left out.
    pub hidden_limits: usize,
    pub active: Vec<ActiveSession>,
    /// Free-text line (collector / agy outcome, errors).
    pub status: String,
}

pub fn build_state(
    conn: &Connection,
    prices: &PriceTable,
    cfg: &Config,
    now: i64,
    status: String,
) -> anyhow::Result<TuiState> {
    let all = limits::current(conn, cfg, now)?;
    let total = all.len();
    // same rule as the web Overview: dead accounts (expired and stale) are hidden, the no-data placeholder stays
    let limits: Vec<_> = all
        .into_iter()
        .filter(|c| c.no_data || !(c.expired && c.stale))
        .collect();
    Ok(TuiState {
        now,
        today: cost::totals(conn, prices, &Filter::days(1))?,
        d7: cost::totals(conn, prices, &Filter::days(7))?,
        d30: cost::totals(conn, prices, &Filter::days(30))?,
        hidden_limits: total - limits.len(),
        limits,
        active: active::list(conn, prices, cfg, now)?,
        status,
    })
}

fn money(c: f64) -> String {
    format!("${c:.2}")
}

/// `$1.00`, `≥ $0.50` when some model has no price, `n/a` when nothing is priced.
fn total_text(t: &Totals) -> String {
    match (t.unpriced_models.is_empty(), t.cost > 0.0) {
        (true, _) => money(t.cost),
        (false, true) => format!("≥ {}", money(t.cost)),
        (false, false) => "n/a".into(),
    }
}

fn opt_cost(cost: Option<f64>, priced: f64) -> String {
    match cost {
        Some(c) => money(c),
        None if priced > 0.0 => format!("≥ {}", money(priced)),
        None => "n/a".into(),
    }
}

fn local_hm(ts: i64) -> String {
    DateTime::from_timestamp(ts, 0)
        .map(|d| d.with_timezone(&Local).format("%H:%M").to_string())
        .unwrap_or_else(|| "?".into())
}

fn ago(now: i64, ts: i64) -> String {
    let s = (now - ts).max(0);
    match s {
        0..60 => "vừa xong".into(),
        60..3600 => format!("{}p trước", s / 60),
        3600..86400 => format!("{}g trước", s / 3600),
        _ => format!("{}n trước", s / 86400),
    }
}

fn project_name(p: &str) -> &str {
    p.rsplit('/').find(|s| !s.is_empty()).unwrap_or(p)
}

fn gauge_color(pct: f64) -> Color {
    if pct >= 90.0 {
        Color::Red
    } else if pct >= 70.0 {
        Color::Yellow
    } else {
        Color::Green
    }
}

fn limit_label(c: &LimitCard, now: i64) -> String {
    if c.no_data {
        return format!("{} — {}", c.account_label, c.eta_note);
    }
    let head = format!("{} {}", c.account_label, c.scope);
    let measured = format!("đo {}", ago(now, c.measured_at));
    let stale = if c.stale { " [CŨ]" } else { "" };
    if c.expired {
        return format!("{head} {EXPIRED_NOTE} · {measured}{stale}");
    }
    let reset = c
        .resets_at
        .map(|r| format!(" · reset {}", local_hm(r)))
        .unwrap_or_default();
    format!("{head} {:.0}%{reset} · {measured}{stale}", c.used_percent)
}

pub fn render(f: &mut Frame, st: &TuiState) {
    let area = f.area();
    let n_cards = st.limits.len().min(MAX_GAUGES)
        + usize::from(st.limits.len() > MAX_GAUGES || st.hidden_limits > 0);
    let [head, mid, bottom, foot] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Max(n_cards.max(1) as u16 + 2),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas(area);

    let totals = format!(
        "hôm nay {} · 7 ngày {} · 30 ngày {}",
        total_text(&st.today),
        total_text(&st.d7),
        total_text(&st.d30)
    );
    f.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(
                totals,
                Style::new().add_modifier(Modifier::BOLD),
            )),
            Line::from(DISCLAIMER),
        ]),
        head,
    );
    render_limits(f, st, mid);
    render_active(f, st, bottom);
    let foot_text = if st.status.is_empty() {
        "q thoát · r làm mới".to_string()
    } else {
        format!("{} · q thoát · r làm mới", st.status)
    };
    f.render_widget(
        Paragraph::new(foot_text).style(Style::new().fg(Color::DarkGray)),
        foot,
    );
}

fn render_limits(f: &mut Frame, st: &TuiState, area: Rect) {
    let block = Block::default().borders(Borders::ALL).title("Hạn mức");
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.height == 0 {
        return;
    }
    if st.limits.is_empty() {
        f.render_widget(Paragraph::new("Chưa có dữ liệu hạn mức."), inner);
        return;
    }
    let rows = inner.height as usize;
    let extra = st.hidden_limits + st.limits.len().saturating_sub(MAX_GAUGES);
    // keep one row for the "+N" note when something is left out
    let fit = if extra > 0 && rows > 1 {
        rows - 1
    } else {
        rows
    };
    let shown = st.limits.len().min(MAX_GAUGES).min(fit);
    let left_out = st.hidden_limits + st.limits.len() - shown;
    for (i, c) in st.limits.iter().take(shown).enumerate() {
        let r = Rect {
            x: inner.x,
            y: inner.y + i as u16,
            width: inner.width,
            height: 1,
        };
        let (ratio, color) = if c.expired || c.no_data {
            (0.0, Color::DarkGray)
        } else {
            let p = c.used_percent.clamp(0.0, 100.0);
            (
                p / 100.0,
                if c.stale {
                    Color::DarkGray
                } else {
                    gauge_color(p)
                },
            )
        };
        let g = Gauge::default()
            .ratio(ratio)
            .gauge_style(Style::new().fg(color).bg(Color::Black))
            .label(Span::styled(
                limit_label(c, st.now),
                Style::new().fg(Color::White),
            ));
        f.render_widget(g, r);
    }
    if left_out > 0 && shown < rows {
        let r = Rect {
            x: inner.x,
            y: inner.y + shown as u16,
            width: inner.width,
            height: 1,
        };
        f.render_widget(
            Paragraph::new(format!("+{left_out} thẻ cũ / còn lại bị ẩn"))
                .style(Style::new().fg(Color::DarkGray)),
            r,
        );
    }
}

fn render_active(f: &mut Frame, st: &TuiState, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!("Phiên đang chạy ({})", st.active.len()));
    if st.active.is_empty() {
        f.render_widget(
            Paragraph::new("Không có phiên nào đang chạy.").block(block),
            area,
        );
        return;
    }
    let rows = st.active.iter().map(|s| {
        Row::new(vec![
            s.vendor.clone(),
            project_name(&s.project).to_string(),
            s.model.clone().unwrap_or_else(|| "?".into()),
            ago(st.now, s.last_write),
            opt_cost(s.cost_so_far, s.priced_so_far),
        ])
    });
    let widths = [
        Constraint::Length(9),
        Constraint::Min(8),
        Constraint::Min(10),
        Constraint::Length(10),
        Constraint::Length(10),
    ];
    let header = Row::new(["hãng", "dự án", "model", "cập nhật", "chi phí"])
        .style(Style::new().add_modifier(Modifier::BOLD));
    f.render_widget(Table::new(rows, widths).header(header).block(block), area);
}

enum Cmd {
    /// Collect now; with `true` also run `agy /usage` (throttled by agy itself).
    Now(bool),
}

pub enum Msg {
    Done(String),
    /// The collector thread has exited (reason); sent exactly once, last.
    Died(String),
}

const WORKER_NAME: &str = "agent-usage-collector";

/// The panic hook restores the terminal and prints only for panics outside the collector thread.
pub fn is_worker_thread(name: Option<&str>) -> bool {
    name == Some(WORKER_NAME)
}

pub fn panic_message(p: &(dyn std::any::Any + Send)) -> String {
    p.downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| p.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "không rõ".into())
}

pub fn died_status(reason: &str) -> String {
    format!("bộ thu thập đã dừng: {reason}")
}

/// Runs `body` under a panic guard and always reports how it ended as the last message.
pub fn guarded_worker(tx: &mpsc::Sender<Msg>, body: impl FnOnce() -> String) {
    let reason = match catch_unwind(AssertUnwindSafe(body)) {
        Ok(r) => r,
        Err(p) => format!("panic: {}", panic_message(&*p)),
    };
    let _ = tx.send(Msg::Died(reason));
}

/// Replace `state` with a successful rebuild; on failure keep the old numbers and show the error
/// as the status until a later rebuild succeeds.
pub fn apply_rebuild(state: &mut TuiState, res: anyhow::Result<TuiState>) {
    match res {
        Ok(s) => *state = s,
        Err(e) => state.status = format!("lỗi đọc DB: {e:#}"),
    }
}

fn agy_note(r: anyhow::Result<agy::RefreshOutcome>) -> String {
    match r {
        Ok(agy::RefreshOutcome::Ran) => "agy: đã làm mới".into(),
        Ok(agy::RefreshOutcome::Throttled { next_allowed }) => format!(
            "agy: vừa làm mới, lần sau được phép lúc {}",
            local_hm(next_allowed)
        ),
        Ok(agy::RefreshOutcome::NotInstalled) => "agy: not found on PATH".into(),
        Err(e) => format!("agy: lỗi {e:#}"),
    }
}

/// Collector thread: own connection and adapters (not Send), collects every
/// `refresh_seconds` and on demand; reports a status line per pass. Returns why it stopped.
fn worker_body(
    paths: Paths,
    cfg: Config,
    rx: mpsc::Receiver<Cmd>,
    tx: mpsc::Sender<Msg>,
) -> String {
    let adapters = collect::default_adapters();
    let mut conn = match db::open(&paths.db()) {
        Ok(c) => c,
        Err(e) => return format!("không mở được DB: {e:#}"),
    };
    let every = Duration::from_secs(cfg.refresh_seconds.max(1));
    let mut want = Some(false);
    loop {
        if let Some(with_agy) = want.take() {
            let out = catch_unwind(AssertUnwindSafe(|| {
                let statuses = collect::run_all(&mut conn, &paths, &cfg, &adapters);
                let mut note = match statuses
                    .iter()
                    .find_map(|s| s.error.as_ref().map(|e| (s.vendor, e)))
                {
                    Some((v, e)) => format!("lỗi {}: {e}", v.as_str()),
                    None => format!("đã thu thập lúc {}", Local::now().format("%H:%M:%S")),
                };
                if with_agy {
                    note = format!(
                        "{note} · {}",
                        agy_note(agy::refresh(&paths, &cfg, &mut conn))
                    );
                }
                note
            }));
            let note = out.unwrap_or_else(|p| {
                format!("thu thập bị lỗi nội bộ (panic): {}", panic_message(&*p))
            });
            if tx.send(Msg::Done(note)).is_err() {
                return "giao diện đã đóng".into();
            }
        }
        match rx.recv_timeout(every) {
            Ok(Cmd::Now(a)) => want = Some(a),
            Err(mpsc::RecvTimeoutError::Timeout) => want = Some(false),
            Err(mpsc::RecvTimeoutError::Disconnected) => return "giao diện đã đóng".into(),
        }
    }
}

fn open_reader(paths: &Paths) -> anyhow::Result<Connection> {
    let c = Connection::open_with_flags(
        paths.db(),
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    c.busy_timeout(Duration::from_millis(2000))?;
    Ok(c)
}

/// Blocking. Always leaves the alternate screen / raw mode, also on error and
/// panic (`ratatui::init` installs a restoring panic hook).
pub fn run(paths: Paths) -> anyhow::Result<()> {
    let cfg = Config::load(&paths)?;
    let prices = PriceTable::load(&paths)?;
    drop(db::open(&paths.db())?); // create/migrate before the read-only connection
    let reader = open_reader(&paths)?;
    let (cmd_tx, cmd_rx) = mpsc::channel();
    let (msg_tx, msg_rx) = mpsc::channel();
    let mut terminal = ratatui::init();
    // ratatui's hook restores the terminal on whatever thread panics; a caught collector panic
    // must neither do that nor print over the UI (its message reaches the status line instead)
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if !is_worker_thread(std::thread::current().name()) {
            prev(info);
        }
    }));
    {
        let (p, c) = (paths.clone(), cfg.clone());
        let spawned = std::thread::Builder::new()
            .name(WORKER_NAME.into())
            .spawn(move || {
                let tx2 = msg_tx.clone();
                guarded_worker(&tx2, move || worker_body(p, c, cmd_rx, msg_tx));
            });
        if let Err(e) = spawned {
            ratatui::restore();
            return Err(e.into());
        }
    }
    let res = event_loop(&mut terminal, &reader, &prices, &cfg, &cmd_tx, &msg_rx);
    ratatui::restore();
    res
}

fn event_loop(
    terminal: &mut ratatui::DefaultTerminal,
    reader: &Connection,
    prices: &PriceTable,
    cfg: &Config,
    cmd_tx: &mpsc::Sender<Cmd>,
    msg_rx: &mpsc::Receiver<Msg>,
) -> anyhow::Result<()> {
    let mut status = "đang thu thập…".to_string();
    let mut died: Option<String> = None;
    let mut state = TuiState {
        status: status.clone(),
        ..Default::default()
    };
    let mut next_rebuild = Instant::now();
    loop {
        if Instant::now() >= next_rebuild {
            apply_rebuild(
                &mut state,
                build_state(
                    reader,
                    prices,
                    cfg,
                    chrono::Utc::now().timestamp(),
                    status.clone(),
                ),
            );
            next_rebuild = Instant::now() + REDRAW_EVERY;
        }
        terminal.draw(|f| render(f, &state))?;
        let wait = next_rebuild
            .saturating_duration_since(Instant::now())
            .min(Duration::from_millis(250));
        if event::poll(wait)? {
            match event::read()? {
                Event::Key(k) if k.kind == KeyEventKind::Press => match k.code {
                    KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                    KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => {
                        return Ok(());
                    }
                    KeyCode::Char('r') => {
                        status = match cmd_tx.send(Cmd::Now(true)) {
                            Ok(()) if died.is_none() => "đang làm mới…".into(),
                            _ => died_status(died.as_deref().unwrap_or("không rõ")),
                        };
                        next_rebuild = Instant::now();
                    }
                    _ => {}
                },
                _ => {} // resize etc.: the next loop turn redraws
            }
        }
        while let Ok(m) = msg_rx.try_recv() {
            match m {
                Msg::Done(s) => status = s,
                Msg::Died(r) => {
                    status = died_status(&r);
                    died = Some(r);
                }
            }
            next_rebuild = Instant::now();
        }
    }
}
