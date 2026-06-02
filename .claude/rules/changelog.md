# Change Log Convention (v2.6)

> v2.6 — changelog YAML list trong frontmatter, NHƯNG chỉ ở **review-unit files**. File phụ trợ (auto-gen, content snippet) routing changelog lên file cha.

## Review-unit files (CÓ changelog frontmatter)

Đây là 6 file per-feature có lifecycle độc lập + stakeholder review trực tiếp:

| Path | Coverage |
|------|----------|
| `docs/{f}/urd.md` | Chính nó |
| `docs/{f}/brd.md` | Chính nó |
| `docs/{f}/prd.md` | Chính nó |
| `docs/{f}/srs/spec.md` | Chính nó + `srs/flows.md` + `srs/states.md` + `srs/erd.md` |
| `docs/{f}/usecases/_index.md` | Chính nó + `usecases/uc-*.md` + `usecases/diagram.md` |
| `docs/{f}/ascii-screen/_index.md` | Chính nó + `ascii-screen/{slug}.md` |
| `docs/{f}/userstories/_index.md` | Chính nó + `userstories/us-*.md` (AC inline cũng route về đây) |

Plus project-level files (cũng có changelog):
- `docs/meetings/*.md`, `docs/decisions/*.md`, `docs/blockers/*.md` (per-file)
- `docs/changes/CR-*.md`, `docs/impacts/CR-*-impact.md`

## Routing rules (skill + hook PHẢI tuân)

| Khi edit file | Ghi changelog vào | Entry prefix |
|---|---|---|
| `urd.md` / `brd.md` / `prd.md` | chính nó | — |
| `srs/spec.md` | chính nó | — |
| `srs/flows.md` (qua `/sequence`, `/activity`) | `srs/spec.md` | `[flows]` |
| `srs/states.md` (qua `/state`) | `srs/spec.md` | `[states]` |
| `srs/erd.md` (qua `/erd`) | `srs/spec.md` | `[erd]` |
| `usecases/_index.md` | chính nó | — |
| `usecases/uc-*.md` (qua `/usecase`) | `usecases/_index.md` | `[uc-{slug}]` |
| `usecases/diagram.md` (qua `/usecase-diagram`) | `usecases/_index.md` | `[diagram]` |
| `ascii-screen/_index.md` | chính nó | — |
| `ascii-screen/{slug}.md` (qua `/srs-add-screen`, `/wireframe`) | `ascii-screen/_index.md` | `[{slug}]` |
| `userstories/_index.md` | chính nó | — |
| `userstories/us-*.md` (qua `/userstory`, `/ac`, `/jira`) | `userstories/_index.md` | `[us-NNN]` |

**Tại sao routing?** File auto-gen (flows/states/erd/diagram) hoặc content snippet (uc-*/screen-*/) không có lifecycle riêng — mọi thay đổi vẫn material với review-unit file cha. Routing tránh changelog scatter + buộc note rõ "cái gì changed" qua prefix.

## Entry format

```
- {date} | {skill-name} | {prefix?} {note}
```

- **date**: ISO `YYYY-MM-DD`.
- **skill-name**: `/urd`, `/sequence`, `/wireframe`, `/cr`, `manual` (hook fallback).
- **prefix** (nếu routed): `[flows]`, `[states]`, `[erd]`, `[uc-{slug}]`, `[{screen-slug}]`, `[diagram]`. Mục đích: reader của review-unit file biết "thay đổi nằm ở artifact nào".
- **note**: imperative/past-tense, ≤80 chars, factual, mô tả **what changed**.

**Examples:**
```yaml
changelog:
  - 2026-05-21 | /sequence | [flows] added refund webhook sequence
  - 2026-05-21 | /erd | [erd] added Refund entity, relation Order:Refund 1:N
  - 2026-05-20 | /review | reviewed by @senior-ba, 1 blocking fix
  - 2026-05-19 | /srs | initial spec scaffolded
```

Trong `usecases/_index.md`:
```yaml
changelog:
  - 2026-05-21 | /usecase | [uc-google-oauth] added new UC
  - 2026-05-20 | /usecase | [uc-login] expected result branches updated
  - 2026-05-19 | /usecase-diagram | [diagram] initial scope diagram
```

Trong `ascii-screen/_index.md`:
```yaml
changelog:
  - 2026-05-21 | /wireframe | [login] HTML mockup added
  - 2026-05-20 | /srs-add-screen | [forgot-password] screen added
  - 2026-05-19 | /srs | initial 3 screens scaffolded
```

## Note style

- Imperative/past-tense, factual, ≤80 chars.
- Good: `[flows] added refund webhook sequence`, `[uc-login] AC for invalid password updated`, `applied CR-20260512-001: added OTP requirement`.
- Bad: `updated stuff`, `fixed things`, `per Hoang's request` (attribution qua skill-name).

## How skills route entries

Skill biết file mình đang edit + route theo bảng trên. Pseudo:

```
EDITED_FILE = "docs/payment/srs/flows.md"
case EDITED_FILE in
  srs/flows.md|srs/states.md|srs/erd.md)
    TARGET = "docs/{feature}/srs/spec.md"
    PREFIX = "[flows]" | "[states]" | "[erd]"
    ;;
  usecases/uc-*.md|usecases/diagram.md)
    TARGET = "docs/{feature}/usecases/_index.md"
    PREFIX = "[uc-{slug}]" | "[diagram]"
    ;;
  ascii-screen/{slug}.md)
    TARGET = "docs/{feature}/ascii-screen/_index.md"
    PREFIX = "[{slug}]"
    ;;
  userstories/us-*.md)
    TARGET = "docs/{feature}/userstories/_index.md"
    PREFIX = "[us-NNN]"
    ;;
  *)
    TARGET = EDITED_FILE  # self
    PREFIX = ""
    ;;
esac

append "- {date} | {skill} | {prefix} {note}" to TARGET.changelog
```

Hook là safety net cho manual edits — env vars `CLAUDE_SKILL_NAME` + `CLAUDE_CHANGELOG_NOTE` skill set trước Edit; nếu miss, hook default `manual | manual edit` vào file routing target.

## Dedupe by date + prefix

Trong v2.6, dedupe key = `(date, prefix)`. Cùng date nhưng khác prefix (vd `[flows]` và `[erd]`) → log riêng. Cùng date + cùng prefix → skip duplicate. Cho phép multi-artifact changes trong 1 ngày vẫn được audit.

## Files excluded từ auto-changelog

Hook skip:
- `docs/feature-list.md` (auto-gen)
- `docs/README.md`
- `docs/_shared/*` (project-level)
- `docs/exports/*` (regenerated)
- `docs/inbox/*` (raw capture)
- `docs/_archive/*` (frozen)

## Migration từ v1/v2.0

Docs cũ có:
- Body `## Change Log` table → migrate sang frontmatter list (script `_scripts/migrate-changelog-v2.sh`).
- Frontmatter `changelog:` trên file auto-gen/snippet (vd flows.md, uc-*.md) → migrate gom vào review-unit file cha với prefix routing.
