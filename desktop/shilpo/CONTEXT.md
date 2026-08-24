# Shilpo Desktop Product Context (`shilpo`)

Consolidated desktop product package. Produces the single installed executable binary target (`shilpo`).

## Internal Submodules

- `shell`: Shell daemon — top bar, workspace overview, notifications, OSD, extension surfaces, action dispatcher,
  animated theme transitions, and `org.shilpo.Shell` / `org.shilpo.Debug` D-Bus control plane
  (see [ADR-0012](../../docs/adr/0012-dbus-shell-control-plane.md), [ADR-0013](../../docs/adr/0013-runtime-debug-control.md), [ADR-0014](../../docs/adr/0014-animated-theme-transitions.md)).
- `settings`: Standalone control panel application for Shilpo configuration and system settings.
- `cli`: Command-line interface dispatcher for subcommands (`shilpo daemon`, `shilpo settings`, `shilpo config`,
  `shilpo theme`, `shilpo doctor`, `shilpo ext`, etc.).
- `config`: TOML configuration loading, schema validation, default resolution, per-output overrides.
- `locale`: First-party application locale resolution, translation, and live refresh.

## Language

**Application Locale**:
The supported BCP 47 language and regional convention selected for Shilpo-owned text in one process.
_Avoid_: System locale, UI locale

**Locale Intent**:
The configured or process-environment locale requested before support and fallback rules are applied.
_Avoid_: Selected locale, active locale

**Resolved Locale**:
The supported Application Locale produced from Locale Intent by exact, language, and default fallback.
_Avoid_: Raw locale, configured locale
