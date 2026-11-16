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

`VERSION` at the root holds the version of the **last release**, the only typed copy; the
`[workspace.package]` version in `Cargo.toml` and the workspace crates in `Cargo.lock` must equal
it (CI checks `Cargo.toml` with `scripts/version.ps1 -Check`). Only a release commit changes them;
never edit them by hand. The full version is derived, never typed:

| Built from | Version |
| --- | --- |
| the release commit, tagged `vX.Y.Z` | `X.Y.Z` |
| any other commit | `X.Y.(Z+1)-dev.<commits since the last v* tag>+<commit>` |
| pull request N, before it merges | `X.Y.(Z+1)-pr.N` |

The build derives it (`crates/build_version.rs`): `qaros-rerun --version` prints it, the viewer
logs it at start and reports it to QAROS as its app version. `scripts/version.ps1` prints the same.
A development build reports the next patch, so it sorts after the release it builds on and before
the next one. A tag whose `X.Y.Z` differs from `VERSION` fails the build and the release.

The version is the viewer's own and follows Semantic Versioning:

- `MINOR`: features. It tries hard not to break users; when it must, the change is under
  `Breaking` with its migration step, and a removal was deprecated at least one minor earlier.
- `PATCH`: fixes only.
- Only the latest version is supported; tags go on `main`.

The QAROS version a release needs is not in its version; the release manifest records it per
asset (`min_qaros_version`, `c_api_header_sha256`).

## Releasing

Every merge to `main` that changes behaviour is a release. Nobody tags or bumps `VERSION` by hand.
The rules are the shared
[ADR-0002: QAROS apps release on every merge that changes behaviour](https://github.com/Quaternar/qaros/blob/main/docs/adr/0002-app-release-on-merge.md).

- **The bump.** A `release:*` label on the pull request wins; without one, its title decides:

  | Label | Title (Conventional Commit) | Bump |
  | --- | --- | --- |
  | `release:major` | | major |
  | | `feat!: ...`, `fix(x)!: ...`, or `BREAKING` in the title | major (minor below 1.0.0) |
  | `release:minor` | `feat: ...` | minor |
  | `release:patch` | `fix: ...`, `perf: ...`, `revert: ...` | patch |
  | `release:none` | `docs`, `ci`, `chore`, `test`, `refactor`, `style`, `build` | none |

  `1.0.0` comes only from `release:major`. The **Release bump** check on every pull request shows
  the version merging it releases, and fails without a known title type or `release:*` label.
- **The changelog sets a floor.** A release ships every `Unreleased` entry: `Breaking` or
  `Removed` entries make it at least a breaking bump, `Features`, `Enhancements` or `Deprecated`
  at least a minor, `Bug fixes` or `Security` at least a patch.
- **CI releases.** On the push to `main` the App release workflow
  (`.github/workflows/app-release.yml`) writes the new version into the files listed in
  `.github/app-release.json`, moves the `Unreleased` entries under `## [X.Y.Z] - YYYY-MM-DD`,
  commits `chore(release): vX.Y.Z`, tags it `vX.Y.Z` and starts the Release workflow for the tag.
  That builds Windows x64 and Linux x64 and publishes the zips and `manifest.json`.
- **No release candidates.** Try a change from its pull request build (`X.Y.(Z+1)-pr.N`) before
  it merges.
- **Merging without releasing.** Label the pull request `release:none`. The next releasing merge
  ships its changes, its bump raised by their changelog entries.
- **A failed release build.** Fix it on `main`; if the fix was in CI only, rerun the Release
  workflow by hand with the tag. A tag never moves; a broken release is fixed by the next one.
