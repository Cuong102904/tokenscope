# agent-usage Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Một binary Rust `agent-usage` đọc log local của Claude Code, Codex, OpenCode và hạn mức của agy, lưu vào SQLite, hiển thị chi phí ước tính + hạn mức qua web (6 tab), TUI, statusline.

**Architecture:** Mỗi hãng một adapter (chỉ đọc) trả về `Batch` bản ghi chuẩn hoá; collector ghi vào `~/.agent-usage/usage.db` (WAL). Chi phí tính lúc truy vấn từ `pricing.toml`. `serve` (axum) chạy luồng thu thập nền 30 giây/lần và phục vụ trang tĩnh nhúng sẵn + API JSON chỉ-đọc.

**Tech Stack:** Rust 1.99 (edition 2024), clap 4, rusqlite (bundled), serde/serde_json, toml, chrono, walkdir, axum 0.8 + tokio, ratatui + crossterm, sha2, anyhow. Dev: tempfile, assert_cmd. Frontend: HTML + JS thuần, Chart.js 4.4.1 vendored (không CDN).

**Spec:** `docs/superpowers/specs/2026-11-04-agent-usage-design.md`

## Global Constraints

- Một binary, Linux. Dữ liệu chỉ ở `~/.agent-usage/`. Server chỉ bind `127.0.0.1`, cổng mặc định `8765`, không CORS, không API ghi.
- Chỉ **đọc** dữ liệu công cụ khác; SQLite của họ mở bằng URI `file:<path>?mode=ro`.
- Danh sách không-bao-giờ-đọc (mã hoá cứng, có test): mọi file tên `auth.json`, `openai-codex-accounts.json*`, `.credentials.json`, `oauth*.json`, và mọi đường dẫn chứa `credential` hoặc `token`. Không đọc/log/lưu token.
- Không gọi API nhà cung cấp. agy `/usage` chỉ chạy theo yêu cầu, tối đa 1 lần / 5 phút.
- Định danh (email, account id) chỉ lưu dạng `sha256` hex 8 ký tự đầu; hiển thị nhãn từ `config.toml` hoặc băm.
- Mọi giao diện có dòng: "Ước tính theo giá API, không phải hoá đơn."
- Model không có giá → chi phí `null`, hiện "n/a", liệt kê riêng; không bao giờ tính $0 thầm lặng.
- Hạn mức chuẩn hoá về **% đã dùng**; luôn kèm `measured_at`; cờ `stale` khi quá ngưỡng.
- Ngày (`day`) tính theo **giờ địa phương** của máy, không phải UTC.
- Adapter lỗi → hãng đó `unavailable` kèm lý do, phần khác vẫn chạy.
- Giao diện web không phụ thuộc mạng; tự làm mới 30 giây; bộ lọc lưu trong URL.

## Quy ước token chuẩn hoá (quyết định của plan, đã kiểm trên dữ liệu thật 2026-11-05)

Cột trong `usage`: `input` = input **không** tính cache; `output` = output **đã gồm** reasoning (để tính tiền theo giá output); `reasoning` = chỉ để hiển thị; `cache_read`, `cache_write_5m`, `cache_write_1h`.

| Hãng | input | output | cache_read | cache_write_5m |
|---|---|---|---|---|
| Claude | `input_tokens` | `output_tokens` | `cache_read_input_tokens` | `cache_creation.ephemeral_5m_input_tokens` (thiếu `cache_creation` → `cache_creation_input_tokens`); 1h = `ephemeral_1h_input_tokens` |
| Codex (`last_token_usage`) | `input_tokens − cached_input_tokens` | `output_tokens` (đã gồm reasoning, `last_usage_includes_reasoning: true`) | `cached_input_tokens` | `cache_write_input_tokens` |
| OpenCode (`tokens`) | `input` | `output + reasoning` (vì `total = input+output+reasoning+cache.read`) | `cache.read` | `cache.write` |

## Review Focus

1. Dòng cuối của file log đang được ghi dở (JSON chưa đủ) → bỏ qua dòng đó, không lỗi, lần quét sau đọc được. (Test ở Task 2.)
2. Thiếu hẳn một nguồn (không có `~/.codex`, không có `agy` trong PATH) → `doctor` báo "not found", `collect`/`serve` vẫn chạy cho hãng khác. (Test ở Task 5.)
3. Codex lặp sự kiện `token_count` với cùng `total_token_usage` → không cộng trùng. (Test ở Task 7.)
4. Status line cũ của người dùng bị treo/lỗi, hoặc thư mục snapshots không ghi được → vẫn in đầu ra của status line cũ (hoặc chuỗi rỗng) trong ≤ 1 giây, exit 0. (Test ở Task 6.)
5. Một tin nhắn lúc 06:30 giờ VN (23:30 UTC hôm trước) → thuộc ngày VN, không phải ngày UTC. (Test ở Task 8.)

---

## Cấu trúc file

```
agent-usage/
  Cargo.toml
  prototype/                 # bản Python cũ, chuyển vào đây ở Task 1 (giữ nguyên)
  assets/pricing.toml        # bảng giá mặc định, nhúng bằng include_str!
  assets/web/index.html      # khung + 6 tab (hash router)
  assets/web/app.js          # fetch API, vẽ chart
  assets/web/chart.umd.min.js# Chart.js 4.4.1 vendored
  src/main.rs                # clap: collect | serve | tui | statusline | status | doctor
  src/paths.rs               # Paths từ $HOME (để test dùng HOME giả)
  src/model.rs               # Vendor, UsageRecord, LimitSnapshot, LimitEvent, SkillEvent, ToolEvent, Batch
  src/safety.rs              # never-read list + open_guarded()
  src/db.rs                  # schema, migrate, write_batch
  src/scan.rs                # FileTracker (files table), read_jsonl_lines
  src/pricing.rs             # PriceTable, cost()
  src/config.rs              # config.toml: nhãn tài khoản, ngưỡng
  src/collect.rs             # chạy các adapter, ghi DB, AdapterStatus
  src/adapters/{mod,claude,claude_snapshots,codex,opencode,agy}.rs
  src/statusline.rs
  src/query/{mod,cost,sessions,limits,skills,active}.rs   # trả về struct Serialize
  src/server.rs
  src/tui.rs
  tests/fixtures/{claude,codex,opencode,agy}/...
  tests/*.rs
```

## Lược đồ DB (Task 1 tạo; các task sau dùng đúng tên này)

```sql
CREATE TABLE IF NOT EXISTS files(path TEXT PRIMARY KEY, size INTEGER, mtime INTEGER);
CREATE TABLE IF NOT EXISTS usage(
  msg_key TEXT PRIMARY KEY, vendor TEXT, account TEXT, session_id TEXT, project TEXT,
  ts INTEGER, day TEXT, model TEXT, input INTEGER, output INTEGER, reasoning INTEGER,
  cache_read INTEGER, cache_write_5m INTEGER, cache_write_1h INTEGER,
  sidechain INTEGER, skill TEXT, plugin TEXT);
CREATE INDEX IF NOT EXISTS usage_day ON usage(day);
CREATE INDEX IF NOT EXISTS usage_session ON usage(vendor, session_id);
CREATE TABLE IF NOT EXISTS limit_snapshots(
  vendor TEXT, account TEXT, scope TEXT, window_minutes INTEGER, used_percent REAL,
  resets_at INTEGER, plan_type TEXT, measured_at INTEGER, source TEXT,
  PRIMARY KEY(vendor, account, scope, measured_at));
CREATE TABLE IF NOT EXISTS limit_events(vendor TEXT, account TEXT, ts INTEGER, scope TEXT, resets_at INTEGER,
  PRIMARY KEY(vendor, account, ts, scope));
CREATE TABLE IF NOT EXISTS skill_events(vendor TEXT, session_id TEXT, ts INTEGER, skill TEXT, kind TEXT, key TEXT PRIMARY KEY);
CREATE TABLE IF NOT EXISTS tool_events(vendor TEXT, session_id TEXT, ts INTEGER, tool TEXT, key TEXT PRIMARY KEY);
CREATE TABLE IF NOT EXISTS sessions(vendor TEXT, session_id TEXT, project TEXT, title TEXT, source_path TEXT, last_write INTEGER,
  PRIMARY KEY(vendor, session_id));
```
`account` dùng chuỗi rỗng `''` thay cho NULL (để khoá chính hoạt động). `ts`, `resets_at`, `measured_at` là epoch giây UTC.

---

# Mốc 1 — Lõi, Claude chi phí, statusline

### Task 1: Khung crate, kiểu dữ liệu, DB, guard an toàn

**Files:**
- Create: `Cargo.toml`, `src/main.rs`, `src/paths.rs`, `src/model.rs`, `src/safety.rs`, `src/db.rs`, `.gitignore`
- Move: `cli.py dashboard.py pricing.py report.py scanner.py static/` → `prototype/`
- Test: `tests/db_safety.rs`

**Interfaces:**
- Produces:
  - `paths::Paths { home: PathBuf }` với `Paths::from_env() -> Paths`, `.data_dir()` = `home/.agent-usage`, `.db()`, `.snapshots_dir()`, `.config()`, `.claude_projects()`, `.codex_sessions()`, `.opencode_db()`, `.agy_log_dir()` (`home/.gemini/antigravity-cli/log`).
  - `model::Vendor { Claude, Codex, OpenCode, Agy }` + `as_str() -> &'static str` ("claude","codex","opencode","agy").
  - `model::UsageRecord { msg_key: String, vendor: Vendor, account: String, session_id: String, project: String, ts: i64, model: String, input: u64, output: u64, reasoning: u64, cache_read: u64, cache_write_5m: u64, cache_write_1h: u64, sidechain: bool, skill: Option<String>, plugin: Option<String> }`
  - `model::LimitSnapshot { vendor, account: String, scope: String, window_minutes: Option<i64>, used_percent: f64, resets_at: Option<i64>, plan_type: Option<String>, measured_at: i64, source: String }`
  - `model::LimitEvent { vendor, account: String, ts: i64, scope: String, resets_at: Option<i64> }`
  - `model::SkillEvent { key: String, vendor, session_id: String, ts: i64, skill: String, kind: &'static str }` (kind ∈ `"invoked"`, `"file_read"`)
  - `model::ToolEvent { key: String, vendor, session_id: String, ts: i64, tool: String }`
  - `model::SessionInfo { vendor, session_id: String, project: String, title: Option<String>, source_path: String, last_write: i64 }`
  - `model::Batch { usage: Vec<UsageRecord>, limits: Vec<LimitSnapshot>, limit_events: Vec<LimitEvent>, skills: Vec<SkillEvent>, tools: Vec<ToolEvent>, sessions: Vec<SessionInfo> }` (derive Default)
  - `db::open(path: &Path) -> anyhow::Result<rusqlite::Connection>` (tạo thư mục, `journal_mode=WAL`, `busy_timeout=5000`, chạy schema ở trên)
  - `db::write_batch(conn: &mut Connection, b: &Batch) -> anyhow::Result<()>` — một transaction. `usage` upsert: khi trùng `msg_key` chỉ thay nếu `output` mới lớn hơn. Các bảng khác `INSERT OR REPLACE` (sessions) / `INSERT OR IGNORE` (còn lại). Cột `day` = `chrono::Local` từ `ts`, dạng `YYYY-MM-DD`.
  - `model::sha8(s: &str) -> String` — 8 ký tự hex đầu của sha256; dùng cho mọi định danh tài khoản.
  - `safety::is_forbidden(p: &Path) -> bool`, `safety::open_guarded(p: &Path) -> io::Result<File>` (trả `PermissionDenied` nếu forbidden). **Mọi adapter mở file qua hàm này.**

- [ ] **Step 1:** `cd ~/Desktop/DEV/agent-usage && git init`, `mkdir prototype && git mv`-tương đương (`mv`) các file Python vào `prototype/`, `cargo init --name agent-usage`, thêm dependencies ở Tech Stack. `.gitignore`: `target/`, `__pycache__/`.
- [ ] **Step 2: Viết test**

```rust
#[test] fn forbidden_paths() {
    for p in ["/h/.codex/auth.json", "/h/.codex/multi-auth/openai-codex-accounts.json.bak",
              "/h/.claude/.credentials.json", "/h/x/oauth_tokens.json", "/h/.local/share/opencode/auth.json"] {
        assert!(safety::is_forbidden(Path::new(p)), "{p}");
    }
    assert!(!safety::is_forbidden(Path::new("/h/.claude/projects/a/b.jsonl")));
    assert!(!safety::is_forbidden(Path::new("/h/.codex/multi-auth/quota-cache.json")));
}
#[test] fn upsert_keeps_max_output() {
    // write_batch hai lần cùng msg_key, output 10 rồi 5 rồi 20 → còn 20; một dòng duy nhất
}
#[test] fn open_creates_wal_db() { /* tempdir; open(); PRAGMA journal_mode == "wal" */ }
```
- [ ] **Step 3:** `cargo test --test db_safety` → FAIL (chưa có module).
- [ ] **Step 4:** Hiện thực các module ở Interfaces. `main.rs` chỉ khai báo clap với 6 lệnh con trả `todo!()` trừ khi task sau điền.
- [ ] **Step 5:** `cargo test --test db_safety` → PASS. `cargo clippy -- -D warnings` sạch.
- [ ] **Step 6:** Commit `feat: crate skeleton, schema, safety guard`.

### Task 2: Quét tăng dần + đọc JSONL an toàn

**Files:** Create `src/scan.rs`; Test `tests/scan.rs`

**Interfaces:**
- Consumes: `safety::open_guarded`, bảng `files`.
- Produces:
  - `scan::changed_files(conn: &Connection, root: &Path, ext: &str) -> anyhow::Result<Vec<PathBuf>>` — walkdir đệ quy, so `(size, mtime)` với bảng `files`; root không tồn tại → `Ok(vec![])`.
  - `scan::mark_done(conn: &Connection, paths: &[PathBuf]) -> anyhow::Result<()>`
  - `scan::read_jsonl(p: &Path) -> anyhow::Result<Vec<serde_json::Value>>` — bỏ qua dòng không parse được **chỉ khi là dòng cuối và file không kết thúc bằng `\n`**; dòng hỏng ở giữa: bỏ qua và đếm.
  - Ghi `files` **sau** khi `write_batch` thành công (collector làm, Task 5), để lỗi giữa chừng thì lần sau quét lại.

- [ ] **Step 1: Test**

```rust
#[test] fn partial_last_line_skipped_then_read() {
    // file: dòng1 hợp lệ + "\n" + "{\"a\":" (không \n) → read_jsonl trả 1 phần tử
    // nối thêm "1}\n" → trả 2 phần tử
}
#[test] fn unchanged_file_not_returned_twice() { /* changed_files → 1; mark_done; changed_files → 0; sửa file → 1 */ }
#[test] fn missing_root_is_empty() { /* changed_files(conn, "/không/tồn/tại", "jsonl") == [] */ }
```
- [ ] **Step 2:** `cargo test --test scan` → FAIL. **Step 3:** hiện thực. **Step 4:** PASS.
- [ ] **Step 5:** Commit `feat: incremental file scan and tolerant jsonl reader`.

### Task 3: Bảng giá và tính chi phí

**Files:** Create `src/pricing.rs`, `assets/pricing.toml`; Test `tests/pricing.rs`

**Interfaces:**
- Produces:
  - `pricing::Price { input: f64, output: f64, cache_read: f64 }` ($/1M token); hệ số ghi cache cố định `WRITE_5M = 1.25`, `WRITE_1H = 2.0` nhân với `input`.
  - `pricing::PriceTable::load(paths: &Paths) -> anyhow::Result<PriceTable>` — `~/.agent-usage/pricing.toml` nếu có, không thì bản nhúng.
  - `PriceTable::lookup(&self, model: &str) -> Option<&Price>` — khớp **tiền tố dài nhất**.
  - `PriceTable::cost(&self, model: &str, input, output, cache_read, w5m, w1h: u64) -> Option<f64>`.
- `assets/pricing.toml` dạng `[models."claude-opus-5-5"] input=4 output=20 cache_read=0.20` kèm `source`/`date` ở đầu file. Giá Claude chép đúng spec mục 6: fable-5-1 & mythos-5-1 10/50/0.25; fable-5 & mythos-5 10/50/1.00; opus-5-5 4/20/0.20; opus-5, opus-4-8, opus-4-7, opus-4-6 5/25/0.50; sonnet-5-5, sonnet-5 2/10/0.20; sonnet-4-6 3/15/0.30; haiku-5-5 0.10/0.50/0.01; haiku-4-5 1/5/0.10 (khoá có tiền tố `claude-`). OpenAI/Google thêm ở Task 7/10.

- [ ] **Step 1: Test**

```rust
#[test] fn longest_prefix_wins() { // "claude-opus-5-5-20261001" → 4.0 input, không phải opus-5 (5.0)
}
#[test] fn cost_formula() {
    // opus-5-5: 1M input + 1M output + 1M cache_read + 1M w5m + 1M w1h
    // = 4 + 20 + 0.20 + 4*1.25 + 4*2.0 = 37.20
    assert!((t.cost("claude-opus-5-5", 1_000_000,1_000_000,1_000_000,1_000_000,1_000_000).unwrap() - 37.20).abs() < 1e-9);
}
#[test] fn unknown_model_is_none() { assert!(t.cost("gpt-unknown", 1,1,0,0,0).is_none()); }
```
- [ ] **Step 2–4:** FAIL → hiện thực → PASS.
- [ ] **Step 5:** Commit `feat: configurable price table`.

### Task 4: Adapter Claude (token, skill, công cụ, sự cố chạm giới hạn)

**Files:** Create `src/adapters/mod.rs`, `src/adapters/claude.rs`, `tests/fixtures/claude/projects/-home-u-proj/s1.jsonl`, `.../s1/subagents/agent-x.jsonl`; Test `tests/claude.rs`

**Interfaces:**
- Produces:
  - `adapters::Adapter` trait:
    ```rust
    pub trait Adapter {
        fn vendor(&self) -> Vendor;
        fn discover(&self, paths: &Paths) -> Discovery;          // Discovery { found: bool, detail: String }
        fn collect(&self, ctx: &mut CollectCtx) -> anyhow::Result<Batch>;
    }
    pub struct CollectCtx<'a> { pub conn: &'a Connection, pub paths: &'a Paths, pub config: &'a Config, pub done_files: Vec<PathBuf> }
    ```
    Adapter thêm file đã xử lý vào `ctx.done_files`; collector gọi `scan::mark_done` sau khi ghi.
  - `adapters::claude::ClaudeAdapter`.
  - `adapters::claude::parse_line(v: &Value, account: &str, out: &mut Batch)` (pub để test).
- Quy tắc (spec 5.1, đã kiểm):
  - Chỉ dòng `type == "assistant"` có `message.usage`. Bỏ model bắt đầu bằng `<`.
  - `msg_key = "claude:" + message.id + "|" + requestId`. `session_id = sessionId`, `project = cwd`, `sidechain = isSidechain`, `skill = attributionSkill`, `plugin = attributionPlugin` (trường cấp cao nhất).
  - Mỗi `message.content[]` có `type=="tool_use"`: `ToolEvent { key: "claude:"+id, tool: name }`; nếu `name=="Skill"` thêm `SkillEvent { kind:"invoked", skill: input.skill, key: "claude:"+id }`.
  - Dòng có `apiError=="usage_limit_reached"`: `LimitEvent { scope: apiErrorParams.rate_limit_info.rateLimitType, resets_at: ...resetsAt, ts: timestamp }` (không tạo usage).
  - Mỗi file → một `SessionInfo { last_write: mtime, source_path }` theo `sessionId`.
  - `account` = `config.claude_account` (mặc định `"default"`).

- [ ] **Step 1:** Tạo fixture tổng hợp (không chép hội thoại thật): `s1.jsonl` có 2 bản ghi assistant cùng `message.id`/`requestId` (output 5 và 50), 1 bản ghi model `<synthetic>` có `apiError` five_hour `resetsAt: 1792996200`, 1 bản ghi có `attributionSkill:"superpowers:brainstorming"` và tool_use `Skill`, 1 dòng cuối viết dở. `subagents/agent-x.jsonl` có 1 bản ghi `isSidechain:true`.
- [ ] **Step 2: Test**

```rust
#[test] fn claude_fixture() {
    // collect qua ClaudeAdapter với Paths{home: fixtures/claude-home}, write_batch vào DB tạm
    // SELECT count(*) FROM usage == 3 (trùng gộp, <synthetic> bỏ)
    // output của bản ghi trùng == 50
    // SELECT count(*) FROM usage WHERE sidechain=1 == 1
    // limit_events: 1 dòng scope "five_hour", resets_at 1792996200
    // skill_events: 1 dòng kind "invoked"; usage.skill có "superpowers:brainstorming"
}
#[test] fn cache_creation_fallback() { /* thiếu cache_creation → cache_write_5m = cache_creation_input_tokens */ }
```
- [ ] **Step 3–5:** FAIL → hiện thực → PASS.
- [ ] **Step 6:** Commit `feat: claude adapter`.

### Task 5: Collector, `collect` / `status` / `doctor`, đối chiếu và đo hiệu năng

**Files:** Create `src/collect.rs`, `src/config.rs`; Modify `src/main.rs`; Create `src/query/mod.rs`, `src/query/cost.rs` (phần tổng); Test `tests/cli.rs`

**Interfaces:**
- Consumes: `Adapter`, `db::write_batch`, `scan::mark_done`, `PriceTable`.
- Produces:
  - `config::Config { claude_account: String, account_labels: HashMap<String,String>, active_minutes: i64 /*5*/, refresh_seconds: u64 /*30*/ }`, `Config::load(paths) -> anyhow::Result<Config>` (thiếu file → mặc định). Khoá nhãn dạng `"codex:f8ce04de"`.
  - `config::label(&self, vendor: Vendor, account: &str) -> String` → nhãn nếu có, không thì `"{vendor}:{account}"`, rỗng → `"{vendor}"`.
  - `collect::AdapterStatus { vendor: Vendor, found: bool, detail: String, records: usize, error: Option<String>, last_run: i64 }`
  - `collect::run_all(conn: &mut Connection, paths: &Paths, cfg: &Config, adapters: &[Box<dyn Adapter>]) -> Vec<AdapterStatus>` — cuối mỗi vòng ghi nguyên tử `~/.agent-usage/today.txt` (chi phí hôm nay, ví dụ `$12.00`) cho statusline; mỗi adapter bọc trong `catch_unwind` + `Result`; lỗi → `error = Some`, không dừng các adapter khác; lưu kết quả cuối vào bảng `kv(key TEXT PRIMARY KEY, value TEXT)` khoá `status:<vendor>` (JSON) để `serve`/`doctor` đọc.
  - `query::Filter { days: Option<u32>, vendor: Option<String>, model: Option<String>, project: Option<String> }` + `Filter::sql_where(&self) -> (String, Vec<rusqlite::types::Value>)`.
  - `query::cost::totals(conn, prices, f: &Filter) -> Totals { cost: f64, unpriced_models: Vec<String>, sessions: u64, messages: u64, input, output, cache_read, cache_write: u64 }` (cache_write = 5m+1h).
- CLI: `agent-usage collect [--bench]` (in số file, số bản ghi, thời gian); `agent-usage status [--json]` in `hôm nay $X · 7 ngày $Y · 30 ngày $Z`; `agent-usage doctor` in bảng AdapterStatus.

- [ ] **Step 1: Test** (assert_cmd, `HOME` = tempdir chép từ fixtures)

```rust
#[test] fn doctor_reports_missing_sources() {
    // HOME chỉ có .claude → doctor exit 0, stdout chứa "claude" + "found" và "codex" + "not found"
}
#[test] fn adapter_error_does_not_stop_others() {
    // adapter giả trả Err → status của nó có error, ClaudeAdapter vẫn ghi đủ 3 bản ghi
}
#[test] fn status_prints_totals() { /* stdout khớp regex r"hôm nay \$\d" */ }
```
- [ ] **Step 2–4:** FAIL → hiện thực → PASS.
- [ ] **Step 5: Đối chiếu bản Python (sanity 1%).** Chép `~/.claude/projects` vào `$SCRATCH/home/.claude/projects`, rồi chạy cả hai trên cùng bản chép:
  `HOME=$SCRATCH/home python3 prototype/cli.py stats` và `HOME=$SCRATCH/home cargo run --release -- status --json`. Expected: tổng chi phí và số tin nhắn lệch < 1%. Lệch hơn → xem bản ghi nào khác, dữ liệu gốc là chuẩn (spec 11b.3), ghi kết luận vào commit message.
- [ ] **Step 6: Hiệu năng.** `HOME=$SCRATCH/home cargo run --release -- collect --bench` trên DB trống. Expected: < 5,7 giây (ràng buộc cứng); chạy lại lần hai < 0,5 giây (mục tiêu).
- [ ] **Step 7:** Commit `feat: collector, status, doctor`.

### Task 6: `statusline` (bọc status line cũ) + nạp snapshot hạn mức Claude

**Files:** Create `src/statusline.rs`, `src/adapters/claude_snapshots.rs`; Modify `src/main.rs`; Test `tests/statusline.rs`

**Interfaces:**
- Produces:
  - CLI: `agent-usage statusline [--wrap <shell command>] [--show] [--account <name>] [--print-snippet]`.
    - Đọc toàn bộ stdin (JSON từ Claude Code). Nếu có `rate_limits.five_hour` / `rate_limits.seven_day` (`used_percentage`, `resets_at` epoch giây) → ghi `~/.agent-usage/snapshots/claude-<epoch_ms>-<pid>.json` = `{account, measured_at, five_hour:{used_percent,resets_at}, seven_day:{...}}`, ghi file tạm rồi `rename`. **Không mở SQLite.**
    - `--wrap`: chạy `sh -c <cmd>` với cùng stdin, timeout **800 ms**, in stdout của nó nguyên văn. Hết giờ/lỗi → in rỗng.
    - `--show`: nối thêm `5h 44% · tuần 54% · $12 hôm nay` — % lấy từ stdin hiện tại; số tiền đọc từ file `~/.agent-usage/today.txt` do collector ghi (nguyên tử) sau mỗi vòng thu thập. Không đọc DB; thiếu file thì bỏ phần tiền.
    - Mọi lỗi của phần ta bị nuốt; exit luôn 0.
    - `--account` ưu tiên hơn biến môi trường `AGENT_USAGE_ACCOUNT`, mặc định `config.claude_account`.
    - `--print-snippet`: đọc lệnh `statusLine.command` hiện có trong `~/.claude/settings.json` (chỉ đọc) và in đoạn JSON gợi ý `{"statusLine":{"type":"command","command":"agent-usage statusline --wrap '<lệnh cũ>'"}}`. **Không ghi file.**
  - `adapters::claude_snapshots::ClaudeSnapshotsAdapter` — đọc mọi `snapshots/claude-*.json`, tạo `LimitSnapshot { vendor: Claude, scope: "five_hour"|"weekly", window_minutes: 300|10080, source: "statusline" }`, sau khi ghi DB thành công thì xoá file (file của chính ta). Phiên bản đầu: đặt vendor Claude là `Vendor::Claude` (cùng enum, adapter riêng).

- [ ] **Step 1: Test**

```rust
#[test] fn wraps_and_writes_snapshot() {
    // stdin = {"rate_limits":{"five_hour":{"used_percentage":44,"resets_at":1792996200},"seven_day":{"used_percentage":54,"resets_at":1793500000}}}
    // --wrap "echo OLD" → stdout == "OLD\n"; snapshots/ có đúng 1 file chứa used_percent 44
}
#[test] fn hanging_wrapped_command_times_out() { /* --wrap "sleep 5" → xong < 1s, exit 0 */ }
#[test] fn unwritable_snapshot_dir_still_prints() { /* snapshots là file thường, không phải thư mục → stdout vẫn "OLD\n" */ }
#[test] fn missing_rate_limits_writes_nothing() { /* stdin {} → không file */ }
#[test] fn show_appends_today_cost() { /* today.txt = "$12.00" → --show in "5h 44% · tuần 54% · $12.00 hôm nay" */ }
#[test] fn snapshots_ingested_and_removed() { /* collect → limit_snapshots 2 dòng (five_hour, weekly); thư mục rỗng */ }
```
- [ ] **Step 2–4:** FAIL → hiện thực → PASS.
- [ ] **Step 5: Đo độ trễ.** `hyperfine 'echo "{}" | target/release/agent-usage statusline --wrap true'` (hoặc lặp 100 lần với `time`). Expected: thêm < 20 ms so với `true` (mục tiêu).
- [ ] **Step 6:** Chạy `agent-usage statusline --print-snippet`, đưa đoạn in ra cho **người dùng tự** dán vào `~/.claude/settings.json`. Kiểm: sau 1 lượt Claude, `ls ~/.agent-usage/snapshots/` có file.
- [ ] **Step 7:** Commit `feat: claude statusline wrapper and limit snapshots`.

# Mốc 2 — Codex + Web (tab Tổng quan, Chi phí, Session)

### Task 7: Adapter Codex (token, hạn mức, tài khoản) + giá OpenAI

**Files:** Create `src/adapters/codex.rs`, `tests/fixtures/codex/sessions/2026/11/02/rollout-a.jsonl`; Modify `assets/pricing.toml`; Test `tests/codex.rs`

**Interfaces:**
- Produces: `adapters::codex::CodexAdapter`.
- Quy tắc (đã kiểm 2026-11-05):
  - Dòng 1 `type=="session_meta"`: `session_id = payload.id`, `project = payload.cwd`, `account = sha8(payload.creator_account_id)` hoặc `""` nếu không có. **Không lưu giá trị gốc.**
  - `type=="turn_context"`: cập nhật `model = payload.model` cho các sự kiện sau.
  - `payload.type=="token_count"` có `info`: nếu `info.total_token_usage.total_tokens` **bằng** lần trước trong cùng file → bỏ (sự kiện lặp). Ngược lại tạo `UsageRecord` từ `info.last_token_usage` theo bảng quy ước token; `msg_key = "codex:" + session_id + ":" + total_tokens`; `ts = timestamp`.
  - `payload.rate_limits` (kể cả khi `info` null): với `primary` và `secondary` không null → `LimitSnapshot { scope: scope_for(window_minutes), window_minutes, used_percent, resets_at, plan_type, measured_at: ts, source: "codex-log" }`, `scope_for`: 300→`"five_hour"`, 10080→`"weekly"`, 43200→`"monthly"`, khác→`"window_<n>m"`. `limit_id != "codex"` → scope có tiền tố `"<limit_id>:"`.
  - `rate_limit_reached_type` khác null → `LimitEvent`.
  - Đọc `SKILL.md`: `function_call` / `exec_command` có đối số chứa `/SKILL.md` → `SkillEvent { kind:"file_read", skill: tên thư mục cha }`. Key = `"codex:" + call_id`.
  - Tên công cụ: `response_item` `payload.type=="function_call"` → `ToolEvent { tool: payload.name }`.
- Giá OpenAI: lấy từ trang giá chính thức của OpenAI cho các model thấy trong log (`gpt-5.6-terra`, `gpt-5.4-mini`, …: liệt kê bằng `SELECT DISTINCT model FROM usage WHERE vendor IN ('codex','opencode')`), ghi `source` URL + ngày vào `pricing.toml`. Không tìm được giá chính thức của model nào → để trống, hiện "n/a" (spec 11b.4).

- [ ] **Step 1:** Fixture tổng hợp: session_meta có `creator_account_id:"acct-test"`; turn_context model `gpt-5.6-terra`; 3 token_count (tổng 19771, 19771 lặp lại, 59761) với rate_limits primary 300 phút 37% và secondary 10080 phút 12%; 1 function_call đọc `/home/u/.agents/skills/foo/SKILL.md`.
- [ ] **Step 2: Test**

```rust
#[test] fn codex_fixture() {
    // usage: 2 dòng (bản lặp bị bỏ); dòng đầu input = 19606-9984 = 9622, cache_read 9984, output 165
    // account == sha8("acct-test"), và DB không chứa chuỗi "acct-test" ở bất kỳ bảng nào
    // limit_snapshots có scope "five_hour" 37.0 và "weekly" 12.0
    // skill_events: skill "foo", kind "file_read"
}
#[test] fn free_plan_monthly_window() { /* window 43200 → scope "monthly", không bịa five_hour */ }
```
- [ ] **Step 3–5:** FAIL → hiện thực → PASS. Chạy `agent-usage collect` trên dữ liệu thật; so `limit_snapshots` mới nhất với `/status` của Codex một lần bằng tay.
- [ ] **Step 6:** Commit `feat: codex adapter and openai prices`.

### Task 8: Truy vấn cho tab Tổng quan / Chi phí / Session

**Files:** Modify `src/query/cost.rs`; Create `src/query/sessions.rs`; Test `tests/query_cost.rs`

**Interfaces:**
- Consumes: `Filter`, `PriceTable`, `Config::label`.
- Produces (tất cả `#[derive(Serialize)]`, `cost: Option<f64>` = `None` khi có model chưa có giá trong nhóm):
  - `cost::daily(conn, prices, f) -> Vec<DayRow { day: String, by_model: BTreeMap<String, f64> }>` — liệt kê đủ mọi ngày trong khoảng (ngày trống = map rỗng).
  - `cost::by_model(...) -> Vec<ModelRow { model, vendor, cost: Option<f64>, messages, input, output, cache_read, cache_write }>`
  - `cost::by_project(...) -> Vec<ProjectRow { project, cost: Option<f64>, sessions, messages }>` (sắp giảm dần)
  - `cost::efficiency(...) -> Efficiency { cache_hit_ratio: f64 /* cache_read / (input+cache_read) */, subagent_cost_share: f64 }`
  - `cost::by_vendor(...) -> Vec<VendorRow { vendor, cost: Option<f64>, messages }>`
  - `sessions::list(conn, prices, cfg, f, limit: usize) -> Vec<SessionRow { vendor, session_id, project, title: Option<String>, models: Vec<String>, messages, subagent_messages, cost: Option<f64>, first_ts, last_ts }>` sắp theo cost giảm dần.
  - `sessions::timeline(conn, prices, vendor: &str, session_id: &str) -> Vec<(i64 /*ts*/, f64 /*cost tích luỹ*/)>`
- Chi phí mỗi dòng = `PriceTable::cost`; cộng gộp trong Rust (không trong SQL) vì giá theo tiền tố.

- [ ] **Step 1: Test**

```rust
#[test] fn day_uses_local_time() {
    // TZ=Asia/Ho_Chi_Minh, ts = 2026-11-04T23:30:00Z → day "2026-11-05"
    // (chạy test với std::env::set_var("TZ", ...) trước khi gọi, hoặc test riêng bằng assert_cmd)
}
#[test] fn unpriced_model_is_null_not_zero() { /* model "mystery-1" → by_model cost None; totals.unpriced_models chứa nó */ }
#[test] fn daily_fills_gaps() { /* dữ liệu ngày 1 và 3, days=3 → 3 DayRow, ngày 2 map rỗng */ }
#[test] fn timeline_is_cumulative_and_sorted() {}
```
- [ ] **Step 2–4:** FAIL → hiện thực → PASS.
- [ ] **Step 5:** Commit `feat: cost and session queries`.

### Task 9: Web server + giao diện tab 1, 3, 4

**Files:** Create `src/server.rs`, `assets/web/index.html`, `assets/web/app.js`, `assets/web/chart.umd.min.js`; Modify `src/main.rs`; Test `tests/server.rs`

**Interfaces:**
- Consumes: mọi hàm query ở Task 8, `collect::run_all`.
- Produces:
  - `agent-usage serve [--port 8765]`: bind **cứng** `127.0.0.1`; luồng nền gọi `run_all` mỗi `cfg.refresh_seconds`; DB đọc bằng kết nối riêng.
  - Route (GET, JSON, query `days, vendor, model, project` → `Filter`):
    - `/api/overview` → `{ totals_today, totals_7d, totals_30d, active_count, daily, limits_mini }` (`limits_mini` trả `[]` cho tới Task 12)
    - `/api/cost` → `{ totals, daily, by_model, by_project, by_vendor, efficiency }`
    - `/api/sessions?limit=200` → `Vec<SessionRow>`; `/api/session/{vendor}/{id}/timeline`
    - `/api/meta` → `{ vendors, models, projects }` để đổ bộ lọc
    - `/`, `/app.js`, `/chart.js` → tài sản nhúng bằng `include_str!`. Mọi thứ khác 404.
  - `chart.umd.min.js`: tải một lần `https://cdnjs.cloudflare.com/ajax/libs/Chart.js/4.4.1/chart.umd.min.js`, commit vào repo.
- Giao diện (dựa trên `prototype/static/index.html`: bảng màu, dark mode, `esc()`, thu gọn mục):
  - Thanh tab trên cùng: **Tổng quan · Hạn mức · Chi phí · Session · Skill & công cụ · Đang chạy** (tab 2, 5, 6 hiện "sắp có" cho tới Task 12–14). Hash router `#overview` …; bộ lọc chung lưu ở query string; tự làm mới 30 giây; dòng "Ước tính theo giá API, không phải hoá đơn." luôn hiện.
  - **Tab 1 Tổng quan:** 4 thẻ số (hôm nay, 7 ngày, 30 ngày, đang chạy); cột xếp chồng chi phí theo ngày theo model (30 ngày); khu vực thẻ hạn mức thu nhỏ.
  - **Tab 3 Chi phí:** cột xếp chồng theo ngày (theo bộ lọc); donut chi phí theo hãng; bảng theo model (cost "n/a" khi null); cột ngang top 10 project; hai thẻ số tỉ lệ cache và phần subagent; cảnh báo danh sách model chưa có giá.
  - **Tab 4 Session:** bảng sắp xếp được (project, hãng, model, tin nhắn, subagent, chi phí, đầu/cuối); bấm một dòng → khung dưới vẽ đường chi phí tích luỹ của session đó.

- [ ] **Step 1: Test** (khởi `server::router(state)` với DB fixture, gọi bằng `tower::ServiceExt::oneshot`)

```rust
#[test] fn api_cost_shape() { /* 200, JSON có keys totals,daily,by_model,by_project,by_vendor,efficiency */ }
#[test] fn unknown_path_404() {}
#[test] fn serves_index_without_cdn() { /* body "/" không chứa "cdnjs" hay "http" trong thẻ <script src> */ }
#[test] fn binds_loopback_only() { /* server::bind_addr(8765) == "127.0.0.1:8765" */ }
```
- [ ] **Step 2–4:** FAIL → hiện thực → PASS.
- [ ] **Step 5: Kiểm tay.** `cargo run --release -- serve`, mở `http://127.0.0.1:8765` trong Firefox: 3 tab có số, bộ lọc đổi URL, dark mode đọc được, rộng 390px không cuộn ngang, ngắt mạng vẫn có biểu đồ.
- [ ] **Step 6:** Commit `feat: web dashboard (overview, cost, sessions)`.

# Mốc 3 — agy, OpenCode, tab Hạn mức

### Task 10: Adapter agy (hạn mức theo yêu cầu)

**Files:** Create `src/adapters/agy.rs`, `tests/fixtures/agy/usage.txt`; Test `tests/agy.rs`

**Interfaces:**
- Produces:
  - `agy::parse_usage(text: &str, measured_at: i64, account: &str) -> Vec<LimitSnapshot>` — dòng tách tab `<nhóm>\tWeekly Limit Remaining\t<n>%\t<ISO UTC>`; `used_percent = 100 − n`; `scope = "weekly:" + nhóm` (ví dụ `"weekly:Gemini Models"`), `window_minutes = 10080`, `source = "agy /usage"`. Dòng không khớp → bỏ.
  - `agy::account_hint(log_dir: &Path) -> String` — dòng cuối cùng có `applyAuthResult: email=` trong file log mới nhất → `sha8(email)`; không thấy → `""`.
  - `AgyAdapter::collect` **không** chạy lệnh. Lệnh chỉ chạy qua `agy::refresh(paths, cfg, conn) -> anyhow::Result<RefreshOutcome>` (`Ran | Throttled { next_allowed: i64 } | NotInstalled`): chạy `agy -p "/usage"` timeout 30 giây, ghi snapshot; chặn nếu lần chạy trước < 300 giây (lưu ở bảng `kv` khoá `agy:last_refresh`).
  - Gọi `refresh` từ: `agent-usage status --refresh` và route `/api/limits?refresh=agy` (Task 12). `discover`: `which agy`.
- Giá Google: chưa cần ở bản này (agy chưa có token, spec 5.3).

- [ ] **Step 1: Test**

```rust
#[test] fn parse_usage_fixture() {
    // "Gemini Models\tWeekly Limit Remaining\t80%\t2026-11-09T00:00:00Z\nClaude and GPT models\tWeekly Limit Remaining\t35%\t2026-11-09T00:00:00Z\nrác"
    // → 2 snapshot, used_percent 20.0 và 65.0, resets_at = epoch của 2026-11-09T00:00:00Z
}
#[test] fn refresh_throttled_within_5_min() {}
#[test] fn email_never_stored_raw() { /* account_hint trả 8 ký tự hex */ }
```
- [ ] **Step 2–4:** FAIL → hiện thực → PASS. Kiểm tay: `agent-usage status --refresh` khớp `agy -p "/usage"`.
- [ ] **Step 5:** Commit `feat: agy limits adapter`.

### Task 11: Adapter OpenCode (token)

**Files:** Create `src/adapters/opencode.rs`, `tests/fixtures/opencode/make_db.sql`; Test `tests/opencode.rs`

**Interfaces:**
- Produces: `adapters::opencode::OpenCodeAdapter`.
- Quy tắc (đã kiểm): mở `opencode.db` bằng `file:…?mode=ro`. Tăng dần bằng `kv` khoá `opencode:max_time_updated` (lấy `message.time_updated > last`). Với `json_extract(data,'$.role')='assistant'`: `msg_key = "opencode:" + id`, `session_id`, `model = data.modelID`, token theo bảng quy ước (output = output + reasoning), `ts = data.time.created / 1000`. `project` và `title` từ bảng `session` (`directory`, `title`) → `SessionInfo`. `account = ""`. Bỏ qua cột `cost` (luôn 0).
- Test tạo DB tạm từ `make_db.sql` (bảng `message(id, session_id, time_created, time_updated, data)` và `session(id, directory, title, time_updated)` tối thiểu).

- [ ] **Step 1: Test**

```rust
#[test] fn opencode_tokens() {
    // tokens {"input":676,"output":60,"reasoning":22,"cache":{"write":0,"read":73216}}
    // → input 676, output 82, reasoning 22, cache_read 73216
}
#[test] fn incremental_by_time_updated() { /* chạy 2 lần → không thêm dòng; thêm 1 message → +1 */ }
#[test] fn locked_or_missing_db_is_unavailable_not_panic() {}
```
- [ ] **Step 2–4:** FAIL → hiện thực → PASS.
- [ ] **Step 5:** Commit `feat: opencode adapter`.

### Task 12: Truy vấn hạn mức + tab 2 Hạn mức

**Files:** Create `src/query/limits.rs`; Modify `src/server.rs`, `assets/web/*`, `src/db.rs` (hàm giảm mật độ); Test `tests/limits.rs`

**Interfaces:**
- Produces:
  - `limits::current(conn, cfg, now: i64) -> Vec<LimitCard { vendor, account_label, scope, window_minutes, used_percent, resets_at, measured_at, source, plan_type, stale: bool, eta: Option<i64>, eta_note: String, warning: Option<String> }>` — bản đo mới nhất cho mỗi `(vendor, account, scope)`. Bỏ thẻ có `resets_at < now` và đánh dấu `"đã reset, chờ số mới"` thay vì hiện số cũ.
  - Ngưỡng `stale`: `window_minutes ≤ 300` → 30 phút; còn lại → 6 giờ.
  - `limits::history(conn, vendor, account, scope, since: i64) -> Vec<(i64, f64)>`
  - `limits::eta(points: &[(i64, f64)], resets_at: i64) -> EtaResult { Some(ts) | None(reason) }`:
    - chỉ lấy điểm cùng `resets_at` hiện tại, trong 60 phút gần nhất;
    - cần ≥ 3 điểm, trải ≥ 10 phút; nếu có điểm giảm (không đơn điệu) → `None("số liệu lẫn tài khoản hoặc nhiễu")` và `warning` trên thẻ (spec 11b.2);
    - hồi quy tuyến tính, thời điểm chạm 100%; nếu sau `resets_at` → `None("không chạm trước khi reset")`.
  - `limits::events(conn, since) -> Vec<LimitEvent>`
  - `db::downsample_limits(conn, older_than: i64)` — với bản đo cũ hơn 90 ngày, giữ tối đa 1 mẫu / 5 phút / `(vendor, account, scope)`; gọi trong vòng thu thập 1 lần / ngày.
  - Route: `/api/limits` → `{ cards, events }`; `/api/limits/history?vendor&account&scope&hours=24`; `/api/limits?refresh=agy` gọi `agy::refresh`. `/api/overview.limits_mini` = `cards`.
- **Tab 2 Hạn mức:** nhóm theo hãng; mỗi thẻ: thanh % đã dùng (xanh < 70, vàng < 90, đỏ ≥ 90), giờ reset theo giờ địa phương + "còn x giờ", "đo lúc … (nguồn)", nhãn **CŨ** khi stale, dòng ETA hoặc lý do không có. Bấm thẻ → đường % theo thời gian (24 giờ / 7 ngày), có vạch reset. Dưới cùng: bảng các lần chạm giới hạn. Thẻ Claude khi chưa có snapshot: "Chưa có dữ liệu hạn mức — chạy `agent-usage statusline --print-snippet`". Thẻ Codex có `account=""`: ghi "log cũ không phân biệt tài khoản". Nút "Làm mới agy".

- [ ] **Step 1: Test**

```rust
#[test] fn stale_flag() { /* five_hour đo cách now 31 phút → stale; 29 phút → không */ }
#[test] fn eta_linear() { /* (0,10),(600,20),(1200,30) resets_at 100000 → Some(6600) */ }
#[test] fn eta_disabled_on_drop() { /* (0,40),(600,20),(1200,45) → None, reason chứa "lẫn" */ }
#[test] fn eta_after_reset_is_none() {}
#[test] fn expired_window_not_shown_as_current() {}
#[test] fn downsample_keeps_one_per_5min() {}
```
- [ ] **Step 2–4:** FAIL → hiện thực → PASS. Kiểm tay trong trình duyệt.
- [ ] **Step 5:** Commit `feat: limits view with stale flags and eta`.

# Mốc 4 — Skill, Đang chạy, TUI

### Task 13: Skill & công cụ + tab 5

**Files:** Create `src/query/skills.rs`; Modify `src/server.rs`, `assets/web/*`; Test `tests/skills.rs`

**Interfaces:**
- Produces:
  - `skills::by_cost(conn, prices, f) -> Vec<SkillCost { skill, plugin: Option<String>, cost: Option<f64>, messages }>` — từ `usage.skill` (Claude, "tin nhắn tạo ra trong lúc skill hoạt động").
  - `skills::invocations(conn, f) -> Vec<SkillCount { vendor, skill, kind, count, sessions }>` — từ `skill_events`.
  - `skills::by_plugin(conn, prices, f) -> Vec<PluginCost { plugin, cost: Option<f64> }>`
  - `skills::tools(conn, f) -> Vec<ToolCount { vendor, tool, count }>` top 20.
  - Route `/api/skills` → `{ by_cost, invocations, by_plugin, tools }`.
- **Tab 5:** cột ngang top 15 skill theo chi phí (Claude); cột ngang số lần gọi/đọc theo skill, tách màu theo hãng, chú thích "Codex: đếm số lần đọc SKILL.md — xếp hạng tương đối"; donut chi phí theo plugin; cột ngang top 20 công cụ. Mỗi biểu đồ có dòng "đo bằng cách nào".

- [ ] **Step 1: Test:** fixture Claude + Codex → `invocations` có 1 dòng `invoked` (claude) và 1 dòng `file_read` (codex); `by_cost` có `superpowers:brainstorming` với cost > 0.
- [ ] **Step 2–4:** FAIL → hiện thực → PASS.
- [ ] **Step 5:** Commit `feat: skills and tools view`.

### Task 14: Phiên đang chạy + trạng thái nguồn + tab 6

**Files:** Create `src/query/active.rs`; Modify `src/server.rs`, `assets/web/*`; Test `tests/active.rs`

**Interfaces:**
- Produces:
  - `active::list(conn, prices, cfg, now) -> Vec<ActiveSession { vendor, session_id, project, title, model, last_write, cost_so_far: Option<f64>, cost_last_hour: Option<f64> }>` — `sessions.last_write ≥ now − active_minutes*60`.
  - Collector cập nhật `sessions.last_write` theo mtime file (Claude, Codex) / `time_updated` (OpenCode) ở mọi vòng, kể cả khi file không đổi nội dung đáng kể.
  - Route `/api/active` → `{ sessions, sources: Vec<AdapterStatus> }`; `/api/overview.active_count` = `sessions.len()`.
- **Tab 6:** bảng phiên đang chạy (hãng, project, model, cập nhật lần cuối "x giây trước", chi phí đến giờ, chi phí 1 giờ qua); tự làm mới 10 giây khi tab đang mở. Dưới: bảng "Nguồn dữ liệu" (hãng, tìm thấy, số bản ghi, lần chạy cuối, lỗi).

- [ ] **Step 1: Test:** session có `last_write = now − 120` → có trong list; `now − 600` (active_minutes 5) → không.
- [ ] **Step 2–4:** FAIL → hiện thực → PASS.
- [ ] **Step 5:** Commit `feat: active sessions and source status`.

### Task 15: TUI

**Files:** Create `src/tui.rs`; Modify `src/main.rs`; Test `tests/tui.rs`

**Interfaces:**
- Consumes: `query::cost::totals`, `limits::current`, `active::list`, `collect::run_all`.
- Produces: `agent-usage tui`. Một màn hình, làm mới 5 giây (thu thập mỗi `refresh_seconds`):
  - Trên: 3 số (hôm nay / 7 ngày / 30 ngày) + dòng "ước tính".
  - Giữa: mỗi thẻ hạn mức một `Gauge` (nhãn `"<tài khoản> <scope> 44% · reset 15:30 · đo 3p trước"`, `[CŨ]` khi stale).
  - Dưới: bảng phiên đang chạy.
  - Phím: `q` thoát, `r` làm mới ngay (kể cả `agy::refresh`).
  - `tui::render(frame, &TuiState)` tách khỏi vòng sự kiện để test bằng `ratatui::backend::TestBackend`.
- [ ] **Step 1: Test:** `TestBackend` 100×30 với state giả → buffer chứa "hôm nay", "44%", "[CŨ]".
- [ ] **Step 2–4:** FAIL → hiện thực → PASS. Kiểm tay trong terminal (herdr).
- [ ] **Step 5:** Commit `feat: terminal ui`.

---

## Ngoài phạm vi plan này (mốc 5, plan riêng nếu cần)
- Đọc `~/.codex/multi-auth/quota-cache.json` (chờ người dùng đồng ý).
- Probe chủ động endpoint nhà cung cấp.
- Token/lịch sử của agy (`conversations/*.db` chưa rõ định dạng).
- Giá Google.
