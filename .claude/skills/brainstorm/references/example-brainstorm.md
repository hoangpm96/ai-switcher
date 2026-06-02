---
type: brainstorm
feature: payment
idea_slug: checkout-flow
status: draft
lang: vi
owner: "@hoangphan"
created: 2026-05-12
updated: 2026-05-12
links:
  - docs/meetings/2026-05-12-client-payment-kickoff.md
tags: [brainstorm, payment, checkout]
stale_reason: ""
changelog:
  - 2026-05-12 | /brainstorm | initial scaffold từ client meeting + seed
---

# Payment Checkout Flow — Brainstorm

> Feature: payment | Idea: checkout-flow

## 1. Idea Seed

> "Thêm thanh toán online cho ứng dụng e-commerce. Hiện chỉ có COD, mất 35% cart abandon. Client muốn launch v1 Q3, ưu tiên Momo + VNPay + Card. Guest checkout không bắt đăng ký. Returning customer có saved card."

(Seed từ client kickoff meeting 2026-05-12.)

## 2. Context

- Hiện tại: chỉ COD, 35% cart abandon ở checkout step.
- Market: Shopee/Lazada đã có 1-tap checkout, ta chậm 2-3 năm.
- Regulatory: SBV cho phép tokenize card on-merchant từ 2025.
- Internal: AWS infra hiện tại, no PCI compliance yet.

## 3. User Types (preliminary)

| User Type | Pain Point | Primary Need |
|-----------|-----------|--------------|
| Khách mới (guest) | Sợ đăng ký + form thẻ dài | Checkout nhanh, không tài khoản |
| Khách thân thiết | Nhập lại thẻ mỗi lần | Saved card 1-tap |
| Admin shop | Manual reconcile COD | Auto reconcile + refund flow |

## 4. Capabilities Breakdown

### P0 — must have
- Guest checkout với Momo + VNPay + Card.
- Pre-confirm screen show tổng + phí.
- Email confirmation sau success.
- Error handling: timeout, declined, insufficient fund.
- Admin transaction dashboard.
- Full refund flow.
- PCI-DSS Level 2 compliance.

### P1 — should have
- Saved card cho returning (PCI tokenized).
- 1-tap với saved card + CVV.
- SMS confirmation parallel email.
- Admin filter/search transactions.

### P2 — nice to have
- ATM napas.
- Partial refund.
- Fraud rule engine basic.
- Multi-card per user.

> P0/P1/P2 là tentative; chốt formal ở `/prd payment`.

## 5. Edge Cases / Scenarios

- **Gateway timeout** (>30s no webhook) — show "Thử lại" page, polling status fallback.
- **User abandon mid-payment** (closed tab khi đang ở gateway) — webhook arrive sau khi tab closed → email confirmation đến nhưng user không biết success. Cần "Check status" page.
- **Card declined insufficient fund** — show specific error, suggest method khác.
- **Webhook signature invalid** — log + alert ops, KHÔNG update transaction state.
- **Idempotency conflict** — cùng order_ref charged 2 lần → reject 2nd, return existing transaction.
- **3DS challenge timeout** (user idle >5min) — retry path.
- **Order amount mismatch** (FE submit khác BE state) — BE reject + audit log.
- **Refund spike** — multiple refunds same day → CS load tăng → cần playbook.
- **Network slow 4G** — checkout phải work với payload <80KB initial, lazy load Stripe.js.

## 6. Assumptions

- AWS RDS có encryption at-rest support (KMS).
- Momo + VNPay accept webhook receiver IP allowlist (public HTTPS).
- Stripe Vietnam có available cho Vietnamese entities (need legal confirm).
- User base mobile-first (>70% traffic mobile).
- Order amount currency VND only v1.

## 7. Risks

| Rủi ro | Khả năng | Hậu quả nghiệp vụ | Cách phòng |
|--------|----------|-------------------|-----------|
| PCI audit fail chặn launch (compliance) | thỉnh thoảng | Không launch được, mất Q4 revenue | Thuê consultant tháng đầu, internal review tháng 3 |
| Đối tác Momo đổi API/phí (vendor) | hiếm | Phí vượt budget hoặc downtime giao dịch | Hợp đồng SLA + monitor sandbox release notes |
| Fraud spike sau launch (process) | thỉnh thoảng | Chargeback tăng → mất tiền + bị Visa/Master phạt | Rule fraud cơ bản + escalation team CSKH |
| User không tin card-saving (adoption) | thỉnh thoảng | Conversion thấp, không đạt mục tiêu repeat purchase | Onboarding tooltip + T&C rõ + badge "secured by Stripe" |
| Stripe chưa hỗ trợ VN (vendor) | hiếm | Phải dùng vendor backup, delay launch 2-3 tuần | Sớm confirm với Stripe sales + có Momo làm fallback |

## 8. Success Criteria (preliminary)

- Conversion checkout step >= 85% (p75).
- Time-to-success p75 <= 90s.
- 0 PCI incident / 12 tháng.
- CSAT post-checkout >= 4.2/5.

*Final metrics chốt ở `/brd payment`.*

## 9. Open Questions

- [ ] OQ: Fraud detection bên thứ 3 (Sift, Stripe Radar) hay basic rule tự build?
- [ ] OQ: Saved card OTP confirm mỗi lần dùng (UX vs security)?
- [ ] OQ: Refund partial scope v1 hay v1.1?
- [ ] OQ: KYC light cho guest checkout > 5M VND?
- [ ] OQ: ATM napas integration v1 (nhiều user request) hay defer?

## 10. Next Steps

Sau khi BA review + approve brainstorm này:
- `/urd payment` — capture user perspective + journeys
- `/brd payment` — business case + SMART objectives + ROI
- `/prd payment` — chốt P0/P1/P2 scope + release plan

*KHÔNG nhảy thẳng `/srs payment` — qua PRD trước.*

## Bonus: Checkout Flow ASCII Sketch

```
User → Cart → "Thanh toán" → Method Select → ┬→ Momo redirect → Webhook → Success
                                              ├→ VNPay redirect → Webhook → Success
                                              └→ Card form (Stripe) → 3DS? → Success
                                                                            ↓
                                                                            Email + SMS
```
