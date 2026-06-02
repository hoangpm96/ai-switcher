# Naming Conventions

## Slugs (folder & file names)

- All lowercase, kebab-case
- ASCII only (avoid Vietnamese diacritics — use English/transliteration)
- No spaces, no underscores, no special chars
- Strip leading/trailing dashes
- Max 50 chars

| Input | Slug |
|-------|------|
| "User Login" | `user-login` |
| "Forgot Password (v2)" | `forgot-password-v2` |
| "Thanh toán đơn hàng" | `payment-checkout` (preferred) |
| "2FA / OTP" | `two-factor-auth` |

## File path patterns

| Doc type | Path |
|----------|------|
| URD | `docs/{feature}/urd.md` |
| BRD | `docs/{feature}/brd.md` |
| PRD | `docs/{feature}/prd.md` |
| SRS spec | `docs/{feature}/srs/spec.md` |
| SRS flows (sequence + activity, 1 file gộp) | `docs/{feature}/srs/flows.md` — mỗi flow 1 `## Flow: {title}` section với mermaid sequence/flowchart inline. Output cố định của `/sequence` và `/activity`. |
| SRS states (state diagrams, 1 file gộp) | `docs/{feature}/srs/states.md` — mỗi entity 1 `## State: {Entity}` section với mermaid `stateDiagram-v2`. Output cố định của `/state`. |
| SRS ERD | `docs/{feature}/srs/erd.md` |
| Screen index (master metadata, v2.6.1) | `docs/{feature}/ascii-screen/_index.md` — frontmatter v2 + table Screens (status/used-by/designs Figma+HTML/updated) + section `## Descriptions` (H3 per screen, 1-2 câu purpose) + changelog tổng. **Single source of metadata + descriptions** cho mọi screens của feature. |
| Screen content (minimal, v2.6.1) | `docs/{feature}/ascii-screen/{screen-slug}.md` — **zero frontmatter**, chỉ **2 sections body** (Wireframe ASCII INLINE Mục 1 / Screen description table 4 cột với `• `+`<br>` format Mục 2). **KHÔNG có Description ở đây** — sống trong `_index.md`. |
| HTML mockup | `docs/{feature}/html-design/{screen-slug}.html` (folder riêng) — path lưu trong `_index.md` cột `HTML`. |
| Figma frame URL | URL lưu trong `_index.md` cột `Figma` (output của `/figma`). KHÔNG file local. |
| HTML prototype | `docs/{feature}/html-design/prototype.html` (output của `/prototype`, multi-screen clickable, self-contained). Reference trong `_index.md` cột `HTML prototype` dạng `prototype.html#{slug}`. |
| Brainstorm | `docs/{feature}/brainstorms/{idea-slug}.md` |
| User story index (master metadata, v2.6) | `docs/{feature}/userstories/_index.md` — frontmatter v2 + table Stories (ID/title/persona/FR/screens/priority/status/jira-key/updated) + changelog tổng. **Single source of metadata + status + jira key** cho mọi stories của feature. |
| User story content (minimal, v2.6) | `docs/{feature}/userstories/us-{NNN}.md` — **zero frontmatter**, chỉ prose sections (User Story / Context / Linked Requirements / Acceptance Criteria inline / UI refs / Error refs / Dependencies / OQs). Metadata + status + jira **sống ở `_index.md`**. |
| Function / Use case (function-centric v2.3, 8 sections PROSE) | `docs/{feature}/usecases/uc-{slug}.md` — self-contained 1 file: intro + actors + preconds + expected result + activity diagram OPTIONAL Mục e + screens involved + FR map + OQs. **Sequence/state KHÔNG embed UC** — thuộc `srs/flows.md` / `srs/states.md`. |
| Use Case diagram (visual scope) | `docs/{feature}/usecases/diagram.md` (output cố định của `/usecase-diagram`) |
| Traceability | `docs/_shared/traceability.md` (auto from /gap) |
| Jira map | `docs/_shared/jira-map.md` (auto from /jira) |
| Meeting | `docs/meetings/YYYY-MM-DD-{type}-{slug}.md` (project-level) |
| Decision | `docs/decisions/YYYY-MM-DD-{slug}.md` (project-level) |
| Blocker | `docs/blockers/YYYY-MM-DD-{slug}.md` (project-level) |
| Inbox capture | `docs/inbox/YYYY-MM-DD-{slug}.md` (project-level) |
| Change Request | `docs/changes/CR-{YYYYMMDD}-{NNN}.md` (project-level) |
| Impact Report | `docs/impacts/CR-{cr_id}-impact.md` (project-level) |
| Export package | `docs/exports/{date}-{scope}{-feature}-package.{md|html|pdf|docx}` |

## Wikilinks

Format: `[[docs/payment/srs/spec|Payment SRS]]`

- Use full path from project root (Obsidian + GitHub render correctly)
- Optional display text after `|`
- Don't use `[[Login Feature]]` (Obsidian-only style) — breaks on GitHub

## Frontmatter requirements

Every doc-type file MUST have YAML frontmatter at the top:

```yaml
---
type: srs-feature           # see types below
status: draft               # see status-lifecycle.md
created: 2026-05-09         # ISO date
updated: 2026-05-09         # ISO date
---
```

Recommended optional fields:
- `owner`: handle (e.g. `@hoang`)
- `priority`: P0 / P1 / P2
- `version`: semver (e.g. `0.1.0`)
- `tags`: list of strings
- `links`: dict of related-doc paths

## Doc type values

| Type | Use for |
|------|---------|
| `srs` | `docs/{feature}/srs/spec.md` (FULL frontmatter v2: status/owner/links/changelog/...) |
| `srs-flows` | `docs/{feature}/srs/flows.md` (sequence + activity diagrams, 1 file gộp). **Slim frontmatter v2.6**: chỉ `type`/`feature`/`updated`. Lifecycle inherit từ spec.md. |
| `srs-states` | `docs/{feature}/srs/states.md` (state diagrams, 1 file gộp per entity). **Slim frontmatter v2.6**: chỉ `type`/`feature`/`updated`. |
| `srs-erd` | `docs/{feature}/srs/erd.md` (Mermaid `erDiagram`). **Slim frontmatter v2.6**: chỉ `type`/`feature`/`updated`. |
| `screen-index` | `docs/{feature}/ascii-screen/_index.md` (master metadata + changelog + designs map cho toàn bộ screens, v2.6) |
| `screen` | `docs/{feature}/ascii-screen/{slug}.md` (minimal content file, **zero frontmatter** — không có `type:` field trong file; type này chỉ dùng để classify khi cần grep) |
| `urd` / `brd` / `prd` | per-feature requirements docs (`docs/{feature}/{urd,brd,prd}.md`) |
| `brainstorm` | `docs/{feature}/brainstorms/*.md` |
| `userstory-index` | `docs/{feature}/userstories/_index.md` (master metadata + status/priority/jira-key/changelog cho toàn bộ stories, v2.6) |
| `user-story` | `docs/{feature}/userstories/us-{NNN}.md` (minimal content file, **zero frontmatter** — type này chỉ dùng để classify khi cần grep) |
| `use-case` | `docs/{feature}/usecases/*.md` |
| `diagram-usecase` | `docs/{feature}/usecases/diagram.md` (output cố định của `/usecase-diagram`) |

> **v2.6 simplification:** `diagram-sequence` / `diagram-activity` / `diagram-state` / `diagram-erd` type cũ bị bỏ — file container dùng `srs-flows` / `srs-states` / `srs-erd` type. Mỗi diagram là 1 section trong file gộp, KHÔNG có frontmatter riêng.
| `change-request` | `docs/changes/CR-*.md` |
| `impact-report` | `docs/impacts/CR-*-impact.md` |
| `archive-stub` | original path sau khi file moved to `_archive/` |
| `traceability` | `docs/_shared/traceability.md` |
| `jira-map` | `docs/_shared/jira-map.md` |
| `export-package` | `docs/exports/*.md` |
| `meeting` | `docs/meetings/*.md` |
| `decision` | `docs/decisions/*.md` |
| `blocker` | `docs/blockers/*.md` |
| `inbox` | `docs/inbox/*.md` |

## ID conventions (cross-doc references)

Mọi ID trong frontmatter `links:` hoặc body phải tuân format dưới. Format này đảm bảo `/gap` traceability matrix không collision khi cross-aggregate cross-feature.

### Format chung

| Loại | Format | Ví dụ | Scope |
|------|--------|-------|-------|
| Business Objective | `BO-{feature}-{NNN}` | `BO-payment-01` | Per-feature, trong `brd.md` Mục 4 |
| PRD Capability | `CAP-{feature}-{NNN}` | `CAP-payment-01` | Per-feature, trong `prd.md` Mục 4 |
| Functional Requirement | `FR-{feature}-{NNN}` | `FR-payment-001` | Per-feature, trong `srs/spec.md` Mục 2 |
| Non-Functional Requirement | `NFR-{feature}-{NNN}` | `NFR-payment-001` | Per-feature, trong `srs/spec.md` Mục 3 |
| Business Rule | `BR-{feature}-{NNN}` | `BR-payment-001` | Per-feature, trong `srs/spec.md` Mục 4 |
| Error Code | `E-{feature}-{NNN}` | `E-payment-001` | Per-feature, trong `srs/spec.md` Mục 5 |
| User Story | `US-{NNN}` | `US-001` | Per-feature folder (`docs/payment/userstories/us-001.md`) — feature ngầm hiểu qua path |
| Use Case | `UC-{slug}` | `UC-checkout` | Per-feature folder, slug human-readable |
| Acceptance Criterion | `AC-{NNN}` | `AC-001` | Per-user-story (scope trong file `us-{NNN}.md`) |
| Change Request | `CR-{YYYYMMDD}-{NNN}` | `CR-20260512-001` | Project-wide (`docs/changes/`) |
| Decision | `D-{YYYY-MM-DD}-{slug}` | `D-2026-05-12-stripe-vs-momo` | Project-wide |
| Blocker | `B-{YYYY-MM-DD}-{slug}` | `B-2026-05-12-vendor-delay` | Project-wide |

### Rules

- **Feature prefix bắt buộc** cho BO/CAP/FR/NFR/BR/E. Mục đích: tránh collision khi `/gap` cross-feature aggregate (vd `FR-001` ambiguous khi 2 features).
- **US/AC/UC scope qua path** (không cần feature prefix trong ID) vì luôn nằm trong folder feature.
- **NNN = 3 digit zero-pad** cho BO/CAP/FR/NFR/BR/E (vd `001`, `042`). NN cũng OK cho BO/CAP (`01`, `02`) vì thường ít hơn.
- **CR/D/B prefix date** vì là sự kiện theo thời gian, ordering theo date.
- ID không reuse khi delete — luôn increment max + 1.
- Slug trong ID kebab-case, max 30 chars.

### Cross-references

Khi 1 doc reference ID của doc khác:
- Frontmatter `links:` flat list với full path: `links: [docs/payment/srs/spec.md, docs/payment/userstories/us-001.md]`
- Body inline reference: `[[docs/payment/srs/spec.md#FR-payment-001|FR-payment-001]]` (Obsidian-compatible anchor).
- `/gap` parse cả 2 dạng để build relationship graph.
