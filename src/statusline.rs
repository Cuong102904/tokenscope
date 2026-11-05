//! `agent-usage statusline`: hot path run by Claude Code on every refresh.
//! Must stay fast: no SQLite, no price table, every failure swallowed.
use crate::paths::Paths;
use serde_json::Value;
use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

const WRAP_TIMEOUT: Duration = Duration::from_millis(800);
const WRAPPED_FILE: &str = "wrapped-statusline.sh";

#[derive(Debug, Default, Clone)]
pub struct Args {
    pub wrap: Option<String>,
    pub wrap_file: Option<PathBuf>,
    pub show: bool,
    pub account: Option<String>,
    pub print_snippet: bool,
}

#[derive(Debug, Default, Clone, Copy, PartialEq)]
struct Window {
    used: f64,
    resets_at: Option<i64>,
}

fn window(v: &Value) -> Option<Window> {
    Some(Window { used: v["used_percentage"].as_f64()?, resets_at: v["resets_at"].as_i64() })
}

fn kill_group(child: &mut Child) {
    // the child leads its own process group; kill the whole group so grandchildren die too
    let _ = Command::new("kill")
        .args(["-KILL", "--", &format!("-{}", child.id())])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let _ = child.kill();
    let _ = child.wait();
}

/// Run `cmd` with `input` on stdin; return its stdout, or "" on timeout/error.
fn run_wrapped(mut cmd: Command, input: &[u8]) -> Vec<u8> {
    cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).process_group(0);
    let Ok(mut child) = cmd.spawn() else { return vec![] };
    let deadline = Instant::now() + WRAP_TIMEOUT;
    if let Some(mut si) = child.stdin.take() {
        let data = input.to_vec();
        // separate thread: a child that never reads stdin must not block us
        std::thread::spawn(move || {
            let _ = si.write_all(&data);
        });
    }
    let (tx, rx) = mpsc::channel();
    if let Some(mut so) = child.stdout.take() {
        std::thread::spawn(move || {
            let mut buf = vec![];
            let _ = so.read_to_end(&mut buf);
            let _ = tx.send(buf);
        });
    }
    let out = rx.recv_timeout(deadline.saturating_duration_since(Instant::now()));
    // stdout closed (or timed out): give the process the rest of the budget to exit
    let mut exited = false;
    while Instant::now() < deadline {
        if matches!(child.try_wait(), Ok(Some(_))) {
            exited = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    if !exited {
        kill_group(&mut child);
        return vec![];
    }
    out.unwrap_or_default()
}

fn write_snapshot(paths: &Paths, account: &str, five: Option<Window>, seven: Option<Window>) -> anyhow::Result<()> {
    let dir = paths.snapshots_dir();
    std::fs::create_dir_all(&dir)?;
    let now = chrono::Utc::now();
    let w = |w: Option<Window>| w.map(|w| serde_json::json!({"used_percent": w.used, "resets_at": w.resets_at}));
    let body = serde_json::json!({
        "account": account, "measured_at": now.timestamp(),
        "five_hour": w(five), "seven_day": w(seven),
    });
    // one file per account per UTC minute, overwritten in place: bounded growth, no read_dir
    let name = format!("claude-{}-{}.json", crate::model::sha8(account), now.format("%Y%m%d%H%M"));
    let tmp = dir.join(format!(".{name}.{}.tmp", std::process::id()));
    std::fs::write(&tmp, body.to_string())?;
    std::fs::rename(&tmp, dir.join(name))?;
    Ok(())
}

fn today_cost(paths: &Paths) -> Option<String> {
    let t = std::fs::read_to_string(paths.data_dir().join("today.txt")).ok()?;
    let (date, amount) = t.trim().split_once(' ')?;
    (date == chrono::Local::now().format("%Y-%m-%d").to_string() && !amount.is_empty()).then(|| amount.to_string())
}

fn summary(paths: Option<&Paths>, five: Option<Window>, seven: Option<Window>) -> String {
    let mut parts = vec![];
    if let Some(w) = five {
        parts.push(format!("5h {:.0}%", w.used));
    }
    if let Some(w) = seven {
        parts.push(format!("tuần {:.0}%", w.used));
    }
    if let Some(c) = paths.and_then(today_cost) {
        parts.push(format!("{c} hôm nay"));
    }
    parts.join(" · ")
}

/// Fallback for unparseable arguments: the status line must still exit 0.
pub fn passthrough() {
    let mut sink = vec![];
    let _ = std::io::stdin().read_to_end(&mut sink);
}

pub fn run(args: &Args, paths: Option<&Paths>) {
    if args.print_snippet {
        if let Some(p) = paths {
            print!("{}", print_snippet(p));
        }
        return;
    }
    let mut input = vec![];
    let _ = std::io::stdin().read_to_end(&mut input);
    let stdout = {
        let cmd = if let Some(c) = &args.wrap {
            let mut x = Command::new("sh");
            x.arg("-c").arg(c);
            Some(x)
        } else {
            args.wrap_file.as_ref().map(|f| {
                let mut x = Command::new("sh");
                x.arg(f);
                x
            })
        };
        cmd.map(|c| run_wrapped(c, &input)).unwrap_or_default()
    };
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(&stdout);

    let limits = serde_json::from_slice::<Value>(&input).ok().map(|v| (window(&v["rate_limits"]["five_hour"]), window(&v["rate_limits"]["seven_day"])));
    let (five, seven) = limits.unwrap_or_default();
    if (five.is_some() || seven.is_some())
        && let Some(p) = paths
    {
        let account = args
            .account
            .clone()
            .or_else(|| std::env::var("AGENT_USAGE_ACCOUNT").ok().filter(|a| !a.is_empty()))
            .unwrap_or_else(|| crate::config::Config::load(p).map(|c| c.claude_account).unwrap_or_else(|_| "default".into()));
        let _ = write_snapshot(p, &account, five, seven);
    }
    if args.show {
        let s = summary(paths, five, seven);
        if !s.is_empty() {
            if !stdout.is_empty() && !stdout.ends_with(b"\n") {
                let _ = out.write_all(b"\n");
            }
            let _ = writeln!(out, "{s}");
        }
    }
    let _ = out.flush();
}

fn write_executable_atomic(path: &Path, content: &str) -> anyhow::Result<()> {
    let tmp = path.with_extension(format!("sh.{}.tmp", std::process::id()));
    {
        let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o755).open(&tmp)?;
        f.write_all(content.as_bytes())?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Build the suggested `statusLine` snippet. Only reads `~/.claude/settings.json`;
/// the user's old command is copied to our own `wrapped-statusline.sh`.
pub fn print_snippet(paths: &Paths) -> String {
    let existing = std::fs::read_to_string(paths.home.join(".claude/settings.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| v["statusLine"]["command"].as_str().map(String::from));
    let mut note = String::new();
    let command = match existing {
        Some(cmd) if cmd.contains("agent-usage statusline") => {
            note.push_str("settings.json already uses agent-usage statusline; the saved wrapped command was left untouched.\n");
            wrapped_command()
        }
        Some(cmd) => {
            let dir = paths.data_dir();
            match std::fs::create_dir_all(&dir).and_then(|_| write_executable_atomic(&dir.join(WRAPPED_FILE), &cmd).map_err(std::io::Error::other)) {
                Ok(()) => wrapped_command(),
                Err(e) => {
                    note.push_str(&format!("could not save the existing command ({e}); snippet has no --wrap-file.\n"));
                    "agent-usage statusline --show".to_string()
                }
            }
        }
        None => "agent-usage statusline --show".to_string(),
    };
    let snippet = serde_json::json!({"statusLine": {"type": "command", "command": command}});
    format!(
        "{}\n{note}Paste the statusLine block above into ~/.claude/settings.json yourself (this command never edits it).\n",
        serde_json::to_string_pretty(&snippet).unwrap_or_default()
    )
}

fn wrapped_command() -> String {
    format!("agent-usage statusline --wrap-file ~/.agent-usage/{WRAPPED_FILE} --show")
}
