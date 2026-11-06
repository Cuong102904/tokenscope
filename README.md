# tokenscope

**See what your AI coding agents cost and how close you are to their rate limits, in one local dashboard.**

tokenscope reads the session logs that Claude Code, OpenAI Codex CLI, OpenCode and Antigravity (`agy`) already write to your disk. It turns them into token counts, estimated cost per session, project and model, and rate-limit usage over time. Everything runs on your machine. Nothing is uploaded, and no API keys are needed.

> The binary and data directory are still named `agent-usage` / `~/.agent-usage`. The examples below use those names.

## Why

If you use several coding agents on subscriptions, it's hard to answer simple questions:

- How much would today's work have cost at API prices? Which project or model is eating the budget?
- How close am I to the 5-hour or weekly limit, and when will I hit it at the current pace?
- Which sessions are running right now, and which skills or tools do the agents use most?

Each vendor shows part of this in a different place, or not at all. tokenscope puts it all in one place.

## Features

- **Four agents, one view.** Supports Claude Code, Codex CLI, OpenCode and Antigravity. Multiple accounts per vendor are kept apart.
- **Estimated cost.** Prices come from a built-in table of API list prices (`assets/pricing.toml`) and are computed at query time, so a price update applies to history too. When a model has no known price, totals say `≥ $X` or `n/a` instead of showing a silent `$0`.
- **Rate limits.** Shows 5-hour and weekly usage %, reset times and a projected time-to-limit. Old readings are marked stale instead of being shown as current.
- **Web dashboard** with six tabs: Overview, Limits, Cost, Sessions, Skills & tools, Active.
- **Terminal UI** for a quick look without a browser.
- **Claude Code status line** that adds a line like `5h 44% · tuần 54% · $12.00 hôm nay` (5-hour, weekly, today's cost) under your existing status line.
- **Privacy.** Account IDs are stored only as short hashes. Credential files are never opened. The web server binds to `127.0.0.1` only and rejects foreign `Host` headers.
- **Single binary.** Data lives in SQLite (bundled) and the web assets are embedded. There is no runtime to install.

## Install

Requires Rust 1.85+ (edition 2024).

```sh
cargo install --git https://github.com/Cuong102904/tokenscope
# or, from a clone:
cargo install --path .
```

This installs the `agent-usage` binary into `~/.cargo/bin`.

## Quick start

```sh
agent-usage doctor    # which agents were found on this machine
agent-usage collect   # import their logs into the local database
agent-usage serve     # open http://127.0.0.1:8765
```

`serve` collects again in the background every 30 seconds, so the dashboard stays current while you work.

## Commands

| Command | What it does |
|---|---|
| `agent-usage doctor` | Lists each data source with its status, record counts, last limit reading and last error. |
| `agent-usage collect` | Scans all agent logs once. Re-runs are incremental. |
| `agent-usage collect --watch [--interval N]` | Keeps collecting every N seconds. Use this if you only use the TUI or status line. |
| `agent-usage serve [--port 8765]` | Starts the web dashboard on `127.0.0.1`. |
| `agent-usage tui` | Starts the terminal dashboard. `r` refreshes, `q` quits. |
| `agent-usage status [--json] [--refresh]` | Prints a one-shot summary. `--refresh` also asks `agy` for its weekly limits (at most once every 5 minutes). |
| `agent-usage statusline …` | Wraps the Claude Code status line (see below). |

## Where the data comes from

| Agent | Source | Notes |
|---|---|---|
| Claude Code | `~/.claude/projects/**/*.jsonl` | Tokens and cost per message. |
| Claude Code limits | status line snapshots | Claude only reports rate limits to the status line, so you need the status line hook below. |
| Codex CLI | `~/.codex/sessions/**/*.jsonl` | Tokens plus the rate-limit info Codex logs. |
| OpenCode | `~/.local/share/opencode/opencode.db` | Opened read-only. |
| Antigravity | `agy -p "/usage"` | Run on demand only, at most once every 5 minutes. |

Log files are only read, never modified.

## Claude Code rate limits (status line)

Claude Code sends its 5-hour and weekly usage only to the status line command. To capture it:

1. Print the snippet:

   ```sh
   agent-usage statusline --print-snippet
   ```

2. Paste the `statusLine` block it prints into `~/.claude/settings.json` yourself. tokenscope never edits that file.

Your current status line keeps working. Pass it via `--wrap "<command>"` or `--wrap-file <script>` and its output is printed first. `--show` adds the limits and today's cost on a line below it. If anything goes wrong, the wrapper still prints your original status line and exits 0.

## Configuration

Optional. Create `~/.agent-usage/config.toml`:

```toml
claude_account = "work"   # name for the Claude account behind this status line
active_minutes = 5        # a session counts as "active" if written within N minutes
refresh_seconds = 30      # background collect interval for serve / collect --watch

[labels]                  # friendly names for hashed account IDs (shown on the Limits and Sessions views)
"codex:f8ce04de" = "Codex (personal)"
```

To override prices, copy `assets/pricing.toml` to `~/.agent-usage/pricing.toml` and edit it. Prices are $ per 1M tokens, and the longest model-name prefix wins.

## Files

Everything lives in `~/.agent-usage/`:

- `agent-usage.db`: the SQLite database. Delete it to rebuild from logs.
- `config.toml`, `pricing.toml`: optional overrides.
- `snapshots/`: Claude status line limit readings.
- `today.txt`: today's cost, cached for the status line.

## Caveats

- Costs are **estimates at API list prices**, not your bill. Subscription plans are billed differently.
- The dashboard, TUI and status line text is currently in Vietnamese.
- Rate-limit projections are linear extrapolations of recent usage.

## Development

```sh
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
```

CI runs the same three checks on every pull request. `main` is protected: changes land through pull requests with a green CI.

Design notes live in `docs/superpowers/specs/` and the implementation plan in `docs/superpowers/plans/`.
