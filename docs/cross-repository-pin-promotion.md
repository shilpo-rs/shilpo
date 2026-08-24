# Cross-repository pin promotion

Shilpo's split repositories use exact Git revisions without sharing one release train. Promote a
change only along the edges it affects:

```text
shilpo-rs/ui
  theme ─┐
  macros ├─> m3e ─> shilpo Cargo.toml UI rev
         ┘

shilpo-rs/shilpo
  core/ext-api ─> core/registry-contract ─> extensions generator core rev
       └────────> sdks WIT_REV ─> extensions SDK rev ─> affected extensions
```

An arrow means "validate and update this consumer if the upstream interface it uses changed". It
does not mean every upstream merge forces a downstream PR or release.

## Promotion order

1. **UI:** land and test `shilpo-theme` or `shilpo-macros` first, then test and release
   `shilpo-m3e` against those exact sources. Only bump the `shilpo-m3e` Git `rev` in Shilpo when
   Shilpo needs that UI commit. Theme, macro, and M3E changes share one UI repository commit, but
   this build order keeps M3E's public surface downstream of its inputs.
2. **Core contracts:** land `core/ext-api` WIT/schema changes in Shilpo before adapting
   `core/registry-contract`. When the extensions index generator needs either contract, update both
   of its Shilpo dependencies to the same compatible Shilpo revision and refresh its lockfile.
3. **SDK contract:** after the canonical WIT/schema commit exists in Shilpo, bump the SDK repo's
   `WIT_REV`, regenerate both language SDK outputs, and test them together. Capture the resulting
   SDK commit only after both SDKs agree with that contract.
4. **Extensions:** bump the extensions workspace's `shilpo-ext-sdk` revision only for source that
   needs the new SDK. Bump the generator's Shilpo revision only when its core contracts changed.
   Rebuild and release only extension packages whose source, generated bindings, SDK surface, or
   declared contract changed.

If a pin and the contract or API it selects are unchanged, no downstream release is required. A
documentation-only upstream change, unrelated crate change, or generator-only change does not
create a new extension package release by itself.

## Verification runbook

Check out the four repositories side by side, then run the verifier owned by Shilpo:

```bash
python3 scripts/verify_cross_repo_pins.py \
  --repo shilpo-rs/ui=../ui \
  --repo shilpo-rs/shilpo=. \
  --repo shilpo-rs/sdks=../sdks \
  --repo shilpo-rs/extensions=../extensions
```

The verifier discovers exact `git` plus `rev` pairs in every `Cargo.toml` and the SDK repository's
`WIT_REV`; lockfiles are used only to expand configured short Cargo revisions to their full commit.
For every configuration it prints the consuming repository, the repository that owns the pinned
commit, the configured revision, the resolved commit, and the source location.

Verification fetches each unique revision into a temporary bare repository. It does not checkout,
pull, write a lockfile, update a pin, or modify any supplied repository. It needs Python 3.11 or
newer, Git, and read access to the configured remotes. Any missing revision produces `MISSING` and
a non-zero exit status.

Before merging a promotion PR:

1. Run the verifier and retain its output in the PR checks or description.
2. Review each changed pin against the DAG above; remove unrelated downstream bumps.
3. Run the owning repository's normal generated-file, lockfile, build, and test checks.
4. Merge upstream PRs before their consumer PRs. Tag or publish only the artifacts that changed.

