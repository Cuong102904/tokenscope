# agent-usage — Thiết kế: theo dõi chi phí & hạn mức của nhiều coding agent (Rust, chạy local)

Ngày: 2026-11-04 · Trạng thái: chờ duyệt · Người dùng: một lập trình viên dùng Linux, nhiều agent, nhiều tài khoản.

Quy ước: **[Đã chốt]** = người dùng đã nói. **[Quyết định của tôi]** = tôi chọn, người dùng chưa duyệt. **[Đã kiểm]** = tôi đã quan sát trực tiếp trên máy người dùng ngày 2026-11-04. **[Chưa kiểm]** = suy đoán hoặc lấy từ tài liệu bên thứ ba.

## 1. Ý định và lý do

Người dùng chạy Claude Code, Codex, Antigravity (`agy`) và OpenCode, có nhiều tài khoản trả phí. Họ muốn trả lời hằng ngày hai câu hỏi: (a) **đã đốt bao nhiêu** (theo ngày, 30 ngày, model, session, project, skill), và (b) **còn bao nhiêu hạn mức** (cửa sổ 5 giờ, tuần, tháng) trên từng tài khoản, đang chạy mấy phiên, tốn bao nhiêu. Hiện mỗi hãng có chỗ xem riêng hoặc không có, nên không có cái nhìn chung. Thành công = mở một nơi và biết ngay mình sắp chạm giới hạn ở tài khoản nào, và tiền/token đi đâu.

**[Đã chốt]** Tự xây, viết bằng Rust, chỉ chạy local, mục tiêu nhanh và nhẹ. Hình thức có thể là web, terminal hoặc plugin (xem mục 7).

Đã có một bản thử bằng Python ở `~/Desktop/DEV/agent-usage/` (cli.py, scanner.py, report.py, pricing.py, dashboard.py, static/index.html) cho Claude Code, quét 260 file / 700 MB trong ~5,7 giây. Bản Rust là bản chính thức thay thế nó; bản Python là tài liệu tham chiếu hành vi và là nguồn số liệu đối chiếu.

## 2. Mục tiêu, không-mục-tiêu, phản-mục-tiêu

**Mục tiêu**
- Chi phí lịch sử cho Claude Code, Codex, OpenCode (và agy nếu tìm được nguồn token): theo ngày, 30 ngày, model, project, session.
- Hạn mức còn lại của từng tài khoản/hãng, kèm giờ reset và **thời điểm đo**.
- Xu hướng hạn mức theo thời gian và ước tính khi nào chạm giới hạn.
- Xếp hạng skill dùng nhiều nhất, chi phí theo skill/plugin (Claude), công cụ dùng nhiều nhất, tỉ lệ cache, phần chi phí do subagent.
- Số phiên đang chạy và chi phí của chúng.
- Một file chạy duy nhất, ít RAM khi chạy nền.

**Không-mục-tiêu**
- Không điều khiển hay chuyển tài khoản hộ người dùng (đó là việc của plugin `codex-multi-auth`).
- Không chạy trên máy từ xa, không đồng bộ giữa nhiều máy, không có đăng nhập người dùng.
- Không tính hoá đơn thật. Mọi số tiền là **ước tính quy đổi theo giá API**; người dùng chủ yếu dùng gói thuê bao.
- Không hỗ trợ Windows/macOS ở phiên bản đầu (không cấm, chỉ không cam kết).

**Phản-mục-tiêu (nếu xảy ra thì coi là thất bại dù chạy được)**
- Đọc, ghi log, lưu hoặc gửi đi **bất kỳ token/thông tin đăng nhập nào**.
- Tự gọi API của nhà cung cấp theo cách tốn hạn mức hoặc vi phạm điều khoản mà người dùng không bật rõ ràng.
- Số liệu sai mà trông có vẻ chính xác (ví dụ cộng trùng bản ghi, hiển thị hạn mức cũ như thể mới).
- Một adapter hỏng làm sập cả công cụ.

## 3. Ràng buộc

- Rust, một binary. Nền tảng chính: Linux.
- Dữ liệu chỉ nằm ở `~/.agent-usage/` (SQLite). Server chỉ nghe `127.0.0.1`. **[Quyết định của tôi]** cổng mặc định 8765.
- Chỉ **đọc** dữ liệu của các công cụ khác; mở SQLite của họ ở chế độ chỉ-đọc.
- Giao diện web không phụ thuộc CDN (chạy được khi mất mạng): nhúng sẵn thư viện vẽ biểu đồ vào binary. **[Quyết định của tôi]**
- Thư viện gợi ý (**[Quyết định của tôi]**, người xây có thể đổi): `rusqlite`, `serde_json`, `axum`, `ratatui`, `clap`, `walkdir`, `chrono`.

## 4. Mô hình dữ liệu chung

Mỗi adapter ghi vào cùng một lược đồ (SQLite):
- `accounts(id, vendor, label, plan_type, identity_hint)` — `label` do người dùng đặt trong `~/.agent-usage/accounts.toml`; `identity_hint` là dấu hiệu nhận diện (ví dụ băm của email, `plan_type`), **không bao giờ là token**.
- `usage(msg_key PK, vendor, account_id?, session_id, project, ts, day, model, input, output, reasoning, cache_read, cache_write_5m, cache_write_1h, sidechain, skill?, plugin?)`.
- `limit_snapshots(vendor, account_id?, scope, window_minutes, used_percent, resets_at, plan_type, measured_at, source)` — `scope` ví dụ `five_hour|weekly|monthly|group:<tên>`. **Mọi hạn mức được chuẩn hoá về "đã dùng %"** (agy báo "còn lại %" nên đổi `used = 100 − remaining`).
- `limit_events(vendor, account_id?, ts, window, resets_at)` — các lần chạm giới hạn.
- `skill_events(vendor, session_id, ts, skill, kind)` — `kind = invoked | attributed_message | file_read`.
- `files(path, size, mtime)` để quét tăng dần.
- Chi phí **không lưu**, tính khi truy vấn từ token và bảng giá, để đổi bảng giá là cập nhật cả quá khứ.

## 5. Nguồn dữ liệu từng hãng

### 5.1 Claude Code
- **Token/chi phí** [Đã kiểm]: `~/.claude/projects/**/*.jsonl` (kể cả thư mục `subagents/` lồng sâu). Bản ghi `type:"assistant"` có `message.usage` (`input_tokens`, `output_tokens`, `cache_read_input_tokens`, `cache_creation.ephemeral_5m_input_tokens`/`ephemeral_1h_input_tokens`), `message.model`, `cwd`, `sessionId`, `isSidechain`, `timestamp`.
- **Phải loại trùng** [Đã kiểm]: 12.997 bản ghi chỉ ứng với 6.170 tin nhắn duy nhất (mỗi content block là một bản ghi, và phiên resume chép lại lịch sử). Khoá = `message.id | requestId`; khi trùng giữ bản có `output_tokens` lớn nhất. Bỏ model bắt đầu bằng `<` (như `<synthetic>`).
- **Skill** [Đã kiểm]: bản ghi assistant có `attributionSkill` và `attributionPlugin` (4.886 tin nhắn có nhãn skill; top: superpowers:brainstorming 1.508, superpowers:test-driven-development 839, sdd-kit:implement-feature 680). Ngoài ra `tool_use` tên `Skill` với `input.skill` (34 lần). Ý nghĩa chính xác của `attributionSkill` (tin nhắn tạo ra *trong lúc* skill hoạt động) là [Chưa kiểm].
- **Hạn mức**: JSON do Claude Code truyền cho `statusLine` có `rate_limits.five_hour` và `rate_limits.seven_day`, mỗi cái `{used_percentage, resets_at (epoch giây)}` [Đã kiểm, qua bản Omnigent lưu tạm]. **Claude Code không tự lưu nó.** Giải pháp: binary có lệnh `agent-usage statusline` đọc JSON từ stdin, ghi một `limit_snapshot`, rồi (tuỳ chọn) chạy tiếp lệnh status line cũ của người dùng và in kết quả của nó. Người dùng đã có một status line riêng (trong `~/.claude/settings.json`), nên lệnh này phải bọc được nó chứ không thay nó.
- **Sự cố chạm giới hạn** [Đã kiểm]: log phiên có `apiError:"usage_limit_reached"` kèm `rateLimitType` (ví dụ `five_hour`) và `resetsAt`.
- **Tài khoản**: JSON status line không có trường nhận dạng tài khoản [Đã kiểm]. Mặc định gom thành một tài khoản Claude với nhãn người dùng đặt. Phân biệt nhiều tài khoản Claude là **chưa giải quyết** (xem mục 11).

### 5.2 Codex
- **Token và hạn mức** [Đã kiểm]: `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl`. Sự kiện `event_msg` có `payload.type = "token_count"` mang token và `payload.rate_limits = {limit_id, primary{used_percent, window_minutes, resets_at}, secondary|null, credits, plan_type, rate_limit_reached_type}`.
- Cửa sổ khác nhau theo gói [Đã kiểm trên máy]: `plus` → 300 phút + 10.080 phút; `plus` (một số bản ghi) → chỉ tuần; `free` → 43.200 phút (30 ngày); `limit_id: "premium"` cũng xuất hiện. **Không được giả định luôn có cửa sổ 5 giờ và tuần.**
- **Độ tươi**: số hạn mức chỉ cập nhật khi có phiên Codex chạy; tài khoản không dùng sẽ có số cũ. Mọi hạn mức phải hiển thị `measured_at` và đánh dấu "cũ" khi quá một ngưỡng (**[Quyết định của tôi]** ví dụ 30 phút cho cửa sổ 5 giờ).
- **Tài khoản**: log chỉ cho `plan_type`, không cho danh tính; hai tài khoản cùng gói bị lẫn. Máy người dùng có plugin `codex-multi-auth` (`~/.codex/multi-auth/`) với `quota-cache.json` và `openai-codex-accounts.json` — **chưa mở** [Chưa kiểm]. Adapter tuỳ chọn có thể đọc `quota-cache.json` nếu người dùng đồng ý; không bao giờ đọc `openai-codex-accounts.json` hay `auth.json` (chứa token).
- **Không** có "probe chủ động" mặc định. Plugin `codex-multi-auth` lấy quota bằng cách gửi một yêu cầu model thật rất nhỏ tới `https://chatgpt.com/backend-api/codex/responses` rồi đọc header `x-codex-primary-used-percent` ... [Đã kiểm qua mã nguồn của plugin]; cách này tốn hạn mức, dùng token, endpoint không công bố. Chỉ được làm thành module tắt-mặc-định, bật bằng cờ rõ ràng (xem mục 8).

### 5.3 Antigravity (`agy`)
- **Hạn mức** [Đã kiểm]: chạy `agy -p "/usage"` in ra văn bản phân cách bằng tab: `<nhóm model>\tWeekly Limit Remaining\t<%>\t<ISO UTC reset>`, hai nhóm: "Gemini Models" và "Claude and GPT models". Chỉ có cửa sổ tuần. Là phần trăm **còn lại**. Việc lệnh này có tốn hạn mức hay không là [Chưa kiểm] (có vẻ xử lý cục bộ, trả về gần như tức thì). Lệnh có thể chạy định kỳ; tần suất tối đa **[Quyết định của tôi]** mỗi 5 phút, và chỉ khi có yêu cầu hoặc đang ở chế độ nền.
- **Tài khoản** [Đã kiểm]: log `~/.gemini/antigravity-cli/log/*.log` ghi email đang đăng nhập (`applyAuthResult: email=...`). Dùng làm `identity_hint` dưới dạng băm; không hiển thị email thô.
- **Token/chi phí**: `agy -p ... --output-format json` trả `usage` (token) cho từng lượt, nhưng không có kho lịch sử đã xác nhận. Dữ liệu hội thoại nằm trong `~/.gemini/antigravity-cli/conversations/*.db` (định dạng chưa biết) [Chưa kiểm]. **Phiên bản đầu chỉ hỗ trợ hạn mức cho agy.**

### 5.4 OpenCode
- **Token** [Đã kiểm]: `~/.local/share/opencode/opencode.db` (SQLite). Bảng `message` có cột `data` (JSON); với `role:"assistant"`: `providerID`, `modelID`, `tokens{input, output, reasoning, cache{read, write}}`, `time`, `path`. Trường `cost` luôn bằng 0 → tự tính từ bảng giá. Bảng `session` có `title`, `directory`; bảng `project` có `worktree`.
- **Hạn mức**: không thấy trong DB/log. Nhà cung cấp đăng nhập là `openai` [Đã kiểm, chỉ tên khoá]; giả thuyết là dùng chung đăng nhập ChatGPT với Codex, nên chia sẻ hạn mức [Chưa kiểm]. Hiển thị là "dùng chung tài khoản với Codex (giả định)" chứ không khẳng định.
- 4 tin nhắn có trường `error`, chưa xem nội dung [Chưa kiểm].
- Binary `opencode` không nằm trong PATH lúc kiểm tra; adapter chỉ cần đọc dữ liệu.

## 6. Tính chi phí

- Bảng giá để trong file cấu hình sửa được (`~/.agent-usage/pricing.toml`, có bản mặc định nhúng sẵn), khoá theo tiền tố tên model, khớp theo tiền tố dài nhất. Đơn vị $/1 triệu token: input, output, cache read, hệ số ghi cache 5 phút (1,25) và 1 giờ (2,0).
- Giá Claude: lấy từ `pricing.py` của bản thử (Fable 5.1/Mythos 5.1 10/50, Opus 5.5 4/20, Opus 5/4.8/4.7/4.6 5/25, Sonnet 5.5/5 2/10, Sonnet 4.6 3/15, Haiku 5.5 0,10/0,50, Haiku 4.5 1/5; cache read 0,1× đầu vào, riêng Fable 5.1 0,25, Opus 5.5 0,20). Hệ số ghi cache 1 giờ = 2,0 là [Chưa kiểm]. Haiku 5.5 có mức giá khác cho prompt > 100K, không mô hình hoá.
- **Giá OpenAI (Codex, OpenCode) và Google (agy) chưa được thu thập.** Người xây phải lấy từ nguồn chính thức và ghi nguồn + ngày vào file giá. Model không có giá hiển thị "n/a" và được liệt kê riêng, không tính là $0 thầm lặng.
- Token `reasoning` của OpenAI có hay không đã nằm trong `output` là [Chưa kiểm]; người xây phải kiểm với dữ liệu thật trước khi quyết định cộng hay không (tránh tính trùng).
- Mọi giao diện phải ghi rõ "ước tính theo giá API, không phải hoá đơn".

## 7. Chức năng và giao diện

Một binary `agent-usage` với các lệnh con:

| Lệnh | Việc làm |
|---|---|
| `collect` | Quét nguồn mới/đổi (tăng dần) và ghi DB. Có `--watch` chạy nền định kỳ. |
| `serve` | Web cục bộ (axum), nhúng tài nguyên tĩnh, API JSON. |
| `tui` | Giao diện terminal (ratatui). |
| `statusline` | Bọc status line của Claude Code (mục 5.1) và/hoặc in một dòng ngắn "5h 44% · tuần 54% · $12 hôm nay". |
| `status` | Một dòng/bảng ngắn cho script. |
| `doctor` | Cho từng adapter: tìm thấy nguồn không, bao nhiêu bản ghi, lần đo gần nhất, vì sao "không khả dụng". |

**Các màn hình/chức năng** (mức ưu tiên xem mục 10):
1. Chi phí: hôm nay, 7/30 ngày, tất cả; biểu đồ theo ngày xếp chồng theo model; bảng theo model, project, session (session tốn nhất).
2. Hạn mức: mỗi tài khoản/hãng một thẻ — % đã dùng, giờ reset (giờ địa phương), **thời điểm đo và nguồn**, cờ "cũ".
3. Xu hướng hạn mức: % đã dùng theo thời gian, tốc độ đốt, **ước tính khi nào chạm giới hạn** (**[Quyết định của tôi]**: ngoại suy tuyến tính từ cửa sổ gần đây, ghi rõ là ước tính thô), số lần chạm giới hạn.
4. Skill: xếp hạng; Claude có chi phí theo skill/plugin; Codex chỉ có số lần đọc `SKILL.md` (7.468 lần trong dữ liệu hiện có, có thể bị thổi phồng do đọc lại nhiều lần trong một phiên) → hiển thị là "xếp hạng tương đối". Mỗi biểu đồ ghi "đo bằng cách nào".
5. Công cụ dùng nhiều nhất (Claude), tỉ lệ cache, phần chi phí do subagent.
6. Đang chạy: các phiên có file log được ghi trong vài phút gần nhất (**[Quyết định của tôi]**, ngưỡng cấu hình được) cùng chi phí tạm tính.
7. Bộ lọc theo khoảng ngày, hãng, model, project; URL lưu được bộ lọc; tự làm mới (**[Quyết định của tôi]** 30 giây).

Plugin: `statusline` là "plugin" thực tế duy nhất (nó chạy bên trong Claude Code). Không viết plugin cho Codex/agy/OpenCode ở phiên bản đầu.

## 8. Kiến trúc và an toàn

- Mỗi hãng một **adapter** cùng một trait (khám phá nguồn → quét tăng dần → trả về bản ghi `usage` / `limit_snapshot` / `skill_event`). Adapter lỗi chỉ làm hãng đó "không khả dụng" kèm lý do; phần còn lại vẫn chạy.
- Quét tăng dần theo `(đường dẫn, kích thước, mtime)`; upsert theo khoá; xử lý file đang được ghi dở (dòng cuối chưa đủ) mà không hỏng.
- Mở DB của công cụ khác ở chế độ chỉ-đọc và chịu được file WAL đang ghi.
- **Danh sách "không bao giờ đọc"** được mã hoá cứng: `auth.json`, `openai-codex-accounts.json*`, `~/.local/share/opencode/auth.json`, bất kỳ file credential/token nào. Có kiểm thử đảm bảo công cụ không mở các đường dẫn này.
- Module "probe chủ động" (gọi endpoint của nhà cung cấp bằng token): **tắt mặc định, không có trong phiên bản đầu**; nếu sau này làm, phải bật bằng cờ riêng, đọc token lúc gọi không lưu, cách ly thành crate/feature riêng, và người dùng tự cân nhắc điều khoản dịch vụ.
- Email/định danh hiển thị dưới dạng nhãn do người dùng đặt hoặc băm rút gọn, không hiển thị thô.
- Server chỉ `127.0.0.1`; không có CORS mở; không có API ghi.

## 9. Tiêu chí kiểm tra

- **Tính trùng**: với cùng một ảnh chụp dữ liệu, tổng token/chi phí Claude khớp bản Python (`cli.py stats`) trong sai số 1%. Ví dụ ghi nhận 2026-11-04: bản Python cho 7 ngày = $209,68, 38 session, 2.973 tin nhắn; toàn bộ = $1.610,50, 132 session, 19.526 tin nhắn (số sẽ đổi khi có phiên mới; dùng để đối chiếu cùng thời điểm).
- Dữ liệu thật Codex: các ảnh chụp hạn mức khớp lệnh `/status` của Codex ở cùng thời điểm (kiểm tay một lần).
- `agy`: số trong dashboard khớp đầu ra `agy -p "/usage"`.
- Hiển thị hạn mức luôn có `measured_at`; có kiểm thử cho trường hợp dữ liệu cũ.
- `doctor` báo đúng khi một nguồn không tồn tại (ví dụ thiếu `~/.codex`).
- Hiệu năng: quét lần đầu toàn bộ dữ liệu hiện có (~700 MB Claude + log Codex) nhanh hơn bản Python (5,7 giây); quét tăng dần khi không có file mới dưới 0,5 giây; RAM nền dưới 50 MB (**[Quyết định của tôi]**, mục tiêu để người xây xác nhận).

## 10. Thứ tự giao hàng (**[Quyết định của tôi]**)

1. Lõi + Claude chi phí đạt tương đương bản Python; lệnh `collect`, `status`, `doctor`; bảng giá cấu hình được.
2. Codex (token + hạn mức từ log) và giao diện web: thẻ hạn mức, chi phí theo ngày/model/project/session.
3. `statusline` cho Claude (ghi hạn mức), adapter agy (`/usage`), adapter OpenCode (token).
4. Xu hướng + ước tính chạm giới hạn, màn hình skill/công cụ, phiên đang chạy, `tui`.
5. (Sau, tuỳ chọn) đọc `quota-cache.json` của `codex-multi-auth` nếu người dùng đồng ý; probe chủ động nếu thực sự cần.

## 11. Điều chưa giải quyết (cần người xây xác nhận hoặc người dùng quyết)

1. **Nhiều tài khoản Claude**: JSON status line không có danh tính. Cách tạm: một nhãn do người dùng đặt cho mỗi lần đăng nhập, hoặc chạy `statusline` kèm biến môi trường chỉ tên tài khoản. Chưa chốt.
2. **Nhiều tài khoản Codex cùng gói**: log không phân biệt. Có thể cần `quota-cache.json` của plugin multi-auth (chưa mở).
3. **Giá OpenAI/Google** và việc token reasoning có trùng với output hay không.
4. **agy token/lịch sử**: định dạng `conversations/*.db` chưa xem; phiên bản đầu bỏ qua.
5. **OpenCode dùng chung hạn mức với Codex** hay không.
6. Định dạng đầy đủ của `rate_limits` ở các bản ghi `limit_id: "premium"` (không có cửa sổ) chưa xem kỹ.

## 11b. Quyết định bổ sung sau vòng kiểm tra độc lập (tất cả là **[Quyết định của tôi]**, người dùng chưa duyệt)

1. **`statusline` và cấu hình của Claude Code**: công cụ **không bao giờ tự sửa** `~/.claude/settings.json`. Nó chỉ in đoạn cấu hình mẫu (`agent-usage statusline --print-snippet`) để người dùng tự áp dụng. Lệnh `statusline` phải: (a) ghi ảnh chụp bằng một file JSON nhỏ, ghi nguyên tử (tệp tạm rồi đổi tên) vào `~/.agent-usage/snapshots/`, **không ghi SQLite** trong đường nóng; bộ thu thập sẽ nạp file này sau; (b) chạy lệnh status line cũ của người dùng với cùng stdin, có thời gian chờ, và **luôn in kết quả của nó kể cả khi phần của ta lỗi**; (c) mục tiêu độ trễ thêm dưới 20 ms. Vì ảnh chụp chỉ tích luỹ từ lúc cài hook, lệnh `statusline` được giao **sớm ở mốc 1** (mục 10) để dữ liệu hạn mức Claude bắt đầu được thu thập, dù giao diện hạn mức làm sau. Khi hook chưa cài hoặc Claude không chạy, thẻ Claude hiển thị "chưa có dữ liệu hạn mức" kèm hướng dẫn, không hiển thị số cũ như mới.
2. **Codex nhiều tài khoản cùng gói**: chấp nhận trong phiên bản đầu **kèm cảnh báo hiển thị**. Ảnh chụp khoá theo `(vendor, limit_id, plan_type)` với `account_id` cho phép rỗng. Chỉ tính xu hướng/ước tính khi chuỗi đo **đơn điệu trong cùng một cửa sổ** (cùng `resets_at`); nếu `used_percent` giảm trước giờ reset hoặc `resets_at` nhảy lung tung, coi là lẫn tài khoản, tắt ước tính và hiện cảnh báo. Không để xu hướng sai trông như chính xác.
3. **Đối chiếu với bản Python**: bản Python chỉ là **gợi ý**, không phải chuẩn đúng. Khi khác nhau, dữ liệu gốc thắng. Dùng một **bộ dữ liệu mẫu nhỏ, cố định** (log tổng hợp, không chứa hội thoại thật) làm kiểm thử vàng; không đối chiếu với dữ liệu sống vì nó thay đổi. Việc khớp 1% ở mục 9 chỉ là kiểm tra sanity trên một ảnh chụp thư mục log được sao chép tại một thời điểm.
4. **Giá OpenAI/Google**: người xây lấy từ trang giá **chính thức** và ghi nguồn + ngày. Nếu không lấy được, giao token trước, chi phí hiển thị "n/a" cho đến khi có giá; điều này vẫn hợp lệ cho mốc 2.
5. **Mô hình tiến trình**: một tiến trình dài hạn `serve` có sẵn luồng thu thập (hoặc `collect --watch` riêng, dùng chung file SQLite ở chế độ WAL); `statusline` không chạm DB (xem mục 1). Không cần systemd ở phiên bản đầu (có thể tài liệu hoá một unit mẫu). **Lưu giữ**: giữ `limit_snapshots` vĩnh viễn nhưng giảm mật độ sau 90 ngày (tối đa một mẫu mỗi 5 phút).
6. **agy `/usage`**: phiên bản đầu chỉ gọi **theo yêu cầu** (khi mở giao diện hoặc chạy `status --refresh`); thăm dò nền định kỳ **tắt mặc định** cho đến khi xác nhận lệnh này không tốn hạn mức.
7. **Chỉ tiêu hiệu năng** ở mục 9 là **mục tiêu** (không phải điều kiện đạt/không đạt), trừ ràng buộc "không chậm hơn bản Python". Có một bộ đo hiệu năng nhỏ ở mốc 1.

## 12. Giao cho người xây quyết định

Cấu trúc crate, tên bảng/cột chính xác, giao diện cụ thể của web và TUI, bố cục biểu đồ, cách đóng gói tài nguyên tĩnh, chiến lược kiểm thử chi tiết, định dạng file cấu hình, và việc chọn crate cho từng mục (miễn đáp ứng các ràng buộc ở mục 3 và 8).
