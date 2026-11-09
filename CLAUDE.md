# CLAUDE.md

Guidance for Claude Code working in this repository. The README covers what tokenscope is
and how people use it; this file covers how the code is built and the rules it relies on.
The design spec in `docs/superpowers/specs/2026-11-04-agent-usage-design.md` is the source
of truth for intent; the plan in `docs/superpowers/plans/` is a historical build log.

## Commands

```sh
cargo fmt --all --check                      # CI step 1
cargo clippy --all-targets -- -D warnings    # CI step 2 (warnings are errors)
cargo test --all-targets                     # CI step 3
shellcheck install.sh                        # CI step 4
cargo run --release -- serve --port 8799     # try the dashboard (8765 is usually the installed copy)
cargo run --release -- doctor | status | tui | collect
scripts/demo/screenshots.sh                  # regenerate docs/screenshots/*.png from made-up data
```

Linux only. The binary is still named `tokenscope`; its data lives in `~/.tokenscope/`
(`tokenscope.db`, `config.toml`, optional `pricing.toml`, `snapshots/`, `today.txt`).
The first run moves a pre-rename `~/.agent-usage` there (`Paths::migrate_legacy`).

## Workflow

- `main` is protected: no direct pushes, every change lands through a PR with green CI.
- Push to the **`customer`** remote (`git@github-person:Cuong102904/tokenscope.git`), always
  naming it: `git push customer <branch>`. Never push to `origin`.
- Open PRs with `GH_TOKEN=$(gh auth token -u Cuong102904) gh pr create -R Cuong102904/tokenscope …`.
  Commits use the repo-local identity (Cuong102904).
- Do not merge PRs; the owner merges. When stacking PRs, say the merge order in the body:
  a stacked PR only retargets to `main` if the branch below it is deleted on merge.
- Releases: bump `version` in `Cargo.toml`, merge, then push a `vX.Y.Z` tag to `customer`.
  `.github/workflows/release.yml` builds static musl binaries (x86_64, aarch64) for
  GitHub Releases, which `install.sh` downloads.

## Code map

| Path | Role |
|---|---|
| `src/main.rs` | clap CLI: `collect [--watch]`, `serve`, `tui`, `statusline`, `status [--json] [--refresh]`, `doctor` |
| `src/paths.rs` | every path under `$HOME` (agent log dirs, `~/.tokenscope`) |
| `src/db.rs` | SQLite schema (`user_version = 1`, WAL), `open`, `write_batch` |
| `src/scan.rs` | incremental file scan (`changed_files` by size + mtime) and guarded JSONL reading |
| `src/safety.rs` | never-read list for credential files; every adapter opens files through it |
| `src/collect.rs` | runs the adapters, records per-vendor status, `Collector` for `serve` / `--watch` |
| `src/adapters/` | one module per source; `Adapter` trait: `vendor`, `discover`, `collect` |
| `src/adapters/{claude,codex,opencode,pi}.rs` | usage from local logs (JSONL, or OpenCode's SQLite read-only) |
| `src/adapters/claude_snapshots.rs` | limits dropped by the Claude status line hook |
| `src/adapters/{claude_usage,codex_usage,agy}.rs` | limits on demand by running the tool (`claude /usage`, `codex app-server`, `agy /usage`) |
| `src/adapters/refresh.rs` | shared binary lookup and the 5-minute per-tool throttle (kv table) |
| `src/query/` | read side: `cost`, `sessions`, `limits`, `skills`, `active`; `Filter` in `mod.rs` |
| `src/pricing.rs` + `assets/pricing.toml` | $ per 1M tokens, longest model-name prefix wins; `~/.tokenscope/pricing.toml` overrides |
| `src/server.rs` | axum JSON API + embedded dashboard (`assets/web/`, compiled in with `include_str!`) |
| `src/tui.rs`, `src/statusline.rs` | terminal UI; Claude Code status line wrapper (hot path, must never fail) |
| `assets/web/{index.html,app.js}` | dashboard; plain JS, Chart.js 4 vendored as `chart.umd.min.js` |

Adding a vendor: add a `Vendor` variant (`model.rs`, including `parse`), an adapter, register it
in `collect::default_adapters`, the `ALL_VENDORS` / sources lists (`main.rs`, `query/active.rs`),
`VENDOR_NAME` / `VENDOR_SLOT` in `app.js`, and the README data-source tables. Tests use fixtures
in `tests/fixtures/<vendor>/`; never commit real log lines (they carry project names).

## Rules the code relies on

**Safety and privacy**
- Never open credential files (`auth.json`, anything with `credential`/`token` in the name,
  OAuth JSON). Go through `safety::check_path` / `scan` helpers; tests assert this.
- Account identifiers are stored only as `sha8` (first 8 hex of SHA-256). Never store emails
  or raw account ids. Codex's `creator_account_id` and app-server `accountId` hash the same way.
- The tool never edits `~/.claude/settings.json`; `statusline --print-snippet` only prints.
- The server binds `127.0.0.1` only and rejects other `Host` headers (DNS rebinding).
  Endpoints that run processes (`/api/limits?refresh=…`) also require the `X-Tokenscope` header.
- Tests must never run the real `claude`, `codex` or `agy`: put a fake script first on `PATH`
  or stamp the refresh throttle in `kv` before calling.

**Token conventions (per usage row)**
- `input` excludes cache reads/writes. Codex logs include cached input, so the adapter subtracts it.
- `output` includes reasoning tokens; `reasoning` is informational only (never priced twice).
- Cache writes are split into `cache_write_5m` and `cache_write_1h`.
- `msg_key` is the dedupe key and must be stable across re-reads (`<vendor>:<ids>`).

**Cost**
- Cost is computed at query time from the price table, never stored, so price changes apply
  to history. A total with unpriced models shows as `≥ $X`; nothing priced shows `n/a`;
  `$0.00` only when it really is zero (`Totals::display`, mirrored by `totalText` in `app.js`).
- Labels and dashboards say these are estimates at API list prices, not a bill.

**Limits**
- One snapshot table for every source; scopes are `five_hour`, `weekly`, `monthly`,
  `window_<n>m`, optionally `:<model or group>` (`weekly:Fable`). Keep new sources on these names
  so their readings land on the same cards.
- `limits::current` takes the newest reading per `(vendor, account, scope)`, drops windows last
  seen under an older plan, and hides accounts with no reading for 30 days.
- A reading is stale after 30 minutes for windows up to 5 hours, otherwise after 6 hours.
  A window whose reset has passed is `expired`: its number is shown muted as "before the reset",
  never as current.
- Account names: `config.toml` `[labels]` wins, else the name set from the dashboard (kv rows
  `label:<vendor>:<account>`, `POST /api/account-label`, needs the `X-Tokenscope` header), else
  `vendor:account`. `LimitCard.current` marks the account with the newest reading of its vendor.
- On-demand refreshes are throttled to once per 5 minutes per tool (stamped before running).
  The dashboard refreshes Claude and Codex readings by itself when stale or expired, each reading
  once; agy only by its button, because its `/usage` may use quota.

**Dashboard**
- Charts are updated in place (`draw` in `app.js`) and never animate; re-creating them on refresh
  blanked the canvas in hidden tabs. Timers run only while the page is visible.
- Filters live in the query string, the tab in the hash. Tabs grey out filters they ignore.
- UI text is English. Dates via `fmtTs` ("Nov 6, 14:05"), money via `fmtMoney`.

## Verifying UI changes

`cargo test` covers the API and the TUI buffer, not the browser. For dashboard changes, run
`serve` on a spare port and check it in a real browser (headless Chrome via the DevTools
protocol works well: wait for `#status` to read "Updated at …", check no page errors, take a
screenshot), in both light and dark theme and at phone width (no horizontal page scroll).
Use the demo data from `scripts/demo/` for anything that will be shared or committed.
