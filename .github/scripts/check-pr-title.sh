#!/usr/bin/env bash
set -euo pipefail

title=${1:-${PR_TITLE:-}}
pattern='^(feat|fix|perf|refactor|docs|test|build|ci|chore|style|revert)(\([a-z0-9][a-z0-9._/-]*\))?(!)?: .+'

if [[ ! $title =~ $pattern ]]; then
  echo "PR title must use Conventional Commit style (for example: feat(shell): add overview search)" >&2
  exit 1
fi

