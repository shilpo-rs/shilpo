# Architecture Decision Records

Workspace-wide architectural decisions for Shilpo. Numbered sequentially.

ADRs here are **living reference documents, not immutable snapshots**: when an ADR's own decision changes shape (a
module moves, a placement gets renamed) the ADR is updated in place to keep describing current architecture, rather than
left to describe a superseded state. A change to the *decision itself* — not just its description — gets a new ADR that
marks the old one Superseded. When in doubt, ask whether a future reader trying to understand *why* things are the way
they are needs the old reasoning preserved (new ADR) or would just be confused by it (update in place).

New ADR: copy [`TEMPLATE.md`](TEMPLATE.md), pick the next unused number, add a row below.

## Index

| #                                                         | Title                                                                   | Status   |
|-----------------------------------------------------------|-------------------------------------------------------------------------|----------|
| [0001](0001-cross-platform-linux-split.md)                | Workspace Tiers, Publication Boundaries, and Repo Scope                 | Accepted |
| [0002](0002-theme-crate-split.md)                         | Theme Crate Split into Cross-Platform Core and Linux Daemon             | Accepted |
| [0003](0003-screen-capture-architecture.md)               | Screen Capture Crate Architecture (Screenshot-Only)                     | Accepted |
| [0004](0004-bar-widget-card-surfaces.md)                  | Bar Widget Card Surfaces                                                | Accepted |
| [0005](0005-unified-executable-process-roles.md)          | Unified Executable with Explicit Process Roles                          | Accepted |
| [0006](0006-revisioned-service-domain-ports.md)           | Revisioned Service Domain Ports and Supervision Contract                | Accepted |
| [0007](0007-transactional-layered-config.md)              | Transactional Layered Configuration with Provenance and Scoped Recovery | Accepted |
| [0008](0008-primary-only-config-migrations.md)            | Primary-Only Ordered Configuration Version Migrations                   | Accepted |
| [0009](0009-config-file-watching-and-debounced-reload.md) | Declarative Configuration File Watching and Debounced Reload            | Accepted |
| [0010](0010-comment-preserving-config-override-writes.md) | Comment-Preserving Configuration Override Writes                        | Accepted |
| [0011](0011-opt-in-process-profiling.md)                  | Opt-in Process Profiling Infrastructure                                 | Accepted |
| [0012](0012-dbus-shell-control-plane.md)                  | Standardized D-Bus Shell Control Plane                                  | Accepted |
| [0013](0013-runtime-debug-control.md)                     | Runtime Debug Control Interface                                         | Accepted |
| [0014](0014-animated-theme-transitions.md)                | Animated Material 3 Theme Transitions                                   | Accepted |
| [0015](0015-niri-opt-in-shortcut-include.md)              | Extension Keyboard Shortcuts & Opt-In Niri KDL Projection               | Accepted |
| [0016](0016-wit-extension-contract.md)                    | Canonical WIT Extension Contract and Process Execution Removal          | Accepted |
| [0017](0017-compositor-support-scope-and-backend-tiers.md) | Compositor Support Scope, Abstraction Boundary, and Backend Tiers       | Accepted |
| [0018](0018-extension-registry-distribution.md)           | Extension Registry Distribution and Publication Trust                    | Accepted |
| [0019](0019-application-locale-and-fluent.md)              | Product-Owned Application Locale and Fluent Translation                  | Accepted |

## Chains

Some ADRs build directly on an earlier one; read them in order for full context:

- **Compositor**: 0006 (domain ports) → 0015 (shortcut projection) → 0017 (backend tiers and scope)
- **Config**: 0007 (resolution) → 0008 (migrations) → 0009 (watching/reload) → 0010 (override writes)
- **Shell control plane**: 0012 (D-Bus control plane) → 0013 (debug interface added to the same bus object)
- **Theme**: 0002 (crate split) → 0014 (animated transitions, built on the split)
- **Extension host**: 0005 (process roles, defines the extension-host child) → 0016 (WIT contract for that child)
- **Extension distribution**: 0016 (WIT contract and the trusted-script exclusion) → 0018 (registry, publication trust, and installation provenance)
- **Application locale**: 0007 (transactional config) → 0009 (live config reload) → 0019 (locale resolution and translation refresh)
