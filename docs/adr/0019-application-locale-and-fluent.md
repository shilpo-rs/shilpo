# ADR-0019: Product-Owned Application Locale and Fluent Translation

- **Status**: Accepted
- **Issue**: [#326](https://github.com/shilpo-rs/shilpo/issues/326)

## Context

Shilpo configuration already accepts locale intent and publishes locale changes through the transactional reload path,
while `shilpo-m3e` has a separate selector for its generic component catalogue. Putting application translation in the
publishable Core Tier, adopting M3's selector as the product authority, or adding another process-global translation
macro would split ownership and make live refresh ordering implicit.

## Decision

`desktop/shilpo` owns the Application Locale for each Shell or Settings process. Its locale module presents one deep
interface for resolution, Fluent translation, concurrent reads, and observable refresh. Declarative config is intent,
not another locale authority; the process environment is captured at startup rather than read during translation.

Locale intent precedence is explicit config, `LC_ALL`, `LC_MESSAGES`, `LANG`, then `en-US`. POSIX suffixes and
underscores are normalized to BCP 47. Resolution selects an exact supported locale, then a supported locale with the
same language, then `en-US`. Invalid and unsupported intent therefore resolves deterministically to `en-US`.

Project Fluent catalogs are embedded with mandatory `en-US` coverage and partial locale fallback. `bn-BD` is the pilot
non-English catalog. Callers pass product-owned named argument values through the locale interface rather than
depending on Fluent engine types. Missing, malformed, valueless, and formatting failures remain explicit errors; user,
media, application, and extension-provided text is never treated as a translation key.

Committed config updates replace the immutable translator snapshot atomically, publish one monotonically revisioned
locale update, and refresh open GPUI windows only when the effective Resolved Locale changes. Shell and Settings both
consume the existing ADR-0009 config publication seam. A narrow M3 adapter then projects the Resolved Locale into
`shilpo-m3e::set_locale`; M3 continues to own only its generic component catalogs and is not a second product locale
authority.

Extension localization, extension ABI changes, catalog migration of every existing string, locale-aware RTL layout,
and translation of externally supplied text are outside this decision.

## Consequences

- Shell and Settings share deterministic locale behavior without introducing a publishable locale crate or a third
  global selector.
- Translation reads are concurrent and do not observe partially refreshed catalogs.
- Generic component translations remain reusable in the UI repository, while Shilpo-specific wording stays with the
  desktop product.
- Right-to-left layout and extension localization require later explicit decisions rather than growing this interface.
