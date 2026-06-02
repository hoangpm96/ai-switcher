# Resolve Open Questions (Phase E)

> Canonical pattern cho mọi BA skill (`/brainstorm`, `/urd`, `/brd`, `/prd`, `/srs`) chạy SAU Write doc, TRƯỚC khi suggest downstream skills. Mục đích: không để OQ debt accumulate cross-stage — buộc resolve hoặc ack hold ngay.

## Trigger

Skill chạy Phase E ngay sau khi Write doc thành công (cả create + `--update`).

## Step 1 — Collect OQs

Skill gom 2 nguồn OQs:

1. **Own OQs** — vừa write trong doc hiện tại (parse §"Open Questions").
2. **Inherited OQs** — từ upstream chain còn `status: hold` hoặc `[ ]`. Upstream chain tuỳ skill:

| Skill | Upstream chain để inherit OQs |
|---|---|
| `/brainstorm` | (none — brainstorm là gốc) |
| `/urd` | `docs/{feature}/brainstorms/*.md` §"Open Questions" |
| `/brd` | brainstorm + `urd.md` |
| `/prd` | brainstorm + `urd.md` + `brd.md` |
| `/srs` | brainstorm + `urd.md` + `brd.md` + `prd.md` |

Skill scan §"Open Questions" của mỗi upstream doc, extract OQs còn `[ ]` (unresolved) hoặc `[~]` (deferred but not closed). Bỏ qua `[x]` (resolved).

## Step 2 — Prompt user

In danh sách OQs theo format:

```
📋 Còn {N} câu hỏi mở cần xử lý:

Từ {current_doc_type}:
  - OQ-{id}: {text}
  - OQ-{id}: {text}

Inherited từ {upstream_doc} (chưa resolve):
  - OQ-{id}: {text}  (source: {path})
  - OQ-{id}: {text}  (source: {path})

Resolve ngay bây giờ trước khi qua {next_skill}?
  Y       → em sẽ hỏi từng OQ một
  skip    → giữ OQ, downstream skill sẽ inherit lại
  <ids>   → chỉ resolve OQ cụ thể (vd "OQ-3" hoặc "OQ-3,OQ-4")
```

Nếu `N == 0` → skip Phase E entirely, đi thẳng final report.

## Step 3 — Resolve loop (one-at-a-time)

User chọn `Y` hoặc `<ids>` → loop từng OQ targeted:

1. In OQ + context (1-2 dòng từ section liên quan của source doc nếu áp dụng).
2. Wait user reply. Reply có 4 dạng:
   - **Answer cụ thể** → mark `[x]`, format "Resolved: {answer}", ghi vào doc.
   - **`skip` / `hold`** → giữ `[ ]`, note "hold tới {next_skill}".
   - **`không cần` / `OOS`** → mark `[~]`, note "out of scope".
   - **`không biết` / `hỏi sau`** → giữ `[ ]`, không retry trong session này.
3. **Side-effect detection (within current doc)** — propose L2 diff cho section chịu ảnh hưởng của OQ answer. Xem Step 3.5 mapping.
4. Nếu OQ inherited từ upstream → skill **update cả upstream doc** (mark `[x]`/`[~]` + thêm note "resolved via /{current_skill}"). L2 diff cho mỗi upstream edit. Append changelog entry vào upstream doc.

## Step 3.5 — Cascade scan (CRITICAL — đừng skip)

Sau khi user accept answer cho 1 OQ, skill **PHẢI** quét rộng hơn marker checkbox để propagate nghiệp vụ vào docs/sections liên quan. KHÔNG để OQ resolved mà các section vẫn ghi giả định cũ.

### 3.5.1 — Scan trong doc hiện tại

Skill grep toàn bộ doc hiện tại tìm:

1. **Direct reference**: pattern `OQ-{id}`, `xem Mục {N} OQ-{id}`, `chờ OQ-{id}`, `pending OQ-{id}`. Mỗi match → propose L2 diff: xóa reference hoặc thay bằng resolution.
2. **Next Steps bullet**: pattern `- Resolve OQ-{id}` trong Mục Next Steps. Match → propose xóa bullet.
3. **Topic-based scan**: dùng mapping table (Mục 3.5.3) để identify sections có thể chứa giả định cũ. Đọc nội dung section đó, đối chiếu với OQ answer, nếu có conflict → propose L2 diff update content.

### 3.5.2 — Scan downstream docs

Nếu doc hiện tại không phải cuối chain, skill scan downstream docs trong cùng feature folder. Downstream chain ngược upstream:

| Resolve qua skill | Scan downstream docs |
|---|---|
| `/brainstorm` | `urd.md`, `brd.md`, `prd.md`, `srs/spec.md` (nếu tồn tại) |
| `/urd` | `brd.md`, `prd.md`, `srs/spec.md` |
| `/brd` | `prd.md`, `srs/spec.md` |
| `/prd` | `srs/spec.md`, `srs/flows.md`, `srs/erd.md`, `ascii-screen/*.md` |
| `/srs` | `userstories/*.md`, `usecases/*.md` |

Mỗi downstream doc: apply cùng pattern 3.5.1 (direct ref + topic scan). L2 diff per match.

### 3.5.3 — Topic → Section mapping (heuristic)

Khi OQ chứa keyword sau, scan section tương ứng:

| OQ topic keywords | Sections cần check (trong mọi doc liên quan) |
|---|---|
| region, compliance, GDPR, PDPA, country, EU, VN, global | Assumptions, Risks, Out of Scope, NFR security/privacy, BRD Stakeholders, PRD Non-goals |
| platform, mobile, iOS, Android, web, desktop | Constraints, Capabilities (P0/P1/P2), OOS, NFR usability |
| timeline, budget, deadline, Q1/Q2/release | Risks, Success Criteria, Capabilities priority, BRD Timeline, PRD Release plan |
| vendor, third-party, SDK, BaaS, build-vs-buy | Dependencies (PRD/SRS), Risks, BRD Cost |
| scope, include, exclude, feature inclusion | Capabilities, OOS, PRD Goals/Non-goals |
| data, privacy, retention, PII, storage | Assumptions, NFR security, Risks compliance, ERD entities |
| auth, security, encryption, hash, lockout | NFR security, Business Rules, Error Matrix |
| performance, latency, throughput, scale | NFR performance, Capabilities (rate limits) |
| user role, permission, RBAC, admin | User Types, Capabilities, Business Rules |
| pricing, payment, billing, subscription | BRD ROI, Capabilities, Dependencies, NFR availability |

Mapping này là gợi ý — skill có thể detect thêm topic khác từ semantic của answer. Nếu uncertain, hỏi user "Mục nào còn liên quan?".

### 3.5.4 — L2 diff aggregation

Sau khi scan xong cho 1 OQ, in tóm tắt impact trước khi loop diff:

```
🔗 OQ-{id} resolved → đã phát hiện {K} sections/docs cần update:

Doc hiện tại ({path}):
  - Mục {N} {section}: {1-line preview thay đổi}
  - Mục {N} {section}: {preview}

Downstream:
  - {downstream_path} Mục {N}: {preview}

Apply lần lượt từng L2 diff? (Y / skip-all / chọn id)
```

Sau đó loop L2 diff per item, user Y/n mỗi cái.

### 3.5.5 — Changelog cascade

Mỗi doc bị update (hiện tại + upstream + downstream) → append changelog entry riêng:

```yaml
- {date} | /{current_skill} | cascade từ OQ-{id} resolved: {section list} updated
```

## Step 4 — Changelog

Sau khi loop xong (resolve ≥1 OQ), append entry vào doc hiện tại:

```yaml
changelog:
  - {date} | /{current_skill} | resolved OQ-{ids}: {short summary}
```

Nếu có upstream doc bị update → mỗi upstream doc cũng có changelog entry tương tự.

## Step 5 — Final report

```
✅ {Doc_type} finalized: docs/{feature}/{path}
   Resolved OQs trong session: {R}/{N}
   Còn hold: {M} (sẽ inherit khi chạy {next_skill})

Recommended next:
  - /{next_skill_1} {feature}   — {description}
  - /{next_skill_2} {feature}   — {description}
```

Nếu user skip Phase E:
```
⚠️  {N} OQ vẫn hold. Khi chạy {next_skill}, em sẽ inherit list này
    và hỏi lại trong context của {next_skill}.
```

## Constraints

- **Resolve loop one-at-a-time** — KHÔNG dồn batch 5 câu vào 1 prompt.
- **L2 diff** trước mọi Edit (cả doc hiện tại + upstream doc + downstream doc).
- **Push exact values** — vague answer ("để sau", "không rõ") → retry 1 lần với câu hỏi cụ thể hơn. Vẫn vague → giữ `[ ]`, không force.
- **Side-effect updates KHÔNG silent** — luôn propose L2 trước.
- **Cascade scan BẮT BUỘC** — Step 3.5 không phải optional. Đánh `[x]` xong mà không scan = OQ "resolved" trên giấy nhưng các section khác vẫn ghi giả định cũ → BUG nghiệp vụ.
- **Inherited OQ resolve qua doc nào thì doc đó được update** — không chỉ ghi vào doc hiện tại.
- **No-re-ask** — OQ đã `[x]` resolved trong upstream → KHÔNG hỏi lại.

## Anti-patterns

- ❌ Output report ngay sau Write mà bỏ qua OQs.
- ❌ Gom "Resolve OQs" vào "Recommended next" list (làm mất ưu tiên).
- ❌ Ép user resolve hết — user có quyền `skip` hoặc `hold`.
- ❌ Update upstream OQ silent (không L2 diff).
- ❌ Hỏi OQ đã resolved ở upstream.
- ❌ **Mark `[x]` xong nhưng bỏ qua cascade scan** — Assumptions/Risks/Next Steps vẫn ghi giả định cũ là vô nghĩa.
- ❌ Bỏ qua downstream docs — OQ resolved ở `/brainstorm` nhưng URD đã viết vẫn còn "OQ-3 chờ resolve" trong Next Steps.

## Tóm tắt

Sau Write → **Collect OQs (own + inherited) → Prompt Y/skip/ids → Loop 1-by-1: [answer → cascade scan toàn bộ doc + downstream → L2 diff per impacted section] → Changelog per doc → Final report**.
