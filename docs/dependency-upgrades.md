# Dependency upgrade notes

This file records dependency upgrades that can require maintainer attention beyond refreshing `Cargo.lock`. Registry
dependencies in manifests use major-only requirements for stable `1.x` and later releases, and `0.x` requirements retain
the compatible minor line. Git dependencies are intentionally excluded from dependency refreshes.

Exact Git revisions shared across the UI, Shilpo, SDK, and extensions repositories follow the
[cross-repository pin promotion runbook](cross-repository-pin-promotion.md).

## Current major-line migrations

`fake`, `rand`, and `syn` upgrade notes (storybook, and the shared proc-macro crate) moved to
[shilpo-rs/ui](https://github.com/shilpo-rs/ui) along with the UI component library — see that repo for those.

`notify` 9 is currently prerelease-only, so the workspace remains on the stable 8.x line until a stable 9.x release is
available. The desktop watchers use
`RecommendedWatcher`, `Event`, `Config`, and `RecursiveMode`; update the watchers in `desktop/shilpo` and
`desktop/services` when that stable migration becomes appropriate.

`ddc` remains on the 0.2 line because `ddc-i2c` 0.2.2 implements its device traits against `ddc` 0.2. Upgrading the
workspace dependency independently to 0.3 creates two incompatible trait versions and breaks the brightness adapter.
Upgrade both crates together once `ddc-i2c` publishes support for `ddc` 0.3.
