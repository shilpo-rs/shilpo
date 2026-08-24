# Shilpo

## Install from source

Shilpo includes a small installer that builds the release binary and installs it to your `PATH`:

```bash
./setup install
shilpo setup
```

`./setup install` builds and installs the binary. `shilpo setup` is an interactive, Arch Linux-only wizard that
configures your session (compositor, GPU drivers, systemd units) and ends by offering to reboot. See
[the installation guide](docs/installation.md) for details.

A modern, high-performance desktop shell built on top of [GPUI](https://github.com/zed-industries/zed), rendered with
Material Design 3 (M3) & Material Expressive components from [shilpo-rs/ui](https://github.com/shilpo-rs/ui).

---

## Workspace Crates

| Crate                     | Description                                                     | Directory                                      |
|:--------------------------|:----------------------------------------------------------------|:-----------------------------------------------|
| **`shilpo-ext-api`**      | Cross-platform extension contract                               | [`core/ext-api`](core/ext-api)                 |
| **`shilpo`**              | Consolidated desktop product (Shell, Settings, CLI, Config)     | [`desktop/shilpo`](desktop/shilpo)             |
| **`shilpo-device`**       | Presentation-neutral device domain protocol & typed DBus client | [`desktop/device`](desktop/device)             |
| **`shilpo-services`**     | Linux system service integrations & capture domain              | [`desktop/services`](desktop/services)         |
| **`shilpo-ext-runtime`**  | Wasmtime extension runtime                                      | [`desktop/ext-runtime`](desktop/ext-runtime)   |
| **`shilpo-theme-daemon`** | Theme DBus daemon & system sync                                 | [`desktop/theme-daemon`](desktop/theme-daemon) |

The UI component library (`shilpo-m3e`), its color math (`shilpo-theme`), shared macros (`shilpo-macros`), and the
`storybook` component gallery live in [shilpo-rs/ui](https://github.com/shilpo-rs/ui), consumed here as a git
dependency pinned to an exact revision (see the root `Cargo.toml`).
See the [cross-repository pin promotion runbook](docs/cross-repository-pin-promotion.md) before promoting changes across
the UI, Shilpo, SDK, and extensions repositories.

---

## Extensions & Ecosystem

Shilpo features a sandboxed WebAssembly extension runtime and developer ecosystem. Build custom top bar widgets, dropdown menus, desktop widgets, settings panels, and command palette actions in TypeScript or Rust.

- [**Extension Documentation Hub**](docs/extensions/index.md): Complete authoring guides, API references, and security model.
- [**First-party extensions**](https://github.com/shilpo-rs/extensions): the TypeScript showcase, Rust WASI Preview 2 reference, and Trusted Local Script examples.
- [**Extension SDKs**](https://github.com/shilpo-rs/sdks): official Rust and TypeScript SDKs for authoring extensions.

---

## Developer Workflows

Shilpo uses [`just`](https://just.systems/) as a command runner for common development, formatting, linting, testing, and static analysis workflows. Development commands require `just` along with Cargo tools (`cargo-nextest` for tests and `cargo-llvm-cov` for coverage).

To discover all available recipes:

```bash
just --list
```

Common recipes:

```bash
# Format Rust files in place
just fmt

# Run Clippy lints with zero warning tolerance
just lint

# Run all workspace tests
just test

# Run tests for a specific crate
just test shilpo-services

# Run mutating formatting, linting, and workspace tests in sequence
just check
```

> **Note**: `just fmt` (and consequently `just check`) modifies Rust source files in place to enforce workspace formatting rules.

### Opt-in profiling

Profile a durable role locally by setting `SHILPO_PROFILE=1` before starting or restarting it, exercise the workload, then
stop or restart the role so its completed trace is finalized. Inspect the local inventory and export a trace with:

```bash
SHILPO_PROFILE=1 shilpo shell restart
shilpo doctor --telemetry
shilpo profile export --output trace.json
```

Open the exported JSON in Perfetto or Chrome Trace. Active `.json.part` files are incomplete; runtime rotation belongs to
the later runtime-control work.

### Shell D-Bus control

The running shell owns `org.shilpo.Shell` on the user session bus at `/org/shilpo/Shell`. Inspect the typed interfaces (`org.shilpo.Shell` and `org.shilpo.Debug`) with:

```bash
busctl --user introspect org.shilpo.Shell /org/shilpo/Shell
busctl --user call org.shilpo.Shell /org/shilpo/Shell org.shilpo.Shell GetStatus
busctl --user call org.shilpo.Shell /org/shilpo/Shell org.shilpo.Shell ToggleBar
busctl --user call org.shilpo.Shell /org/shilpo/Shell org.shilpo.Debug GetLogFilter
busctl --user call org.shilpo.Shell /org/shilpo/Shell org.shilpo.Debug SetLogFilter s "info,shilpo=debug"
busctl --user call org.shilpo.Shell /org/shilpo/Shell org.shilpo.Debug EmitTestNotification ss "Test Title" "Test Body"
```

The `shilpo shell`, workspace, window, capture, brightness, and config commands use this interface; debug operations are
available through the `busctl` calls above. No shell socket or lock file is required.

### Theme transitions

Shell color changes use a perceptual OKLCH transition by default. Configure the duration in the primary TOML file:

```toml
[theme]
transition_duration_ms = 300 # 0..=5000 milliseconds
reduced_motion = false
```

Set `transition_duration_ms = 0` or `reduced_motion = true` to apply theme changes immediately. Existing configuration
files omit the new key safely; the default remains 300 ms.

Repeated unchanged wallpaper analysis is cached in memory and invalidates on file modification or scheme-variant changes.

### Testing

Run unit, integration, and property tests across the workspace:

```bash
cargo nextest run --workspace
```

- **Hermetic D-Bus P2P Harness**: D-Bus tests use an in-memory `UnixStream::pair()` harness and never connect to a system or session D-Bus daemon.
- **Property-Based Testing**: `proptest` validates OKLCH color math, extension identity parsing/serde, and layered TOML config merge associativity.
- **Failure Reproduction**: On failure, `proptest` writes the minimized case to a `proptest-regressions/*.txt` file beside the tested source. Keep that file and rerun the same test command; the persisted case runs before newly generated cases. To replay a reported RNG seed directly, use `PROPTEST_RNG_SEED`:

```bash
PROPTEST_RNG_SEED="0123456789abcdef..." cargo test -p shilpo
```

### Benchmarking

Shilpo includes Criterion wall-clock and CodSpeed continuous benchmark suites covering extension identities, ViewTree validation, configuration resolution, and Wasm component cold loading.

Run the benchmarks locally:

```bash
# Fast smoke verification of all benchmark targets
./scripts/bench.sh smoke

# Run individual suites
./scripts/bench.sh core
./scripts/bench.sh config
./scripts/bench.sh wasm
```

See [the benchmarking documentation](docs/benchmarks.md) for measured boundaries, stable identifiers, CI architecture, and artifact retention policies.

---

## Guidelines for AI Assistants & Contributors

If you are an AI coding assistant or open-source contributor working on this repository, please consult [
`AGENTS.md`](AGENTS.md) for architecture layout, `rtk` command execution guidelines, clippy/nextest standards, and
design system rules.

---

## Acknowledgements & Prior Art

`Shilpo` started as a fork and copy of [`gpui-component`](https://github.com/longbridge/gpui-component). We extend our
deep gratitude to the original authors and maintainers of `gpui-component` for creating a fantastic foundation.

`Shilpo` has since evolved with extensive modifications, including Material Design 3 / Material Expressive design
tokens, customized layout physics, desktop notification integrations, and tailored component styling.

> **Disclaimer**: `Shilpo` is an independent open-source project and is **not affiliated with, endorsed by, or supported
by Google or the `gpui-component` maintainers in any way.**

---

## License

Licensed under the [Apache License, Version 2.0](LICENSE).
