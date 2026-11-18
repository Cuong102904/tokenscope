# Multi-account Claude Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Show per-account usage and remaining quota for every Claude account on the machine by real email. No account switching.

**Architecture:** Layer 1 reads the owner of each transcript (`session-owner`) and the signed-in account from `~/.claude.json`, keys everything by one internal account key, and keeps a local registry key → email/org/plan. Layer 2 is a small read-only `QuotaSource` trait whose first backend runs `claude-swap list --json` and feeds ordinary `limit_snapshots`. A new Accounts tab and `/api/accounts` read both.

**Tech Stack:** Rust 2024 (axum, rusqlite, serde_json, chrono), vanilla JS dashboard in `assets/web`, tests in `tests/*.rs` with fake scripts on `PATH`.

**Spec:** `docs/superpowers/specs/2026-11-17-multi-account-design.md`

**Scope of this plan:** spec Layer 1 and the claude-swap read-only source (D2). The Switch button and any `switch` operation are out of scope for good (spec non-goal). `claude_dirs` (spec D4, unverified), codex-multi-auth and the user-defined `[quota.*]` command are a later plan; this plan's `QuotaSource` trait is what they plug into.

## Global Constraints

- Work on a new branch `feat/multi-account` from `main`; one PR per phase is fine (Phase 1 ships on its own). Remote for tokenscope is `customer` (not `origin`); `gh` needs `-R Cuong102904/tokenscope` and the `Cuong102904` token.
- Never read, copy or log tokens or credentials. Only `oauthAccount` metadata fields are read from `~/.claude.json`, through `safety::read_to_string_guarded`.
- Real emails are stored in the local DB and shown in dashboard and TUI; never sent anywhere, never in committed fixtures (use `me@example.com` style).
- The DB key for an account is internal and never displayed; `sha8` of the identity (Task 2 defines the exact input).
- Server binds `127.0.0.1` only; endpoints that run processes need the `X-Tokenscope` header (`REFRESH_HEADER` in `src/server.rs`). Nothing in this plan writes to the user's login.
- Tests never run the real `claude`, `codex` or `claude-swap`: use a fake script on `PATH`, and serialize script-write-plus-spawn with a lock as `tests/claude_usage.rs` (`SPAWN`, `locked_refresh`) does.
- tokenscope must not fail or slow down when claude-swap is missing, slow (timeout 10 s, never inside the collect loop) or returns anything but `schemaVersion` 1.
- No new DB tables or columns: registry and quota-source state live in `kv`.
- `cargo test --locked --all-targets` and `cargo clippy --locked --all-targets -- -D warnings` pass before every commit.

## Review Focus

1. `session-owner` missing (old sessions), empty, non-string, or not the first line: rows get the fallback account, nothing panics (Task 3).
2. `~/.claude.json` missing, not JSON, or without `oauthAccount`: behave exactly as today (`sha8(email)` from `claude auth status`, or the configured key) (Task 2).
3. A subagent transcript changes while its parent transcript does not: it still gets the parent's account (Task 3).
4. Two accounts with the same email in different orgs stay two accounts; claude-swap matching uses email **and** organizationUuid (Tasks 2, 8).
5. claude-swap absent, hanging, exiting non-zero, printing invalid JSON or `schemaVersion` 2: Accounts tab still renders from Layer 1 (a not-signed-in account shows its last reading marked stale); no error surfaced as a 500 (Tasks 8, 9).

---

## File Structure

- Create `src/accounts.rs`: registry in `kv`, account key function, `AccountInfo`.
- Create `src/adapters/claude_identity.rs`: reads `oauthAccount` from `~/.claude.json`, key migration.
- Modify `src/adapters/claude.rs`: per-file owner attribution; `src/adapters/claude_usage.rs`, `claude_snapshots.rs`: use the new key.
- Create `src/query/accounts.rs`: the overview rows; modify `src/query/cost.rs` (usage per account), `src/query/limits.rs` (`account_label` falls back to the registry email).
- Create `src/quota/mod.rs`, `src/quota/claude_swap.rs`.
- Modify `src/paths.rs`, `src/lib.rs`, `src/server.rs`, `assets/web/index.html`, `assets/web/app.js`, `CLAUDE.md`, `README.md`.
- Tests: `tests/accounts.rs`, `tests/claude_identity.rs`, `tests/claude.rs` (extend), `tests/quota.rs`, `tests/server.rs` (extend). New fixtures under `tests/fixtures/claude-owner/`.

---

## Phase 1: per-account usage and quota, no external tool

### Task 1: Account registry and key

**Files:**
- Create: `src/accounts.rs`; Modify: `src/lib.rs` (add `pub mod accounts;`), `CLAUDE.md` (privacy rule, see step 5)
- Test: `tests/accounts.rs`

**Interfaces:**
- Produces:
  - `pub struct AccountInfo { pub email: String, pub org: Option<String>, pub plan: Option<String>, pub last_seen: i64 }` (Serialize, Deserialize, Clone, PartialEq, Debug)
  - `pub fn claude_key(account_uuid: &str, org_uuid: Option<&str>) -> String` = `model::sha8(&format!("{account_uuid}:{}", org_uuid.unwrap_or("")))`
  - `pub fn upsert(conn: &Connection, vendor: Vendor, key: &str, info: &AccountInfo) -> anyhow::Result<()>` (kv key `account:<vendor>:<key>`, JSON value; keeps the larger `last_seen`)
  - `pub fn get(conn: &Connection, vendor: Vendor, key: &str) -> anyhow::Result<Option<AccountInfo>>`
  - `pub fn all(conn: &Connection, vendor: Vendor) -> anyhow::Result<Vec<(String, AccountInfo)>>`

- [ ] **Step 1: Write failing tests** in `tests/accounts.rs`: `claude_key_differs_by_org` (same uuid, orgs `a`/`b`/none give three different keys; same inputs give the same key; key is 8 hex chars); `upsert_then_get_round_trips`; `upsert_keeps_latest_last_seen` (second upsert with older `last_seen` does not lower it but updates email); `all_lists_only_that_vendor`.
- [ ] **Step 2: Run** `cargo test --test accounts` → FAIL (module missing).
- [ ] **Step 3: Implement** the interface above in `src/accounts.rs`; use `serde_json` for the value.
- [ ] **Step 4: Run** `cargo test --test accounts` → PASS.
- [ ] **Step 5: Update `CLAUDE.md`** "Safety and privacy": the `sha8`-only line becomes "DB account key is `sha8`, never shown; real emails are kept in the local DB and shown (owner's decision, 2026-11-17); never sent anywhere; never in fixtures."
- [ ] **Step 6: Commit** `feat: account registry and account key`.

### Task 2: Identify the signed-in Claude account (uuid) and unify keys

**Files:**
- Create: `src/adapters/claude_identity.rs`; Modify: `src/adapters/mod.rs` (add module), `src/adapters/claude_usage.rs` (`refresh_with`, `remember_identity`), `src/adapters/claude_snapshots.rs`, `src/paths.rs`
- Test: `tests/claude_identity.rs`, extend `tests/claude_usage.rs`

**Interfaces:**
- Consumes: Task 1 `claude_key`, `upsert`, `AccountInfo`.
- Produces:
  - `Paths::claude_json(&self) -> PathBuf` = `home/.claude.json`
  - `pub struct SignedIn { pub key: String, pub info: AccountInfo }`
  - `pub fn signed_in(paths: &Paths, now: i64) -> Option<SignedIn>`: reads `~/.claude.json` via `safety::read_to_string_guarded`, takes only `oauthAccount.{accountUuid, emailAddress, organizationUuid, organizationName, organizationType/seatTier as plan}`; `None` if file/field missing or `accountUuid`/`emailAddress` empty. Plan text: `subscriptionType` is not in this file; use `organizationRateLimitTier` verbatim or leave `None` (builder's call, note it in the commit).
  - `pub fn migrate_email_key(conn: &Connection, email: &str, new_key: &str) -> anyhow::Result<()>`: moves `limit_snapshots`/`limit_events` rows with `vendor='claude'` and `account = sha8(email)` to `new_key`, and moves the `label:claude:<old>` kv row, once (idempotent: no rows → no-op).
  - `claude_usage::identity_key(conn)` keeps its signature but now returns the Task 2 key.

- [ ] **Step 1: Write failing tests** in `tests/claude_identity.rs` using a temp `home` with a fixture `.claude.json`: `reads_oauth_account_fields`; `returns_none_when_file_missing`; `returns_none_for_invalid_json`; `returns_none_without_oauth_account`; `key_includes_org` (same uuid, other org → other key). In `tests/claude_usage.rs`: `refresh_uses_the_uuid_key_and_fills_the_registry` (fake `claude`, `.claude.json` present → `limit_snapshots.account == claude_key(..)` and `accounts::get` returns the email); `refresh_falls_back_to_the_email_key_without_claude_json` (today's behaviour, assertion copied from the existing `readings_are_keyed_and_named_by_the_signed_in_account`); `old_email_keyed_rows_move_to_the_uuid_key`.
- [ ] **Step 2: Run** `cargo test --test claude_identity --test claude_usage` → FAIL.
- [ ] **Step 3: Implement** `signed_in`, `migrate_email_key`; in `refresh_with` and `ClaudeSnapshotsAdapter::collect` prefer `signed_in(...)` (upsert registry, call `migrate_email_key(conn, &email, &key)`, store `KV_IDENTITY_KEY = key`), else the existing `identify` path unchanged. The configured non-default `claude_account` still wins.
- [ ] **Step 4: Run** the two test files plus `cargo test --test limits` → PASS.
- [ ] **Step 5: Commit** `feat: key the signed-in Claude account by its uuid`.

### Task 3: Attribute transcript usage to the session owner

**Files:**
- Modify: `src/adapters/claude.rs`
- Test: extend `tests/claude.rs`; fixtures `tests/fixtures/claude-owner/` (a session with a `session-owner` line for `me@example.com`'s uuid, one without, a `subagents/` file whose parent has the owner line)

**Interfaces:**
- Consumes: Task 1 `claude_key`, `upsert`.
- Produces:
  - `pub fn parse_owner(v: &Value) -> Option<(String, Option<String>)>`: `(accountUuid, organizationUuid)` from a line with `type == "session-owner"` and non-empty string `accountUuid`.
  - `fn owner_of_file(path: &Path, lines: &[Value]) -> Option<(String, Option<String>)>`: the file's own `session-owner` line, else, when a path component is `subagents`, the owner of the parent file `<dir>/<sessionId>.jsonl` (read with `scan::read_jsonl`).
  - `parse_line` keeps its signature; `collect` passes the owner key as `account` for that file, else `ctx.config.claude_account`.

- [ ] **Step 1: Write failing tests**: `parse_owner_reads_uuid_and_org`; `parse_owner_rejects_empty_or_non_string_uuid`; `usage_rows_take_the_owner_account` (two sessions, two owners → two distinct `usage.account` values); `session_without_owner_keeps_the_configured_account`; `subagent_file_uses_the_parent_owner_even_when_parent_unchanged` (run collect once, touch only the subagent file, collect again, assert its rows' account is the parent's key); `rescan_moves_old_default_rows` (rows previously stored under `default` are rewritten to the owner key after the one-time rescan).
- [ ] **Step 2: Run** `cargo test --test claude` → FAIL.
- [ ] **Step 3: Implement** the functions. Existing usage rows were stored under `default` and their files are marked done: add a one-time rescan guarded by `kv` key `claude:owner_scan_v1` (when absent, `DELETE FROM files WHERE path LIKE '<claude_projects>/%'`, then set the key); `db::write_batch` already updates `account` on conflict of `msg_key`.
- [ ] **Step 4: Run** `cargo test --test claude` and `cargo test --locked --all-targets` → PASS.
- [ ] **Step 5: Commit** `feat: attribute Claude usage to the account that owns the session`.

### Task 4: Accounts overview query and API

**Files:**
- Create: `src/query/accounts.rs`; Modify: `src/query/mod.rs`, `src/query/cost.rs`, `src/query/limits.rs` (`account_label`), `src/server.rs`
- Test: extend `tests/query_cost.rs`, `tests/limits.rs`, `tests/server.rs`

**Interfaces:**
- Consumes: Task 1 registry (`get`, `all`), existing `limits::current(conn, cfg, now) -> Vec<LimitCard>`, `limits::stored_labels`, `cost` row loader.
- Produces:
  - `cost::by_account(conn: &Connection, prices: &PriceTable, f: &Filter) -> anyhow::Result<Vec<AccountUse>>` with `AccountUse { vendor: String, account: String, tokens: u64, cost: Option<f64>, priced: bool }` (same pricing rules as `by_vendor`).
  - `accounts::overview(conn, cfg, prices, f, now) -> anyhow::Result<Vec<AccountRow>>`, `AccountRow { vendor, account, label, email: Option<String>, org: Option<String>, plan: Option<String>, current: bool, windows: Vec<AccountWindow>, tokens: u64, cost: Option<f64>, priced: bool }`, `AccountWindow { scope, used_percent, resets_at, eta: Option<i64>, expired: bool }`. Order: current first, then ascending worst `used_percent` (most quota left first), so a 100% account sorts last.
  - `limits::account_label` precedence becomes: `config.toml` label, dashboard label, registry email (new), `vendor:account`.
  - `GET /api/accounts?days=&vendor=` → `{"accounts": [AccountRow...]}`.

- [ ] **Step 1: Write failing tests**: `by_account_sums_tokens_and_cost_per_account`; `overview_orders_current_first_then_most_quota_left`; `overview_label_is_the_registry_email_unless_named`; `account_label_prefers_dashboard_name_over_email`; server test `api_accounts_returns_rows_and_never_the_raw_key_as_label`.
- [ ] **Step 2: Run** the three test files → FAIL.
- [ ] **Step 3: Implement**; reuse `limits::current` for windows and `current`; keep `AccountRow.account` (raw key) in the JSON only because the history calls need it.
- [ ] **Step 4: Run** `cargo test --locked --all-targets` → PASS.
- [ ] **Step 5: Commit** `feat: accounts overview query and /api/accounts`.

### Task 5: Accounts tab in the dashboard (and TUI label)

**Files:**
- Modify: `assets/web/index.html` (tab link `data-tab="accounts"`, section), `assets/web/app.js` (`TABS`, a `renderAccounts()` following `renderOverview`/limits patterns), `README.md` (Accounts section, claude-swap credit and link)
- Test: `tests/server.rs` (HTML contains the tab), manual screenshot check

**Interfaces:**
- Consumes: `GET /api/accounts` (Task 4).
- Produces: a table, one row per account: email (tooltip: org, plan), a "signed in" dot, one cell each for 5h and weekly (percent, bar, reset time, `expired` shown muted), tokens, cost (`costText`), a muted "reading <age> old" note when the window is stale. Empty state text: "No accounts yet. Sign in once with Claude Code and it appears here." Respect the existing `days`/`vendor` filters via `apiQuery`.

- [ ] **Step 1: Write failing test** `dashboard_html_has_the_accounts_tab` in `tests/server.rs`.
- [ ] **Step 2: Run** `cargo test --test server dashboard_html_has_the_accounts_tab` → FAIL.
- [ ] **Step 3: Implement** the tab, using `fmtMoney`/`costText`/`dur`/`scopeLabel` helpers already in `app.js`; never print the raw account key.
- [ ] **Step 4: Run** the test → PASS; run `tokenscope serve`, open `#accounts`, check both themes and a phone-width window (state what was checked).
- [ ] **Step 5: README**: document the two ways accounts get their name (user label wins over email) and that old sessions without an owner show as one "default" account.
- [ ] **Step 6: Commit** `feat(web): accounts tab with quota left and usage per account`.

---

## Phase 2: optional read-only quota source (claude-swap)

### Task 6: QuotaSource trait and data types

**Files:**
- Create: `src/quota/mod.rs`; Modify: `src/lib.rs` (`pub mod quota;`)
- Test: `tests/quota.rs`

**Interfaces:**
- Produces:
  - `pub struct ManagedAccount { pub account_ref: String, pub email: String, pub org_uuid: Option<String>, pub org: Option<String>, pub plan: Option<String>, pub active: bool, pub windows: Vec<ManagedWindow> }` and `pub struct ManagedWindow { pub scope: String, pub used_percent: f64, pub resets_at: Option<i64> }` (scopes `five_hour`, `weekly`, `weekly:<model>` as `claude_usage` already names them).
  - `pub trait QuotaSource { fn name(&self) -> &'static str; fn list(&self) -> anyhow::Result<Vec<ManagedAccount>>; }` (no switch operation).
  - `pub fn detect(vendor: Vendor) -> Option<Box<dyn QuotaSource>>`: `Claude` → the claude-swap backend when `claude-swap` resolves on `PATH` (`adapters::refresh::resolve`), else `None`; other vendors `None`.

- [ ] **Step 1: Write failing test** `detect_is_none_without_the_binary_on_path` (empty `PATH`; move the `SPAWN` lock helper to a shared `tests/common/mod.rs` if both test files need it).
- [ ] **Step 2: Run** `cargo test --test quota` → FAIL.
- [ ] **Step 3: Implement** types, trait, `detect`.
- [ ] **Step 4: Run** → PASS. **Step 5: Commit** `feat: quota source trait`.

### Task 7: claude-swap registry source (`sequence.json`)

**Files:**
- Modify: `src/paths.rs` (`claude_swap_sequence()` = `$XDG_DATA_HOME/claude-swap/sequence.json` else `home/.local/share/claude-swap/sequence.json`); Create: `src/quota/claude_swap.rs` (first part)
- Test: `tests/quota.rs`

**Interfaces:**
- Produces: `pub fn sequence_accounts(paths: &Paths) -> Vec<SwapAccount>` with `SwapAccount { number: u32, email: String, uuid: String, org_uuid: Option<String>, org: Option<String> }`; empty vec when the file is missing or malformed. Reads metadata fields only (claude-swap's `credentials/` directory is never touched).
- Produces: `pub fn register_all(conn: &Connection, paths: &Paths, now: i64) -> anyhow::Result<()>`: for each, `accounts::upsert(conn, Claude, claude_key(uuid, org_uuid), AccountInfo{..})`.

- [ ] **Step 1: Write failing tests**: `sequence_accounts_reads_number_email_uuid_org` (fixture JSON shaped like claude-swap's, fake emails), `sequence_accounts_empty_for_missing_or_invalid_file`, `register_all_fills_the_registry_with_org_aware_keys`.
- [ ] **Steps 2-5:** run → FAIL, implement, run → PASS, commit `feat: read claude-swap's account list for names`.

### Task 8: claude-swap `list`

**Files:** Modify: `src/quota/claude_swap.rs`; Test: `tests/quota.rs` with a fake `claude-swap` script

**Interfaces:**
- Consumes: Task 6 trait; `statusline::run_with_timeout(cmd, b"", timeout) -> Option<Vec<u8>>`.
- Produces: `pub struct ClaudeSwap { pub bin: PathBuf }` implementing `QuotaSource`; `pub fn parse_list(json: &str) -> anyhow::Result<Vec<ManagedAccount>>`.
  - `list`: runs `claude-swap list --json` (10 s timeout); `parse_list` requires `schemaVersion == 1` (anything else is an `Err("unsupported claude-swap output")`); maps `accounts[].number` → `account_ref` (as text), `email`, `organizationUuid`, `organizationName`, `plan.label`, `active`, and `usage.fiveHour/sevenDay/scoped[]` (`pct`, `resetsAt` RFC 3339 → epoch) to windows `five_hour`, `weekly`, `weekly:<name>`. A usage entry without `pct` is skipped.

- [ ] **Step 1: Write failing tests**: `parse_list_maps_accounts_and_windows` (fixture copied in shape from a real `list --json`, fake emails); `parse_list_rejects_other_schema_versions`; `parse_list_skips_usage_without_pct`; `list_times_out_and_errors` (fake script sleeps); `list_errors_on_nonzero_exit_and_invalid_json`.
- [ ] **Steps 2-5:** run → FAIL, implement, run → PASS, commit `feat: claude-swap read-only quota source`.

### Task 9: Sync quota into the DB and the overview

**Files:** Modify: `src/quota/mod.rs`, `src/query/accounts.rs`, `src/server.rs` (`/api/accounts` handler); Test: `tests/quota.rs`, `tests/server.rs`

**Interfaces:**
- Consumes: Tasks 1, 6, 7, 8.
- Produces: `pub fn sync(conn: &mut Connection, paths: &Paths, src: &dyn QuotaSource, now: i64) -> anyhow::Result<bool>`: throttled by `kv` `quota:last_sync` (min 60 s; returns `false` when throttled); calls `register_all`, then `list()`, matches each `ManagedAccount` to a registry key by **email and org uuid** (unmatched ones are skipped, not guessed), writes `limit_snapshots` (`source = "claude-swap"`, `measured_at = now`; reuse claude-swap's own cache age when it gives `usageAgeSeconds`) through `db::write_batch`.
- The `/api/accounts` handler runs `detect` + `sync` on the blocking pool, ignores any error (logs nothing sensitive), and still returns Layer 1 rows. `AccountRow` gets no switch-related field.

- [ ] **Step 1: Write failing tests**: `sync_writes_limits_for_every_managed_account`; `sync_is_throttled_for_sixty_seconds`; `sync_skips_accounts_it_cannot_match_by_email_and_org` (same email, other org → not merged); `api_accounts_still_works_when_claude_swap_fails` (fake script exits 1, then another prints schemaVersion 2).
- [ ] **Steps 2-5:** run → FAIL, implement, run → PASS, commit `feat: show every claude-swap account's limits`.
- [ ] **Step 6: README**: the claude-swap credit and link (MIT), "fresh numbers for accounts that are not signed in need claude-swap", and the `--no-share-history` caveat. Commit `docs: claude-swap as optional quota source`.

### Not in this plan (later)

- `claude_dirs` in `config.toml` (spec D4): first verify that `CLAUDE_CONFIG_DIR=<dir> claude -p --no-session-persistence "/usage"` returns 5h/weekly for a signed-in second dir; only then plan it.
- codex-multi-auth and the user-defined `[quota.claude] list = "..."` command: both implement `QuotaSource`.

---

## Self-review notes

- Spec coverage: Layer 1 goals 1, 2 → Tasks 2-5; goals 3, 4 → Tasks 6-9 (no source ⇒ Layer 1 only, a not-signed-in account shows its stale reading; Task 9 tests); goal 5 (pluggable) → `QuotaSource` in Task 6, later sources in the follow-up plan; empty-state text → Task 5; README and `CLAUDE.md` → Tasks 1, 5, 9; non-goals (no switching, no login, no credentials, no wrapper CLI) hold: only `oauthAccount` metadata, `sequence.json` and `claude-swap list --json` output are read.
- One refinement of the spec's D1: the key is `sha8(accountUuid:organizationUuid)` (Task 1), not `sha8(accountUuid)`, so the same email in two orgs stays two accounts; `session-owner` carries both ids. Overrule if you prefer uuid alone.
- Names match across tasks: `claude_key`, `AccountInfo`, `ManagedAccount`, `QuotaSource`, `sync`.
- Open spec question that stays with the builder: resumed-session owner lines (Task 3 test `session_without_owner...` plus a manual check).
