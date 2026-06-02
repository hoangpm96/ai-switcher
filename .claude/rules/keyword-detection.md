# Keyword Detection Patterns

> Patterns để skill `/meet`, `/brainstorm`, `/legacy` extract structured info từ unstructured text (transcript, notes, legacy docs). Hỗ trợ tiếng Việt + tiếng Anh.

## Language detection

Detect language từ content trước khi extract.

**Tiếng Việt indicators:** dòng có ≥2 diacritic chars `à á ả ã ạ ă ằ ắ ẳ ẵ ặ â ầ ấ ẩ ẫ ậ è é ẻ ẽ ẹ ê ề ế ể ễ ệ ì í ỉ ĩ ị ò ó ỏ õ ọ ô ồ ố ổ ỗ ộ ơ ờ ớ ở ỡ ợ ù ú ủ ũ ụ ư ừ ứ ử ữ ự ỳ ý ỷ ỹ ỵ đ Đ`.

**Tiếng Anh indicators:** không có diacritic + dùng phổ biến EN words ("the", "and", "is", "we", "will").

**Mixed:** >30% dòng EN-only → mark `lang: mixed`. Skill ưu tiên format VN.

## Decision patterns

**Trigger phrases:**

| VN | EN |
|----|-----|
| "Chốt là...", "Quyết định...", "Thống nhất...", "Đồng ý...", "Đã chốt..." | "Decided to...", "Agreed to...", "We will...", "Final decision..." |
| "Sẽ làm...", "Triển khai...", "Áp dụng..." | "Will proceed with...", "Going with..." |
| "Chọn X thay vì Y" | "Choosing X over Y" |

**Extraction format:**
- Title verb-object: "Chốt là dùng Stripe" → `"Use Stripe for payments"`
- Slug: kebab-case, max 40 chars, ASCII transliteration cho VN.

## Blocker patterns

**Trigger phrases:**

| VN | EN |
|----|-----|
| "Vướng...", "Block...", "Chưa làm được vì...", "Phải chờ..." | "Blocked by...", "Waiting on...", "Cannot proceed because..." |
| "Thiếu...", "Còn thiếu..." | "Missing...", "Need..." |
| "Vấn đề...", "Khó khăn..." | "Issue...", "Problem..." |

**Severity inference:**
- `high`: mentions money, compliance, security, legal, production, deadline.
- `medium`: mentions integration, third-party, vendor.
- `low`: nice-to-have, formatting, minor.

## Action item patterns

**Trigger phrases:**

| VN | EN |
|----|-----|
| "{Name} sẽ làm...", "{Name} phụ trách...", "Giao cho {Name}..." | "{Name} will...", "{Name} to...", "Action: {Name}..." |
| "Cần phải...", "Trước thứ X phải xong..." | "Need to...", "By {date}, must..." |

**Extraction:**
- Owner: extract `{Name}` (Vietnamese names có thể có dấu — normalize ASCII fallback nếu cần).
- Due date: parse "thứ X" / "Monday" / "tomorrow" / "tuần sau" → ISO date.
- Description: rest of sentence.

## Open question patterns

**Trigger phrases:**

| VN | EN |
|----|-----|
| "?", "Câu hỏi...", "Chưa rõ...", "Cần check..." | "?", "Question...", "Unclear...", "TBD..." |
| "Không biết...", "Liệu..." | "Not sure...", "Wonder if..." |

**Capture:**
- Format: `- [ ] OQ: {question}`.
- Owner default `TBD`.

## Attendee patterns

**Indicators:**
- Lines starting `- {Name}:` hoặc `{Name}:` followed by speech.
- Bullet list with names trong section "Tham dự" / "Attendees" / "Present".
- `@mentions` trong note (vd `@hoang`, `@anh`).

**Roles inferred:**
- "client", "khách hàng" → external.
- Empty → team member.

## Decision/blocker title generation

Apply verb-object pattern:

1. Find verb (action word): "use", "implement", "fix", "add", "block".
2. Find object (noun phrase).
3. Combine: `{Verb} {Object}` (max 60 chars).
4. Slug: lowercase, kebab-case, ASCII only, max 40 chars.

Examples:

| Source | Title | Slug |
|--------|-------|------|
| "Chốt là dùng Stripe" | "Use Stripe for payments" | `use-stripe-for-payments` |
| "We agreed to launch in Q3" | "Launch in Q3" | `launch-in-q3` |
| "Vướng do thiếu OAuth credentials" | "Missing OAuth credentials" | `missing-oauth-credentials` |

Nếu không tìm được verb-object rõ → fallback first 5-7 meaningful words.

## Confidence scoring

Mỗi extraction có confidence:
- **High:** trigger phrase explicit + clear subject/object.
- **Medium:** trigger phrase implicit hoặc context inferred.
- **Low:** ambiguous, đề xuất user confirm.

Skill nên show preview với confidence levels trước khi write — user review high-confidence batches, manual confirm medium/low.

## Legacy doc classification patterns

| Source content signal | Inferred type |
|------------------------|---------------|
| Headers "Functional Requirements", "FR-NNN" table | `srs` |
| Headers "Business Objectives", "ROI", "Stakeholders" | `brd` |
| Headers "User Personas", "Capabilities P0/P1" | `prd` |
| Headers "User Needs", "User Journey" | `urd` |
| "User Story", "As a... I want..." | `user-story` |
| "Use Case", "Actor", "Main Success Scenario" | `use-case` |
| "Sequence Diagram", mermaid `sequenceDiagram` | `srs-flows` |
| Tables với "Screen", "Field", "Validation" | `srs-screen` |
| Meeting metadata (date + attendees + agenda) | `meeting` |
| Empty / unclear | `inbox` (raw capture) |

Multi-pattern detect → priority: explicit metadata > section headers > content keywords.
