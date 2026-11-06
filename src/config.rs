use crate::model::Vendor;
use crate::paths::Paths;
use anyhow::Context;
use serde::Deserialize;
use std::collections::HashMap;

/// `~/.tokenscope/config.toml`. A missing file gives the defaults; a parse
/// error is an error.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Config {
    pub claude_account: String,
    /// Account labels keyed like `"codex:f8ce04de"`.
    #[serde(rename = "labels")]
    pub account_labels: HashMap<String, String>,
    /// A session whose process cannot be seen counts as working when written within this
    /// many minutes (open Claude Code sessions are seen directly).
    pub active_minutes: i64,
    pub refresh_seconds: u64,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            claude_account: "default".to_string(),
            account_labels: HashMap::new(),
            active_minutes: 5,
            refresh_seconds: 30,
        }
    }
}

impl Config {
    pub fn load(paths: &Paths) -> anyhow::Result<Config> {
        let p = paths.config();
        if !p.is_file() {
            return Ok(Config::default());
        }
        let text =
            std::fs::read_to_string(&p).with_context(|| format!("reading {}", p.display()))?;
        toml::from_str(&text).with_context(|| format!("parsing {}", p.display()))
    }

    /// Configured label, else `vendor:account`, else just `vendor` for an empty account.
    pub fn label(&self, vendor: Vendor, account: &str) -> String {
        let key = format!("{}:{}", vendor.as_str(), account);
        if let Some(l) = self.account_labels.get(&key) {
            return l.clone();
        }
        if account.is_empty() {
            vendor.as_str().to_string()
        } else {
            key
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn label_rules() {
        let mut c = Config::default();
        c.account_labels
            .insert("codex:f8ce04de".into(), "work".into());
        assert_eq!(c.label(Vendor::Codex, "f8ce04de"), "work");
        assert_eq!(c.label(Vendor::Codex, "aaaa0000"), "codex:aaaa0000");
        assert_eq!(c.label(Vendor::Agy, ""), "agy");
    }

    #[test]
    fn load_defaults_file_and_error() {
        let d = tempfile::tempdir().unwrap();
        let paths = Paths {
            home: d.path().to_path_buf(),
        };
        assert_eq!(Config::load(&paths).unwrap().active_minutes, 5);
        std::fs::create_dir_all(paths.data_dir()).unwrap();
        std::fs::write(
            paths.config(),
            "active_minutes = 9\n[labels]\n\"claude:x\" = \"me\"\n",
        )
        .unwrap();
        let c = Config::load(&paths).unwrap();
        assert_eq!(
            (
                c.active_minutes,
                c.refresh_seconds,
                c.claude_account.as_str()
            ),
            (9, 30, "default")
        );
        assert_eq!(c.label(Vendor::Claude, "x"), "me");
        std::fs::write(paths.config(), "active_minutes = [").unwrap();
        assert!(Config::load(&paths).is_err());
    }
}
