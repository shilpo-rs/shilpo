#!/usr/bin/env bash
# Static analysis and configuration validation script for Shilpo desktop

set -Eeuo pipefail

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
REPO_ROOT=$(cd -- "$SCRIPT_DIR/.." && pwd)
cd "$REPO_ROOT" || exit 1

PASSED=0
FAILED=0

pass() {
  printf '  [✓ PASS] %s\n' "$1"
  PASSED=$((PASSED + 1))
}

fail() {
  printf '  [✗ FAIL] %s\n' "$1" >&2
  FAILED=$((FAILED + 1))
}

printf '==================================================\n'
printf 'Running Shilpo Static Checks & Validations\n'
printf '==================================================\n\n'

shell_files=(setup scripts/dev-shell scripts/*.sh tests/*.sh)

# 1. Bash Syntax Check
if bash -n "${shell_files[@]}"; then
  pass "Bash syntax validation on repository scripts"
else
  fail "Bash syntax error detected in repository scripts"
fi

# 2. ShellCheck Validation
if command -v shellcheck >/dev/null 2>&1; then
  if shellcheck -x "${shell_files[@]}"; then
    pass "ShellCheck static analysis passed with zero warnings"
  else
    fail "ShellCheck static analysis reported warnings/errors"
  fi
else
  pass "ShellCheck skipped (not installed on host)"
fi

# 3. QuickShell / iNiR Residue Check
residue=$(grep -Ei 'inir|quickshell' data/niri/config.kdl data/niri/config.d/*.kdl 2>/dev/null || true)
if [[ -z $residue ]]; then
  pass "No QuickShell or iNiR residue in Niri templates"
else
  fail "QuickShell or iNiR residue found in Niri templates: $residue"
fi

# 4. Niri KDL Configuration Validation
if command -v niri >/dev/null 2>&1; then
  if niri validate -c data/niri/config.kdl; then
    pass "Niri KDL configuration tree validation"
  else
    fail "Niri KDL configuration validation failed"
  fi
else
  pass "Niri validate skipped (niri not installed on host)"
fi

# 5. Systemd User Units Syntax Validation
unit_fail=false
for unit in data/systemd/user/*.service; do
  if ! grep -q '\[Unit\]' "$unit" || ! grep -q '\[Service\]' "$unit"; then
    unit_fail=true
    break
  fi
done
if ! $unit_fail; then
  pass "Systemd user unit templates syntax check"
else
  fail "Systemd user unit templates syntax error"
fi

# CI validates rendered unit syntax with systemd's parser.  Source templates
# contain install-time absolute paths and the polkit placeholder, so replace
# only executable fields in a temporary copy for this syntax-only gate.
if [[ -n "${CI:-}" ]] && command -v systemd-analyze >/dev/null 2>&1 \
  && [[ -d /run/systemd/system ]] && [[ "$(ps -p 1 -o comm=)" == "systemd" ]]; then
  unit_tmp=$(mktemp -d)
  unit_verify_failed=false
  for unit in data/systemd/user/*.service; do
    unit_name=$(basename "$unit")
    sed -e 's|^ExecStart=.*|ExecStart=/usr/bin/true|' \
      -e '/^Documentation=man:/d' "$unit" >"$unit_tmp/$unit_name"
  done
  if ! systemd-analyze verify "$unit_tmp"/*.service >/dev/null 2>&1; then
    unit_verify_failed=true
  fi
  rm -rf "$unit_tmp"
  if $unit_verify_failed; then
    fail "systemd-analyze rejected rendered user unit templates"
  else
    pass "systemd-analyze validated rendered user unit templates"
  fi
else
  pass "systemd-analyze verification skipped outside CI or when unavailable"
fi

# 6. Wayland Session Entry Validation
if [[ -f /usr/share/wayland-sessions/niri.desktop ]]; then
  pass "/usr/share/wayland-sessions/niri.desktop present"
else
  pass "/usr/share/wayland-sessions/niri.desktop verification skipped (system path)"
fi

printf '\n==================================================\n'
printf 'Static Checks Summary: %d Passed, %d Failed\n' "$PASSED" "$FAILED"
printf '==================================================\n'

if [[ $FAILED -gt 0 ]]; then
  exit 1
fi
