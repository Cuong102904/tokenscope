use agent_usage::{db, scan};
use std::io::Write;
use std::path::Path;

fn conn(dir: &Path) -> rusqlite::Connection {
    db::open(&dir.join("t.db")).unwrap()
}

#[test]
fn partial_last_line_skipped_then_read() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("a.jsonl");
    std::fs::write(&p, "{\"x\":1}\n{\"a\":").unwrap();
    assert_eq!(scan::read_jsonl(&p).unwrap().len(), 1);
    let mut f = std::fs::OpenOptions::new().append(true).open(&p).unwrap();
    f.write_all(b"1}\n").unwrap();
    assert_eq!(scan::read_jsonl(&p).unwrap().len(), 2);
}

#[test]
fn malformed_middle_line_skipped_and_counted() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("a.jsonl");
    std::fs::write(&p, "{\"x\":1}\nnot json\n\n{\"y\":2}\n").unwrap();
    let (v, skipped) = scan::read_jsonl_counted(&p).unwrap();
    assert_eq!(v.len(), 2);
    assert_eq!(skipped, 1);
    // malformed last line WITH trailing newline is a real malformed line
    std::fs::write(&p, "{\"x\":1}\nbad\n").unwrap();
    assert_eq!(scan::read_jsonl_counted(&p).unwrap(), (vec![serde_json::json!({"x":1})], 1));
}

#[test]
fn unchanged_file_not_returned_twice() {
    let d = tempfile::tempdir().unwrap();
    let c = conn(d.path());
    let root = d.path().join("root");
    std::fs::create_dir_all(root.join("sub")).unwrap();
    let p = root.join("sub/s.jsonl");
    std::fs::write(&p, "{}\n").unwrap();
    std::fs::write(root.join("other.txt"), "x").unwrap();
    let got = scan::changed_files(&c, &root, "jsonl").unwrap();
    assert_eq!(got.iter().map(|s| s.path.clone()).collect::<Vec<_>>(), vec![p.clone()]);
    // not marked done inside changed_files
    assert_eq!(scan::changed_files(&c, &root, "jsonl").unwrap().len(), 1);
    scan::mark_done(&c, &got).unwrap();
    assert!(scan::changed_files(&c, &root, "jsonl").unwrap().is_empty());
    std::fs::write(&p, "{}\n{}\n").unwrap();
    assert_eq!(scan::changed_files(&c, &root, "jsonl").unwrap().len(), 1);
}

#[test]
fn missing_root_is_empty() {
    let d = tempfile::tempdir().unwrap();
    let c = conn(d.path());
    assert!(scan::changed_files(&c, Path::new("/không/tồn/tại"), "jsonl").unwrap().is_empty());
}

#[test]
fn forbidden_file_is_skipped() {
    let d = tempfile::tempdir().unwrap();
    let c = conn(d.path());
    std::fs::write(d.path().join("oauth.json"), "{}").unwrap();
    std::fs::write(d.path().join("ok.json"), "{}").unwrap();
    let got = scan::changed_files(&c, d.path(), "json").unwrap();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].path, d.path().join("ok.json"));
}

#[test]
fn append_after_scan_is_not_lost() {
    let d = tempfile::tempdir().unwrap();
    let c = conn(d.path());
    let p = d.path().join("s.jsonl");
    std::fs::write(&p, "{}\n").unwrap();
    let stamps = scan::changed_files(&c, d.path(), "jsonl").unwrap();
    assert_eq!(stamps.len(), 1);
    let mut f = std::fs::OpenOptions::new().append(true).open(&p).unwrap();
    f.write_all(b"{}\n").unwrap();
    scan::mark_done(&c, &stamps).unwrap();
    assert_eq!(scan::changed_files(&c, d.path(), "jsonl").unwrap().len(), 1);
}
