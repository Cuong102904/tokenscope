use tokenscope::paths::Paths;
use tokenscope::pricing::PriceTable;

fn embedded() -> PriceTable {
    let dir = tempfile::tempdir().unwrap();
    PriceTable::load(&Paths {
        home: dir.path().to_path_buf(),
    })
    .unwrap()
}

fn with_user_file(content: &str) -> anyhow::Result<PriceTable> {
    let dir = tempfile::tempdir().unwrap();
    let p = Paths {
        home: dir.path().to_path_buf(),
    };
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
        .cost(
            "claude-opus-5-5",
            1_000_000,
            1_000_000,
            1_000_000,
            1_000_000,
            1_000_000,
        )
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
fn all_prices_pinned() {
    // (model name, input, output, cache_read) in $ per 1M tokens.
    let cases: &[(&str, f64, f64, f64)] = &[
        // Claude (14 keys, claude- prefix)
        ("claude-fable-5-1-20261001", 10.0, 50.0, 0.25),
        ("claude-mythos-5-1-20261001", 10.0, 50.0, 0.25),
        ("claude-fable-5-20261001", 10.0, 50.0, 1.00),
        ("claude-mythos-5-20261001", 10.0, 50.0, 1.00),
        ("claude-opus-5-5-20261001", 4.0, 20.0, 0.20),
        ("claude-opus-5-20261001", 5.0, 25.0, 0.50),
        ("claude-opus-4-8-20261001", 5.0, 25.0, 0.50),
        ("claude-opus-4-7-20261001", 5.0, 25.0, 0.50),
        ("claude-opus-4-6-20261001", 5.0, 25.0, 0.50),
        ("claude-sonnet-5-5-20261001", 2.0, 10.0, 0.20),
        ("claude-sonnet-5-20261001", 2.0, 10.0, 0.20),
        ("claude-sonnet-4-6-20261001", 3.0, 15.0, 0.30),
        ("claude-haiku-5-5-20261001", 0.10, 0.50, 0.01),
        ("claude-haiku-4-5-20261001", 1.0, 5.0, 0.10),
        // OpenAI (10 keys)
        ("gpt-5.4", 2.50, 15.00, 0.25),
        ("gpt-5.4-mini", 0.75, 4.50, 0.075),
        ("gpt-5.5", 5.00, 30.00, 0.50),
        ("gpt-5.6-luna", 0.20, 1.20, 0.02),
        ("gpt-5.6-sol", 4.00, 20.00, 0.40),
        ("gpt-5.6-terra", 2.00, 12.00, 0.20),
        ("gpt-6-astra", 10.00, 50.00, 1.00),
        ("gpt-6-luna", 0.10, 0.50, 0.01),
        ("gpt-6.1-sol", 2.00, 10.00, 0.10),
        ("gpt-6.1-terra", 1.00, 5.00, 0.05),
    ];
    let t = embedded();
    for &(model, input, output, cache_read) in cases {
        let p = t
            .lookup(model)
            .unwrap_or_else(|| panic!("no price for {model}"));
        assert!(
            (p.input - input).abs() < 1e-12,
            "{model} input: {}",
            p.input
        );
        assert!(
            (p.output - output).abs() < 1e-12,
            "{model} output: {}",
            p.output
        );
        assert!(
            (p.cache_read - cache_read).abs() < 1e-12,
            "{model} cache_read: {}",
            p.cache_read
        );
    }
    // Not on the official page: must stay unpriced.
    assert!(t.lookup("gpt-5.3-codex-spark").is_none());
    // Longest-prefix pins between sibling keys.
    assert_eq!(t.lookup("gpt-5.4-mini").unwrap().input, 0.75);
    assert_eq!(t.lookup("gpt-6.1-sol").unwrap().input, 2.00);
    assert_eq!(
        t.lookup("claude-fable-5-1-20261001").unwrap().cache_read,
        0.25
    );
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
