use agent_usage::collect::{self, AdapterStatus};
use agent_usage::config::Config;
use agent_usage::model::Vendor;
use agent_usage::paths::Paths;
use agent_usage::pricing::PriceTable;
use agent_usage::query::{Filter, cost};
use agent_usage::db;
use clap::{Parser, Subcommand};
use rusqlite::Connection;

#[derive(Parser)]
#[command(name = "agent-usage", version, about = "Estimated cost and rate limits for local coding agents")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Scan agent logs into the local database
    Collect {
        /// Print timing
        #[arg(long)]
        bench: bool,
    },
    /// Serve the web dashboard on 127.0.0.1
    Serve,
    /// Terminal UI
    Tui,
    /// Status line output
    Statusline,
    /// One-shot status summary
    Status {
        #[arg(long)]
        json: bool,
    },
    /// Check which data sources are available
    Doctor,
}

const DISCLAIMER: &str = "Ước tính theo giá API, không phải hoá đơn.";
const ALL_VENDORS: [Vendor; 4] = [Vendor::Claude, Vendor::Codex, Vendor::OpenCode, Vendor::Agy];

fn collect_once(paths: &Paths) -> anyhow::Result<(Connection, Config, Vec<AdapterStatus>)> {
    let cfg = Config::load(paths)?;
    let mut conn = db::open(&paths.db())?;
    let st = collect::run_all(&mut conn, paths, &cfg, &collect::default_adapters());
    Ok((conn, cfg, st))
}

fn money(c: f64) -> String {
    format!("${c:.2}")
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let paths = Paths::from_env()?;
    match cli.cmd {
        Cmd::Collect { bench } => {
            let t0 = std::time::Instant::now();
            let (_, _, statuses) = collect_once(&paths)?;
            let elapsed = t0.elapsed();
            for s in &statuses {
                match &s.error {
                    Some(e) => println!("{}: ERROR {e}", s.vendor.as_str()),
                    None => println!("{}: {} -> {} records", s.vendor.as_str(), s.detail, s.records),
                }
            }
            if bench {
                println!("bench: {:.3}s", elapsed.as_secs_f64());
            } else {
                println!("done in {:.2}s", elapsed.as_secs_f64());
            }
        }
        Cmd::Status { json } => {
            let (conn, _, _) = collect_once(&paths)?;
            let prices = PriceTable::load(&paths)?;
            let t = |f: Filter| cost::totals(&conn, &prices, &f);
            let (today, d7, d30, all) = (t(Filter::days(1))?, t(Filter::days(7))?, t(Filter::days(30))?, t(Filter::default())?);
            if json {
                let v = serde_json::json!({
                    "today": today, "days7": d7, "days30": d30, "all_time": all,
                    "disclaimer": DISCLAIMER,
                });
                println!("{}", serde_json::to_string_pretty(&v)?);
            } else {
                println!("hôm nay {} · 7 ngày {} · 30 ngày {}", money(today.cost), money(d7.cost), money(d30.cost));
                if !all.unpriced_models.is_empty() {
                    println!("n/a (no price): {}", all.unpriced_models.join(", "));
                }
                println!("{DISCLAIMER}");
            }
        }
        Cmd::Doctor => {
            let conn = db::open(&paths.db())?;
            let adapters = collect::default_adapters();
            println!("{:<10} {:<10} {:<28} {:>8}  last run / error", "vendor", "status", "detail", "records");
            for v in ALL_VENDORS {
                let (found, detail) = match adapters.iter().find(|a| a.vendor() == v) {
                    Some(a) => {
                        let d = a.discover(&paths);
                        (d.found, d.detail)
                    }
                    None => (false, "no adapter yet".to_string()),
                };
                let last = collect::load_status(&conn, v);
                let tail = match &last {
                    Some(s) => match &s.error {
                        Some(e) => format!("ERROR {e}"),
                        None => format!("ok @ {}", s.last_run),
                    },
                    None => "never run".to_string(),
                };
                println!(
                    "{:<10} {:<10} {:<28} {:>8}  {}",
                    v.as_str(),
                    if found { "found" } else { "not found" },
                    detail,
                    last.as_ref().map_or(0, |s| s.records),
                    tail
                );
            }
        }
        Cmd::Serve => todo!("serve"),
        Cmd::Tui => todo!("tui"),
        Cmd::Statusline => todo!("statusline"),
    }
    Ok(())
}
