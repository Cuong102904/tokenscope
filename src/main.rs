use agent_usage::collect::{self, AdapterStatus};
use agent_usage::config::Config;
use agent_usage::model::Vendor;
use agent_usage::paths::Paths;
use agent_usage::pricing::PriceTable;
use agent_usage::adapters::agy;
use agent_usage::{server, statusline};
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
    Serve {
        #[arg(long, default_value_t = 8765)]
        port: u16,
    },
    /// Terminal UI
    Tui,
    /// Claude Code status line wrapper (reads Claude's JSON on stdin)
    Statusline {
        /// Shell command whose output is printed first (your old status line)
        #[arg(long)]
        wrap: Option<String>,
        /// Like --wrap, but runs `sh <path>`
        #[arg(long, conflicts_with = "wrap")]
        wrap_file: Option<std::path::PathBuf>,
        /// Append `5h 44% · tuần 54% · $12.00 hôm nay` on its own line
        #[arg(long)]
        show: bool,
        /// Account name for the snapshot (default: $AGENT_USAGE_ACCOUNT, then config)
        #[arg(long)]
        account: Option<String>,
        /// Print the settings.json snippet to paste (never writes settings.json)
        #[arg(long)]
        print_snippet: bool,
    },
    /// One-shot status summary
    Status {
        #[arg(long)]
        json: bool,
        /// Also run `agy -p "/usage"` (at most once per 5 minutes) and show its weekly limits
        #[arg(long)]
        refresh: bool,
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

/// Latest agy weekly snapshot per scope, as `agy <scope>: N% used (resets <UTC>)`.
fn print_agy(conn: &Connection, note: Option<&str>) {
    if let Some(n) = note {
        println!("{n}");
    }
    let Ok(mut st) = conn.prepare(
        "SELECT scope, used_percent, resets_at, measured_at FROM limit_snapshots l WHERE vendor='agy'
         AND measured_at=(SELECT max(measured_at) FROM limit_snapshots WHERE vendor='agy' AND scope=l.scope AND account=l.account)
         ORDER BY scope",
    ) else {
        return;
    };
    let rows = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?, r.get::<_, Option<i64>>(2)?, r.get::<_, i64>(3)?)));
    for (scope, used, resets, at) in rows.into_iter().flatten().flatten() {
        let fmt = |t: i64| chrono::DateTime::from_timestamp(t, 0).map(|d| d.format("%Y-%m-%d %H:%M UTC").to_string()).unwrap_or_default();
        println!("agy {scope}: {used:.0}% used (resets {}, measured {})", resets.map(fmt).unwrap_or_else(|| "?".into()), fmt(at));
    }
}

fn money(c: f64) -> String {
    format!("${c:.2}")
}

fn main() -> anyhow::Result<()> {
    let cli = match Cli::try_parse() {
        Ok(c) => c,
        Err(e) => {
            if e.use_stderr() && std::env::args().nth(1).as_deref() == Some("statusline") {
                statusline::passthrough();
                return Ok(());
            }
            e.exit()
        }
    };
    if let Cmd::Statusline { wrap, wrap_file, show, account, print_snippet } = cli.cmd {
        // never fails: the status line must always print something and exit 0
        let args = statusline::Args { wrap, wrap_file, show, account, print_snippet };
        statusline::run(&args, Paths::from_env().ok().as_ref());
        return Ok(());
    }
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
        Cmd::Status { json, refresh } => {
            let (mut conn, cfg, _) = collect_once(&paths)?;
            let agy_note = if refresh {
                match agy::refresh(&paths, &cfg, &mut conn) {
                    Ok(agy::RefreshOutcome::Ran) => None,
                    Ok(agy::RefreshOutcome::Throttled { next_allowed }) => Some(format!("agy: refreshed recently, next allowed at epoch {next_allowed}")),
                    Ok(agy::RefreshOutcome::NotInstalled) => Some("agy: not found on PATH".to_string()),
                    Err(e) => Some(format!("agy: refresh failed: {e:#}")),
                }
            } else {
                None
            };
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
                if refresh {
                    print_agy(&conn, agy_note.as_deref());
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
                        Some(e) => format!("ERROR {e} (last run: {})", s.detail),
                        None => format!("ok @ {}: {}", s.last_run, s.detail),
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
        Cmd::Serve { port } => server::serve(paths, port)?,
        Cmd::Tui => todo!("tui"),
        Cmd::Statusline { .. } => unreachable!("handled above"),
    }
    Ok(())
}
