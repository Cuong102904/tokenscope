# Multi-account Claude (and Codex): per-account usage, and optional switching

Status: draft for approval. Date: 2026-11-17.

## Intent and why

The owner runs two Claude accounts (a Team org account and a personal one) and switches between
them with [claude-swap](https://github.com/realiti4/claude-swap) (MIT, Python). tokenscope today
cannot tell the accounts apart:

- Token and cost rows for Claude are all written with the fixed account `claude_account`
  (default `"default"`) in `ClaudeAdapter::collect` (`src/adapters/claude.rs`).
- Limit readings (5h / weekly) are keyed `sha8(email)` of the signed-in account since #17, so
  usage and limits now sit under different account keys and cannot be joined.
- Claude and Codex only report limits for the account that is logged in right now.

The owner wants to see how much each account has spent, see every account's limits, and, where a
switcher is installed, switch from the dashboard. Not everyone uses claude-swap or
codex-multi-auth, so the feature must work, in a useful reduced form, without any of them.

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
  with `pct`, `resetsAt`) for every account, active or not. `claude-swap switch <num|email>`
  changes the active account.
- **Key mismatch to resolve.** `session-owner` carries `accountUuid`, not an email. #17 keys
  limits by `sha8(email)`. Both must end up on one key (see Decisions, D1).
- **Login cannot be automated.** Login is OAuth in the browser inside `claude /login`. Adding an
  account to claude-swap is `claude /login` as the new account, then `claude-swap add`.
  tokenscope must not do or wrap the login (`CLAUDE.md`: never open credential files).

## Goals

1. Token and cost rows for Claude are attributed to the real account, with no tool installed.
2. The Limits and cost views can show and filter per account for Claude, using names
   (email, or the user's own label from #16) instead of hashes.
3. When a switcher backend is available, show every account's limits, not only the active one's,
   and offer a Switch action with a confirmation.
4. Without any switcher, everything in 1 and 2 works and the Switch control is absent.
5. The switcher is pluggable: claude-swap first, then codex-multi-auth, then a user-defined
   command in `config.toml` that speaks a documented JSON contract.

## Non-goals

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
- A Switch that silently changes the login under a running Claude session without the user
  being told.
- Different keys for the same account in usage vs limits, so the dashboard shows it twice.
- Making tokenscope fail or slow down when claude-swap is missing, slow or returns unexpected JSON.

## Constraints

- Rust, single static binary, existing adapter/collect/query layout; reuse the `account` columns
  in `usage`, `limit_snapshots`, `limit_events`, and `kv` for labels (`label:claude:<key>`).
- Privacy rules in `CLAUDE.md` hold: account ids stored only as `sha8`; the one existing
  exception is the signed-in Claude account's email as a local display name (owner's decision in
  #17). Names for other accounts may come from claude-swap's output on the same local-only basis.
- Server binds 127.0.0.1, endpoints that run processes need the `X-Tokenscope` header. A Switch
  endpoint is such an endpoint, and a write: POST only, same header check.
- Tests never run the real `claude`, `codex`, `claude-swap`; they use a fake script on `PATH`
  (serialized with the spawn lock added in #17's test fix).

## Approaches considered

1. **Own switcher in tokenscope** (snapshot and swap credentials). Rejected: this is the hard,
   fragile part of claude-swap (rotating refresh tokens, macOS keychain, races with running
   Claude) and breaks the no-credentials rule.
2. **Depend on claude-swap only** (call its CLI, nothing without it). Rejected by the owner:
   most users will not have it, and per-account usage does not need it.
3. **Two layers (chosen by the owner):** passive attribution that needs nothing, plus an optional,
   pluggable switcher interface with claude-swap as first backend and a user-defined command as
   the generic escape hatch.

## Design

### Layer 1: passive attribution (no external tool)

- `ClaudeAdapter::collect` reads the `session-owner` line of each transcript and sets `account`
  for that file's usage rows. Subagent files share the parent's `sessionId`; resolve their
  account from the parent session's owner (or from their own line if present).
- Account key and mapping: see D1. Sessions with no owner line use the existing fallback
  (`claude_account`, i.e. `"default"`), shown as "unknown".
- Limit readings: the signed-in account's key comes from the same identity, so a `/usage` or
  statusline reading lands on the same key as that account's token rows.
- Names: the dashboard (#16) already shows a user label, else `claude:<sha8>`; the signed-in
  account gets its email (#17). Other accounts show their hash until named, or until a switcher
  backend supplies a name (Layer 2).
- Account views (cost and limits) filter and group by account for Claude as they already do for
  Codex.

### Layer 2: optional switcher

A small internal interface, independent of Claude/Codex, with two operations:

- `list() -> Vec<ManagedAccount>`: per account `{ref, display_name, active, org?, plan?,
  limits: five_hour/weekly/scoped (percent, resets_at)}`; account identity is whatever the
  backend can give that maps to the Layer-1 key (see D1).
- `switch(ref) -> Result`.

Backends:

1. **claude-swap**: `claude-swap list --json`, `claude-swap switch <num>`. Found on `PATH`;
   absent means the layer is off. Accept `schemaVersion` 1 only and degrade to "off, with a
   note" on anything else. Run with a timeout, never block collect.
2. **codex-multi-auth** (second): list and switch via its CLI; details to be settled when
   building it; same interface.
3. **Custom command** in `config.toml`, for any other tool or the user's own script:

   ```toml
   [switcher.claude]
   list   = "my-script list"        # prints the JSON below on stdout
   switch = "my-script switch {ref}"
   ```

   The documented JSON for `list` mirrors `ManagedAccount` (schema version field included).
   tokenscope runs it without a shell string interpolation of untrusted input: `ref` comes only
   from the previous `list` output and is passed as a single argument.

What Layer 2 feeds:

- `list()` results are written as ordinary `limit_snapshots` (source e.g. `claude-swap`) so the
  existing Limits cards, history and projections work unchanged, now for every account.
- A Switch button appears on a non-active account row only when a backend is available. It asks
  for confirmation that states what will change (the login used by new Claude sessions), calls
  `switch`, then refreshes. The endpoint is a POST guarded like the other process-running ones.
- No backend: no button, no error; one line in the empty state explains how to get one
  (link to claude-swap, or the custom-command snippet).

### Onboarding text (no code that logs in)

When claude-swap is installed but manages fewer than two accounts, the accounts area shows the
three steps: `claude /login` as the new account, `claude-swap add`, repeat. Wording only.

### README

Document both layers, the claude-swap credit and link (MIT), the custom-command contract, and
the `--no-share-history` caveat.

## Decisions made by the designer (owner may overrule)

- **D1 account key.** Use `sha8(accountUuid)` for Claude as the one key, because the transcript
  gives only the uuid. The signed-in account's uuid is read from `oauthAccount.accountUuid` in
  `~/.claude.json` (metadata, no token), or from `claude auth status --json` if it exposes it;
  the builder verifies which works and records it. #17's `sha8(email)` keys are migrated to the
  uuid key once the uuid for that email is known (kv labels follow). If the uuid cannot be
  obtained for the signed-in account, fall back to the current `sha8(email)` behaviour and say so.
- **D2 order of work.** Layer 1 (usage attribution plus key unification) first and shippable on
  its own; then switcher interface plus claude-swap backend plus Switch button; then
  codex-multi-auth and the custom command.
- **D3 sessions without an owner** stay under the fallback key and show as "unknown"; no
  guessing from timing.

## Open questions for the builder

- What happens to a running Claude session when `claude-swap switch` runs, and does claude-swap
  refuse or warn when Claude is running (it has process detection)? The confirmation text and
  whether to pass a flag depend on this. Check claude-swap's docs/help; do not assume.
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
`claude-swap` script for `list --json` (ok, unknown schemaVersion, timeout, absent) and for
`switch`, the POST endpoint's header and method checks, and the custom-command contract.
Never commit real transcript lines or real emails.

## Left to the builder

UI layout of the accounts area and the Switch confirmation, the exact internal trait/struct
names, DB schema additions beyond the existing columns (prefer none), the second and third
backends' details, and the README wording.
