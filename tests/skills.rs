use agent_usage::adapters::{Adapter, CollectCtx, claude::ClaudeAdapter, codex::CodexAdapter};
use agent_usage::query::{Filter, skills};
use agent_usage::{config::Config, db, paths::Paths, pricing::PriceTable, scan};
use rusqlite::Connection;
use std::path::Path;

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

fn collect(ad: &dyn Adapter, conn: &mut Connection, paths: &Paths) {
    let cfg = Config::default();
    let mut ctx = CollectCtx {
        conn,
        paths,
        config: &cfg,
        done_files: vec![],
        unreadable: 0,
        remove_after_write: vec![],
    };
    let batch = ad.collect(&mut ctx).unwrap();
    let done = std::mem::take(&mut ctx.done_files);
    db::write_batch(conn, &batch).unwrap();
    scan::mark_done(conn, &done).unwrap();
}

fn setup() -> (tempfile::TempDir, Connection, PriceTable) {
    let d = tempfile::tempdir().unwrap();
    for h in ["claude-home", "codex-home"] {
        copy_dir(
            &Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures")
                .join(h),
            d.path(),
        );
    }
    let paths = Paths {
        home: d.path().to_path_buf(),
    };
    let mut conn = db::open(&d.path().join("t.db")).unwrap();
    collect(&ClaudeAdapter, &mut conn, &paths);
    collect(&CodexAdapter, &mut conn, &paths);
    let prices = PriceTable::load(&paths).unwrap();
    (d, conn, prices)
}

#[test]
fn skills_and_tools_from_fixtures() {
    let (_d, conn, prices) = setup();
    let all = Filter::default();

    let inv = skills::invocations(&conn, &all).unwrap();
    let claude: Vec<_> = inv.iter().filter(|r| r.vendor == "claude").collect();
    let codex: Vec<_> = inv.iter().filter(|r| r.vendor == "codex").collect();
    assert_eq!(
        (claude.len(), claude[0].kind.as_str(), claude[0].count),
        (1, "invoked", 1)
    );
    assert_eq!(
        (codex.len(), codex[0].kind.as_str(), codex[0].skill.as_str()),
        (1, "file_read", "foo")
    );

    let cost = skills::by_cost(&conn, &prices, &all).unwrap();
    let b = cost
        .iter()
        .find(|s| s.skill == "superpowers:brainstorming")
        .expect("brainstorming");
    assert!(b.cost.unwrap() > 0.0, "{b:?}");
    assert_eq!(b.plugin.as_deref(), Some("superpowers"));
    assert_eq!(b.messages, 1);

    let plugins = skills::by_plugin(&conn, &prices, &all).unwrap();
    assert_eq!(plugins[0].plugin, "superpowers");
    assert!(plugins[0].cost.unwrap() > 0.0);

    let tools = skills::tools(&conn, &all).unwrap();
    assert!(
        tools
            .iter()
            .any(|t| t.vendor == "codex" && t.tool == "exec_command" && t.count == 2)
    );
    assert!(tools.windows(2).all(|w| w[0].count >= w[1].count));
}

#[test]
fn filters_apply_to_events_and_cost() {
    let (_d, conn, prices) = setup();
    let codex = Filter {
        vendor: Some("codex".into()),
        ..Default::default()
    };
    assert!(
        skills::invocations(&conn, &codex)
            .unwrap()
            .iter()
            .all(|r| r.vendor == "codex")
    );
    assert!(skills::by_cost(&conn, &prices, &codex).unwrap().is_empty());
    assert!(
        skills::tools(&conn, &codex)
            .unwrap()
            .iter()
            .all(|r| r.vendor == "codex")
    );

    // project filter joins through usage on (vendor, session_id)
    let proj = Filter {
        project: Some("/home/u/proj".into()),
        ..Default::default()
    };
    assert!(!skills::invocations(&conn, &proj).unwrap().is_empty());
    let other = Filter {
        project: Some("/nowhere".into()),
        ..Default::default()
    };
    assert!(skills::invocations(&conn, &other).unwrap().is_empty());
    assert!(skills::tools(&conn, &other).unwrap().is_empty());
}

#[test]
fn unpriced_skill_cost_is_none_with_lower_bound() {
    let d = tempfile::tempdir().unwrap();
    let paths = Paths {
        home: d.path().to_path_buf(),
    };
    let prices = PriceTable::load(&paths).unwrap();
    let conn = db::open(&d.path().join("t.db")).unwrap();
    for (k, model) in [("a", "claude-fable-5-1"), ("b", "no-such-model")] {
        conn.execute(
            "INSERT INTO usage(msg_key,vendor,account,session_id,project,ts,day,model,input,output,reasoning,cache_read,cache_write_5m,cache_write_1h,sidechain,skill,plugin)
             VALUES(?1,'claude','','s','p',0,'2026-11-05',?2,1000,1000,0,0,0,0,0,'x:y','x')",
            rusqlite::params![k, model],
        ).unwrap();
    }
    let v = skills::by_cost(&conn, &prices, &Filter::default()).unwrap();
    assert_eq!(v.len(), 1);
    assert!(v[0].cost.is_none());
    assert!(v[0].priced_cost > 0.0);
    assert!(
        skills::by_plugin(&conn, &prices, &Filter::default()).unwrap()[0]
            .cost
            .is_none()
    );
}

fn mem() -> (tempfile::TempDir, Connection, PriceTable) {
    let d = tempfile::tempdir().unwrap();
    let paths = Paths {
        home: d.path().to_path_buf(),
    };
    let prices = PriceTable::load(&paths).unwrap();
    let conn = db::open(&d.path().join("t.db")).unwrap();
    (d, conn, prices)
}

fn usage(
    c: &Connection,
    key: &str,
    sess: &str,
    project: &str,
    model: &str,
    skill: &str,
    plugin: &str,
) {
    let day = chrono::Local::now().format("%Y-%m-%d").to_string();
    c.execute(
        "INSERT INTO usage(msg_key,vendor,account,session_id,project,ts,day,model,input,output,reasoning,cache_read,cache_write_5m,cache_write_1h,sidechain,skill,plugin)
         VALUES(?1,'claude','',?2,?3,0,?4,?5,1000,1000,0,0,0,0,0,?6,?7)",
        rusqlite::params![key, sess, project, day, model, skill, plugin],
    ).unwrap();
}

fn skill_ev(c: &Connection, key: &str, sess: &str, ts: i64, skill: &str) {
    c.execute("INSERT INTO skill_events(vendor,session_id,ts,skill,kind,key) VALUES('claude',?1,?2,?3,'invoked',?4)", rusqlite::params![sess, ts, skill, key]).unwrap();
    c.execute(
        "INSERT INTO tool_events(vendor,session_id,ts,tool,key) VALUES('claude',?1,?2,?3,?4)",
        rusqlite::params![sess, ts, format!("T-{skill}"), key],
    )
    .unwrap();
}

#[test]
fn days_filter_uses_ts_without_date_guard() {
    let (_d, conn, _p) = mem();
    let now = chrono::Utc::now().timestamp();
    skill_ev(&conn, "old", "s", now - 3 * 86400, "old-skill");
    skill_ev(&conn, "new", "s", now, "new-skill");
    let inv = skills::invocations(&conn, &Filter::days(1)).unwrap();
    assert_eq!(
        inv.iter().map(|r| r.skill.as_str()).collect::<Vec<_>>(),
        ["new-skill"]
    );
    let tl = skills::tools(&conn, &Filter::days(1)).unwrap();
    assert_eq!(
        tl.iter().map(|r| r.tool.as_str()).collect::<Vec<_>>(),
        ["T-new-skill"]
    );
    assert_eq!(
        skills::invocations(&conn, &Filter::days(7)).unwrap().len(),
        2
    );
}

#[test]
fn model_and_project_filters_change_results() {
    let (_d, conn, prices) = mem();
    let now = chrono::Utc::now().timestamp();
    usage(&conn, "a", "s1", "/a", "claude-fable-5-1", "p:one", "p");
    usage(&conn, "b", "s2", "/b", "claude-sonnet-5-5", "q:two", "q");
    skill_ev(&conn, "e1", "s1", now, "p:one");
    skill_ev(&conn, "e2", "s2", now, "q:two");
    let names = |f: &Filter| -> (Vec<String>, Vec<String>, Vec<String>, Vec<String>) {
        (
            skills::by_cost(&conn, &prices, f)
                .unwrap()
                .into_iter()
                .map(|r| r.skill)
                .collect(),
            skills::by_plugin(&conn, &prices, f)
                .unwrap()
                .into_iter()
                .map(|r| r.plugin)
                .collect(),
            skills::invocations(&conn, f)
                .unwrap()
                .into_iter()
                .map(|r| r.skill)
                .collect(),
            skills::tools(&conn, f)
                .unwrap()
                .into_iter()
                .map(|r| r.tool)
                .collect(),
        )
    };
    let m = Filter {
        model: Some("claude-fable-5-1".into()),
        ..Default::default()
    };
    assert_eq!(
        names(&m),
        (
            vec!["p:one".into()],
            vec!["p".into()],
            vec!["p:one".into()],
            vec!["T-p:one".into()]
        )
    );
    let p = Filter {
        project: Some("/b".into()),
        ..Default::default()
    };
    assert_eq!(
        names(&p),
        (
            vec!["q:two".into()],
            vec!["q".into()],
            vec!["q:two".into()],
            vec!["T-q:two".into()]
        )
    );
    assert_eq!(
        skills::by_cost(&conn, &prices, &Filter::default())
            .unwrap()
            .len(),
        2
    );
}
