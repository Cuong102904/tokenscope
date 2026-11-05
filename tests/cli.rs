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
    assert!(today.starts_with('$'));
    let (_, j) = run(h.path(), &["status", "--json"]);
    let v: serde_json::Value = serde_json::from_str(&j).unwrap();
    assert_eq!(v["all_time"]["messages"], 3);
}
