# Context Map

Shilpo is a Linux desktop environment ecosystem built on GPUI, rendered with UI components from a separate
[shilpo-rs/ui](https://github.com/shilpo-rs/ui) repository.

## Contexts

### Cross-Platform (`core/`)

- **Assets** (`core/assets/icons`) — Plain canonical SVG icon data. Applications own their GPUI `AssetSource`; no asset
  Cargo package or default runtime loader is published.
- **Extension API** (`core/ext-api`) — Extension identity types (`ExtensionId`, `ContributionId`, `CanonicalId`,
  `IdError`), extension manifest schemas, events, guest host effects, ViewTree declarative UI family, API/schema
  constants, and WIT interface contract (`wit/extension.wit`). Cross-platform, zero runtime/Wasmtime dependencies.

### UI

The UI component library (`shilpo-m3e`, Material Design 3 Expressive), its color math (`shilpo-theme`), shared
procedural macros (`shilpo-macros`), and the `storybook` component gallery live in a separate repository,
[shilpo-rs/ui](https://github.com/shilpo-rs/ui), not in this one. `shilpo-m3e` owns the GPUI `Theme` global,
`ActiveTheme` trait, and `ThemeColor` token system; `shilpo-theme` provides the underlying M3 color math (seed color →
scheme generation via `mcu_material_color`) that `shilpo-m3e` re-exports. Consumed here as a git dependency pinned to
an exact revision of that repository (see the root `Cargo.toml`), not a local workspace member.

### Linux Desktop (`desktop/`)

- **Shilpo** (`desktop/shilpo`) — Consolidated desktop product package. Contains Shell daemon (`shell`), Settings app
  (`settings`), public CLI dispatch (`cli`), declarative TOML configuration/validation (`config`), and the product-owned
  application locale module (`locale`). Exposes the
  `org.shilpo.Shell` and `org.shilpo.Debug` D-Bus control plane
  ([ADR-0012](docs/adr/0012-dbus-shell-control-plane.md), [ADR-0013](docs/adr/0013-runtime-debug-control.md)). Produces
  the single installed executable binary target (`shilpo`).
- **Device** (`desktop/device`) — Presentation-neutral versioned device domain protocol (`protocol`) and typed DBus
  client (`client`) with degraded/reconnect projections and client-side debounce.
- **Services** (`desktop/services`) — Linux system integration services. Device daemon (`DeviceDaemonService`),
  Wayland/Niri compositor IPC, audio, bluetooth, brightness, caffeine, clipboard, location, media, network, night light,
  notifications, power profile, screen capture domain (`capture`), tray, upower, app scanning, and LMDB session store.
- **Extension Runtime** (`desktop/ext-runtime`) — Wasmtime-sandboxed extension runtime. Capability authorization,
  package catalog/registry index, WASI component-model host, worker process protocol (`shilpo extension-host`).
- **Theme Daemon** (`desktop/theme-daemon`) — Linux theme system integration. DBus service (`org.shilpo.Theme`), XDG
  portal appearance sync, wallpaper watching, bounded in-memory wallpaper analysis caching, atomic JSON persistence,
  theme adapters for third-party tools (GTK, Foot, Alacritty, Kitty, Hyprland).
- **Observability** (`desktop/observability`) — Internal process observability crate. Standardized subscriber
  initialization with reloadable filter controller (`LogFilterController`), opt-in Chrome trace generation
  (`SHILPO_PROFILE`), collision-resistant trace path management, trace discovery/export (`shilpo profile export`), and
  local telemetry summary inventory (`shilpo doctor --telemetry`).

### SDKs

The official extension SDKs (TypeScript's `@shilpo/ext-sdk`, Rust's `shilpo-ext-sdk`, and future
languages) live in a separate repository,
[shilpo-rs/sdks](https://github.com/shilpo-rs/sdks), not in this one. Both SDKs target the
`shilpo:extension` WIT contract defined here in `core/ext-api`, pinned to an exact revision of this
repository (see that repo's `WIT_REV`) rather than a live path dependency, since neither SDK depends
on the extension runtime that also consumes `core/ext-api` — see "Relationships" below.

## Relationships

- **Shilpo → shilpo-m3e**: Shilpo renders Shell and Settings UI using M3 components from
  [shilpo-rs/ui](https://github.com/shilpo-rs/ui), and projects its resolved application locale into M3's generic
  component catalogue.
- **Shilpo → Theme Daemon**: Shilpo subscribes to theme daemon for system-wide theme synchronization and runs the theme
  daemon role with narrow options.
- **Shilpo → Services**: Shilpo wires system service data into presentational widgets via service worker channels.
- **Shilpo → Device**: Shilpo submits live device commands and observes per-domain daemon revisions through the typed
  client seam.
- **Services → Device**: Services hosts the device daemon (`DeviceDaemonService`), which owns command arbitration,
  per-domain queues, and Linux service adapters.
- **Shilpo → Ext Runtime**: Shilpo hosts and coordinates extensions via supervisor worker process
  (`shilpo extension-host`).
- **Ext Runtime → Ext API**: Ext Runtime implements guest contracts and capability authorization for Extension API
  types.
- **Services → LMDB Session Store**: Services owns operational/session persistence (clipboard history, output state)
  independently of Shilpo declarative config.
- **Theme Daemon → shilpo-m3e**: Daemon uses `shilpo-m3e`'s theme types (re-exported from `shilpo-theme`) and color
  generation.
- **SDKs → Ext API**: Both official SDKs, in [shilpo-rs/sdks](https://github.com/shilpo-rs/sdks), generate typed
  interfaces from `core/ext-api/wit/extension.wit` and manifest schema from
  `core/ext-api/schema/extension-v1.schema.json`, fetched at a pinned revision rather than a live path dependency.
- **Services Domain Ports**: Long-lived service domains use domain-specific ports with the shared ADR-0006 operational
  semantics; process-owned ports use typed DBus clients and in-process ports use narrow handles.
