use agent_usage::config::Config;
use agent_usage::db;
use agent_usage::paths::Paths;
use agent_usage::query::limits::{self, EtaResult, LimitCard};
use rusqlite::{Connection, params};

const NOW: i64 = 1_800_000_000;
const MIN: i64 = 60;
const HOUR: i64 = 3600;
const DAY: i64 = 86400;

fn setup() -> (tempfile::TempDir, Connection) {
    let d = tempfile::tempdir().unwrap();
    let paths = Paths { home: d.path().to_path_buf() };
    let c = db::open(&paths.db()).unwrap();
    (d, c)
}

#[allow(clippy::too_many_arguments)]
fn snap(c: &Connection, vendor: &str, account: &str, scope: &str, wm: i64, used: f64, resets_at: i64, at: i64) {
    c.execute(
        "INSERT INTO limit_snapshots(vendor,account,scope,window_minutes,used_percent,resets_at,plan_type,measured_at,source)
         VALUES(?1,?2,?3,?4,?5,?6,'plus',?7,'test')",
        params![vendor, account, scope, wm, used, resets_at, at],
    )
    .unwrap();
}

fn card<'a>(cards: &'a [LimitCard], vendor: &str, account: &str, scope: &str) -> &'a LimitCard {
    cards
        .iter()
        .find(|c| c.vendor == vendor && c.account == account && c.scope == scope)
        .unwrap_or_else(|| panic!("no card {vendor}/{account}/{scope} in {cards:?}"))
}

fn reason(r: EtaResult) -> String {
    match r {
        EtaResult::Unavailable(s) => s,
        EtaResult::At(t) => panic!("expected no ETA, got {t}"),
    }
}

#[test]
fn stale_flag() {
    let (_d, c) = setup();
    snap(&c, "claude", "a", "five_hour", 300, 10.0, NOW + HOUR, NOW - 31 * MIN);
    snap(&c, "claude", "b", "five_hour", 300, 10.0, NOW + HOUR, NOW - 29 * MIN);
    snap(&c, "claude", "c", "five_hour", 300, 10.0, NOW + HOUR, NOW - 30 * MIN);
    snap(&c, "codex", "a", "weekly", 10080, 10.0, NOW + DAY, NOW - 5 * HOUR);
    snap(&c, "codex", "b", "weekly", 10080, 10.0, NOW + DAY, NOW - 7 * HOUR);
    let cards = limits::current(&c, &Config::default(), NOW).unwrap();
    assert!(card(&cards, "claude", "a", "five_hour").stale);
    assert!(!card(&cards, "claude", "b", "five_hour").stale);
    assert!(!card(&cards, "claude", "c", "five_hour").stale, "exactly 30 min is not stale yet");
    assert!(!card(&cards, "codex", "a", "weekly").stale);
    assert!(card(&cards, "codex", "b", "weekly").stale);
}

#[test]
fn current_takes_latest_per_key_and_labels() {
    let (_d, c) = setup();
    snap(&c, "codex", "", "five_hour", 300, 5.0, NOW + HOUR, NOW - 20 * MIN);
    snap(&c, "codex", "", "five_hour", 300, 7.0, NOW + HOUR, NOW - 10 * MIN);
    snap(&c, "codex", "f8ce04de", "five_hour", 300, 50.0, NOW + HOUR, NOW - 10 * MIN);
    let mut cfg = Config::default();
    cfg.account_labels.insert("codex:f8ce04de".into(), "work".into());
    let cards = limits::current(&c, &cfg, NOW).unwrap();
    let old = card(&cards, "codex", "", "five_hour");
    assert_eq!(old.used_percent, 7.0);
    assert_eq!(old.measured_at, NOW - 10 * MIN);
    assert_eq!(old.account_label, "log cũ không phân biệt tài khoản");
    assert_eq!(old.plan_type.as_deref(), Some("plus"));
    assert_eq!(card(&cards, "codex", "f8ce04de", "five_hour").account_label, "work");
    // no Claude snapshot at all: a placeholder card tells how to get one
    let ph = cards.iter().find(|c| c.vendor == "claude").unwrap();
    assert!(ph.no_data);
    assert!(ph.eta_note.contains("agent-usage statusline --print-snippet"), "{}", ph.eta_note);
    assert_eq!(cards[0].vendor, "claude", "claude is listed first");
}

#[test]
fn expired_window_not_shown_as_current() {
    let (_d, c) = setup();
    snap(&c, "claude", "a", "five_hour", 300, 88.0, NOW - 60, NOW - 10 * MIN);
    snap(&c, "claude", "a", "weekly", 10080, 20.0, NOW + DAY, NOW - 10 * MIN);
    let cards = limits::current(&c, &Config::default(), NOW).unwrap();
    let x = card(&cards, "claude", "a", "five_hour");
    assert!(x.expired);
    assert_eq!(x.eta, None);
    assert_eq!(x.eta_note, "đã reset, chờ số mới");
    assert!(!card(&cards, "claude", "a", "weekly").expired);
    assert!(!cards.iter().any(|c| c.no_data), "claude has data: no placeholder");
}

#[test]
fn eta_linear() {
    assert_eq!(limits::eta(&[(0, 10.0), (600, 20.0), (1200, 30.0)], 100_000), EtaResult::At(5400));
}

#[test]
fn eta_disabled_on_drop() {
    let r = reason(limits::eta(&[(0, 40.0), (600, 20.0), (1200, 45.0)], 100_000));
    assert!(r.contains("lẫn"), "{r}");
}

#[test]
fn eta_after_reset_is_none() {
    let r = reason(limits::eta(&[(0, 10.0), (600, 20.0), (1200, 30.0)], 3000));
    assert_eq!(r, "không chạm trước khi reset");
}

#[test]
fn eta_needs_three_points_over_ten_minutes_in_last_hour() {
    assert!(reason(limits::eta(&[(0, 10.0), (1200, 30.0)], 100_000)).contains("3"));
    assert!(reason(limits::eta(&[(0, 10.0), (300, 20.0), (500, 30.0)], 100_000)).contains("10 phút"));
    // the first point is more than 60 min before the last: only two remain
    assert!(reason(limits::eta(&[(0, 10.0), (4000, 20.0), (4600, 30.0)], 100_000)).contains("3"));
    // flat usage never reaches 100%
    assert!(!reason(limits::eta(&[(0, 10.0), (600, 10.0), (1200, 10.0)], 100_000)).is_empty());
    // order of the input does not matter
    assert_eq!(limits::eta(&[(1200, 30.0), (0, 10.0), (600, 20.0)], 100_000), EtaResult::At(5400));
}

#[test]
fn card_eta_uses_same_reset_window_and_warns_on_drop() {
    let (_d, c) = setup();
    // previous window (different resets_at) must not be mixed in
    snap(&c, "codex", "a", "five_hour", 300, 90.0, NOW - 2 * HOUR, NOW - 50 * MIN);
    // resets_at jitters by a second between Codex events: still the same window
    snap(&c, "codex", "a", "five_hour", 300, 10.0, NOW + 4 * HOUR, NOW - 20 * MIN);
    snap(&c, "codex", "a", "five_hour", 300, 20.0, NOW + 4 * HOUR + 1, NOW - 10 * MIN);
    snap(&c, "codex", "a", "five_hour", 300, 30.0, NOW + 4 * HOUR, NOW);
    // mixed accounts: the value drops inside one window
    snap(&c, "codex", "", "weekly", 10080, 40.0, NOW + DAY, NOW - 20 * MIN);
    snap(&c, "codex", "", "weekly", 10080, 20.0, NOW + DAY, NOW - 10 * MIN);
    snap(&c, "codex", "", "weekly", 10080, 45.0, NOW + DAY, NOW);
    let cards = limits::current(&c, &Config::default(), NOW).unwrap();
    let a = card(&cards, "codex", "a", "five_hour");
    assert_eq!(a.eta, Some(NOW + 70 * MIN), "{a:?}");
    assert!(a.warning.is_none());
    let m = card(&cards, "codex", "", "weekly");
    assert_eq!(m.eta, None);
    assert!(m.eta_note.contains("lẫn"));
    assert!(m.warning.as_deref().is_some_and(|w| w.contains("lẫn")), "{m:?}");
}

#[test]
fn history_and_resets() {
    let (_d, c) = setup();
    snap(&c, "claude", "a", "five_hour", 300, 50.0, NOW - HOUR, NOW - 3 * HOUR);
    snap(&c, "claude", "a", "five_hour", 300, 5.0, NOW + 4 * HOUR, NOW - 30 * MIN);
    snap(&c, "claude", "a", "five_hour", 300, 9.0, NOW + 4 * HOUR + 1, NOW - 10 * MIN);
    snap(&c, "claude", "a", "weekly", 10080, 1.0, NOW + DAY, NOW - 10 * MIN);
    snap(&c, "claude", "b", "five_hour", 300, 1.0, NOW + DAY, NOW - 10 * MIN);
    let h = limits::history(&c, "claude", "a", "five_hour", NOW - 2 * HOUR).unwrap();
    assert_eq!(h, vec![(NOW - 30 * MIN, 5.0), (NOW - 10 * MIN, 9.0)]);
    let r = limits::resets(&c, "claude", "a", "five_hour", NOW - 4 * HOUR).unwrap();
    assert_eq!(r, vec![NOW - HOUR, NOW + 4 * HOUR], "jittered resets_at collapse into one");
}

#[test]
fn events_since_newest_first() {
    let (_d, c) = setup();
    for (ts, scope) in [(NOW - 2 * DAY, "primary"), (NOW - HOUR, "secondary"), (NOW - 40 * DAY, "primary")] {
        c.execute(
            "INSERT INTO limit_events(vendor,account,ts,scope,resets_at) VALUES('codex','',?1,?2,?3)",
            params![ts, scope, ts + HOUR],
        )
        .unwrap();
    }
    let ev = limits::events(&c, &Config::default(), NOW - 30 * DAY).unwrap();
    assert_eq!(ev.len(), 2);
    assert_eq!((ev[0].ts, ev[0].scope.as_str()), (NOW - HOUR, "secondary"));
    assert_eq!(ev[0].account_label, "log cũ không phân biệt tài khoản");
    assert_eq!(ev[1].resets_at, Some(NOW - 2 * DAY + HOUR));
}

fn count(c: &Connection, scope: &str, before: i64) -> i64 {
    c.query_row("SELECT count(*) FROM limit_snapshots WHERE scope=?1 AND measured_at < ?2", params![scope, before], |r| r.get(0)).unwrap()
}

#[test]
fn downsample_keeps_one_per_5min() {
    let (_d, c) = setup();
    let cutoff = NOW - 90 * DAY;
    // 15 minutes of per-minute samples, aligned to 5-minute buckets, well before the cutoff
    let old = (cutoff - 10 * DAY) / 300 * 300;
    for i in 0..15 {
        snap(&c, "claude", "a", "five_hour", 300, i as f64, old + HOUR, old + i * MIN);
        snap(&c, "claude", "a", "weekly", 10080, i as f64, old + DAY, old + i * MIN);
    }
    // recent samples are untouched
    for i in 0..15 {
        snap(&c, "claude", "a", "five_hour", 300, i as f64, NOW + HOUR, NOW - 30 * MIN + i * MIN);
    }
    let removed = db::downsample_limits(&c, cutoff).unwrap();
    assert_eq!(removed, 24);
    assert_eq!(count(&c, "five_hour", cutoff), 3);
    assert_eq!(count(&c, "weekly", cutoff), 3);
    assert_eq!(count(&c, "five_hour", NOW + 1) - 3, 15);
    // the earliest sample of each bucket is the one kept
    let kept: Vec<i64> = c
        .prepare("SELECT measured_at FROM limit_snapshots WHERE scope='five_hour' AND measured_at < ?1 ORDER BY measured_at")
        .unwrap()
        .query_map([cutoff], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(kept, vec![old, old + 5 * MIN, old + 10 * MIN]);
    assert_eq!(db::downsample_limits(&c, cutoff).unwrap(), 0);
}

#[test]
fn downsample_runs_at_most_once_a_day() {
    let (_d, c) = setup();
    let old = NOW - 100 * DAY;
    snap(&c, "claude", "a", "weekly", 10080, 1.0, old + DAY, old);
    snap(&c, "claude", "a", "weekly", 10080, 2.0, old + DAY, old + MIN);
    assert_eq!(db::downsample_daily(&c, NOW).unwrap(), Some(1));
    snap(&c, "claude", "a", "weekly", 10080, 3.0, old + DAY, old + 2 * MIN);
    assert_eq!(db::downsample_daily(&c, NOW + DAY - 1).unwrap(), None, "ran less than a day ago");
    assert_eq!(db::downsample_daily(&c, NOW + DAY).unwrap(), Some(1));
}

#[test]
fn stale_card_has_no_eta() {
    let (_d, c) = setup();
    // a clean upward trend that would have hit 100% long ago, measured 2 days ago
    for (i, used) in [10.0, 20.0, 30.0].iter().enumerate() {
        snap(&c, "codex", "a", "monthly", 43200, *used, NOW + 10 * DAY, NOW - 2 * DAY + 600 * i as i64);
    }
    let cards = limits::current(&c, &Config::default(), NOW).unwrap();
    let x = card(&cards, "codex", "a", "monthly");
    assert!(x.stale);
    assert_eq!(x.eta, None);
    assert!(x.eta_note.contains("cũ"), "{}", x.eta_note);
}
