---
name: implement
description: Use when user wants to code/build a feature từ brainstorm doc — đi thẳng từ `docs/{feature}/brainstorms/*.md` sang implementation thực tế. Triggered by `/implement <feature>` hoặc `/implement <feature> <idea-slug>`. Đọc brainstorm → recon codebase (tái dùng stack có sẵn) → lập implementation plan (L1 approval) → tạo nhánh git local → code P0 → chạy /run + /verify + /code-review → commit lên nhánh → bàn giao để user tự merge local + test + deploy. KHÔNG push/PR lên remote. Solo-git workflow.
allowed-tools: Read, Write, Edit, Bash, Glob, Grep, Skill, TodoWrite
user-invocable: true
argument-hint: "<feature> [<idea-slug>] [--p0-only] [--continue]"
---

# /implement — Brainstorm → Code (local branch)

## Goal

Biến brainstorm doc thành code chạy được trong repo này, theo workflow solo-git của user: **đọc brainstorm → recon codebase → plan (L1) → nhánh local → code P0 → mechanical gate (tsc/lint/build/run) → đối chiếu code vs brainstorm (Pass/Fail/Missing + fix loop) → commit lên nhánh → bàn giao**. User tự merge local + test lại + deploy. Skill KHÔNG push, KHÔNG mở PR remote.

Không phải BA tool — đây là bước coding. Quyết định kỹ thuật (DB schema, API route, validation, integration) nằm ở đây, dựa trên nghiệp vụ đã chốt trong brainstorm.

## Constraints

- **Nguồn sự thật = brainstorm doc.** MUST Read đầy đủ `docs/{feature}/brainstorms/{idea-slug}.md` trước khi plan. Mọi capability/flow/wording/limit/error lấy từ doc — KHÔNG bịa nghiệp vụ mới. Wording (error/success/info) dùng **exact string** trong Mục 7.3 của brainstorm.
- **Tái dùng stack có sẵn — KHÔNG thêm framework/lib mới khi đã có tương đương.** Recon trước, map sang module hiện hữu (xem "Stack ai4ba"). Thêm dependency mới phải nêu trong L1 + lý do.
- **P0 trước.** Mặc định chỉ build P0 của brainstorm. P1/P2 chỉ khi user yêu cầu (hoặc `--all`). Cái gì hoãn → ghi rõ trong report.
- **OQ còn `[ ]` (hold)** — surface ở L1. Nếu OQ chặn 1 phần implement → hỏi user chốt nhanh hoặc đánh dấu phần đó "skip, chờ OQ". KHÔNG tự quyết nghiệp vụ thay user.
- **Nhánh local, KHÔNG remote.** Tạo branch `feat/{feature}` (hoặc tên user chọn) từ nhánh hiện tại. Code + commit lên branch đó. **KHÔNG `git push`, KHÔNG `gh pr create`.** Kết thúc: hướng dẫn user merge local + test + deploy.
- **Working tree có thể đang dirty** — `git status` TRƯỚC khi tạo nhánh. Nếu có thay đổi chưa commit ngoài scope feature, hỏi user xử lý (stash / commit riêng / mang theo) thay vì âm thầm nuốt hết. Commit cuối **chỉ stage file của feature**, KHÔNG `git add -A` mù.
- **L1 plan approval** trước khi viết/sửa file đầu tiên — bảng các thay đổi (file + việc) + branch name + dependency mới (nếu có) + P0 scope + OQ ảnh hưởng. User Y → code. (Theo `_rules/approval-gate.md`.)
- **Commit chỉ khi code chạy + đúng doc.** Sau Phase E (tsc/lint/build + run sạch) **và** Phase F (đối chiếu code vs brainstorm không còn ❌/⚠️) → mới commit. Còn lỗi/lệch → báo user, KHÔNG commit lén.
- **Evidence before claims** (skill `verification-before-completion`) — chạy lệnh thật + đọc output trước khi nói "xong". KHÔNG dùng từ lấp lửng "chắc chạy được".
- **Verify 2 lớp:** (E) mechanical = `npx tsc --noEmit` + `npm run lint` + `npm run build` + `/run`/`/verify` + `/code-review`; (F) compliance = đối chiếu từng requirement của brainstorm (capability/flow/limit/wording/state) với code → Pass/Fail/Missing → fix loop tối đa 3 vòng.
- **Vietnamese-first** khi tương tác.
- **KHÔNG đụng file ngoài scope feature** trừ khi plan nêu rõ (vd thêm route vào nav, thêm migration).

## Stack ai4ba — convention thực tế (đã verify từ code, MUST follow)

> Đọc lại file gốc mỗi lần để chắc, nhưng đây là pattern bắt buộc tuân theo. Code mới phải trông giống code cũ.

**Data layer (quan trọng nhất — sai chỗ này là sai cả feature):**
- DB = **better-sqlite3, ĐỒNG BỘ** (không async/await cho query), 1 connection chia sẻ qua `getDb()` trong `lib/db.ts`. WAL + `foreign_keys = ON`. File tại `SQLITE_DB_PATH` (default `./data/ai4ba.db`) — đĩa persistent, ⚠️ KHÔNG Vercel serverless.
- **Schema sống ở `lib/schema.sql`** — `getDb()` chạy `db.exec(schema)` mỗi lần khởi động, toàn bộ là `CREATE TABLE/INDEX IF NOT EXISTS` (idempotent). **Thêm bảng mới = thêm block vào file này**, KHÔNG viết migration runner, KHÔNG inline SQL trong route.
- Type mapping: uuid → `TEXT` (id = `randomUUID()`), boolean → `INTEGER` 0/1, timestamp → `TEXT` ISO (`new Date().toISOString()`).
- **Mọi truy cập DB = hàm typed export trong `lib/db.ts`**: `db.prepare(...).run/get/all`, bind `@named` params, kèm `interface` Row + New*. Route KHÔNG tự viết SQL — gọi hàm db.ts. Theo style `createRegistration` / `validateCoupon` / `listCoupons`.
- **Tái dùng bảng có sẵn:** `email_logs` (trạng thái gửi mail: pending/sent/delivered/bounce/failed) và `coupons`+`coupon_usages` (code + max_uses + used_count + is_active) — workshop dùng đúng pattern này thay vì dựng mới.

**API routes:** `app/api/.../route.ts`, export `async GET/POST(request: NextRequest)`, trả `NextResponse.json(...)`. Parse body trong try/catch → 400 `"Dữ liệu không hợp lệ"`. Vi phạm UNIQUE → catch `String(e).includes('UNIQUE')` → 409. Error message **tiếng Việt**. Validate input trong route (xem `validateCouponInput`).

**Admin:** mỗi route admin mở đầu `if (!(await isAuthedRequest())) return NextResponse.json({error:'Unauthorized'},{status:401})` (`lib/admin-auth.ts`). `middleware.ts` cũng chặn ở Edge (HMAC phải sync với admin-auth nếu đụng auth). Cookie `ai4ba_admin`, env `ADMIN_PASSWORD`. UI admin = panel trong `components/admin/*` (vd `coupons-panel.tsx`) + trang `app/admin/*`.

**Email:** `sendEmail(to, subject, html, text?, attachments?)` + `logEmailToDatabase(...)` trong `lib/mailersend.ts`; template HTML trong `lib/email-templates.ts`. Key qua env `MAILERSEND_API_KEY` / `MAILERSEND_FROM_EMAIL`. Bổ sung Resend (brainstorm OQ-5) = thêm provider song song, cùng interface `SendEmailResult` — đừng thay MailerSend.

**Validation/limits có sẵn:** `validateEmailServer` (format + disposable) trong `lib/email-validator-server.ts`; rate-limit in-memory 5/phút/IP + regex SĐT VN `^(0|\+84)[0-9]{9,10}$` trong `app/api/register/route.ts`. Thanh toán: `app/api/webhooks/sepay/route.ts` + bảng `payments` (match theo `sepay_transaction_id` / id-prefix). Notification: `lib/telegram.ts`.

**UI:** shadcn/radix trong `components/ui/*` (button/input/select/dialog/checkbox/switch/tabs...), Tailwind v4, `lucide-react` icon, `sonner` toast. Form public kiểu `components/ai4ba/RegistrationModal.tsx`.

**Scripts:** `npm run dev` (next dev --turbopack), `npm run build`, `npm run lint`. **KHÔNG có test script** → verify gate = `build` + `lint` + chạy `dev` thật.

## Inputs

```
/implement <feature>                 # build P0 từ brainstorm của feature
/implement <feature> <idea-slug>     # chỉ định brainstorm cụ thể (nếu feature có nhiều)
/implement <feature> --p0-only       # (mặc định) chỉ P0
/implement <feature> --all           # build cả P1/P2
/implement <feature> --continue      # tiếp tục trên branch feat/{feature} đã có
```

Examples:
```
/implement workshops                 # đọc docs/workshops/brainstorms/*, build P0
/implement workshops workshop-registration
/implement workshops --continue
```

## Approach

### Phase A — Resolve brainstorm (silent + confirm)

1. Resolve feature folder `docs/{feature}/`. Không có → báo lỗi, gợi ý chạy `/brainstorm` trước.
2. List `docs/{feature}/brainstorms/*.md`. 1 file → dùng luôn. Nhiều file → ask user chọn (hoặc dùng `<idea-slug>` arg).
3. **Read full** brainstorm doc. Trích: Capabilities (P0/P1/P2), Core Flows + ASCII, Decision Points, State Transitions, Interrupted-tx, Validation rules, Limits (exact), Wording (exact error/success/info), Assumptions, OQ còn hold.

### Phase B — Codebase recon (silent)

4. Grep/Read các module liên quan ("Stack ai4ba") để map mỗi capability sang chỗ tái dùng được. Note rõ: cái gì có sẵn (reuse), cái gì phải tạo mới (route/page/component/table/migration).
5. Detect dependency thiếu (vd Resend, reCAPTCHA verify, Google Map embed) → liệt kê để nêu ở L1.

### Phase C — Implementation plan (L1 approval)

6. Map **mỗi P0 capability** của brainstorm sang ≥1 hạng mục build (traceability — không bỏ sót capability nào; nếu capability nào không build được vì OQ hold/thiếu secret thì đánh dấu rõ). Lập **TodoWrite** task list theo flow brainstorm.
7. In **L1 plan** (dev-facing OK ở skill này — bảng file + việc hợp lệ):

   ```
   [/implement {feature}] Branch: feat/{feature} (tạo từ {current_branch})
   
   Scope: P0 ({n} capability) | P1/P2: hoãn
   
   Sẽ build:
     # | path                                   | action | việc
     1 | lib/schema.sql                         | edit   | thêm bảng workshops + workshop_registrations (CREATE IF NOT EXISTS)
     2 | lib/db.ts                              | edit   | thêm hàm typed: createWorkshop/listOpenWorkshops/registerWorkshop/countSlots...
     3 | app/workshops/page.tsx                 | create | trang list workshop đang mở
     4 | app/workshop/[slug]/page.tsx           | create | trang chi tiết + form
     5 | app/api/workshops/register/route.ts    | create | đăng ký + validate + giữ slot (gọi hàm db.ts)
     6 | components/admin/workshops-panel.tsx   | create | quản lý workshop + copy email hàng loạt
     ...
   
   Dependency mới (nếu có): {Resend SDK — gửi email; reCAPTCHA — verify token}
   OQ còn hold ảnh hưởng: {OQ-x: ... → đề xuất xử lý}
   
   Apply? (Y / sửa / branch <tên> / select skip <#>)
   ```
8. User Y → tiếp. Free text → re-plan. `branch <tên>` → đổi tên nhánh.

### Phase D — Branch + build

9. `git status` kiểm tra working tree. Có thay đổi chưa commit ngoài scope → hỏi user (stash / để nguyên / commit riêng) trước. Sau đó tạo nhánh local: `git checkout -b feat/{feature}` (nếu `--continue` thì checkout nhánh có sẵn). KHÔNG push.
10. Code theo todo list, P0 trước. Tôn trọng pattern hiện hữu (db access, route style, component style, admin auth). Wording dùng exact string từ brainstorm.
11. Update TodoWrite khi xong từng hạng mục.
12. File đã tồn tại → edit theo approval-gate (sửa lớn thì tóm tắt diff trước).

### Phase E — Mechanical gate (build / type / lint)

> Theo skill `verification-before-completion`: **evidence before claims**. Chạy lệnh thật + đọc output, KHÔNG tuyên bố "xong" theo trí nhớ.

13. Chạy tới sạch (fix → chạy lại):
    - `npx tsc --noEmit` → 0 lỗi type
    - `npm run lint` → 0 lỗi
    - `npm run build` → exit 0
    (Repo KHÔNG có test script.)
14. Chạy thật bằng `/run` (hoặc `/verify`): xác nhận happy path (trang list, trang chi tiết, submit form). Thiếu env/secret (Sepay/Resend/reCAPTCHA) chặn runtime → ghi rõ phần nào CHƯA verify được, đừng coi là đã verify.
15. `/code-review` trên diff: bug correctness → fix; finding nhỏ → note cho user.

### Phase F — Đối chiếu code vs brainstorm (compliance + fix loop) ⭐

> Vòng "code có đúng tài liệu không" — cốt lõi. Phase E chỉ chứng minh code *chạy*; Phase F chứng minh code *đúng nghiệp vụ đã chốt*. Theo pattern Pass/Fail/Missing.

16. Trích brainstorm thành **checklist verifiable**: mỗi P0 capability, mỗi flow step, mỗi validation rule, mỗi limit (số chính xác), mỗi wording error/success/info (exact string), mỗi state transition.
17. Audit code cho từng mục — **nên spawn Agent (general-purpose / Explore) làm reviewer độc lập** cho khách quan, prompt: "đối chiếu từng requirement dưới đây với code, trả Pass/Fail/Missing + file:line". Feature nhỏ thì tự grep/đọc.
18. In bảng đối chiếu:
    ```
    🔍 Đối chiếu {feature} vs brainstorm
    | # | Requirement (nguồn) | Doc | Code | file:line | Status |
    |---|---------------------|-----|------|-----------|--------|
    | 1 | Giữ slot chờ TT     | 30 phút | 30 phút | api/.../route.ts:42 | ✅ Pass |
    | 2 | Error trùng email   | "Email này đã đăng ký workshop." | "Email đã tồn tại" | :88 | ❌ Mismatch |
    | 3 | reCAPTCHA verify    | bắt buộc | — | — | ⚠️ Missing |
    Tổng: ✅{n}  ❌{n}  ⚠️{n}
    ```
19. **Fix loop:** mọi ❌ Mismatch + ⚠️ Missing → sửa code khớp doc (wording/limit/flow). Sửa xong chạy lại Phase E cho phần đụng tới, rồi re-check mục đó. Lặp tối đa 3 vòng; còn sót → liệt kê rõ trong report, KHÔNG giấu.
    - Mismatch do **doc sai** (code mới là đúng ý) → KHÔNG sửa code; flag user cập nhật brainstorm (L2 diff), đừng tự đổi nghiệp vụ.

### Phase G — Commit + bàn giao

20. Chỉ commit khi Phase E sạch + Phase F không còn ❌/⚠️ (hoặc còn nhưng đã báo + user chấp nhận). Đối chiếu `git status`, **stage đúng file feature** (không `git add -A` mù):
    ```
    git add <feature files...> && git commit -m "{message}"
    ```
    Message kết bằng dòng Co-Authored-By chuẩn.
21. Append changelog brainstorm: `- {date} | /implement | built P0 + verified vs doc: {tóm tắt}, nhánh feat/{feature}`.
22. **Report bàn giao:**
    ```
    ✅ Implemented P0 cho {feature} trên nhánh feat/{feature} (local, chưa push)
    
    Đã build: {danh sách}
    Hoãn (P1/P2): {danh sách}
    Đối chiếu doc: ✅{n} pass / ❌{n} đã fix / ⚠️{n} còn thiếu (lý do)
    Mechanical: tsc ✅ | lint ✅ | build ✅ | run {kết quả}
    OQ còn hold: {danh sách}
    Dependency mới cần cài: {npm install ... nếu có}
    
    Bước của bạn (solo-git):
      1. Review: git diff main...feat/{feature}
      2. Test lại local
      3. Merge: git checkout main && git merge feat/{feature}
      4. Deploy (self-hosted — KHÔNG Vercel serverless vì SQLite)
    ```

## Gotchas

- **SQLite self-hosted** — đừng giả định môi trường Vercel serverless; ghi DB cần đĩa persistent.
- **Schema evolution bị giới hạn** — `lib/schema.sql` toàn `CREATE TABLE IF NOT EXISTS`, nên **thêm cột/đổi CHECK của bảng đã tồn tại sẽ KHÔNG tự áp** (bảng đã có rồi, exec lại là no-op). Feature mới → ưu tiên **bảng mới** (`workshops`, `workshop_registrations`). Nếu buộc đổi bảng cũ (vd thêm giá trị vào CHECK `email_logs.email_type`) → phải migration tay + nêu rõ trong L1, đừng tưởng IF NOT EXISTS lo được.
- **Route KHÔNG tự viết SQL + KHÔNG async query** — luôn thêm hàm typed vào `lib/db.ts` (better-sqlite3 đồng bộ). Sai pattern này là dấu hiệu code lạc style.
- **Đừng thay module có sẵn bằng cái mới** (MailerSend, Sepay, email-validator, admin auth) — tái dùng. Chỉ thêm mới khi brainstorm yêu cầu (vd Resend bổ sung quota).
- **Wording phải khớp brainstorm** — copy exact string error/success/info, đừng tự chế lại tiếng Việt.
- **Limit/threshold lấy đúng số** trong brainstorm (vd giữ slot 30p, rate limit 5/phút) — đừng hardcode số khác.
- **OQ hold = chưa chốt nghiệp vụ** — không tự quyết; hỏi user hoặc khoanh vùng skip.
- **KHÔNG push/PR** — user merge local. Nếu user muốn push/PR, họ nói riêng (skill này mặc định không đụng remote).
- **Commit chỉ khi chạy được** — chưa /run pass thì báo lỗi, đừng commit code hỏng.
- **Secret/env** (Sepay key, Resend key, reCAPTCHA secret, MailerSend) — KHÔNG hardcode; đọc/đặt qua env, hướng dẫn user thêm vào `.env`.
- **Feature lớn** — nếu P0 quá rộng cho 1 lượt, đề xuất chia milestone trong L1, build phần lõi trước (vd: free-flow trước, paid-flow sau).

## References

- @../../rules/approval-gate.md
- @../../rules/naming-conventions.md
- @../brainstorm/SKILL.md
- @../verification-before-completion/SKILL.md
