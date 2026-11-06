use std::path::Path;
use std::process::Command;
use tokenscope::adapters::{Adapter, CollectCtx, Discovery, claude::ClaudeAdapter};
use tokenscope::collect::run_all;
use tokenscope::model::{Batch, Vendor};
use tokenscope::{config::Config, db, paths::Paths};

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let t = to.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_dir(&e.path(), &t);
        } else {
            std::fs::copy(e.path(), t).unwrap();
        }
    }
}

fn home() -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    copy_dir(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/claude-home"),
        d.path(),
    );
    d
}

fn run(home: &Path, args: &[&str]) -> (bool, String) {
    let o = Command::new(env!("CARGO_BIN_EXE_tokenscope"))
        .args(args)
        .env("HOME", home)
        .output()
        .unwrap();
    (
        o.status.success(),
        String::from_utf8_lossy(&o.stdout).into_owned(),
    )
}

#[test]
fn doctor_reports_missing_sources() {
    let h = home();
    let (ok, out) = run(h.path(), &["doctor"]);
    assert!(ok);
    let line = |v: &str| {
        out.lines()
            .find(|l| l.starts_with(v))
            .unwrap_or_else(|| panic!("no {v} line in {out}"))
            .to_string()
    };
    assert!(line("claude").contains(" found") && !line("claude").contains("not found"));
    assert!(line("codex").contains("not found"));
}

struct Failing(bool);
impl Adapter for Failing {
    fn vendor(&self) -> Vendor {
        Vendor::Codex
    }
    fn discover(&self, _: &Paths) -> Discovery {
        Discovery {
            found: true,
            detail: "fake".into(),
        }
    }
    fn collect(&self, _: &mut CollectCtx) -> anyhow::Result<Batch> {
        if self.0 {
            panic!("boom")
        } else {
            anyhow::bail!("bad source")
        }
    }
}

#[test]
fn adapter_error_does_not_stop_others() {
    for panics in [false, true] {
        let h = home();
        let paths = Paths {
            home: h.path().to_path_buf(),
        };
        let mut conn = db::open(&h.path().join("t.db")).unwrap();
        let adapters: Vec<Box<dyn Adapter>> =
            vec![Box::new(Failing(panics)), Box::new(ClaudeAdapter)];
        let st = run_all(&mut conn, &paths, &Config::default(), &adapters);
        assert!(st[0].error.as_deref().unwrap().contains(if panics {
            "boom"
        } else {
            "bad source"
        }));
        assert!(st[1].error.is_none());
        assert_eq!(st[1].records, 4); // batch rows; the db dedupes msg_1 to 3
        let n: i64 = conn
            .query_row("SELECT count(*) FROM usage", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 3);
        let kv: i64 = conn
            .query_row(
                "SELECT count(*) FROM kv WHERE key LIKE 'status:%'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(kv, 2);
        assert!(h.path().join(".tokenscope/today.txt").exists());
    }
}

#[test]
fn status_prints_totals() {
    let h = home();
    let (ok, out) = run(h.path(), &["status"]);
    assert!(ok, "{out}");
    // the fixture has an unpriced model (claude-sonnet-4-5): totals are lower bounds
    assert!(out.contains("tất cả ≥ $"), "{out}");
    assert!(out.contains("n/a (no price): claude-sonnet-4-5"), "{out}");
    let today = std::fs::read_to_string(h.path().join(".tokenscope/today.txt")).unwrap();
    let (date, amt) = today.split_once(' ').unwrap();
    assert_eq!(date, chrono::Local::now().format("%Y-%m-%d").to_string());
    // fixture rows are dated 2026-11-05: today's text is a lower bound, or $0.00 on later days
    assert!(amt.starts_with("≥ $") || amt == "$0.00", "{today}");
    let (_, j) = run(h.path(), &["status", "--json"]);
    let v: serde_json::Value = serde_json::from_str(&j).unwrap();
    assert_eq!(v["all_time"]["messages"], 3);
}

fn walk(d: &Path) -> Vec<std::path::PathBuf> {
    let mut v = vec![];
    for e in std::fs::read_dir(d).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            v.extend(walk(&p))
        } else {
            v.push(p)
        }
    }
    v
}

#[test]
fn unreadable_file_is_counted_and_shown_by_doctor() {
    use std::os::unix::fs::PermissionsExt;
    let h = home();
    let dir = walk(&h.path().join(".claude/projects"))
        .pop()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let x = dir.join("x.jsonl");
    std::fs::write(&x, "{}\n").unwrap();
    std::fs::set_permissions(&x, std::fs::Permissions::from_mode(0o000)).unwrap();
    if std::fs::File::open(&x).is_ok() {
        return; // running as root: chmod 000 does not block reads
    }
    let paths = Paths {
        home: h.path().to_path_buf(),
    };
    let mut conn = db::open(&h.path().join("t.db")).unwrap();
    let st = run_all(
        &mut conn,
        &paths,
        &Config::default(),
        &[Box::new(ClaudeAdapter)],
    );
    assert!(st[0].error.is_none());
    assert!(st[0].detail.ends_with(", 1 unreadable"), "{}", st[0].detail);
    assert_eq!(st[0].records, 4);
    let (_, out) = run(h.path(), &["collect"]);
    assert!(out.contains("1 unreadable"), "{out}");
    let (_, out) = run(h.path(), &["doctor"]);
    assert!(
        out.lines()
            .find(|l| l.starts_with("claude"))
            .unwrap()
            .contains("1 unreadable"),
        "{out}"
    );
}

struct BadWrite;
impl Adapter for BadWrite {
    fn vendor(&self) -> Vendor {
        Vendor::Codex
    }
    fn discover(&self, _: &Paths) -> Discovery {
        Discovery {
            found: true,
            detail: "fake".into(),
        }
    }
    fn collect(&self, ctx: &mut CollectCtx) -> anyhow::Result<Batch> {
        ctx.done_files.push(tokenscope::scan::FileStamp {
            path: "/x/y.jsonl".into(),
            size: 1,
            mtime: 2,
        });
        // a write cannot succeed once the table is gone
        ctx.conn.execute_batch("DROP TABLE usage")?;
        let mut b = Batch::default();
        b.usage.push(tokenscope::model::UsageRecord {
            msg_key: "k".into(),
            vendor: Vendor::Codex,
            account: String::new(),
            session_id: "s".into(),
            project: String::new(),
            ts: 0,
            model: "m".into(),
            input: 1,
            output: 1,
            reasoning: 0,
            cache_read: 0,
            cache_write_5m: 0,
            cache_write_1h: 0,
            sidechain: false,
            skill: None,
            plugin: None,
        });
        Ok(b)
    }
}

#[test]
fn files_not_marked_done_when_write_fails() {
    let h = home();
    let paths = Paths {
        home: h.path().to_path_buf(),
    };
    let mut conn = db::open(&h.path().join("t.db")).unwrap();
    let adapters: Vec<Box<dyn Adapter>> = vec![Box::new(BadWrite)];
    let st = run_all(&mut conn, &paths, &Config::default(), &adapters);
    assert!(st[0].error.is_some());
    assert_eq!(st[0].records, 0);
    let n: i64 = conn
        .query_row("SELECT count(*) FROM files", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 0);
}

fn usage_rec(key: &str, model: &str, input: u64) -> tokenscope::model::UsageRecord {
    tokenscope::model::UsageRecord {
        msg_key: key.into(),
        vendor: Vendor::Claude,
        account: String::new(),
        session_id: "s".into(),
        project: "p".into(),
        ts: chrono::Utc::now().timestamp(),
        model: model.into(),
        input,
        output: 0,
        reasoning: 0,
        cache_read: 0,
        cache_write_5m: 0,
        cache_write_1h: 0,
        sidechain: false,
        skill: None,
        plugin: None,
    }
}

/// Empty HOME whose DB already holds today's rows (no adapter finds anything).
fn seeded_home(rows: Vec<tokenscope::model::UsageRecord>) -> tempfile::TempDir {
    let h = tempfile::tempdir().unwrap();
    let paths = Paths {
        home: h.path().to_path_buf(),
    };
    let mut c = db::open(&paths.db()).unwrap();
    db::write_batch(
        &mut c,
        &Batch {
            usage: rows,
            ..Default::default()
        },
    )
    .unwrap();
    h
}

fn show(home: &Path) -> String {
    use std::io::Write;
    let mut ch = Command::new(env!("CARGO_BIN_EXE_tokenscope"))
        .args(["statusline", "--show"])
        .env("HOME", home)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    ch.stdin.take().unwrap().write_all(b"{}").unwrap();
    String::from_utf8_lossy(&ch.wait_with_output().unwrap().stdout).into_owned()
}

#[test]
fn partly_and_fully_unpriced_today_never_shows_zero() {
    // claude-fable-5-1 input is $10/M -> 100k tokens = $1.00
    let h = seeded_home(vec![
        usage_rec("a", "claude-fable-5-1", 100_000),
        usage_rec("b", "mystery-model", 5),
    ]);
    let (ok, out) = run(h.path(), &["status"]);
    assert!(ok, "{out}");
    assert!(out.contains("hôm nay ≥ $1.00"), "{out}");
    assert!(show(h.path()).contains("≥ $1.00 hôm nay"));

    let h = seeded_home(vec![usage_rec("b", "mystery-model", 5)]);
    let (ok, out) = run(h.path(), &["status"]);
    assert!(ok, "{out}");
    assert!(out.contains("hôm nay n/a"), "{out}");
    assert!(show(h.path()).contains("n/a hôm nay"));
}

#[test]
fn status_lists_unavailable_vendor() {
    let h = seeded_home(vec![]);
    let oc = h.path().join(".local/share/opencode/opencode.db");
    std::fs::create_dir_all(oc.parent().unwrap()).unwrap();
    std::fs::write(&oc, b"not a database at all, just text").unwrap();
    let (ok, out) = run(h.path(), &["status"]);
    assert!(ok, "{out}");
    let line = out
        .lines()
        .find(|l| l.starts_with("opencode: unavailable ("))
        .unwrap_or_else(|| panic!("no unavailable line in {out}"));
    assert!(line.ends_with(')'), "{line}");
    assert!(out.contains("hôm nay $0.00"), "{out}");
}

#[test]
fn doctor_shows_stored_totals_and_last_limit() {
    let h = seeded_home(vec![
        usage_rec("a", "claude-fable-5-1", 1),
        usage_rec("b", "claude-fable-5-1", 2),
    ]);
    let at = 1_800_000_000i64;
    {
        let c = db::open(&h.path().join(".tokenscope/tokenscope.db")).unwrap();
        c.execute(
            "INSERT INTO limit_snapshots(vendor,account,scope,window_minutes,used_percent,resets_at,plan_type,measured_at,source) VALUES('claude','','five_hour',300,10,NULL,NULL,?1,'t')",
            [at],
        )
        .unwrap();
    }
    // a collect run finds no Claude logs in this HOME: 0 new records, 2 stored
    assert!(run(h.path(), &["collect"]).0);
    let (ok, out) = run(h.path(), &["doctor"]);
    assert!(ok, "{out}");
    let header = out.lines().next().unwrap();
    assert!(
        header.contains("total") && header.contains("new") && header.contains("last limit"),
        "{header}"
    );
    let line = out.lines().find(|l| l.starts_with("claude")).unwrap();
    let when = chrono::DateTime::from_timestamp(at, 0)
        .unwrap()
        .with_timezone(&chrono::Local)
        .format("%Y-%m-%d %H:%M")
        .to_string();
    assert!(
        line.contains(&format!("{:>8} {:>6}  {when:<16}", 2, 0)),
        "{line}"
    );
    let codex = out.lines().find(|l| l.starts_with("codex")).unwrap();
    assert!(
        codex.contains(&format!("{:>8} {:>6}  {:<16}", 0, 0, "-")),
        "{codex}"
    );
}

/// Fails while the flag is set.
struct Toggle(std::sync::Arc<std::sync::atomic::AtomicBool>);
impl Adapter for Toggle {
    fn vendor(&self) -> Vendor {
        Vendor::Codex
    }
    fn discover(&self, _: &Paths) -> Discovery {
        Discovery {
            found: true,
            detail: "fake".into(),
        }
    }
    fn collect(&self, _: &mut CollectCtx) -> anyhow::Result<Batch> {
        if self.0.load(std::sync::atomic::Ordering::SeqCst) {
            anyhow::bail!("bad source")
        }
        Ok(Batch::default())
    }
}

#[test]
fn collector_logs_adapter_error_once_per_streak() {
    use std::sync::atomic::Ordering;
    use tokenscope::collect::Collector;
    let h = tempfile::tempdir().unwrap();
    let paths = Paths {
        home: h.path().to_path_buf(),
    };
    let fail = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let mut c = Collector::with_adapters(
        paths,
        Config::default(),
        vec![Box::new(Toggle(fail.clone()))],
    );
    for _ in 0..3 {
        assert!(c.tick().ok());
    }
    let log = c.take_log();
    assert_eq!(log.len(), 1, "{log:?}");
    assert!(
        log[0].contains("codex") && log[0].contains("bad source"),
        "{log:?}"
    );
    fail.store(false, Ordering::SeqCst);
    c.tick();
    c.tick();
    let log = c.take_log();
    assert_eq!(log.len(), 1, "{log:?}");
    assert!(
        log[0].contains("codex") && log[0].contains("ok again"),
        "{log:?}"
    );
    fail.store(true, Ordering::SeqCst);
    c.tick();
    assert_eq!(c.take_log().len(), 1, "a new streak is logged again");
}

#[test]
fn collect_watch_loops_and_logs_errors_once() {
    let h = home();
    let oc = h.path().join(".local/share/opencode/opencode.db");
    std::fs::create_dir_all(oc.parent().unwrap()).unwrap();
    std::fs::write(&oc, b"not a database at all, just text").unwrap();
    let mut ch = Command::new(env!("CARGO_BIN_EXE_tokenscope"))
        .args(["collect", "--watch", "--interval", "1"])
        .env("HOME", h.path())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    // ~2 failing ticks, then the source disappears and later ticks recover
    std::thread::sleep(std::time::Duration::from_millis(1600));
    std::fs::remove_file(&oc).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(2000));
    let _ = ch.kill();
    let o = ch.wait_with_output().unwrap();
    let err = String::from_utf8_lossy(&o.stderr);
    let oc_lines: Vec<_> = err.lines().filter(|l| l.contains("opencode")).collect();
    assert_eq!(oc_lines.len(), 2, "error once, recovery once:\n{err}");
    assert!(!oc_lines[0].contains("ok again"), "{err}");
    assert!(oc_lines[1].ends_with("opencode: ok again"), "{err}");
    let c = db::open(&h.path().join(".tokenscope/tokenscope.db")).unwrap();
    let n: i64 = c
        .query_row("SELECT count(*) FROM usage", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 3, "claude rows collected");
}
