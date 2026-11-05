/// Minimal config; Task 5 extends it with labels and thresholds.
#[derive(Debug, Clone)]
pub struct Config {
    pub claude_account: String,
}

impl Default for Config {
    fn default() -> Self {
        Config { claude_account: "default".to_string() }
    }
}
