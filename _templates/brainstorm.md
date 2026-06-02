---
type: brainstorm
feature: {{feature}}
idea_slug: {{idea_slug}}
status: draft
mode: {{mode}}                # deep | shallow
lang: {{lang}}
owner: {{owner}}
created: {{date}}
updated: {{date}}
complexity_flags: {{complexity_flags}}   # list: has_external_redirect, has_state_machine, has_multi_role, has_async_flow, has_throttle_rules
links: {{links}}
tags: [brainstorm, {{feature}}]
stale_reason: ""
changelog: []
---

# {{title}}

> Feature: {{feature}} | Idea: {{idea_slug}}
> 1 feature có thể có nhiều brainstorm — đây là 1 idea/draft độc lập.

## 1. Idea Seed

{{seed}}

*Raw input từ user — câu/đoạn description gốc.*

## 2. Context

{{context}}

*Background, why now, related features, market signal.*

## 3. User Types (preliminary)

| User Type | Pain Point | Primary Need |
|-----------|-----------|--------------|
| {{user_type}} | {{pain}} | {{need}} |

## 4. Capabilities Breakdown

### P0 — must have
{{p0_capabilities}}

### P1 — should have
{{p1_capabilities}}

### P2 — nice to have
{{p2_capabilities}}

> P0/P1/P2 là tentative; final scope chốt ở `/prd <feature>`.

## 5. Core Flows (Happy Path)

> Mỗi flow chính = numbered steps từ góc user + system. **ASCII diagram đi kèm** cho flow có branching ≥2 / external redirect / async. Flow nhỏ riêng — không gom thành 1 prose paragraph dài.

### 5.1 {{flow_1_name}}

1. {{step_1}}
2. {{step_2}}
3. {{step_3}}

```
{{ascii_flow_1}}
```

### 5.2 {{flow_2_name}}

1. ...

```
{{ascii_flow_2}}
```

*Liệt kê đủ flows chính (vd: signup, login, forgot password, logout, OAuth, ...). Mỗi flow độc lập, có ASCII riêng nếu phức tạp.*

## 6. System Behavior Deep Dive

> Mandatory tables tùy `complexity_flags`. Skill bỏ qua subsection nào không trigger.

### 6.1 Decision Points

| ID | Flow | Khi nào | YES (nhánh đồng ý) | NO (nhánh từ chối) |
|---|---|---|---|---|
| D1 | {{flow_name}} | {{condition}} | {{yes_action}} | {{no_action}} |

*Mỗi decision point = 1 câu hỏi YES/NO trong flow. ID `D1, D2…` để cross-ref. Capture tất cả branching điểm chính (validate email tồn tại, captcha trigger, lockout, OAuth callback…).*

### 6.2 Scenario Matrix (nếu `has_multi_role` / ≥2 input states)

| From State | To State | Rule | Action | Result |
|------------|----------|------|--------|--------|
| {{scenario_row}} | | | | |

### 6.3 State Transitions (nếu `has_state_machine`)

```
{{entity}}: {{state_a}} → {{state_b}} → {{state_c}}
                      ↘ {{state_d}} (alternative)
```

| Entity | Từ | Sang | Trigger | Quay lại được? |
|--------|------|----|---------|-------------|
| {{entity}} | {{from}} | {{to}} | {{trigger}} | có/không |

### 6.4 Interrupted Transactions (nếu `has_external_redirect` / `has_async_flow`)

| Tình huống | Hệ thống còn lại gì | Resume | Cleanup |
|---|---|---|---|
| Browser/app đóng giữa flow | {{state}} | {{resume}} | {{cleanup}} |
| External service fail/timeout | {{state}} | {{resume}} | {{cleanup}} |
| Link/token hết hạn | {{state}} | {{resume}} | {{cleanup}} |
| 2 device cùng action | {{state}} | {{resume}} | {{cleanup}} |
| Flow mới khi flow cũ còn pending | {{state}} | {{resume}} | {{cleanup}} |

### 6.5 Other Edge Cases

{{edge_cases}}

*Edge cases gom chung ở đây — KHÔNG tách section riêng. Bao gồm: empty state, concurrent, timeout, network down, validation fail, không thuộc các matrix trên.*

## 7. Validation, Limits & Wording

### 7.1 Validation rules

| Field | Rule |
|---|---|
| {{field}} | {{validation_rule}} |

*Format, độ dài, required, unique, ràng buộc nghiệp vụ.*

### 7.2 Limits & Quotas (exact values)

| Tham số | Giá trị | Window | Behavior khi vượt |
|---|---|---|---|
| {{limit_name}} | {{value}} | {{window}} | {{action}} |

*BẮT BUỘC số liệu cụ thể (vd "5 lần/phút", "24 giờ"), KHÔNG viết "rate limit phù hợp".*

### 7.3 Wording samples (exact strings)

#### Error messages

| Tình huống | Wording | Code |
|---|---|---|
| {{error_case}} | "{{exact_string}}" | E-? |

#### Success messages

| Tình huống | Wording |
|---|---|
| {{success_case}} | "{{exact_string}}" |

#### Info / neutral messages

| Tình huống | Wording |
|---|---|
| {{info_case}} | "{{exact_string}}" |

*Wording trong dấu nháy = exact string user sẽ thấy. Tiếng Việt natural, KHÔNG dịch máy. Code map sang error matrix ở `/srs`.*

## 8. Assumptions

{{assumptions}}

*Giả định đang dùng — cần verify trước khi spec. Vd: "Email là unique identifier", "Region: global", "Mobile + Web share backend".*

## 9. Risks

| Rủi ro | Khả năng | Hậu quả nghiệp vụ | Cách phòng |
|--------|----------|-------------------|-----------|
| {{risk}} | thường / thỉnh thoảng / hiếm | {{impact_business}} | {{mitigation}} |

*Rủi ro nghiệp vụ — KHÔNG phải bug/infra. Ví dụ loại:*
- *Adoption: "User không hiểu flow mới → conversion giảm"*
- *Vendor: "Đối tác thanh toán đổi phí → vượt budget"*
- *Compliance: "Chưa đủ document cho audit PCI"*
- *Process: "Team CSKH chưa được train flow refund"*
- *Timeline: "Legal review T&C chậm → trễ launch"*
- *Data: "Dữ liệu user cũ thiếu phone → OTP fail khi migrate"*

*Hậu quả nghiệp vụ viết bằng từ business: mất khách / mất doanh thu / complaint tăng / delay launch / vi phạm hợp đồng. KHÔNG viết "API chậm", "DB lock".*

## 10. Success Criteria (preliminary)

{{success_criteria}}

*Measurable hint — chốt formal ở URD/BRD. Vd: "Signup conversion rate ≥ X%", "Time-to-first-login < Y giây".*

## 11. Open Questions

- [ ] OQ-1: {{open_question_1}}
- [ ] OQ-2: {{open_question_2}}
- [ ] OQ-3: {{open_question_3}}

## 12. Next Steps

Sau brainstorm này (sau khi BA approve):
- `/urd {{feature}}` — capture user perspective
- `/brd {{feature}}` — business case
- `/prd {{feature}}` — product scope

*KHÔNG nhảy thẳng SRS — qua PRD trước.*
