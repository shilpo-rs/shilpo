#!/usr/bin/env bash
set -euo pipefail

ROOT=$(git rev-parse --show-toplevel)
CHECK_TITLE="$ROOT/.github/scripts/check-pr-title.sh"

"$CHECK_TITLE" "feat(shell): add release surface"
"$CHECK_TITLE" "fix!: preserve a breaking change marker"
"$CHECK_TITLE" "docs(release/contracts): clarify tags"

if "$CHECK_TITLE" "Add release surface" >/dev/null 2>&1; then
  echo "non-conventional PR title was accepted" >&2
  exit 1
fi

if "$CHECK_TITLE" "Feat: uppercase types are not canonical" >/dev/null 2>&1; then
  echo "uppercase Conventional Commit type was accepted" >&2
  exit 1
fi

echo "release contract tests passed"

