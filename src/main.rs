use clap::{Parser, Subcommand};
use rusqlite::Connection;
use tokenscope::adapters::agy;
use tokenscope::collect::{self, AdapterStatus};
use tokenscope::config::Config;
use tokenscope::db;
use tokenscope::model::Vendor;
use tokenscope::paths::Paths;
use tokenscope::pricing::PriceTable;
use tokenscope::query::{Filter, active, cost};
use tokenscope::{server, statusline};

#[derive(Parser)]
#[command(
    name = "tokenscope",
    version,
    about = "Estimated cost and rate limits for local coding agents"
)]
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
        /// Keep running: collect again every --interval seconds (errors logged once per streak)
        #[arg(long, conflicts_with = "bench")]
        watch: bool,
        /// Seconds between passes with --watch (default: refresh_seconds from config.toml)
        #[arg(long, requires = "watch")]
        interval: Option<u64>,
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
        /// Append `5h 44% · week 54% · $12.00 today` on its own line
        #[arg(long)]
        show: bool,
        /// Account name for the snapshot (default: $TOKENSCOPE_ACCOUNT, then config)
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

const DISCLAIMER: &str = "Estimates at API list prices, not your bill.";
const ALL_VENDORS: [Vendor; 4] = [Vendor::Claude, Vendor::Codex, Vendor::OpenCode, Vendor::Agy];

fn collect_once(paths: &Paths) -> anyhow::Result<(Connection, Config, Vec<AdapterStatus>)> {
    let cfg = Config::load(paths)?;
    let mut conn = db::open(&paths.db())?;
    let st = collect::run_all(&mut conn, paths, &cfg, &collect::default_adapters());
    Ok((conn, cfg, st))
}

/// Latest agy weekly snapshot per scope: (scope, used_percent, resets_at, measured_at).
fn agy_rows(conn: &Connection) -> Vec<(String, f64, Option<i64>, i64)> {
    let Ok(mut st) = conn.prepare(
        "SELECT scope, used_percent, resets_at, measured_at FROM limit_snapshots l WHERE vendor='agy'
         AND measured_at=(SELECT max(measured_at) FROM limit_snapshots WHERE vendor='agy' AND scope=l.scope AND account=l.account)
         ORDER BY scope",
    ) else {
        return vec![];
    };
    let rows = st.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, f64>(1)?,
            r.get::<_, Option<i64>>(2)?,
            r.get::<_, i64>(3)?,
        ))
    });
    rows.into_iter().flatten().flatten().collect()
}

/// `agy <scope>: N% used (resets <local time>)` lines.
fn print_agy(conn: &Connection, note: Option<&str>) {
    if let Some(n) = note {
        println!("{n}");
    }
    for (scope, used, resets, at) in agy_rows(conn) {
        let fmt = |t: i64| {
            chrono::DateTime::from_timestamp(t, 0)
                .map(|d| {
                    d.with_timezone(&chrono::Local)
                        .format("%Y-%m-%d %H:%M")
                        .to_string()
                })
                .unwrap_or_default()
        };
        println!(
            "agy {scope}: {used:.0}% used (resets {}, measured {})",
            resets.map(fmt).unwrap_or_else(|| "?".into()),
            fmt(at)
        );
    }
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
    if let Cmd::Statusline {
        wrap,
        wrap_file,
        show,
        account,
        print_snippet,
    } = cli.cmd
    {
        // never fails: the status line must always print something and exit 0
        let args = statusline::Args {
            wrap,
            wrap_file,
            show,
            account,
            print_snippet,
        };
        let paths = Paths::from_env().ok();
        if let Some(p) = &paths {
            // silent: the status line's stdout/stderr belong to Claude Code
            let _ = p.migrate_legacy();
        }
        statusline::run(&args, paths.as_ref());
        return Ok(());
    }
    let paths = Paths::from_env()?;
    match paths.migrate_legacy() {
        Ok(true) => eprintln!(
            "moved ~/.agent-usage to {} (renamed in tokenscope 0.1)",
            paths.data_dir().display()
        ),
        Ok(false) => {}
        Err(e) => eprintln!("warning: could not move ~/.agent-usage to ~/.tokenscope: {e}"),
    }
    match cli.cmd {
        Cmd::Collect {
            watch: true,
            interval,
            ..
        } => {
            let cfg = Config::load(&paths)?;
            let every =
                std::time::Duration::from_secs(interval.unwrap_or(cfg.refresh_seconds).max(1));
            eprintln!(
                "collect --watch: every {}s (Ctrl-C to stop)",
                every.as_secs()
            );
            let mut c = collect::Collector::new(paths, cfg);
            loop {
                c.tick_logged("collect");
                std::thread::sleep(every);
            }
        }
        Cmd::Collect { bench, .. } => {
            let t0 = std::time::Instant::now();
            let (_, _, statuses) = collect_once(&paths)?;
            let elapsed = t0.elapsed();
            for s in &statuses {
                match &s.error {
                    Some(e) => println!("{}: ERROR {e}", s.vendor.as_str()),
                    None => println!(
                        "{}: {} -> {} records",
                        s.vendor.as_str(),
                        s.detail,
                        s.records
                    ),
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
                    Ok(agy::RefreshOutcome::Throttled { next_allowed }) => Some(format!(
                        "agy: refreshed recently, next allowed at epoch {next_allowed}"
                    )),
                    Ok(agy::RefreshOutcome::NotInstalled) => {
                        Some("agy: not found on PATH".to_string())
                    }
                    Err(e) => Some(format!("agy: refresh failed: {e:#}")),
                }
            } else {
                None
            };
            let prices = PriceTable::load(&paths)?;
            let t = |f: Filter| cost::totals(&conn, &prices, &f);
            let (today, d7, d30, all) = (
                t(Filter::days(1))?,
                t(Filter::days(7))?,
                t(Filter::days(30))?,
                t(Filter::default())?,
            );
            if json {
                let v = serde_json::json!({
                    "today": today, "days7": d7, "days30": d30, "all_time": all,
                    "disclaimer": DISCLAIMER,
                });
                let mut v = v;
                if refresh {
                    let snaps: Vec<_> = agy_rows(&conn)
                        .into_iter()
                        .map(|(scope, used, resets, at)| serde_json::json!({"scope": scope, "used_percent": used, "resets_at": resets, "measured_at": at}))
                        .collect();
                    v["agy"] = serde_json::json!({"snapshots": snaps, "note": agy_note});
                }
                println!("{}", serde_json::to_string_pretty(&v)?);
            } else {
                println!(
                    "today {} · 7 days {} · 30 days {} · all time {}",
                    today.display(),
                    d7.display(),
                    d30.display(),
                    all.display()
                );
                if !all.unpriced_models.is_empty() {
                    println!("n/a (no price): {}", all.unpriced_models.join(", "));
                }
                for v in ALL_VENDORS {
                    if let Some(e) = collect::load_status(&conn, v).and_then(|s| s.error) {
                        println!("{}: unavailable ({e})", v.as_str());
                    }
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
            println!(
                "{:<10} {:<10} {:<28} {:>8} {:>6}  {:<16}  last run / error",
                "vendor", "status", "detail", "total", "new", "last limit"
            );
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
                // total = rows stored for the vendor; new = rows added by its last run
                let (total, last_limit) = active::stored_counts(&conn, v);
                let last_limit = last_limit
                    .and_then(|t| chrono::DateTime::from_timestamp(t, 0))
                    .map(|d| {
                        d.with_timezone(&chrono::Local)
                            .format("%Y-%m-%d %H:%M")
                            .to_string()
                    })
                    .unwrap_or_else(|| "-".into());
                println!(
                    "{:<10} {:<10} {:<28} {:>8} {:>6}  {:<16}  {}",
                    v.as_str(),
                    if found { "found" } else { "not found" },
                    detail,
                    total,
                    last.as_ref().map_or(0, |s| s.records),
                    last_limit,
                    tail
                );
            }
        }
        Cmd::Serve { port } => server::serve(paths, port)?,
        Cmd::Tui => tokenscope::tui::run(paths)?,
        Cmd::Statusline { .. } => unreachable!("handled above"),
    }
    Ok(())
}
