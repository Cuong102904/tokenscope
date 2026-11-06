use chrono::{Local, TimeZone};
use ratatui::{Terminal, backend::TestBackend};
use tokenscope::config::Config;
use tokenscope::db;
use tokenscope::paths::Paths;
use tokenscope::pricing::PriceTable;
use tokenscope::query::active::ActiveSession;
use tokenscope::query::cost::Totals;
use tokenscope::query::limits::LimitCard;
use tokenscope::tui::{self, TuiState};

const NOW: i64 = 1_800_000_000;

fn card(scope: &str, pct: f64, stale: bool, expired: bool) -> LimitCard {
    LimitCard {
        vendor: "claude".into(),
        account: "".into(),
        account_label: "work".into(),
        scope: scope.into(),
        window_minutes: Some(300),
        used_percent: pct,
        resets_at: Some(if expired { NOW - 600 } else { NOW + 3600 }),
        measured_at: NOW - 180,
        source: "statusline".into(),
        plan_type: None,
        stale,
        expired,
        no_data: false,
        eta: None,
        eta_note: String::new(),
        warning: None,
    }
}

fn session(project: &str) -> ActiveSession {
    ActiveSession {
        vendor: "claude".into(),
        session_id: "s1".into(),
        project: project.into(),
        title: None,
        model: Some("claude-fable-5-1".into()),
        last_write: NOW - 120,
        cost_so_far: Some(1.5),
        priced_so_far: 1.5,
        cost_last_hour: None,
        priced_last_hour: 0.0,
    }
}

fn state() -> TuiState {
    let t = |cost: f64, unpriced: &[&str]| Totals {
        cost,
        unpriced_models: unpriced.iter().map(|s| s.to_string()).collect(),
        ..Default::default()
    };
    TuiState {
        now: NOW,
        today: t(1.25, &[]),
        d7: t(7.5, &["mystery-model"]),
        d30: t(0.0, &["mystery-model"]),
        limits: vec![
            card("five_hour", 44.0, false, false),
            card("weekly", 95.0, true, false),
            card("old", 10.0, false, true),
        ],
        hidden_limits: 2,
        active: vec![session("/home/u/my-proj")],
        status: "agy: not found on PATH".into(),
    }
}

fn draw(st: &TuiState, w: u16, h: u16) -> String {
    let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
    term.draw(|f| tui::render(f, st)).unwrap();
    let buf = term.backend().buffer().clone();
    (0..h)
        .map(|y| {
            (0..w)
                .map(|x| buf[(x, y)].symbol().to_string())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn renders_totals_limits_and_sessions() {
    let s = draw(&state(), 100, 30);
    for want in [
        "hôm nay",
        "$1.25",
        "≥ $7.50",
        "n/a",
        "44%",
        "[CŨ]",
        "đã reset, chờ số mới",
        "my-proj",
        "claude-fable-5-1",
        "$1.50",
        "Ước tính theo giá API, không phải hoá đơn.",
        "agy: not found",
        "2 thẻ cũ",
    ] {
        assert!(s.contains(want), "missing {want:?} in:\n{s}");
    }
    let reset = Local
        .timestamp_opt(NOW + 3600, 0)
        .unwrap()
        .format("reset %H:%M")
        .to_string();
    assert!(s.contains(&reset), "missing {reset:?}");
    assert!(s.contains("đo 3p trước"), "{s}");
}

#[test]
fn fits_80x24_and_survives_tiny_sizes() {
    let s = draw(&state(), 80, 24);
    assert!(s.contains("44%") && s.contains("my-proj"), "{s}");
    for (w, h) in [(20, 5), (1, 1), (0, 0), (10, 40), (200, 3)] {
        draw(&state(), w, h);
    }
}

#[test]
fn build_state_hides_dead_cards_and_uses_injected_now() {
    let d = tempfile::tempdir().unwrap();
    let paths = Paths {
        home: d.path().to_path_buf(),
    };
    let prices = PriceTable::load(&paths).unwrap();
    let cfg = Config::default();
    let conn = db::open(&d.path().join("t.db")).unwrap();
    let snap = |scope: &str, resets: i64, at: i64| {
        conn.execute(
            "INSERT INTO limit_snapshots(vendor,account,scope,window_minutes,used_percent,resets_at,plan_type,measured_at,source) VALUES('codex','',?1,300,50,?2,NULL,?3,'t')",
            rusqlite::params![scope, resets, at],
        )
        .unwrap();
    };
    snap("live", NOW + 1000, NOW - 10);
    snap("dead", NOW - 5000, NOW - 99_999); // expired and stale
    conn.execute("INSERT INTO sessions(vendor,session_id,project,title,source_path,last_write) VALUES('claude','s1','/a/b/proj',NULL,'x',?1)", [NOW - 30]).unwrap();
    let st = tui::build_state(&conn, &prices, &cfg, NOW, "ok".into()).unwrap();
    let scopes: Vec<_> = st
        .limits
        .iter()
        .filter(|c| !c.no_data)
        .map(|c| c.scope.as_str())
        .collect();
    assert_eq!(scopes, ["live"]);
    assert_eq!(st.hidden_limits, 1);
    assert_eq!(st.active.len(), 1);
    assert_eq!(st.now, NOW);
    assert_eq!(st.status, "ok");
}

#[test]
fn hook_skips_only_the_worker_thread() {
    assert!(tui::is_worker_thread(Some("tokenscope-collector")));
    assert!(!tui::is_worker_thread(Some("main")));
    assert!(!tui::is_worker_thread(None));
}

#[test]
fn guarded_worker_reports_panic_and_normal_exit() {
    let (tx, rx) = std::sync::mpsc::channel();
    // the default hook would print the expected panic; silence is irrelevant to the assertion
    tui::guarded_worker(&tx, || panic!("boom {}", 7));
    match rx.recv().unwrap() {
        tui::Msg::Died(r) => assert_eq!(r, "panic: boom 7"),
        _ => panic!("expected Died"),
    }
    tui::guarded_worker(&tx, || "stopped".to_string());
    assert!(matches!(rx.recv().unwrap(), tui::Msg::Died(r) if r == "stopped"));
    assert_eq!(tui::died_status("x"), "bộ thu thập đã dừng: x");
}

#[test]
fn db_error_is_sticky_until_next_good_rebuild() {
    let mut s = state();
    tui::apply_rebuild(&mut s, Err(anyhow::anyhow!("locked")));
    assert!(s.status.contains("locked") && s.today.cost == 1.25);
    tui::apply_rebuild(&mut s, Err(anyhow::anyhow!("locked again")));
    assert!(s.status.contains("locked again"));
    let mut fresh = state();
    fresh.status = "ok".into();
    tui::apply_rebuild(&mut s, Ok(fresh));
    assert_eq!(s.status, "ok");
}
