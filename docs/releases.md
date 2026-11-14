---
title: Releases and QAROS registration
description: Release packages for the QAROS Visualizer's Sources catalog, their manifest, versions, and registering a hand-built viewer with QAROS.
sidebar_position: 10
---

# Releases and QAROS registration

The QAROS Visualizer's Sources catalog has a **Rerun** tile. It finds the viewer in one of two ways:

| Tile state | Where the viewer comes from |
| --- | --- |
| Installable | A release of this repository has a package for this platform that the installed QAROS can run |
| Installed | QAROS downloaded that package |
| Needs update | QAROS downloaded a package, and a newer compatible release exists, or its C API header hash does not match the runtime |
| Detected | You built the viewer yourself and registered it with `scripts/register-qaros-app` (below) |

## Versions

Releases carry the viewer's own [semantic version](https://semver.org/). `VERSION` at the root of the repository holds the next planned `X.Y.Z`; the full version is derived from it and the git tags:

| Built from | Version | Example |
| --- | --- | --- |
| tag `vX.Y.Z` | `X.Y.Z` | `0.1.0` |
| tag `vX.Y.Z-rc.N` | `X.Y.Z-rc.N` | `0.1.0-rc.2` |
| anything else | `X.Y.Z-dev.<commits since the last tag>+<commit>` | `0.2.0-dev.14+1a2b3c4d` |

`qaros-rerun --version` prints it; the viewer logs it at start and reports it to QAROS. `scripts/version.ps1` prints the version of the checkout. A tag whose `X.Y.Z` differs from `VERSION` fails the build and the release. The release steps are in [AGENTS.md](../AGENTS.md#releasing).

Which QAROS a release runs with is not part of its version: the manifest records it per package, as `min_qaros_version` and `c_api_header_sha256`.

## Release packages

Each tag `vX.Y.Z` or `vX.Y.Z-rc.N` runs the [Release workflow](../.github/workflows/release.yml): a release build of the viewer for `windows-x64` (Windows Server, MSVC) and `linux-x64` (Ubuntu 22.04), published as a GitHub release (a prerelease for a release candidate) with these assets:

| Asset | What it holds |
| --- | --- |
| `qaros-rerun-<version>-windows-x64.zip` | `qaros-rerun.exe`, `log-test-scene.exe`, `LICENSE`, `THIRD-PARTY-LICENSES.txt`, `README.md`, `BUILD.txt`, all at the root of the zip |
| `qaros-rerun-<version>-linux-x64.zip` | The same for Linux, executables marked executable |
| `manifest.json` | The release and, per zip, its SHA-256, size, platform, executable, viewer version and QAROS compatibility |

The packages do not contain the QAROS runtime: the viewer loads the QAROS installed on the PC. The Windows viewer needs the Microsoft Visual C++ 2015-2022 Redistributable (x64), which the package does not carry: the QAROS setup installs it system wide from QAROS 0.10.0, and a PC without QAROS installs it from Microsoft (`winget install Microsoft.VCRedist.2015+.x64`).

The packages are built with the `dist` profile (`Cargo.toml`): the release profile with thin LTO and no symbols in the executables. On Windows, `.cargo/dist-windows.toml` writes line tables to `.pdb` files, which stay out of the zip; each run uploads them as the workflow artifact `qaros-rerun-<version>-windows-x64-pdb`, kept for 90 days, for symbolicating crash reports.

A pull request into `main` builds and packages every platform as workflow artifacts, with the version `X.Y.Z-pr.<number>`, and publishes nothing. `workflow_dispatch` with a tag rebuilds that tag and replaces its assets; without a tag it builds the selected branch with its dev version and keeps the packages as workflow artifacts only.

### manifest.json

The field names are those of the qaros-ros-rviz release manifest.

```json
{
  "version": 1,
  "release": "0.1.0",
  "package": "qaros-rerun",
  "assets": [
    {
      "name": "qaros-rerun-0.1.0-linux-x64.zip",
      "sha256": "<64 hex digits>",
      "size": 31457280,
      "platform": "linux-x64",
      "executable": "qaros-rerun",
      "app_version": "0.1.0",
      "min_qaros_version": "0.9.0",
      "c_api_header_sha256": "<64 hex digits>"
    }
  ]
}
```

| Field | Meaning |
| --- | --- |
| `version` | Format version of the manifest, 1 |
| `release` | The release version, the tag without `v` |
| `assets[].name`, `sha256`, `size` | The zip, its SHA-256 (lower-case hex) and its size in bytes |
| `assets[].platform` | `windows-x64` or `linux-x64` |
| `assets[].executable` | The viewer's path inside the zip |
| `assets[].app_version` | The version the viewer in the zip reports |
| `assets[].min_qaros_version` | The QAROS version of the C API header the viewer is built against: `VERSION` of the qaros revision pinned in `Cargo.toml` |
| `assets[].c_api_header_sha256` | SHA-256 of `qar-streaming-c/include/qar_streaming.h` at that revision, lower-case hex (CRLF line endings read as LF) |

### Package locally

After a release build, on the platform being packaged:

```powershell
cargo build --profile dist -p qaros-rerun --config .cargo/dist-windows.toml    # Windows
cargo build --profile dist -p qaros-rerun                                      # Linux
.\scripts\package-release.ps1 -OutDir out/release
```

The version defaults to the derived one, the binaries to `target/dist` (`-CargoProfile` and `-TargetDir` choose others). On Linux, run the same script with PowerShell 7 (`pwsh`). A `manifest.json` already in the output folder for the same release gets this platform's entry added; `-MergeManifests a.json, b.json` merges the manifests of several platforms, as the workflow does.

`THIRD-PARTY-LICENSES.txt` lists every crate compiled into the two programs for the target, with its declared license, and the license texts the crates ship, each text once. Rerun publishes its crates without license files, so their entry carries the Rerun repository's license files from `patches/re_renderer`; a crate from a git repository gets the license at that repository's root, and an Apache-2.0 crate without a license file the standard Apache-2.0 text.

## Register a hand-built viewer

After building the viewer yourself, register it so the Visualizer's Rerun tile shows it as Detected and launches it:

```powershell
cargo build --release -p qaros-rerun
.\scripts\register-qaros-app.ps1                       # Windows
```

```bash
cargo build --release -p qaros-rerun
scripts/register-qaros-app.sh                          # Linux, needs python3
```

- The script checks that the executable exists, takes the derived version, computes the C API header hash from the pinned qaros revision (through `cargo metadata`), and writes one record to the per-user registry. No admin rights.
- Options: `-Executable` / `--executable` for a viewer elsewhere, `-AppVersion`, `-CapiHeaderSha256`, `-Platform`.
- `-Unregister` / `--unregister` removes the record again.
- Running it again replaces the record; other apps' records and QAROS's own (downloaded) records stay.

The registry is `apps.json` in the QAROS user config folder: `%APPDATA%\Quaternar\apps.json` on Windows, `$XDG_CONFIG_HOME/quaternar/apps.json` (default `~/.config/quaternar/apps.json`) on Linux, next to qaros-ros-rviz's `ros-plugins.json` and with the same field names. It is written to a temporary file and renamed over the old one, so a reader never sees half a file.

```json
{
  "version": 1,
  "apps": [
    {
      "app": "qaros-rerun",
      "platform": "windows-x64",
      "source": "local",
      "executable": "C:\\src\\qaros-rerun\\target\\release\\qaros-rerun.exe",
      "app_version": "0.1.0-dev.42+1a2b3c4d",
      "c_api_header_sha256": "<64 hex digits>",
      "registered_at": "2026-11-14T10:00:00Z"
    }
  ]
}
```

A `local` record wins over a downloaded (`prebuilt`) one for the same app and platform: the developer's own build is what they mean to run.
