#!/usr/bin/env bash
set -Eeuo pipefail

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
REPO_ROOT=$(cd -- "$SCRIPT_DIR/.." && pwd)
TEST_DIR=$(mktemp -d)
trap 'rm -rf -- "$TEST_DIR"' EXIT

fail() {
  printf 'FAIL: %s\n' "$1" >&2
  exit 1
}

help_output=$("$REPO_ROOT/setup" --help)
[[ $help_output == *"Shilpo Build & Install"* ]] || fail "help summary is missing"
[[ $help_output == *"./setup install [--prefix DIR]"* ]] || fail "install usage is missing"
[[ $help_output == *"./setup uninstall [--prefix DIR]"* ]] || fail "uninstall usage is missing"

if "$REPO_ROOT/setup" install --dry-run >/dev/null 2>&1; then
  fail "unsupported install options must be rejected"
fi

fixture_repo="$TEST_DIR/repository"
fixture_bin="$TEST_DIR/mock-bin"
prefix="$TEST_DIR/prefix"
mkdir -p "$fixture_repo" "$fixture_bin"
cp "$REPO_ROOT/setup" "$fixture_repo/setup"

cat >"$fixture_bin/cargo" <<'EOF'
#!/usr/bin/env bash
[[ $* == "build --locked --release -p shilpo" ]] || exit 64
mkdir -p target/release
printf '#!/usr/bin/env bash\necho shilpo fixture\n' >target/release/shilpo
chmod +x target/release/shilpo
EOF
chmod +x "$fixture_bin/cargo"

install_output=$(PATH="$fixture_bin:$PATH" "$fixture_repo/setup" install --prefix "$prefix")
[[ -x $prefix/bin/shilpo ]] || fail "install did not create an executable shilpo binary"
[[ $install_output == *"Installed shilpo to $prefix/bin/shilpo"* ]] || fail "install path was not reported"

uninstall_output=$(PATH="$fixture_bin:$PATH" "$fixture_repo/setup" uninstall --prefix "$prefix")
[[ ! -e $prefix/bin/shilpo ]] || fail "uninstall did not remove the shilpo binary"
[[ $uninstall_output == *"Removed $prefix/bin/shilpo"* ]] || fail "removal path was not reported"

printf 'Installer integration checks passed.\n'
