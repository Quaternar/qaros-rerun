---
title: Installation
description: Build qaros-rerun from source on Windows (or a Mac for development) and run it once without a headset.
sidebar_position: 1
---

# Installation

How to build `qaros-rerun` from source and run it once, before any headset is involved. Streaming to headsets through QAROS is covered by the [integration guide](integration-guide.md).

## Prerequisites

| | Windows (the Hub PC) | Mac (development only) |
| --- | --- | --- |
| C and C++ build tools | Visual Studio Build Tools 2022 with the "Desktop development with C++" workload | Xcode Command Line Tools (`xcode-select --install`) |
| Rust | [rustup](https://rustup.rs) | [rustup](https://rustup.rs) |
| Git | [Git for Windows](https://git-scm.com/download/win) | included with the Command Line Tools |
| Demos | [uv](https://docs.astral.sh/uv/) | [uv](https://docs.astral.sh/uv/) |
| Streaming to headsets | QAROS 0.9 and a GPU with a Vulkan driver | not supported ([Developing on a Mac](mac.md)) |

On Windows the build tools, rustup, Git and uv can be installed with `winget`:

```powershell
winget install Microsoft.VisualStudio.2022.BuildTools --override "--wait --passive --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"
winget install Rustlang.Rustup
winget install Git.Git
winget install astral-sh.uv
```

`rust-toolchain.toml` pins Rust 1.96 (the minimum for Rerun 0.40.0) with clippy, rustfmt and the Windows target. rustup fetches it on the first `cargo` command run inside the repository; run `cargo` from inside the repository, since elsewhere rustup picks your default toolchain.

The first build needs network access: Cargo fetches crates.io dependencies and the `qaros` binding, `qaros-stub` and a test extension from [github.com/Quaternar/qaros](https://github.com/Quaternar/qaros), pinned by revision in `Cargo.toml`.

## Build

```sh
git clone https://github.com/Quaternar/qaros-rerun
cd qaros-rerun
cargo build --release -p qaros-rerun
```

The first build compiles Rerun and takes several minutes, longer on a laptop. The binaries land in `target/release/`: `qaros-rerun` (the viewer) and `log-test-scene`.

## Test

```sh
cargo test --workspace
```

The QAROS tests load `qaros-stub`, so no QAROS installation is needed.

## First run

Without a headset, the viewer's default sink writes the eyes as PNGs with scripted head motion. Start it headless, log the included test scene to it, and look at the frames. In PowerShell:

```powershell
$viewer = Start-Process .\target\release\qaros-rerun.exe -PassThru -NoNewWindow -ArgumentList "--headless 1600x1000 --out out\headset --frames 240 --write-every 30 --quit-when-done"
Start-Sleep 5
.\target\release\log-test-scene.exe 300
$viewer.WaitForExit()
Get-ChildItem out\headset\user-1
```

In a Mac or Linux shell:

```sh
./target/release/qaros-rerun --headless 1600x1000 --out out/headset \
    --frames 240 --write-every 30 --quit-when-done &
sleep 5
./target/release/log-test-scene 300
wait
ls out/headset/user-1
```

The viewer exits by itself after 240 shown frames and prints a summary line `headset: frames started 240, shown 240, abandoned 0`. `out/headset/user-1` then holds `frame_NNNNN_color.png` (left and right eye side by side), `frame_NNNNN_depth.png` and `frame_NNNNN.txt` for every 30th frame.

Drop `--headless` for a desktop window, and see `qaros-rerun --help` for every option.

## Next steps

- Stream to headsets on Windows with QAROS: [integration guide](integration-guide.md).
- Real Rerun examples in the glasses: [demos](demos/README.md).
