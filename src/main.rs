use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "agent-usage", version, about = "Estimated cost and rate limits for local coding agents")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Scan agent logs into the local database
    Collect,
    /// Serve the web dashboard on 127.0.0.1
    Serve,
    /// Terminal UI
    Tui,
    /// Status line output
    Statusline,
    /// One-shot status summary
    Status,
    /// Check which data sources are available
    Doctor,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Collect => todo!("collect"),
        Cmd::Serve => todo!("serve"),
        Cmd::Tui => todo!("tui"),
        Cmd::Statusline => todo!("statusline"),
        Cmd::Status => todo!("status"),
        Cmd::Doctor => todo!("doctor"),
    }
}
