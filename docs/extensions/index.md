# Shilpo Extension Documentation Hub

Welcome to the official authoring and developer documentation for the **Shilpo Desktop Extension Ecosystem**.

Shilpo extensions allow developers to extend the Linux desktop environment with custom bar widgets, dropdown menus, desktop canvas widgets, settings panels, dockable side panels, search providers, command palette actions, global keyboard shortcuts, background tasks, and dynamic wallpapers.

---

## Extension Execution Models

Shilpo provides two distinct extension models:

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                          Shilpo Extension Models                            │
├──────────────────────────────────────┬──────────────────────────────────────┤
│ 1. Sandboxed WebAssembly Extensions  │ 2. Trusted Local Scripts             │
├──────────────────────────────────────┼──────────────────────────────────────┤
│ • Canonical WASI Preview 2 components│ • Local-only executable scripts      │
│ • Sandboxed inside Wasmtime          │ • Unsandboxed (runs as local user)   │
│ • Strict least-privilege capability  │ • Outside WASM capability model      │
│   declaration and prompt consent     │                                      │
│ • Full declarative ViewTree UI,      │ • Read-only polling/streaming bar    │
│   menus, widgets, actions, & state   │   widgets only                       │
│ • Written in TypeScript or Rust      │ • Written in Bash, Python, etc.      │
└──────────────────────────────────────┴──────────────────────────────────────┘
```

---

## Documentation Roadmap

| Guide | Description |
| :--- | :--- |
| [**TypeScript Getting Started**](getting-started-typescript.md) | Step-by-step guide to scaffolding, building, checking, and packaging TypeScript extensions using `@shilpo/ext-sdk`. |
| [**Rust Getting Started**](getting-started-rust.md) | Guide to building native WASI Preview 2 component extensions in Rust using `shilpo-ext-sdk`. |
| [**Manifest Reference**](manifest-reference.md) | Complete reference for `extension.toml`, all 10 contribution families, capabilities, subscriptions, and settings schema. |
| [**Architecture & Lifecycle**](architecture-and-lifecycle.md) | In-depth exploration of extension activation, event dispatch, declarative ViewTree UI, and key-value state store. |
| [**Security & Capabilities**](security-and-capabilities.md) | Sandboxing principles, default-deny WASI policy, explicit capability scopes, and permission prompt mechanics. |
| [**Trusted Local Scripts**](trusted-local-scripts.md) | Complete reference for lightweight local polling/streaming bar widget scripts. |
| [**Testing Guide**](testing-guide.md) | How to write hermetic automated unit tests using `FakeHost` and mock test harnesses without running a live desktop shell. |
| [**Troubleshooting & Smoke Testing**](troubleshooting-and-smoke.md) | Common errors, diagnostic codes, bounded timeouts, and manual live-shell verification checklists. |
| [**Contribution Coverage Matrix**](coverage-matrix.md) | Mechanically validated matrix mapping all 10 contribution families to manifests, code, docs, and tests. |

---

## Reference Implementations

- **[`extensions/example`](https://github.com/shilpo-rs/extensions/tree/main/example)**: The single canonical TypeScript showcase demonstrating all 10 contribution families, least-privilege capabilities, and hermetic testing.
- **[`extensions/world-clock`](https://github.com/shilpo-rs/extensions/tree/main/world-clock)**: Experimental official Rust WASI Preview 2 component extension.
- **[`local-scripts/cpu-temp-script`](https://github.com/shilpo-rs/extensions/tree/main/local-scripts/cpu-temp-script)**: Reference Trusted Local Script demonstrating a polling CPU temperature bar widget.

---

## Quick CLI Reference

```bash
# Scaffold a new extension
shilpo ext new my-extension --typescript --starter bar-widget

# Build the WebAssembly component
shilpo ext build my-extension

# Lint manifest, capabilities, schemas, and assets ahead of time
shilpo ext lint my-extension

# Validate built component and runtime package
shilpo ext check my-extension

# In one terminal, explicitly allow local extension code for this daemon process
systemctl --user stop shilpo-shell.service
shilpo daemon --developer-mode

# In another terminal, start live hot-reloading development
shilpo ext dev my-extension

# Package into a distributable .shilpo-ext archive
shilpo ext pack my-extension
```

`--developer-mode` permits callers on the user session bus to ask Shilpo to load local extension code. It is denied by
default, is never persisted in configuration, and lasts only for the lifetime of the `shilpo daemon` process started
with the flag. Each accepted development session remains owned by its initiating unique D-Bus sender; other senders
cannot reload or end it. Stop the foreground daemon and restart `shilpo-shell.service` to return to normal operation.
