# Installation

How to build `qaros-rerun` from source and check it once on a Mac or a Windows PC. Streaming to headsets through QAROS is covered by the [integration guide](integration-guide.md).

## Prerequisites

| | Mac | Windows |
| --- | --- | --- |
| C and C++ build tools | Xcode Command Line Tools (`xcode-select --install`) | Visual Studio Build Tools 2022 with the "Desktop development with C++" workload |
| Rust | [rustup](https://rustup.rs) | [rustup](https://rustup.rs) |
| Git | included with the Command Line Tools | [Git for Windows](https://git-scm.com/download/win) |
| Demos (optional) | [uv](https://docs.astral.sh/uv/) | [uv](https://docs.astral.sh/uv/) |
| Streaming to headsets | not supported | QAROS 0.9 and a GPU with a Vulkan driver |

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

Without a headset, the viewer's default sink writes the eyes as PNGs with scripted head motion. Start it headless, log the included test scene to it, and look at the frames:

```sh
./target/release/qaros-rerun --headless 1600x1000 --out out/headset \
    --frames 240 --write-every 30 --panels 127.0.0.1:9877 --quit-when-done &
sleep 5
./target/release/log-test-scene 300
wait
ls out/headset/user-1
```

In PowerShell:

```powershell
$viewer = Start-Process .\target\release\qaros-rerun.exe -PassThru -NoNewWindow -ArgumentList "--headless 1600x1000 --out out\headset --frames 240 --write-every 30 --panels 127.0.0.1:9877 --quit-when-done"
Start-Sleep 5
.\target\release\log-test-scene.exe 300
$viewer.WaitForExit()
Get-ChildItem out\headset\user-1
```

The viewer exits by itself after 240 shown frames and prints a summary line `headset: frames started 240, shown 240, abandoned 0`. `out/headset/user-1` then holds `frame_NNNNN_color.png` (left and right eye side by side), `frame_NNNNN_depth.png` and `frame_NNNNN.txt` for every 30th frame. While the viewer runs, `http://127.0.0.1:9877/views` lists the 2D views it serves as panels.

Drop `--headless` for a desktop window, and see `qaros-rerun --help` for every option.

## Next steps

- Stream to headsets on Windows with QAROS: [integration guide](integration-guide.md).
- Real Rerun examples in the glasses: [demos](demos.md), for example `uv run demos/robot_arm/run.py`.
