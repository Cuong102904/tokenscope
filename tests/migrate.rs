use std::fs;
use tokenscope::paths::Paths;

fn paths(h: &tempfile::TempDir) -> Paths {
    Paths {
        home: h.path().to_path_buf(),
    }
}

#[test]
fn moves_legacy_dir_and_renames_db_files() {
    let h = tempfile::tempdir().unwrap();
    let old = h.path().join(".agent-usage");
    fs::create_dir_all(old.join("snapshots")).unwrap();
    for f in [
        "agent-usage.db",
        "agent-usage.db-wal",
        "config.toml",
        "usage.db",
    ] {
        fs::write(old.join(f), f).unwrap();
    }
    let p = paths(&h);
    assert!(p.migrate_legacy().unwrap());
    assert!(!old.exists());
    assert_eq!(fs::read_to_string(p.db()).unwrap(), "agent-usage.db");
    let new = p.data_dir();
    assert_eq!(
        fs::read_to_string(new.join("tokenscope.db-wal")).unwrap(),
        "agent-usage.db-wal"
    );
    assert!(!new.join("tokenscope.db-shm").exists());
    assert_eq!(fs::read_to_string(p.config()).unwrap(), "config.toml");
    assert!(new.join("usage.db").exists() && p.snapshots_dir().is_dir());
    // second run is a no-op
    assert!(!p.migrate_legacy().unwrap());
}

#[test]
fn never_overwrites_existing_tokenscope_dir() {
    let h = tempfile::tempdir().unwrap();
    let p = paths(&h);
    fs::create_dir_all(h.path().join(".agent-usage")).unwrap();
    fs::write(h.path().join(".agent-usage/agent-usage.db"), "old").unwrap();
    fs::create_dir_all(p.data_dir()).unwrap();
    fs::write(p.db(), "new").unwrap();
    assert!(!p.migrate_legacy().unwrap());
    assert_eq!(fs::read_to_string(p.db()).unwrap(), "new");
    assert!(h.path().join(".agent-usage/agent-usage.db").exists());
}

#[test]
fn nothing_to_do_without_legacy_dir() {
    let h = tempfile::tempdir().unwrap();
    let p = paths(&h);
    assert!(!p.migrate_legacy().unwrap());
    assert!(!p.data_dir().exists());
}

#[test]
fn cli_migrates_on_first_run() {
    let h = tempfile::tempdir().unwrap();
    fs::create_dir_all(h.path().join(".agent-usage")).unwrap();
    fs::write(
        h.path().join(".agent-usage/config.toml"),
        "active_minutes = 7\n",
    )
    .unwrap();
    let o = std::process::Command::new(env!("CARGO_BIN_EXE_tokenscope"))
        .arg("doctor")
        .env("HOME", h.path())
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(String::from_utf8_lossy(&o.stderr).contains("moved ~/.agent-usage"));
    assert!(h.path().join(".tokenscope/config.toml").exists());
    assert!(!h.path().join(".agent-usage").exists());
}
