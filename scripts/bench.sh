#!/usr/bin/env bash
# Shilpo Benchmark Runner
# Standard developer and CI entry point for running Criterion / CodSpeed benchmarks.
#
# Usage:
#   ./scripts/bench.sh [suite] [extra cargo bench arguments...]
#
# Suites:
#   core    - Extension identity and ViewTree validation benchmarks
#   config  - Configuration deserialization, validation, and layered resolution benchmarks
#   wasm    - Wasm extension cold load and instantiation benchmarks
#   all     - Runs all benchmark suites (core, config, wasm)
#   smoke   - Fast compilation and execution check of all benchmark targets without statistical sampling
#   --help  - Display this help message

set -Eeuo pipefail

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
REPO_ROOT=$(cd -- "$SCRIPT_DIR/.." && pwd)
cd "$REPO_ROOT" || exit 1

# shellcheck disable=SC2206
CARGO_CMD=(${CARGO:-cargo})
SUITE="${1:-smoke}"
shift || true

usage() {
  cat <<'EOF'
Shilpo Benchmark Suite Runner

Usage:
  ./scripts/bench.sh [suite] [extra cargo args...]

Suites:
  core    Extension identity and ViewTree validation
  config  Configuration deserialization, validation, and layered resolution
  wasm    Wasm extension cold load and instantiation
  all     Run all suites (core, config, wasm)
  smoke   Quick execution check of all benchmark targets (default)
  --help  Show this help message

Examples:
  ./scripts/bench.sh smoke
  ./scripts/bench.sh core
  ./scripts/bench.sh config
  ./scripts/bench.sh wasm
  ./scripts/bench.sh all
  ./scripts/bench.sh core -- --quick
EOF
}

case "$SUITE" in
  --help|-h|help)
    usage
    exit 0
    ;;
  core)
    printf "=== Running Core Benchmarks (identity, view_tree) ===\n"
    "${CARGO_CMD[@]}" bench -p shilpo-ext-api --bench identity --bench view_tree "$@"
    ;;
  config)
    printf "=== Running Configuration Benchmarks (config) ===\n"
    "${CARGO_CMD[@]}" bench -p shilpo --bench config "$@"
    ;;
  wasm)
    printf "=== Running Wasm Cold Load Benchmarks (wasm) ===\n"
    "${CARGO_CMD[@]}" bench -p shilpo-ext-runtime --features sdk-fixture --bench wasm "$@"
    ;;
  all)
    printf "=== Running All Shilpo Benchmarks ===\n"
    "${CARGO_CMD[@]}" bench -p shilpo-ext-api --bench identity --bench view_tree "$@"
    "${CARGO_CMD[@]}" bench -p shilpo --bench config "$@"
    "${CARGO_CMD[@]}" bench -p shilpo-ext-runtime --features sdk-fixture --bench wasm "$@"
    ;;
  smoke)
    printf "=== Running Benchmark Smoke Checks (single-pass execution) ===\n"
    "${CARGO_CMD[@]}" bench -p shilpo-ext-api --bench identity --bench view_tree -- --test "$@"
    "${CARGO_CMD[@]}" bench -p shilpo --bench config -- --test "$@"
    "${CARGO_CMD[@]}" bench -p shilpo-ext-runtime --features sdk-fixture --bench wasm -- --test "$@"
    printf "=== All benchmark targets verified successfully ===\n"
    ;;
  *)
    printf "Error: Unknown benchmark suite '%s'\n\n" "$SUITE" >&2
    usage >&2
    exit 1
    ;;
esac
