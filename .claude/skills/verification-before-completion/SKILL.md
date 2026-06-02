---
name: verification-before-completion
description: Checklist bắt buộc TRƯỚC khi tuyên bố bất kỳ việc code nào "xong". Evidence before claims — chạy lệnh thật, đọc output, rồi mới khẳng định. Dùng bởi /implement (Phase E + F) và bất cứ lúc nào cần chứng minh code chạy + đúng nghiệp vụ. Lệnh verify cho ai4ba: tsc --noEmit, npm run lint, npm run build, npm run dev. KHÔNG có test framework.
user-invocable: false
---

# Verification Before Completion (ai4ba)

**Quy tắc: bằng chứng trước lời khẳng định, luôn luôn.**

Phỏng theo superpowers framework. Skill này là knowledge module — `/implement` nạp ở Phase E (mechanical) và Phase F (đối chiếu nghiệp vụ).

## Cổng kiểm tra (the gate)

Trước khi nói BẤT KỲ việc gì "xong":

1. **Xác định** lệnh chứng minh được claim.
2. **Chạy** nó đầy đủ, tươi mới (không từ cache/trí nhớ).
3. **Đọc** trọn output + exit code.
4. **Đối chiếu** output có khớp claim không.
5. **Chỉ khi đó** mới khẳng định, kèm bằng chứng.

Bỏ bước nào = thiếu trung thực, không phải tiết kiệm.

## Claim nào cần bằng chứng gì

| Claim | Bằng chứng cần có |
|-------|-------------------|
| "Không lỗi type" | `npx tsc --noEmit` output sạch |
| "Không lỗi lint" | `npm run lint` 0 lỗi |
| "Code build được" | `npm run build` exit 0 |
| "Feature chạy" | Chạy `npm run dev`, thao tác happy path thật trên trình duyệt |
| "Đúng nghiệp vụ" | Đối chiếu từng requirement của brainstorm với code (file:line) — Pass/Fail/Missing |
| "Bug đã fix" | Tái hiện triệu chứng gốc → giờ không còn |

## Lệnh verify cho ai4ba

```bash
npx tsc --noEmit        # type check toàn project
npm run lint            # eslint
npm run build           # next build (turbopack)
npm run dev             # chạy thật để thao tác flow (cần env: SQLITE_DB_PATH, MAILERSEND_*, ADMIN_PASSWORD, Sepay/Resend/reCAPTCHA key)
```

> ai4ba **không có test script**. Gate tối thiểu = `tsc` + `lint` + `build`. "Feature chạy" cần chạy `dev` thật; nếu thiếu secret chặn runtime thì nói rõ phần nào CHƯA verify được — đừng coi là đã verify.

## Đối chiếu code vs brainstorm (compliance)

"Build được" ≠ "đúng nghiệp vụ". Sau mechanical gate, đối chiếu từng mục verifiable của brainstorm:

- Mỗi **limit/số** (vd giữ slot 30 phút, rate limit 5/phút) — code có đúng con số không?
- Mỗi **wording** error/success/info — exact string có khớp từng ký tự không?
- Mỗi **validation rule** — có implement đúng không?
- Mỗi **flow step / state transition** — có đủ nhánh không?

Mỗi mục → `✅ Pass` / `❌ Mismatch` / `⚠️ Missing` + cite `file:line`. Mismatch/Missing → fix rồi re-check. Nếu Mismatch vì **doc sai** (code mới đúng ý) → flag user cập nhật brainstorm, đừng tự đổi nghiệp vụ.

## Cờ đỏ — dừng lại trước khi đi tiếp

- Dùng từ lấp lửng: "chắc chạy được", "có vẻ ổn", "chắc fix rồi".
- Thấy hài lòng mà chưa chạy lệnh verify.
- Tin trí nhớ thay vì chạy lại lệnh.
- Dựa vào kiểm tra một phần ("compile được nên chạy được").
- Khẳng định thành công dựa trên đúng 1 trường hợp.

## Tiêu chuẩn

Chạy lệnh. Đọc output thật. Rồi — và chỉ khi đó — mới tuyên bố kết quả. Không ngoại lệ.
