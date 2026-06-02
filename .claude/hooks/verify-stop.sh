#!/usr/bin/env bash
# Stop hook (ai4ba) — mechanical gate for the /implement workflow.
#
# When the session ends on a `feat/*` branch AND .ts/.tsx files have changed,
# run `tsc --noEmit` + `npm run lint`. If either reports errors, BLOCK the stop
# (exit 2) and feed the errors back so Claude fixes them before finishing.
#
# Deliberately scoped:
#   - only on feat/* branches (created by /implement) — never nags on `main`
#     or doc-only sessions;
#   - only when TypeScript files actually changed;
#   - skips `npm run build` to stay fast (~seconds). Build is covered by
#     /implement Phase E, not this hook.
#   - business-correctness ("matches the brainstorm") is NOT checked here —
#     that needs reasoning and lives in /implement Phase F.
#
# Escape hatch: press ESC to interrupt, or remove the Stop hook from
# .claude/settings.json if a check is wrong.
set -uo pipefail
cat >/dev/null 2>&1 || true   # drain the hook JSON on stdin

DIR="${CLAUDE_PROJECT_DIR:-$PWD}"
cd "$DIR" 2>/dev/null || exit 0

# Scope to /implement feature branches only.
branch=$(git rev-parse --abbrev-ref HEAD 2>/dev/null || echo "")
case "$branch" in
  feat/*) ;;
  *) exit 0 ;;
esac

# Only act when TypeScript files actually changed (staged, unstaged, untracked).
git status --porcelain 2>/dev/null | grep -Eq '\.tsx?($|")' || exit 0

# Need installed deps to run the checks; skip silently on env issues so we
# never false-block on a missing toolchain.
[ -d node_modules ] || exit 0

errs=""

if [ -f tsconfig.json ] && [ -x node_modules/.bin/tsc ]; then
  if ! tsc_out=$(node_modules/.bin/tsc --noEmit 2>&1); then
    errs+=$'\n=== tsc --noEmit ===\n'"$tsc_out"
  fi
fi

if grep -Eq '"lint"[[:space:]]*:' package.json 2>/dev/null; then
  if ! lint_out=$(npm run --silent lint 2>&1); then
    errs+=$'\n=== npm run lint ===\n'"$lint_out"
  fi
fi

[ -z "$errs" ] && exit 0

{
  echo "⛔ Stop hook chặn: còn lỗi type/lint ở file .ts/.tsx vừa sửa (nhánh $branch). Fix hết rồi mới kết thúc lượt."
  printf '%s' "$errs" | tail -c 6000
} >&2
exit 2
