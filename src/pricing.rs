use crate::paths::Paths;
use anyhow::Context;
use serde::Deserialize;
use std::collections::HashMap;

/// Cache-write multipliers, applied to the model's `input` price.
pub const WRITE_5M: f64 = 1.25;
pub const WRITE_1H: f64 = 2.0;

const EMBEDDED: &str = include_str!("../assets/pricing.toml");

/// Prices in $ per 1M tokens.
#[derive(Debug, Clone, Deserialize)]
pub struct Price {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
}

#[derive(Debug, Deserialize)]
struct File {
    #[serde(default)]
    source: String,
    #[serde(default)]
    date: String,
    #[serde(default)]
    models: HashMap<String, Price>,
}

#[derive(Debug)]
pub struct PriceTable {
    pub source: String,
    pub date: String,
    models: HashMap<String, Price>,
}

impl PriceTable {
    /// A user file at `~/.agent-usage/pricing.toml` replaces the embedded
    /// default entirely; a parse error in it is an error.
    pub fn load(paths: &Paths) -> anyhow::Result<PriceTable> {
        let user = paths.data_dir().join("pricing.toml");
        if user.is_file() {
            let text = std::fs::read_to_string(&user)
                .with_context(|| format!("reading {}", user.display()))?;
            Self::parse(&text).with_context(|| format!("parsing {}", user.display()))
        } else {
            Self::parse(EMBEDDED).context("parsing embedded pricing.toml")
        }
    }

    fn parse(text: &str) -> anyhow::Result<PriceTable> {
        let f: File = toml::from_str(text)?;
        Ok(PriceTable { source: f.source, date: f.date, models: f.models })
    }

    /// Longest-prefix match on the model name.
    pub fn lookup(&self, model: &str) -> Option<&Price> {
        self.models
            .iter()
            .filter(|(k, _)| model.starts_with(k.as_str()))
            .max_by_key(|(k, _)| k.len())
            .map(|(_, p)| p)
    }

    /// Cost in dollars, or `None` when the model has no price.
    pub fn cost(
        &self,
        model: &str,
        input: u64,
        output: u64,
        cache_read: u64,
        w5m: u64,
        w1h: u64,
    ) -> Option<f64> {
        let p = self.lookup(model)?;
        let m = 1_000_000.0;
        Some(
            (input as f64 * p.input
                + output as f64 * p.output
                + cache_read as f64 * p.cache_read
                + w5m as f64 * p.input * WRITE_5M
                + w1h as f64 * p.input * WRITE_1H)
                / m,
        )
    }
}
