# Working in qaros-rerun

Rules for everyone changing this repository, people and coding agents alike. What the project is
and how it is built: [README.md](README.md); how a release is packaged:
[docs/releases.md](docs/releases.md).

## Build and test

- `cargo build --release -p qaros-rerun` builds the viewer; `cargo test --workspace` and
  `cargo clippy --workspace --all-targets -- -D warnings` must pass, as in CI.
- `cargo fmt --all` before committing.

## This repository is public

- Never reference internal documents or trackers: no internal architecture decision records, no
  links to private issues or repositories. The records in `docs/decisions/` are public and may be
  cited.
- No AI attribution anywhere: not in commits (no `Co-Authored-By` lines for tools, no "Generated
  with"), not in pull requests, code, comments or docs.
- Commits: a Conventional Commits subject (`feat:`, `fix:`, `docs:`, `ci:`, `refactor:`,
  `test:`, ...), then a body of bullet points.

## Changelog

[CHANGELOG.md](CHANGELOG.md) follows Keep a Changelog. `## [Unreleased]` is on top; under each
version the categories come in this order, and only those with entries appear:

`Breaking`, `Features`, `Enhancements`, `Bug fixes`, `Deprecated`, `Removed`, `Security`.

- Every change a user can notice (a flag, a default, behaviour in the glasses or on the desktop,
  a fixed bug, a release asset, a user-facing doc) adds **one line** under `Unreleased`, in the
  right category, **in the same commit** as the change.
- The line says what changed for the user, not how it was done. Link the related public issue or
  pull request with its full URL (`https://github.com/Quaternar/qaros-rerun/pull/12`) when there
  is one.
- No entry for refactors, tests, CI or other changes a user cannot notice.
- A `Breaking` entry says what breaks and the migration step.

## Versions

`VERSION` at the root holds the next planned `X.Y.Z`, the only typed copy; the
`[workspace.package]` version in `Cargo.toml` must equal it (CI checks it with
`scripts/version.ps1 -Check`). The full version is derived, never typed:

| HEAD | Version |
| --- | --- |
| tagged `vX.Y.Z` | `X.Y.Z` |
| tagged `vX.Y.Z-rc.N` | `X.Y.Z-rc.N` |
| anything else | `X.Y.Z-dev.<commits since the last v* tag>+<commit>` |

The build derives it (`crates/build_version.rs`): `qaros-rerun --version` prints it, the viewer
logs it at start and reports it to QAROS as its app version. `scripts/version.ps1` prints the same.
A tag whose `X.Y.Z` differs from `VERSION` fails the build and the release.

The version is the viewer's own and follows Semantic Versioning:

- `MINOR`: features. It tries hard not to break users; when it must, the change is under
  `Breaking` with its migration step, and a removal was deprecated at least one minor earlier.
- `PATCH`: fixes only.
- Only the latest version is supported; tags go on `main`.

The QAROS version a release needs is not in its version; the release manifest records it per
asset (`min_qaros_version`, `c_api_header_sha256`).

## Releasing

1. **Release candidate** (optional): on the commit to test, `git tag vX.Y.Z-rc.N` and push the
   tag. The Release workflow builds and publishes it as a prerelease.
2. **Release.** In one commit, move the `Unreleased` entries under `## [X.Y.Z] - YYYY-MM-DD`,
   leave an empty `## [Unreleased]` on top and update the compare links at the bottom. Nothing
   else changes in that commit.
3. `git tag vX.Y.Z` on that commit and push the tag. The Release workflow checks the tag against
   `VERSION`, builds Windows x64 and Linux x64, and publishes the zips and `manifest.json`.
4. **Bump.** In the next commit, never the tagged one, set `VERSION` and the `Cargo.toml`
   workspace version to the next planned version (`X.Y+1.0`) and run `cargo check` so
   `Cargo.lock` follows.
