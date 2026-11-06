use std::path::Path;
use tokenscope::model::Vendor;
use tokenscope::open_sessions::{self, OpenSession};

/// One file of Claude's session registry, shaped like Claude Code 2.1 writes it.
fn entry(dir: &Path, pid: u32, session: &str, status: &str, start: &str) {
    std::fs::write(
        dir.join(format!("{pid}.json")),
        format!(
            r#"{{"pid":{pid},"sessionId":"{session}","cwd":"/home/u/{session}","startedAt":1800000000000,
               "procStart":"{start}","kind":"interactive","entrypoint":"cli","status":"{status}",
               "statusUpdatedAt":1800000123456,"updatedAt":1800000100000}}"#
        ),
    )
    .unwrap();
}

/// `(pid, recorded start time)` pairs the liveness check was asked about.
type Asked = Vec<(u32, Option<String>)>;

/// Alive when the recorded start time is "alive"; records which (pid, start) were asked.
fn read(dir: &Path) -> (Option<Vec<OpenSession>>, Asked) {
    let asked = std::cell::RefCell::new(vec![]);
    let alive = |pid: u32, start: Option<&str>| {
        asked.borrow_mut().push((pid, start.map(str::to_string)));
        start == Some("alive")
    };
    let mut got = open_sessions::claude(dir, &alive);
    if let Some(v) = got.as_mut() {
        v.sort_by_key(|s| s.pid);
    }
    let mut asked = asked.into_inner();
    asked.sort();
    (got, asked)
}

#[test]
fn reads_live_entries_with_state_and_time() {
    let d = tempfile::tempdir().unwrap();
    entry(d.path(), 101, "s-busy", "busy", "alive");
    entry(d.path(), 102, "s-idle", "idle", "alive");
    entry(d.path(), 103, "s-gone", "idle", "dead"); // process exited or pid reused
    let (got, asked) = read(d.path());
    assert_eq!(
        got.unwrap(),
        vec![
            OpenSession {
                vendor: Vendor::Claude,
                session_id: "s-busy".into(),
                cwd: "/home/u/s-busy".into(),
                pid: 101,
                busy: true,
                since: Some(1_800_000_123), // statusUpdatedAt is in ms
            },
            OpenSession {
                vendor: Vendor::Claude,
                session_id: "s-idle".into(),
                cwd: "/home/u/s-idle".into(),
                pid: 102,
                busy: false,
                since: Some(1_800_000_123),
            },
        ]
    );
    assert_eq!(
        asked,
        vec![
            (101, Some("alive".into())),
            (102, Some("alive".into())),
            (103, Some("dead".into()))
        ],
        "liveness is checked with the recorded start time"
    );
}

#[test]
fn missing_registry_means_unknown_and_empty_means_none_open() {
    let d = tempfile::tempdir().unwrap();
    assert_eq!(read(&d.path().join("nope")).0, None);
    assert_eq!(read(d.path()).0, Some(vec![]));
}

#[test]
fn never_opens_key_files_or_foreign_names() {
    let d = tempfile::tempdir().unwrap();
    entry(d.path(), 101, "s", "busy", "alive");
    // the .key file next to each entry is a secret; only `<digits>.json` names are read
    std::fs::write(d.path().join("101.abcdef.key"), "secret").unwrap();
    std::fs::write(d.path().join("notes.json"), r#"{"pid":7,"sessionId":"x"}"#).unwrap();
    std::fs::write(d.path().join("102.json.bak"), "{}").unwrap();
    let (got, asked) = read(d.path());
    assert_eq!(got.unwrap().len(), 1);
    assert_eq!(asked.len(), 1, "only 101.json was looked at: {asked:?}");
}

#[test]
fn skips_broken_mismatched_and_oversized_entries() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(d.path().join("201.json"), "{not json").unwrap();
    // pid inside differs from the file name
    std::fs::write(
        d.path().join("202.json"),
        r#"{"pid":999,"sessionId":"s","procStart":"alive"}"#,
    )
    .unwrap();
    // no session id
    std::fs::write(
        d.path().join("203.json"),
        r#"{"pid":203,"procStart":"alive"}"#,
    )
    .unwrap();
    let big = format!(
        r#"{{"pid":204,"sessionId":"s","procStart":"alive","pad":"{}"}}"#,
        "x".repeat(70 * 1024)
    );
    std::fs::write(d.path().join("204.json"), big).unwrap();
    assert_eq!(read(d.path()).0, Some(vec![]));
}

#[test]
fn one_session_in_two_processes_counts_once_and_busy_wins() {
    let d = tempfile::tempdir().unwrap();
    entry(d.path(), 301, "same", "idle", "alive");
    entry(d.path(), 302, "same", "busy", "alive");
    let got = read(d.path()).0.unwrap();
    assert_eq!(got.len(), 1);
    assert!(got[0].busy);
}

#[test]
fn print_mode_runs_are_not_open_sessions() {
    // `claude -p` (scripts, and tokenscope's own `/usage` refresh) registers too, with
    // entrypoint "sdk-cli"; a terminal session has "cli"
    let d = tempfile::tempdir().unwrap();
    entry(d.path(), 401, "terminal", "busy", "alive");
    std::fs::write(
        d.path().join("402.json"),
        r#"{"pid":402,"sessionId":"script","cwd":"/home/u/.tokenscope","procStart":"alive",
            "kind":"interactive","entrypoint":"sdk-cli","status":"busy"}"#,
    )
    .unwrap();
    let got = read(d.path()).0.unwrap();
    let ids: Vec<_> = got.iter().map(|s| s.session_id.as_str()).collect();
    assert_eq!(ids, ["terminal"]);
}

#[test]
fn start_time_is_field_22_even_with_odd_command_names() {
    // field 2 (comm) may hold spaces and parentheses
    let stat =
        "4242 (we ird) (name) S 1 4242 4242 0 -1 4194304 100 0 0 0 5 6 0 0 20 0 9 0 15651 1000 200";
    assert_eq!(open_sessions::proc_start_time(stat), Some("15651"));
    assert_eq!(open_sessions::proc_start_time("garbage"), None);
}

#[test]
fn proc_alive_checks_the_real_process_table() {
    let me = std::process::id();
    let stat = std::fs::read_to_string(format!("/proc/{me}/stat")).unwrap();
    let start = open_sessions::proc_start_time(&stat).unwrap().to_string();
    assert!(open_sessions::proc_alive(me, Some(&start)));
    assert!(open_sessions::proc_alive(me, None));
    assert!(
        !open_sessions::proc_alive(me, Some("1")),
        "same pid, other start time: a reused pid"
    );
    assert!(!open_sessions::proc_alive(u32::MAX - 1, None));
}
