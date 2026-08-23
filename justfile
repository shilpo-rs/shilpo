set shell := ["bash", "-euo", "pipefail", "-c"]
set export := true

root := justfile_directory()

# Format all Rust source files in place
fmt:
    cd "{{root}}" && cargo fmt --all

# Verify Rust formatting without modifying the worktree
fmt-check:
    cd "{{root}}" && cargo fmt --all -- --check

# Run Clippy lints across the workspace treating warnings as errors
lint:
    cd "{{root}}" && cargo clippy --workspace --all-targets -- -D warnings

# Run workspace tests or tests for a single package
test package="__JUST_WORKSPACE_TEST__":
    cd "{{root}}" && case {{ quote(package) }} in '__JUST_WORKSPACE_TEST__') cargo nextest run --workspace ;; '') echo 'usage: just test [non-empty-crate]' >&2; exit 2 ;; *) cargo nextest run -p {{ quote(package) }} ;; esac

# Run code coverage summary across the workspace
coverage:
    cd "{{root}}" && cargo llvm-cov --workspace --summary-only

# Run shell and configuration static checks
static:
    cd "{{root}}" && bash scripts/static_checks.sh

# Verify workspace topology and installer behavior
topology:
    cd "{{root}}" && bash tests/test_workspace_topology.sh

installer-test:
    cd "{{root}}" && bash tests/test_installer.sh

# Check dependency bans, licenses, and sources
deny:
    cd "{{root}}" && cargo deny check bans licenses sources

# Run a benchmark suite (defaults to the fast smoke suite)
bench suite="smoke":
    cd "{{root}}" && ./scripts/bench.sh {{ quote(suite) }}

# Reproduce the pull-request quality gate locally without mutating source files
ci: fmt-check lint test deny static topology installer-test

# Fast local Rust checks
check: fmt-check lint test
