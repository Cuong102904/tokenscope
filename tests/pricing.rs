use agent_usage::paths::Paths;
use agent_usage::pricing::PriceTable;

fn embedded() -> PriceTable {
    let dir = tempfile::tempdir().unwrap();
    PriceTable::load(&Paths { home: dir.path().to_path_buf() }).unwrap()
}

fn with_user_file(content: &str) -> anyhow::Result<PriceTable> {
    let dir = tempfile::tempdir().unwrap();
    let p = Paths { home: dir.path().to_path_buf() };
    std::fs::create_dir_all(p.data_dir()).unwrap();
    std::fs::write(p.data_dir().join("pricing.toml"), content).unwrap();
    PriceTable::load(&p)
}

#[test]
fn longest_prefix_wins() {
    let t = embedded();
    let p = t.lookup("claude-opus-5-5-20261001").unwrap();
    assert_eq!(p.input, 4.0);
    assert_eq!(t.lookup("claude-opus-5-2026").unwrap().input, 5.0);
}

#[test]
fn cost_formula() {
    let t = embedded();
    let c = t
        .cost("claude-opus-5-5", 1_000_000, 1_000_000, 1_000_000, 1_000_000, 1_000_000)
        .unwrap();
    assert!((c - 37.20).abs() < 1e-9, "{c}");
}

#[test]
fn unknown_model_is_none() {
    assert!(embedded().cost("gpt-unknown", 1, 1, 0, 0, 0).is_none());
}

#[test]
fn embedded_has_source_and_date() {
    let t = embedded();
    assert!(!t.source.is_empty());
    assert!(!t.date.is_empty());
}

#[test]
fn user_file_replaces_embedded() {
    let t = with_user_file(
        "source = \"mine\"\ndate = \"2026-01-01\"\n[models.\"foo\"]\ninput = 1\noutput = 2\ncache_read = 0.5\n",
    )
    .unwrap();
    assert_eq!(t.source, "mine");
    assert_eq!(t.date, "2026-01-01");
    assert_eq!(t.lookup("foo-1").unwrap().output, 2.0);
    assert!(t.lookup("claude-opus-5-5").is_none());
}

#[test]
fn user_file_parse_error_is_error() {
    assert!(with_user_file("this is [not toml").is_err());
    assert!(with_user_file("[models.\"x\"]\ninput = \"a\"\n").is_err());
}
