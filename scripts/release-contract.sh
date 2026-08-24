#!/usr/bin/env bash
set -euo pipefail

ROOT=$(git rev-parse --show-toplevel)
SEMVER='[0-9]+\.[0-9]+\.[0-9]+([-+][0-9A-Za-z.-]+)?'

die() {
  echo "release contract: $*" >&2
  exit 1
}

require_git_cliff() {
  command -v git-cliff >/dev/null || die "git-cliff 2.13.1 is required"
  [[ $(git cliff --version) == "git-cliff 2.13.1" ]] || die "git-cliff 2.13.1 is required"
}

require_clean_tree() {
  git diff --quiet || die "tracked files have unstaged changes"
  git diff --cached --quiet || die "tracked files have staged changes"
  [[ -z $(git ls-files --others --exclude-standard) ]] || die "worktree has untracked files"
}

product_version() {
  sed -n 's/^version *= *"\([^"]*\)".*/\1/p' "$ROOT/desktop/shilpo/Cargo.toml" | head -n 1
}

validate_tag() {
  local tag=$1 version expected
  [[ $tag =~ ^shilpo-v$SEMVER$ ]] || die "tag must match shilpo-v<semver>; got $tag"
  git rev-parse --verify --quiet "refs/tags/$tag^{commit}" >/dev/null || die "tag does not exist: $tag"
  git rev-parse --verify --quiet "origin/main^{commit}" >/dev/null || die "origin/main is unavailable; fetch it before validation"
  git merge-base --is-ancestor "refs/tags/$tag" origin/main || die "tag $tag is not an ancestor of origin/main"
  version=$(product_version)
  expected="shilpo-v$version"
  [[ $tag == "$expected" ]] || die "tag $tag does not match product version $version"
}

generate_notes() {
  git cliff --config "$ROOT/cliff.toml" --unreleased --strip header
}

dry_run() {
  local first second
  require_git_cliff
  first=$(mktemp)
  second=$(mktemp)
  trap 'rm -f "$first" "$second"' RETURN
  generate_notes >"$first"
  generate_notes >"$second"
  cmp --silent "$first" "$second" || die "identical history produced different release notes"
  [[ -s $first ]] || die "git-cliff produced empty release notes"
  echo "release contract: deterministic product notes verified"
}

case ${1:-} in
  dry-run)
    dry_run
    ;;
  validate-tag)
    [[ $# -eq 2 ]] || die "usage: $0 validate-tag shilpo-vX.Y.Z"
    require_clean_tree
    validate_tag "$2"
    echo "release contract: $2 is valid"
    ;;
  *)
    die "usage: $0 {dry-run|validate-tag TAG}"
    ;;
esac
