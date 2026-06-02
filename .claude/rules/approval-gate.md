# Approval Gate Convention

> Mọi skill phải tuân thủ rule này khi write/edit file. Mục đích: human-in-the-loop (HITL) thống nhất — không skill nào tự ý ghi file mà không qua approval.

## 3 levels

| Level | Bắt buộc khi | Cơ chế |
|-------|--------------|--------|
| **L1 Plan** | Trước mọi tool `Write` / `Edit` / batch tạo ≥1 file | In bảng plan; user confirm Y/n/select |
| **L2 Diff** | Khi `Edit` 1 file đã tồn tại (kể cả update via `--update` flag) | Hiển thị unified diff; user confirm Y/n/edit-prompt |
| **L3 Iterate** | Output sáng tạo: ASCII wireframe, mermaid diagram, prose draft | Render trong chat → loop refine; max 3 vòng |

## L1 — Plan preview

**Format chuẩn** (skill in ra trước khi ghi):

```
[/skill-name] Sẽ thực hiện:
  # | path                              | action  | summary
  1 | docs/payment/urd.md               | create  | URD draft, 3 user types, 5 needs
  2 | docs/payment/brd.md               | create  | BRD draft, 4 objectives, 3 risks
  3 | docs/payment/srs/spec.md          | update  | thêm 2 FR, sửa NFR perf

Apply? (Y/n/select):
```

**User response:**
- `Y` / `<enter>` / `yes` / `ok` → proceed tất cả.
- `n` / `no` / `abort` → huỷ toàn bộ, không ghi gì.
- `select skip 2,3` → chạy item 1, bỏ 2 và 3.
- `select only 1` → chỉ chạy item 1.
- Bất kỳ free text khác → treat như request thay đổi plan, skill phải re-plan.

**Rules:**
- L1 **bắt buộc** ngay cả khi chỉ tạo 1 file.
- Bảng plan tối đa 1 dòng/file. Summary ngắn (≤50 ký tự).
- Skill KHÔNG được skip L1 với cớ "user đã confirm trong skill khác".

## L2 — Diff confirm

**Khi nào:** edit file đã tồn tại (cả khi chạy `--update` hoặc apply patch trong /cr).

**Format:**

```
[/skill-name] Diff cho docs/payment/urd.md:

--- a/docs/payment/urd.md
+++ b/docs/payment/urd.md
@@ -12,7 +12,8 @@
 ## 3. User Needs
 
-1. Khách thanh toán < 30s
+1. Khách thanh toán < 30s qua Momo/VNPay
+2. Có save card cho lần sau
 
Apply? (Y/n/edit-prompt):
```

**User response:**
- `Y` → apply diff.
- `n` → huỷ edit này (giữ file cũ).
- `edit-prompt: <text>` → quay lại bước synthesize với feedback `<text>`, tạo diff mới.

**Rules:**
- Diff phải là unified format với ≥3 dòng context.
- Nếu diff > 50 dòng: skill cảnh báo "diff lớn, có muốn xem full hay summary?" trước khi in.
- L2 chạy SAU L1 (L1 list path + action `update`, L2 mới show diff khi user đã Y ở L1).

## L3 — Iterate refine

**Khi nào:** output sáng tạo **render được trong chat** cần feedback nhiều vòng. Tiêu biểu:
- ASCII wireframe (`/wireframe ascii`) — monospace render OK
- ASCII flow diagram trong brainstorm — monospace render OK
- Prose draft dài (vd executive summary BRD)

**KHÔNG áp L3 cho mermaid** (`/sequence`, `/erd`) — chat chỉ in source code mermaid, không render diagram. User nhìn text raw không review được. Mermaid skills đi thẳng L1 → Write → user review từ rendered file (IDE/Obsidian/GitHub preview) → sửa qua `--update`.

**Format:**

```
[/skill-name] Phiên bản 1:

<output render trong chat>

Đồng ý / Sửa: <mô tả thay đổi> / Hủy:
```

**User response:**
- `Đồng ý` / `ok` / `approve` / `Y` → tiếp đến L1 (plan write).
- `Sửa: ...` / free text → skill regenerate v2 với feedback.
- `Hủy` / `cancel` / `n` → abort.

**Rules:**
- Max 3 vòng iterate. Vòng 3 (v3) là vòng ép chốt — nếu user vẫn `Sửa:`, skill thông báo "đã đạt max 3 vòng, em chốt v3 và đi tiếp L1; anh edit file manually sau nếu cần."
- Mỗi vòng số hiệu rõ ràng: `Phiên bản 1`, `Phiên bản 2`, `Phiên bản 3`.
- L3 chạy TRƯỚC L1.

## Soft gate vs Hard gate

Approval gate ≠ readiness gate. Hai khái niệm khác nhau:

| | Approval gate (rule này) | Readiness gate (chain rule) |
|---|---|---|
| Hỏi gì | "Apply những thay đổi này không?" | "Có đủ upstream để chạy skill này không?" |
| Khi nào | Trước mỗi write/edit | Bắt đầu skill, kiểm tra prerequisite |
| Default | L1 luôn chạy | **Soft** — warn nếu thiếu, vẫn proceed |

Readiness gate examples:
- `/userstory payment` nhưng chưa có `/usecase` → warn "Chưa có UC, em vẫn chạy" + proceed.
- `/jira push` nhưng có US `status: stale` → **refuse** (đây là exception hard gate, vì Jira là external side-effect).

## Tham chiếu trong SKILL.md

Mọi SKILL.md PHẢI có dòng:

```markdown
References:
- @.claude/rules/approval-gate.md
```

Và trong Processing steps, dùng cụm từ chuẩn:

```markdown
6. **Approval L1:** in plan preview (xem rule approval-gate.md). User Y → tiếp.
7. **Approval L2** (nếu `--update`): show diff trước khi ghi.
```

## Exception — skill không cần approval gate

Chỉ 2 loại skill được miễn:

| Skill | Lý do |
|-------|-------|
| `/health` | Read-only, in console, không ghi file |
| `/gap` | Read-only analysis; L1 bắt buộc cho `traceability.md` write |

Mọi skill khác (kể cả `/legacy` import từ external sources) đều phải qua L1.

## Anti-patterns

❌ **KHÔNG** auto-pick file im lặng dù "có vẻ rõ ràng".
❌ **KHÔNG** gộp L1 + L2 thành 1 prompt ("Tạo file? Y/n" mà không show diff khi update).
❌ **KHÔNG** dùng L3 cho file write thuần (vd `/urd` text — không cần iterate, đi thẳng L1).
❌ **KHÔNG** skip L1 khi chỉ ghi 1 file ngắn.
❌ **KHÔNG** dùng env var `CLAUDE_AUTO_APPROVE` hoặc tương tự để bypass.

## Tóm tắt 1 dòng

> **L3 (iterate nếu sáng tạo) → L1 (plan + Y/n) → L2 (diff nếu update) → Write.**
