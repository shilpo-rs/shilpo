# Release contract

The repositories in the `shilpo-rs` organization release independently. A tag names one release line and never implies
that another repository was released from the same commit or at the same version.

| Repository | Release unit | Tag namespace | Version source |
| --- | --- | --- | --- |
| `shilpo` | Linux desktop product | `shilpo-vX.Y.Z` | `desktop/shilpo/Cargo.toml` |
| `ui` | synchronized `shilpo-m3e`, `shilpo-theme`, and `shilpo-macros` trio | `ui-vX.Y.Z` | the three crate manifests |
| `sdks` | Rust SDK | `rust-sdk-vX.Y.Z` | `rust/Cargo.toml` |
| `sdks` | TypeScript SDK | `typescript-sdk-vX.Y.Z` | `typescript/deno.json` |
| `extensions` | one extension | `<extension-id>-vX.Y.Z` | that extension's `extension.toml` |

Existing migrated tags remain untouched. In particular, a historical tag in the wrong repository is not evidence of a
release line and must not be renamed or deleted.

## Changelog policy

Pull-request titles, rather than every local commit, follow Conventional Commits. Squash merges therefore put one
release-note-worthy subject on `main`. Accepted types are `feat`, `fix`, `perf`, `refactor`, `docs`, `test`, `build`,
`ci`, `chore`, `style`, and `revert`, with an optional lowercase scope and optional `!` before the colon.

`git-cliff` 2.13.1 and the checked-in `cliff.toml` are the reproducible generator contract. Unconventional historical
commits are retained under **Other changes**, so migration history is not silently discarded. `CHANGELOG.md` records
the policy; release automation should generate the final GitHub release notes from the tagged history rather than
editing the file by hand.

Run `scripts/release-contract.sh dry-run` to generate notes twice and verify byte-for-byte determinism.

## Tag preflight

Before creating or publishing a product tag:

1. update `desktop/shilpo/Cargo.toml` to the intended semantic version and merge it to `main`;
2. start from a clean worktree, including no untracked files;
3. create `shilpo-vX.Y.Z` on the release commit;
4. check out that tag and run `scripts/release-contract.sh validate-tag shilpo-vX.Y.Z`;
5. publish only if the tag exists, is an ancestor of the fetched `origin/main`, and exactly matches the manifest version.

The check is intentionally validation-only: it never creates, moves, or deletes tags.
