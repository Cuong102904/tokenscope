# Multi-account Claude (and Codex): per-account usage and quota, read-only

Status: draft for approval. Date: 2026-11-17. Revised 2026-11-17: the account switcher
(Switch button, `switch` operation) was dropped; the optional layer only reads quota.

## Intent and why

The owner runs two Claude accounts (a Team org account and a personal one) and switches between
them with [claude-swap](https://github.com/realiti4/claude-swap) (MIT, Python). tokenscope today
cannot tell the accounts apart:

- Token and cost rows for Claude are all written with the fixed account `claude_account`
  (default `"default"`) in `ClaudeAdapter::collect` (`src/adapters/claude.rs`).
- Limit readings (5h / weekly) are keyed `sha8(email)` of the signed-in account since #17, so
  usage and limits now sit under different account keys and cannot be joined.
- Claude and Codex only report limits for the account that is logged in right now.

The owner's main use is seeing, at a glance, every account on the machine: which is which (by its
real email, never a hash), how much each has used, and how much quota is left. Switching accounts
is not tokenscope's job (the owner switches with other tools). Not everyone uses claude-swap, so
the feature must work, in a useful reduced form, without any external tool.

## Findings that shape the design

- **Per-account attribution needs no tool.** Each Claude transcript has one `session-owner` line
  `{type, accountUuid, organizationUuid, sessionId}`. On the owner's machine 120 of 178 sessions
  have it (114 account 1, 6 account 2), none has two owners; the 58 without are older sessions.
  Codex already gives `creator_account_id` (hashed to sha8 today).
- **claude-swap modes.** `switch` rewrites `~/.claude/.credentials.json` and `oauthAccount`:
  one active account machine-wide, all sessions share `~/.claude/projects`. `run N` launches
  Claude with a per-account `CLAUDE_CONFIG_DIR`; its `projects/` is symlinked to
  `~/.claude/projects` by default (`--no-share-history` breaks that: those transcripts are
  invisible to tokenscope; document it, do not fix it).
- **claude-swap exposes a machine interface**: `claude-swap list --json` (schemaVersion 1) gives,
  per account, number, email, org, `active`, plan, and usage (`fiveHour`, `sevenDay`, `scoped[]`
  with `pct`, `resetsAt`) for every account, active or not. tokenscope only uses `list`.
- **Why only claude-swap can see quota of a logged-out account.** It keeps a backup of every
  account's OAuth credential and calls `api.anthropic.com/api/oauth/usage` with each one. Without
  those tokens nobody can ask, and tokenscope must not read credentials. So for an account that
  is not signed in, tokenscope shows either the last reading taken while it was signed in (marked
  stale or expired by the existing rules) or, when claude-swap is present, its fresh `list` data.
- **Plain Claude Code can hold several logins too**: `CLAUDE_CONFIG_DIR=<dir> claude` is a fully
  separate login, config and `projects/` (verified: an empty dir reports `loggedIn: false` and its
  own `projectsDirectory`, `~/.claude` untouched). Not yet verified: that `claude /usage` run this
  way returns 5h/weekly for a signed-in extra dir (needs a second account to log in). See D4.
- **Key mismatch to resolve.** `session-owner` carries `accountUuid`, not an email. #17 keys
  limits by `sha8(email)`. Both must end up on one key (see Decisions, D1).
- **Login cannot be automated.** Login is OAuth in the browser inside `claude /login`. Adding an
  account to claude-swap is `claude /login` as the new account, then `claude-swap add`.
  tokenscope must not do or wrap the login (`CLAUDE.md`: never open credential files).

## Goals

1. Token and cost rows for Claude are attributed to the real account, with no tool installed.
2. **An accounts overview is the first thing to read**: one row per account showing its real
   email (plus org and plan when known), whether it is the signed-in one, quota left (5h and
   weekly, with reset times and the existing time-to-limit projection) and what it has used
   (tokens and cost over the selected range). This is the screen for deciding whether to switch.
   Cost and Limits views also filter and group per account, labelled by email.
3. When a read-only quota source is available (claude-swap's `list --json` first), show every
   account's limits, not only the signed-in one's.
4. Without any such source, everything in 1 and 2 works; accounts that are not signed in show
   their last known reading, marked stale.
5. The quota source is pluggable (read-only `list`): claude-swap first; later a `claude_dirs`
   list of extra `CLAUDE_CONFIG_DIR`s (after D4 is verified), codex-multi-auth, or a user-defined
   command in `config.toml` that prints a documented JSON.

## Non-goals

- **No account switching.** No Switch button, no `switch` operation, no endpoint that changes
  the login. The owner switches with other tools.
- tokenscope does not log accounts in, add accounts, or swap or copy credentials (no
  `.credentials.json` / keychain handling). Not now, not later.
- No `tokenscope account ...` wrapper CLI that re-exposes claude-swap commands.
- No bundling, vendoring or porting claude-swap code (Python, 30k lines); it is only run as a
  CLI when present.
- No "Add current account" button in this scope (possible later).
- Not rewriting history: sessions with no `session-owner` keep a neutral account, they are not
  guessed from time of day.

## Anti-goals (failure even if it technically works)

- Reading, copying or logging any token or credential, or sending an email or uuid anywhere.
- Different keys for the same account in usage vs limits, so the dashboard shows it twice.
- Making tokenscope fail or slow down when claude-swap is missing, slow or returns unexpected JSON.

## Constraints

- Rust, single static binary, existing adapter/collect/query layout; reuse the `account` columns
  in `usage`, `limit_snapshots`, `limit_events`, and `kv` for labels (`label:claude:<key>`).
- Privacy rules in `CLAUDE.md` change, by the owner's decision: tokenscope runs only on the
  user's machine, so real account emails are stored in the local database and shown in the
  dashboard and TUI. They are never sent anywhere (no network, no logs beyond the local DB, no
  telemetry) and never appear in committed fixtures. Tokens and credentials stay off limits
  as before. `CLAUDE.md` is updated in the same change (the `sha8`-only rule becomes: the
  database key is `sha8` and is never shown; display names are emails).
- Server binds 127.0.0.1, endpoints that run processes need the `X-Tokenscope` header. Reading
  claude-swap's `list` happens in `/api/accounts` (throttled); nothing in this feature writes to
  the user's login.
- Tests never run the real `claude`, `codex`, `claude-swap`; they use a fake script on `PATH`
  (serialized with the spawn lock added in #17's test fix).

## Approaches considered

1. **Own switcher in tokenscope** (snapshot and swap credentials). Rejected: this is the hard,
   fragile part of claude-swap (rotating refresh tokens, macOS keychain, races with running
   Claude) and breaks the no-credentials rule.
2. **Depend on claude-swap only** (call its CLI, nothing without it). Rejected by the owner:
   most users will not have it, and per-account usage does not need it.
3. **Two layers (chosen by the owner):** passive attribution that needs nothing, plus an optional,
   pluggable read-only quota source with claude-swap as first backend and a user-defined command
   as the generic escape hatch.
4. **Add a Switch action on top of layer 2.** Rejected by the owner (2026-11-17): the part that
   writes the login (locks, rollback, refresh-token rotation, keychain, races with a running
   Claude) is the risky one and not what the owner needs.

## Design

### Layer 1: passive attribution (no external tool)

- `ClaudeAdapter::collect` reads the `session-owner` line of each transcript and sets `account`
  for that file's usage rows. Subagent files share the parent's `sessionId`; resolve their
  account from the parent session's owner (or from their own line if present).
- Account key and mapping: see D1. Sessions with no owner line use the existing fallback
  (`claude_account`, i.e. `"default"`), shown as "unknown".
- **Account registry (what makes real emails show without any tool).** A local table/kv set
  `uuid -> {email, org, plan, last_seen}`. It is filled every time tokenscope sees an identity:
  the signed-in account (`~/.claude.json` `oauthAccount` metadata or `claude auth status
  --json`), and, when present, claude-swap's `list --json` / `sequence.json` (which names every
  account it manages, including ones never logged in on this tool's watch). An account whose
  email was never seen shows as "unknown account" with its short id in a tooltip only, until it
  is seen once; the owner's own label (#16) overrides the email.
- Limit readings: the signed-in account's key comes from the same identity, so a `/usage` or
  statusline reading lands on the same key as that account's token rows.
- Names: user label (#16) if set, else the registry email. Hashes are never the visible name.
- Account views (cost and limits) filter and group by account for Claude as they already do for
  Codex.

### Layer 2: optional read-only quota source

A small internal interface, independent of Claude/Codex, with one operation:

- `list() -> Vec<ManagedAccount>`: per account `{ref, email, active, org?, plan?,
  limits: five_hour/weekly/scoped (percent, resets_at)}`; account identity is whatever the
  backend can give that maps to the Layer-1 key (see D1). There is no `switch`.

Backends:

1. **claude-swap**: `claude-swap list --json`. Found on `PATH`; absent means the layer is off.
   Accept `schemaVersion` 1 only and degrade to "off, with a note" on anything else. Run with a
   timeout, never block collect.
2. **Extra Claude config dirs** (after D4 is verified): `claude_dirs = ["~/.claude-work"]` in
   `config.toml`; for each dir tokenscope runs `claude /usage` with `CLAUDE_CONFIG_DIR` set (the
   `claude` binary reads its own login; tokenscope opens no credential file) and scans that
   dir's `projects/` for usage.
3. **codex-multi-auth**: list via its CLI; details settled when building it.
4. **Custom command** in `config.toml`, for any other tool or the user's own script:

   ```toml
   [quota.claude]
   list = "my-script list"        # prints the JSON below on stdout
   ```

   The documented JSON mirrors `ManagedAccount` (schema version field included). The command
   is run without a shell and with no untrusted input in its arguments.

What Layer 2 feeds:

- `list()` results are written as ordinary `limit_snapshots` (source e.g. `claude-swap`) so the
  existing Limits cards, history and projections work unchanged, now for every account.
- Each account row also shows the backend's own plan and "ahead of pace / will last to reset"
  hints when it provides them (claude-swap does), so the overview can say "account 1: weekly
  100%, resets Nov 20" next to "account 2: 7%" without the user doing arithmetic.
- No backend: no error; an account that is not signed in shows its last reading marked stale or
  expired; one line in the empty state says how to get fresh numbers (claude-swap link, or the
  custom-command snippet).

### Accounts overview (priority UI)

Order of importance for the builder: (1) email + signed-in marker + 5h/weekly remaining with
reset, (2) used tokens/cost for the range. Sort: signed-in first, then by
least quota left last. An account at 100% weekly is visibly flagged. Text sketch:

```
Accounts
 ● you@example.com        Team   5h  3%  resets 19:00 | week  7%  resets Nov 19   used 1.2M tok  $14.20
 ○ work@example.org       Max    5h  0%             | week 100%  resets Nov 20   used 9.8M tok  $96.10   (reading 2h old)
```

### Onboarding text (no code that logs in)

When only one account is known, the accounts area explains in one line that other accounts appear
once they have used Claude on this machine, and how to get fresh numbers for them. Wording only;
no code that logs in.

### README

Document both layers, the claude-swap credit and link (MIT), the custom-command contract,
`claude_dirs`, and the `--no-share-history` caveat.

## Decisions made by the designer (owner may overrule)

- **D1 account key.** Internal database key only (never displayed): `sha8(accountUuid:organizationUuid)` for Claude (the org is part of the key so one email in two orgs stays two accounts; `session-owner` carries both ids), because the transcript
  gives only the uuid. The signed-in account's ids are read from `oauthAccount.accountUuid` / `organizationUuid` / `emailAddress` in
  `~/.claude.json` (metadata, no token; verified: `claude auth status --json` has no uuid). #17's `sha8(email)` keys are migrated to the
  uuid key once the uuid for that email is known (kv labels follow, and the email moves into the registry). If the uuid cannot be
  obtained for the signed-in account, fall back to the current `sha8(email)` behaviour and say so.
- **D2 order of work.** Layer 1 (usage attribution plus key unification, Tasks 1-5) first and
  shippable on its own; then the read-only quota source with the claude-swap backend; then
  `claude_dirs`, codex-multi-auth and the custom command.
- **D3 sessions without an owner** stay under the fallback key and show as "unknown"; no
  guessing from timing.
- **D4 `claude_dirs` is unverified.** Before building it, someone with a second account must run
  `CLAUDE_CONFIG_DIR=~/.claude-test claude` (`/login`), then
  `CLAUDE_CONFIG_DIR=~/.claude-test claude -p --no-session-persistence "/usage"` and confirm it
  prints 5-hour and weekly lines. If it does not, claude-swap stays the only fresh source.

## Open questions for the builder

- Codex: does `codex app-server` / `codex login status` expose the account email without reading
  `auth.json`? If so Codex accounts get the same registry; if not they keep the hash-with-label
  behaviour from #16 for now (display emails only where a safe source exists).

- Does a session resumed (`--resume`) under another account get a new `session-owner` line or a
  second line in the same file? Seen so far: never two owners in one file. If it can happen,
  decide per-row attribution by position in the file instead of per-file.
- How to match a claude-swap account to the Layer-1 key: by `uuid` if `list --json` exposes it
  (its `sequence.json` does; `list --json` showed `organizationUuid`, not confirmed for the
  account uuid), else by email via the identity label.
- Exact refresh cadence for `list()` (claude-swap polls usage itself and caches; reuse its
  cache age, `usageAgeSeconds`, rather than forcing fetches).

## Testing

Unit and integration tests with fixtures only (no real accounts): transcripts with and without
`session-owner`, multi-account usage rows, key migration from `sha8(email)`, a fake
`claude-swap` script for `list --json` (ok, unknown schemaVersion, timeout, absent, exit 1), and
the custom-command contract.
Never commit real transcript lines or real emails.

## Left to the builder

UI layout of the accounts area, the exact internal trait/struct
names, DB schema additions beyond the existing columns (prefer none), the later
backends' details, and the README wording.
