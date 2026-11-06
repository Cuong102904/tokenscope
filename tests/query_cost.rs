use agent_usage::config::Config;
use agent_usage::query::{Filter, cost, sessions};
use agent_usage::{db, paths::Paths, pricing::PriceTable};
use chrono::{Duration, Local};
use rusqlite::Connection;
use std::path::Path;
use std::process::Command;

const FABLE: &str = "claude-fable-5-1"; // in $10 / out $50 per 1M

fn day(ago: i64) -> String {
    (Local::now().date_naive() - Duration::days(ago))
        .format("%Y-%m-%d")
        .to_string()
}

struct Ins<'a> {
    key: &'a str,
    vendor: &'a str,
    sess: &'a str,
    project: &'a str,
    model: &'a str,
    day: String,
    ts: i64,
    out: i64,
    side: i64,
}

fn base<'a>(key: &'a str) -> Ins<'a> {
    Ins {
        key,
        vendor: "claude",
        sess: "s1",
        project: "p",
        model: FABLE,
        day: day(0),
        ts: 0,
        out: 0,
        side: 0,
    }
}

fn ins(c: &Connection, i: Ins, input: i64, cache_read: i64) {
    c.execute(
        "INSERT INTO usage(msg_key,vendor,account,session_id,project,ts,day,model,input,output,reasoning,
           cache_read,cache_write_5m,cache_write_1h,sidechain) VALUES(?1,?2,'',?3,?4,?5,?6,?7,?8,?9,0,?10,0,0,?11)",
        rusqlite::params![i.key, i.vendor, i.sess, i.project, i.ts, i.day, i.model, input, i.out, cache_read, i.side],
    )
    .unwrap();
}

fn setup() -> (tempfile::TempDir, Connection, PriceTable) {
    let d = tempfile::tempdir().unwrap();
    let prices = PriceTable::load(&Paths {
        home: d.path().to_path_buf(),
    })
    .unwrap();
    let c = db::open(&d.path().join("t.db")).unwrap();
    (d, c, prices)
}

fn approx(a: f64, b: f64) {
    assert!((a - b).abs() < 1e-9, "{a} != {b}");
}

/// Runs `collect` in a subprocess with the given TZ over one assistant line at
/// 2026-11-04T23:30:00Z and returns `usage.day` from the resulting DB.
fn collected_day(tz: &str) -> String {
    let h = tempfile::tempdir().unwrap();
    let dir = h.path().join(".claude/projects/-home-u-proj");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("s1.jsonl"),
        r#"{"type":"assistant","sessionId":"s1","cwd":"/home/u/proj","isSidechain":false,"requestId":"r1","timestamp":"2026-11-04T23:30:00.000Z","message":{"id":"m1","model":"claude-sonnet-4-5","usage":{"input_tokens":1,"output_tokens":2},"content":[{"type":"text","text":"x"}]}}
"#,
    )
    .unwrap();
    let o = Command::new(env!("CARGO_BIN_EXE_agent-usage"))
        .arg("collect")
        .env("HOME", h.path())
        .env("TZ", tz)
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let c = Connection::open(h.path().join(".agent-usage/agent-usage.db")).unwrap();
    c.query_row("SELECT day FROM usage", [], |r| r.get(0))
        .unwrap()
}

#[test]
fn day_uses_local_time() {
    assert!(
        Path::new("/usr/share/zoneinfo/Asia/Ho_Chi_Minh").exists(),
        "tzdata missing"
    );
    assert_eq!(collected_day("Asia/Ho_Chi_Minh"), "2026-11-05");
    // the same input in UTC lands on the previous day, so the test can fail
    assert_eq!(collected_day("UTC"), "2026-11-04");
}

#[test]
fn unpriced_model_is_null_not_zero() {
    let (_d, c, p) = setup();
    ins(
        &c,
        Ins {
            out: 1000,
            ..base("a")
        },
        0,
        0,
    );
    ins(
        &c,
        Ins {
            model: "mystery-1",
            out: 5,
            ..base("b")
        },
        1,
        0,
    );
    let f = Filter::default();
    let m = cost::by_model(&c, &p, &f).unwrap();
    let mystery = m.iter().find(|r| r.model == "mystery-1").unwrap();
    assert_eq!(mystery.cost, None);
    approx(
        m.iter().find(|r| r.model == FABLE).unwrap().cost.unwrap(),
        0.05,
    );
    assert_eq!(m.last().unwrap().model, "mystery-1"); // unpriced sorts last
    let t = cost::totals(&c, &p, &f).unwrap();
    assert_eq!(t.unpriced_models, vec!["mystery-1".to_string()]);

    // a group mixing priced and unpriced rows is None, with the priced part exposed
    let v = cost::by_vendor(&c, &p, &f).unwrap();
    assert_eq!(v[0].cost, None);
    approx(v[0].priced_cost, 0.05);
    let pr = cost::by_project(&c, &p, &f).unwrap();
    assert_eq!((pr[0].cost, pr[0].messages), (None, 2));
    let s = sessions::list(&c, &p, &Config::default(), &f, 10).unwrap();
    assert_eq!(s[0].cost, None);
    approx(s[0].priced_cost, 0.05);
    let d = cost::daily(&c, &p, &Filter::days(1)).unwrap();
    assert_eq!(d[0].unpriced, vec!["mystery-1".to_string()]);
    assert!(!d[0].by_model.contains_key("mystery-1"));
    let tl = sessions::timeline_detail(&c, &p, "claude", "s1").unwrap();
    assert_eq!(tl.unpriced_models, vec!["mystery-1".to_string()]);
}

#[test]
fn daily_fills_gaps() {
    let (_d, c, p) = setup();
    ins(
        &c,
        Ins {
            day: day(2),
            out: 1000,
            ..base("a")
        },
        0,
        0,
    );
    ins(
        &c,
        Ins {
            day: day(0),
            out: 2000,
            ..base("b")
        },
        0,
        0,
    );
    let d = cost::daily(&c, &p, &Filter::days(3)).unwrap();
    assert_eq!(
        d.iter().map(|r| r.day.clone()).collect::<Vec<_>>(),
        vec![day(2), day(1), day(0)]
    );
    assert!(d[1].by_model.is_empty());
    approx(d[0].by_model[FABLE], 0.05);
    approx(d[2].by_model[FABLE], 0.10);
    // days=1 shows only today
    assert_eq!(cost::daily(&c, &p, &Filter::days(1)).unwrap().len(), 1);
    // all time spans first..last day present, gaps included
    let all = cost::daily(&c, &p, &Filter::default()).unwrap();
    assert_eq!(all.len(), 3);
    assert!(all[1].by_model.is_empty());
    // no data: empty for all time, still the full window for days=N
    let (_d2, c2, p2) = setup();
    assert!(
        cost::daily(&c2, &p2, &Filter::default())
            .unwrap()
            .is_empty()
    );
    assert_eq!(cost::daily(&c2, &p2, &Filter::days(3)).unwrap().len(), 3);
}

#[test]
fn timeline_is_cumulative_and_sorted() {
    let (_d, c, p) = setup();
    // inserted out of order
    ins(
        &c,
        Ins {
            ts: 300,
            out: 1000,
            ..base("c")
        },
        0,
        0,
    );
    ins(
        &c,
        Ins {
            ts: 100,
            out: 1000,
            ..base("a")
        },
        0,
        0,
    );
    ins(
        &c,
        Ins {
            ts: 200,
            model: "mystery-1",
            out: 1000,
            ..base("b")
        },
        0,
        0,
    );
    ins(
        &c,
        Ins {
            ts: 50,
            sess: "other",
            out: 1000,
            ..base("d")
        },
        0,
        0,
    );
    let t = sessions::timeline(&c, &p, "claude", "s1").unwrap();
    assert_eq!(
        t.iter().map(|x| x.0).collect::<Vec<_>>(),
        vec![100, 200, 300]
    );
    approx(t[0].1, 0.05);
    approx(t[1].1, 0.05); // unpriced adds 0
    approx(t[2].1, 0.10);
    assert_eq!(
        sessions::timeline_detail(&c, &p, "claude", "s1")
            .unwrap()
            .unpriced_models,
        vec!["mystery-1".to_string()]
    );
    assert!(
        sessions::timeline(&c, &p, "claude", "nope")
            .unwrap()
            .is_empty()
    );
}

#[test]
fn efficiency_ratios() {
    let (_d, c, p) = setup();
    let f = Filter::default();
    let z = cost::efficiency(&c, &p, &f).unwrap();
    assert_eq!((z.cache_hit_ratio, z.subagent_cost_share), (0.0, 0.0));
    ins(
        &c,
        Ins {
            out: 1000,
            ..base("a")
        },
        100,
        300,
    ); // 0.05 + 0.001
    ins(
        &c,
        Ins {
            out: 3000,
            side: 1,
            ..base("b")
        },
        0,
        0,
    ); // 0.15 sidechain
    let e = cost::efficiency(&c, &p, &f).unwrap();
    approx(e.cache_hit_ratio, 0.75);
    let total = 0.05 + 100.0 * 10.0 / 1e6 + 300.0 * 0.25 / 1e6 + 0.15;
    approx(e.subagent_cost_share, 0.15 / total);
}

#[test]
fn by_project_and_sessions_ordering() {
    let (_d, c, p) = setup();
    ins(
        &c,
        Ins {
            project: "small",
            sess: "s1",
            ts: 10,
            out: 1000,
            ..base("a")
        },
        0,
        0,
    );
    ins(
        &c,
        Ins {
            project: "big",
            sess: "s2",
            ts: 20,
            out: 3000,
            ..base("b")
        },
        0,
        0,
    );
    ins(
        &c,
        Ins {
            project: "big",
            sess: "s2",
            ts: 30,
            out: 0,
            side: 1,
            model: "claude-sonnet-4-5",
            ..base("c")
        },
        0,
        0,
    );
    ins(
        &c,
        Ins {
            project: "tie",
            sess: "s3",
            ts: 99,
            out: 1000,
            ..base("d")
        },
        0,
        0,
    );
    c.execute("INSERT INTO sessions(vendor,session_id,project,title) VALUES('claude','s2','big','Fix bug')", []).unwrap();
    let f = Filter::default();
    let pr = cost::by_project(&c, &p, &f).unwrap();
    assert_eq!(
        pr.iter().map(|r| r.project.as_str()).collect::<Vec<_>>(),
        vec!["big", "small", "tie"]
    );
    assert_eq!((pr[0].sessions, pr[0].messages), (1, 2));
    let s = sessions::list(&c, &p, &Config::default(), &f, 10).unwrap();
    assert_eq!(
        s.iter().map(|r| r.session_id.as_str()).collect::<Vec<_>>(),
        vec!["s2", "s3", "s1"]
    ); // tie: last_ts desc
    assert_eq!(s[0].title.as_deref(), Some("Fix bug"));
    assert_eq!(s[1].title, None);
    assert_eq!(
        (
            s[0].messages,
            s[0].subagent_messages,
            s[0].first_ts,
            s[0].last_ts
        ),
        (2, 1, 20, 30)
    );
    assert_eq!(s[0].account, "claude");
    assert_eq!(
        sessions::list(&c, &p, &Config::default(), &f, 2)
            .unwrap()
            .len(),
        2
    );
    let v = cost::by_vendor(&c, &p, &f).unwrap();
    assert_eq!((v.len(), v[0].messages), (1, 4));
}

#[test]
fn session_models_sorted_by_row_count_and_grouped_by_vendor() {
    let (_d, c, p) = setup();
    ins(
        &c,
        Ins {
            model: "claude-sonnet-4-5",
            ts: 1,
            ..base("a")
        },
        1,
        0,
    );
    ins(&c, Ins { ts: 2, ..base("b") }, 1, 0);
    ins(&c, Ins { ts: 3, ..base("c") }, 1, 0);
    // same session id under another vendor is a separate session
    ins(
        &c,
        Ins {
            vendor: "codex",
            ts: 4,
            ..base("d")
        },
        1,
        0,
    );
    let s = sessions::list(&c, &p, &Config::default(), &Filter::default(), 10).unwrap();
    assert_eq!(s.len(), 2);
    let claude = s.iter().find(|r| r.vendor == "claude").unwrap();
    assert_eq!(
        claude.models,
        vec![FABLE.to_string(), "claude-sonnet-4-5".to_string()]
    );
    let f = Filter {
        vendor: Some("codex".into()),
        ..Default::default()
    };
    assert_eq!(
        sessions::list(&c, &p, &Config::default(), &f, 10)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn totals_display_rule() {
    use agent_usage::query::cost::Totals;
    let t = |cost: f64, unpriced: &[&str], messages: u64| Totals {
        cost,
        unpriced_models: unpriced.iter().map(|s| s.to_string()).collect(),
        messages,
        ..Default::default()
    };
    for (tot, want) in [
        (t(12.0, &[], 3), "$12.00"),
        (t(1.0, &["mystery"], 3), "≥ $1.00"),
        (t(0.0, &["mystery"], 2), "n/a"),
        (t(0.0, &[], 0), "$0.00"),
    ] {
        assert_eq!(tot.display(), want, "{tot:?}");
    }
}
