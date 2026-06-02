# BA Conventions

> Common rules cho mọi BA skills (`/brainstorm`, `/urd`, `/brd`, `/prd`, `/srs`, `/usecase`, `/userstory`, `/ac`). Mỗi skill MUST reference file này trong Constraints + References.

## 1. Owner resolution

- Set `owner` từ memory `user-identity.md` (key `current_user`).
- Nếu memory chưa có → đọc `git config user.name` + `git config user.email` → ask user confirm @handle (vd `@hoangpm` từ `hoangpm.qn96@gmail.com`) → save vào memory.
- KHÔNG kế thừa `owner` từ upstream doc — upstream có thể là người khác. Doc mới luôn dùng `current_user`.
- Optional: thêm field `last_modified_by: "@current_user"` khi update mode (skill chạy lại lần thứ 2+).
- Doc cũ có owner khác giữ nguyên — chỉ áp `current_user` cho doc mới hoặc khi user explicit muốn đổi.

## 2. No-re-ask rule

- KHÔNG hỏi lại câu user đã trả lời (cùng session HOẶC trong file đã tồn tại).
- Trước mỗi vòng câu hỏi: scan idea seed + previous answers + existing doc (continuation/update mode) → loại câu đã có answer.
- Answer partial → follow-up chỉ phần thiếu, KHÔNG hỏi lại từ đầu.
- Continuation/update mode: MUST Read full file trước khi phỏng vấn, đối chiếu mỗi planned question với content có sẵn.

## 3. IT-BA framing (no coding/architect questions)

Skill phục vụ IT Business Analyst, KHÔNG phải developer.

**CẤM hỏi:** tên column DB, schema table, function/service name, API endpoint, JWT vs session, framework choice, refresh-token rotation, hashing algorithm, payload structure, SDK name.

**ĐƯỢC hỏi (business language):**
- "system làm gì" (validate, lưu thông tin, gửi email, gọi dịch vụ ngoài)
- "cần lưu loại thông tin nghiệp vụ gì" (vd email, status, ngày tạo — KHÔNG hỏi column type)
- "có gọi dịch vụ bên ngoài nào" (Google, SendGrid, Stripe — chỉ tên + mục đích, KHÔNG hỏi endpoint/SDK)
- "ai trigger action", "khi nào trigger", "kết quả nghiệp vụ user thấy"

Quyết định kỹ thuật (DB schema, auth strategy, framework choice) là việc của `/srs` + dev/architect, KHÔNG phải BA skills khác.

## 4. Vietnamese-friendly typography

- KHÔNG dùng ký hiệu ngoại lai khó đọc trong prose tiếng Việt: `§` (section sign) → dùng "Mục N", `¶` → "đoạn N".
- `→` chỉ dùng trong flow/diagram/table cell, narration tiếng Việt nên dùng "sang/đến/dẫn tới".
- Bold (`**...**`) dùng bình thường — phục vụ emphasis số liệu, key term, câu chốt.
- Tránh làm doc trông như legal/spec Tây.

## 5. L1 plan preview cho BA, không cho dev

L1 plan preview phải dùng **prose tự nhiên với từ nghiệp vụ**, KHÔNG bảng dày tag/flag/checklist.

**Format đề xuất:**

> Em sẽ {tạo mới | viết lại} file `docs/{feature}/{name}.md` với:
>
> **Thêm/cập nhật nội dung:**
> - {liệt kê 4-8 bullet bằng từ nghiệp vụ: "luồng / bảng / hình minh họa / số liệu cụ thể / wording mẫu"}
> - {các số liệu nghiệp vụ cụ thể nếu có}
>
> **Câu hỏi mở:** {N resolved} đã chốt; còn {M} cho `/{next-skill}`.
>
> **Ghi nhận:** changelog "{note}".
>
> Apply? (Y / sửa)

**CẤM trong L1 BA-facing:**
- Bảng `# | path | action | summary` (kiểu log dev)
- Tag flag: `has_external_redirect=Y`, `Quality checklist: 9/11`, `Mandatory artifacts ✓`
- Từ technical: matrix, diagram, flag, scaffold, schema

**GIỮ:** số liệu nghiệp vụ cụ thể (lockout 5 lần, link 24h) — đó là content nghiệp vụ.
