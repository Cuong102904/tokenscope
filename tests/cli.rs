use agent_usage::adapters::{Adapter, CollectCtx, Discovery, claude::ClaudeAdapter};
use agent_usage::collect::run_all;
use agent_usage::model::{Batch, Vendor};
use agent_usage::{config::Config, db, paths::Paths};
use std::path::Path;
use std::process::Command;

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
    copy_dir(&Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/claude-home"), d.path());
    d
}

fn run(home: &Path, args: &[&str]) -> (bool, String) {
    let o = Command::new(env!("CARGO_BIN_EXE_agent-usage")).args(args).env("HOME", home).output().unwrap();
    (o.status.success(), String::from_utf8_lossy(&o.stdout).into_owned())
}

#[test]
fn doctor_reports_missing_sources() {
    let h = home();
    let (ok, out) = run(h.path(), &["doctor"]);
    assert!(ok);
    let line = |v: &str| out.lines().find(|l| l.starts_with(v)).unwrap_or_else(|| panic!("no {v} line in {out}")).to_string();
    assert!(line("claude").contains(" found") && !line("claude").contains("not found"));
    assert!(line("codex").contains("not found"));
}

struct Failing(bool);
impl Adapter for Failing {
    fn vendor(&self) -> Vendor {
        Vendor::Codex
    }
    fn discover(&self, _: &Paths) -> Discovery {
        Discovery { found: true, detail: "fake".into() }
    }
    fn collect(&self, _: &mut CollectCtx) -> anyhow::Result<Batch> {
        if self.0 { panic!("boom") } else { anyhow::bail!("bad source") }
    }
}

#[test]
fn adapter_error_does_not_stop_others() {
    for panics in [false, true] {
        let h = home();
        let paths = Paths { home: h.path().to_path_buf() };
        let mut conn = db::open(&h.path().join("t.db")).unwrap();
        let adapters: Vec<Box<dyn Adapter>> = vec![Box::new(Failing(panics)), Box::new(ClaudeAdapter)];
        let st = run_all(&mut conn, &paths, &Config::default(), &adapters);
        assert!(st[0].error.as_deref().unwrap().contains(if panics { "boom" } else { "bad source" }));
        assert!(st[1].error.is_none());
        assert_eq!(st[1].records, 4); // batch rows; the db dedupes msg_1 to 3
        let n: i64 = conn.query_row("SELECT count(*) FROM usage", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 3);
        let kv: i64 = conn.query_row("SELECT count(*) FROM kv WHERE key LIKE 'status:%'", [], |r| r.get(0)).unwrap();
        assert_eq!(kv, 2);
        assert!(h.path().join(".agent-usage/today.txt").exists());
    }
}

#[test]
fn status_prints_totals() {
    let h = home();
    let (ok, out) = run(h.path(), &["status"]);
    assert!(ok, "{out}");
    let i = out.find("hôm nay $").expect(&out);
    assert!(out[i + "hôm nay $".len()..].chars().next().unwrap().is_ascii_digit());
    let today = std::fs::read_to_string(h.path().join(".agent-usage/today.txt")).unwrap();
    let (date, amt) = today.split_once(' ').unwrap();
    assert_eq!(date, chrono::Local::now().format("%Y-%m-%d").to_string());
    assert!(amt.starts_with('$'), "{today}");
    let (_, j) = run(h.path(), &["status", "--json"]);
    let v: serde_json::Value = serde_json::from_str(&j).unwrap();
    assert_eq!(v["all_time"]["messages"], 3);
}

fn walk(d: &Path) -> Vec<std::path::PathBuf> {
    let mut v = vec![];
    for e in std::fs::read_dir(d).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() { v.extend(walk(&p)) } else { v.push(p) }
    }
    v
}

#[test]
fn unreadable_file_is_counted_and_shown_by_doctor() {
    use std::os::unix::fs::PermissionsExt;
    let h = home();
    let dir = walk(&h.path().join(".claude/projects")).pop().unwrap().parent().unwrap().to_path_buf();
    let x = dir.join("x.jsonl");
    std::fs::write(&x, "{}\n").unwrap();
    std::fs::set_permissions(&x, std::fs::Permissions::from_mode(0o000)).unwrap();
    if std::fs::File::open(&x).is_ok() {
        return; // running as root: chmod 000 does not block reads
    }
    let paths = Paths { home: h.path().to_path_buf() };
    let mut conn = db::open(&h.path().join("t.db")).unwrap();
    let st = run_all(&mut conn, &paths, &Config::default(), &[Box::new(ClaudeAdapter)]);
    assert!(st[0].error.is_none());
    assert!(st[0].detail.ends_with(", 1 unreadable"), "{}", st[0].detail);
    assert_eq!(st[0].records, 4);
    let (_, out) = run(h.path(), &["collect"]);
    assert!(out.contains("1 unreadable"), "{out}");
    let (_, out) = run(h.path(), &["doctor"]);
    assert!(out.lines().find(|l| l.starts_with("claude")).unwrap().contains("1 unreadable"), "{out}");
}

struct BadWrite;
impl Adapter for BadWrite {
    fn vendor(&self) -> Vendor {
        Vendor::Codex
    }
    fn discover(&self, _: &Paths) -> Discovery {
        Discovery { found: true, detail: "fake".into() }
    }
    fn collect(&self, ctx: &mut CollectCtx) -> anyhow::Result<Batch> {
        ctx.done_files.push(agent_usage::scan::FileStamp { path: "/x/y.jsonl".into(), size: 1, mtime: 2 });
        // a write cannot succeed once the table is gone
        ctx.conn.execute_batch("DROP TABLE usage")?;
        let mut b = Batch::default();
        b.usage.push(agent_usage::model::UsageRecord {
            msg_key: "k".into(), vendor: Vendor::Codex, account: String::new(), session_id: "s".into(),
            project: String::new(), ts: 0, model: "m".into(), input: 1, output: 1, reasoning: 0,
            cache_read: 0, cache_write_5m: 0, cache_write_1h: 0, sidechain: false, skill: None, plugin: None,
        });
        Ok(b)
    }
}

#[test]
fn files_not_marked_done_when_write_fails() {
    let h = home();
    let paths = Paths { home: h.path().to_path_buf() };
    let mut conn = db::open(&h.path().join("t.db")).unwrap();
    let adapters: Vec<Box<dyn Adapter>> = vec![Box::new(BadWrite)];
    let st = run_all(&mut conn, &paths, &Config::default(), &adapters);
    assert!(st[0].error.is_some());
    assert_eq!(st[0].records, 0);
    let n: i64 = conn.query_row("SELECT count(*) FROM files", [], |r| r.get(0)).unwrap();
    assert_eq!(n, 0);
}
