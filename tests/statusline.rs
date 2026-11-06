use agent_usage::adapters::{Adapter, claude_snapshots::ClaudeSnapshotsAdapter};
use agent_usage::collect::run_all;
use agent_usage::{config::Config, db, paths::Paths};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const STDIN: &str = r#"{"rate_limits":{"five_hour":{"used_percentage":44,"resets_at":1792996200},"seven_day":{"used_percentage":54,"resets_at":1793500000}}}"#;

fn run(home: &Path, args: &[&str], stdin: &str, env: &[(&str, &str)]) -> (bool, String, Duration) {
    let t = Instant::now();
    let mut c = Command::new(env!("CARGO_BIN_EXE_agent-usage"));
    c.args(args)
        .env("HOME", home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped());
    for (k, v) in env {
        c.env(k, v);
    }
    let mut ch = c.spawn().unwrap();
    ch.stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    let o = ch.wait_with_output().unwrap();
    (
        o.status.success(),
        String::from_utf8_lossy(&o.stdout).into_owned(),
        t.elapsed(),
    )
}

fn snaps(home: &Path) -> Vec<std::path::PathBuf> {
    match std::fs::read_dir(home.join(".agent-usage/snapshots")) {
        Ok(rd) => rd.map(|e| e.unwrap().path()).collect(),
        Err(_) => vec![],
    }
}

#[test]
fn wraps_and_writes_snapshot() {
    let h = tempfile::tempdir().unwrap();
    let (ok, out, _) = run(h.path(), &["statusline", "--wrap", "echo OLD"], STDIN, &[]);
    assert!(ok);
    assert_eq!(out, "OLD\n");
    let s = snaps(h.path());
    assert_eq!(s.len(), 1);
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&s[0]).unwrap()).unwrap();
    assert_eq!(v["five_hour"]["used_percent"], 44.0);
    assert_eq!(v["five_hour"]["resets_at"], 1792996200);
    assert_eq!(v["seven_day"]["used_percent"], 54.0);
    assert_eq!(v["account"], "default");
}

#[test]
fn account_flag_beats_env() {
    let h = tempfile::tempdir().unwrap();
    run(
        h.path(),
        &["statusline", "--account", "flag"],
        STDIN,
        &[("AGENT_USAGE_ACCOUNT", "env")],
    );
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&snaps(h.path())[0]).unwrap()).unwrap();
    assert_eq!(v["account"], "flag");
    let h = tempfile::tempdir().unwrap();
    run(
        h.path(),
        &["statusline"],
        STDIN,
        &[("AGENT_USAGE_ACCOUNT", "env")],
    );
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&snaps(h.path())[0]).unwrap()).unwrap();
    assert_eq!(v["account"], "env");
}

#[test]
fn hanging_wrapped_command_times_out() {
    let h = tempfile::tempdir().unwrap();
    let (ok, out, d) = run(h.path(), &["statusline", "--wrap", "sleep 5"], STDIN, &[]);
    assert!(ok);
    assert_eq!(out, "");
    assert!(d < Duration::from_millis(1500), "{d:?}");
    // a script that ignores stdin and spawns a grandchild holding stdout
    let sh = h.path().join("hang.sh");
    std::fs::write(&sh, "sleep 5 &\nsleep 5\n").unwrap();
    let (ok, out, d) = run(
        h.path(),
        &["statusline", "--wrap-file", sh.to_str().unwrap()],
        STDIN,
        &[],
    );
    assert!(ok);
    assert_eq!(out, "");
    assert!(d < Duration::from_millis(1500), "{d:?}");
}

#[test]
fn child_ignoring_large_stdin_does_not_deadlock() {
    let h = tempfile::tempdir().unwrap();
    let big = format!("{{\"pad\":\"{}\"}}", "x".repeat(2_000_000));
    let (ok, out, d) = run(h.path(), &["statusline", "--wrap", "echo OLD"], &big, &[]);
    assert!(ok);
    assert_eq!(out, "OLD\n");
    assert!(d < Duration::from_millis(1500), "{d:?}");
}

#[test]
fn unwritable_snapshot_dir_still_prints() {
    let h = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(h.path().join(".agent-usage")).unwrap();
    std::fs::write(h.path().join(".agent-usage/snapshots"), "not a dir").unwrap();
    let (ok, out, _) = run(h.path(), &["statusline", "--wrap", "echo OLD"], STDIN, &[]);
    assert!(ok);
    assert_eq!(out, "OLD\n");
}

#[test]
fn missing_rate_limits_writes_nothing() {
    let h = tempfile::tempdir().unwrap();
    let (ok, out, _) = run(h.path(), &["statusline", "--wrap", "echo OLD"], "{}", &[]);
    assert!(ok);
    assert_eq!(out, "OLD\n");
    assert!(snaps(h.path()).is_empty());
    let (ok, _, _) = run(h.path(), &["statusline"], "not json", &[]);
    assert!(ok);
    assert!(snaps(h.path()).is_empty());
}

fn write_today(home: &Path, date: &str) {
    std::fs::create_dir_all(home.join(".agent-usage")).unwrap();
    std::fs::write(
        home.join(".agent-usage/today.txt"),
        format!("{date} $12.00"),
    )
    .unwrap();
}

#[test]
fn show_appends_today_cost_on_new_line() {
    let h = tempfile::tempdir().unwrap();
    write_today(
        h.path(),
        &chrono::Local::now().format("%Y-%m-%d").to_string(),
    );
    let (_, out, _) = run(
        h.path(),
        &["statusline", "--wrap", "echo OLD", "--show"],
        STDIN,
        &[],
    );
    assert_eq!(out, "OLD\n5h 44% · tuần 54% · $12.00 hôm nay\n");
    // wrapped output without trailing newline still gets its own line
    let (_, out, _) = run(
        h.path(),
        &["statusline", "--wrap", "printf OLD", "--show"],
        STDIN,
        &[],
    );
    assert_eq!(out, "OLD\n5h 44% · tuần 54% · $12.00 hôm nay\n");
}

#[test]
fn show_omits_money_when_stale_or_missing() {
    let h = tempfile::tempdir().unwrap();
    let (_, out, _) = run(h.path(), &["statusline", "--show"], STDIN, &[]);
    assert_eq!(out, "5h 44% · tuần 54%\n");
    write_today(h.path(), "2000-01-01");
    let (_, out, _) = run(h.path(), &["statusline", "--show"], STDIN, &[]);
    assert_eq!(out, "5h 44% · tuần 54%\n");
}

#[test]
fn show_with_partial_limits() {
    let h = tempfile::tempdir().unwrap();
    let (_, out, _) = run(
        h.path(),
        &["statusline", "--show"],
        r#"{"rate_limits":{"seven_day":{"used_percentage":7.4,"resets_at":1}}}"#,
        &[],
    );
    assert_eq!(out, "tuần 7%\n");
}

#[test]
fn snapshots_ingested_and_removed() {
    let h = tempfile::tempdir().unwrap();
    run(h.path(), &["statusline"], STDIN, &[]);
    assert_eq!(snaps(h.path()).len(), 1);
    let paths = Paths {
        home: h.path().to_path_buf(),
    };
    let mut conn = db::open(&paths.db()).unwrap();
    let ads: Vec<Box<dyn Adapter>> = vec![Box::new(ClaudeSnapshotsAdapter)];
    let st = run_all(&mut conn, &paths, &Config::default(), &ads);
    assert!(st[0].error.is_none(), "{:?}", st[0].error);
    let mut stmt = conn
        .prepare("SELECT vendor,account,scope,window_minutes,used_percent,resets_at,source FROM limit_snapshots ORDER BY scope")
        .unwrap();
    let rows: Vec<(String, String, String, i64, f64, i64, String)> = stmt
        .query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
            ))
        })
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    let d = |s: &str, w, p, r| {
        (
            "claude".to_string(),
            "default".to_string(),
            s.to_string(),
            w,
            p,
            r,
            "statusline".to_string(),
        )
    };
    assert_eq!(
        rows,
        vec![
            d("five_hour", 300, 44.0, 1792996200),
            d("weekly", 10080, 54.0, 1793500000)
        ]
    );
    assert!(snaps(h.path()).is_empty());
}

#[test]
fn default_adapters_include_snapshots() {
    let ads = agent_usage::collect::default_adapters();
    assert!(
        ads.iter()
            .any(|a| a.vendor() == agent_usage::model::Vendor::Claude && a.is_auxiliary())
    );
}

#[test]
fn print_snippet_copies_command_verbatim() {
    let h = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(h.path().join(".claude")).unwrap();
    let cmd = "input=$(cat); echo \"it's $(date +%Y) \\\"q\\\"\"; echo 'single $x' | tr a-z A-Z\nprintf '%s' \"$input\" | wc -c";
    let settings =
        serde_json::json!({"model": "x", "statusLine": {"type": "command", "command": cmd}});
    let sp = h.path().join(".claude/settings.json");
    std::fs::write(&sp, settings.to_string()).unwrap();
    let before = std::fs::read(&sp).unwrap();
    let (ok, out, _) = run(h.path(), &["statusline", "--print-snippet"], "", &[]);
    assert!(ok);
    assert_eq!(std::fs::read(&sp).unwrap(), before);
    let sh = h.path().join(".agent-usage/wrapped-statusline.sh");
    assert_eq!(std::fs::read_to_string(&sh).unwrap(), cmd);
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        std::fs::metadata(&sh).unwrap().permissions().mode() & 0o777,
        0o755
    );
    let json_start = out.find('{').unwrap();
    let json_end = out.rfind('}').unwrap();
    let snip: serde_json::Value = serde_json::from_str(&out[json_start..=json_end]).unwrap();
    assert_eq!(snip["statusLine"]["type"], "command");
    assert_eq!(
        snip["statusLine"]["command"],
        "agent-usage statusline --wrap-file ~/.agent-usage/wrapped-statusline.sh --show"
    );
    // running the wrapper reproduces the original command's output
    let mut direct = Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    direct
        .stdin
        .take()
        .unwrap()
        .write_all(STDIN.as_bytes())
        .unwrap();
    let want = String::from_utf8(direct.wait_with_output().unwrap().stdout).unwrap();
    let (_, got, _) = run(
        h.path(),
        &["statusline", "--wrap-file", sh.to_str().unwrap()],
        STDIN,
        &[],
    );
    assert_eq!(got, want);
    assert!(!want.is_empty());
    // re-running after the user pasted our wrapper must not overwrite the saved command
    let pasted = serde_json::json!({"statusLine": {"type": "command", "command": snip["statusLine"]["command"]}});
    std::fs::write(&sp, pasted.to_string()).unwrap();
    run(h.path(), &["statusline", "--print-snippet"], "", &[]);
    assert_eq!(std::fs::read_to_string(&sh).unwrap(), cmd);
}

#[test]
fn print_snippet_without_existing_command() {
    let h = tempfile::tempdir().unwrap();
    let (ok, out, _) = run(h.path(), &["statusline", "--print-snippet"], "", &[]);
    assert!(ok);
    assert!(out.contains("agent-usage statusline --show"), "{out}");
    assert!(!out.contains("--wrap"), "{out}");
    assert!(!h.path().join(".agent-usage/wrapped-statusline.sh").exists());
}

#[test]
fn same_minute_runs_overwrite_one_file() {
    let h = tempfile::tempdir().unwrap();
    let a = r#"{"rate_limits":{"five_hour":{"used_percentage":10,"resets_at":1}}}"#;
    let b = r#"{"rate_limits":{"five_hour":{"used_percentage":20,"resets_at":1}}}"#;
    // retry if the two runs straddle a minute boundary
    for _ in 0..3 {
        let _ = std::fs::remove_dir_all(h.path().join(".agent-usage/snapshots"));
        run(h.path(), &["statusline"], a, &[]);
        run(h.path(), &["statusline"], b, &[]);
        let s = snaps(h.path());
        if s.len() == 1 {
            let v: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(&s[0]).unwrap()).unwrap();
            assert_eq!(v["five_hour"]["used_percent"], 20.0);
            return;
        }
    }
    panic!("never got a single file");
}

#[test]
fn bad_args_still_exit_zero() {
    let h = tempfile::tempdir().unwrap();
    let (ok, out, _) = run(h.path(), &["statusline", "--bogus"], STDIN, &[]);
    assert!(ok);
    assert_eq!(out, "");
}

#[test]
fn corrupt_snapshot_counted_once_then_removed() {
    let h = tempfile::tempdir().unwrap();
    let d = h.path().join(".agent-usage/snapshots");
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join("claude-bad-1.json"), "{nope").unwrap();
    std::fs::write(
        d.join("claude-nomeasure-1.json"),
        r#"{"five_hour":{"used_percent":1}}"#,
    )
    .unwrap();
    let paths = Paths {
        home: h.path().to_path_buf(),
    };
    let mut conn = db::open(&paths.db()).unwrap();
    let ads: Vec<Box<dyn Adapter>> = vec![Box::new(ClaudeSnapshotsAdapter)];
    let st = run_all(&mut conn, &paths, &Config::default(), &ads);
    assert!(st[0].detail.ends_with(", 2 unreadable"), "{}", st[0].detail);
    assert!(snaps(h.path()).is_empty());
    let st = run_all(&mut conn, &paths, &Config::default(), &ads);
    assert!(!st[0].found);
}

#[test]
fn print_snippet_does_not_read_settings_symlinked_to_credentials() {
    let h = tempfile::tempdir().unwrap();
    let dir = h.path().join(".claude");
    std::fs::create_dir_all(&dir).unwrap();
    let cred = dir.join(".credentials.json");
    std::fs::write(&cred, r#"{"statusLine":{"command":"echo SECRET"}}"#).unwrap();
    std::os::unix::fs::symlink(&cred, dir.join("settings.json")).unwrap();
    let out = agent_usage::statusline::print_snippet(&Paths {
        home: h.path().to_path_buf(),
    });
    assert!(!out.contains("--wrap-file"), "{out}");
    assert!(!h.path().join(".agent-usage/wrapped-statusline.sh").exists());
}

#[test]
fn snapshot_symlinked_to_forbidden_name_is_not_read() {
    let h = tempfile::tempdir().unwrap();
    let paths = Paths {
        home: h.path().to_path_buf(),
    };
    let dir = paths.snapshots_dir();
    std::fs::create_dir_all(&dir).unwrap();
    let secret = h.path().join("oauth-x.json");
    std::fs::write(
        &secret,
        r#"{"account":"a","measured_at":1800000000,"five_hour":{"used_percent":5,"resets_at":null}}"#,
    )
    .unwrap();
    std::os::unix::fs::symlink(&secret, dir.join("claude-x-202611050000.json")).unwrap();
    let mut conn = db::open(&paths.db()).unwrap();
    let ads: Vec<Box<dyn Adapter>> = vec![Box::new(ClaudeSnapshotsAdapter)];
    let st = run_all(&mut conn, &paths, &Config::default(), &ads);
    let n: i64 = conn
        .query_row("SELECT count(*) FROM limit_snapshots", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 0, "{:?}", st[0]);
    assert!(st[0].detail.contains("1 unreadable"), "{:?}", st[0]);
    assert!(secret.exists(), "the link target is never touched");
}
