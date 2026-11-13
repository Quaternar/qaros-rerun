---
title: Developing on a Mac
description: Run and test the viewer on macOS without QAROS, with simulated headsets or QAROS's stub runtime.
sidebar_position: 8
---

# Developing on a Mac

Streaming to headsets needs Windows and QAROS ([Installation](INSTALLATION.md)). For development and tests the viewer also builds and runs on macOS, with two stand-ins for the headset:

- the **image sink** (the default `--sink image`), which simulates headsets walking around the scene and writes their eyes as PNGs;
- **`qaros-stub`**, QAROS's stub runtime for tests, which the QAROS sink loads in place of `qar-streaming-c`.

## Image sink

```sh
cargo build -p qaros-rerun
./target/debug/qaros-rerun --headless 1600x1000 --out out/headset \
    --frames 240 --write-every 30 &
./target/debug/log-test-scene 300
```

Drop `--headless` for the normal desktop window. `--sink off` gives a plain viewer. `./target/debug/qaros-rerun --help` lists the options: eye resolution, scripted head motion, simulated users, the frame stamp, placement in the room, near and far, the depth encoding, and the panels.

The sink writes one directory per user (`<out>/user-<n>/`) and, for every n-th shown frame:

- `frame_NNNNN_color.png`: left and right eye side by side, transparent where nothing was drawn;
- `frame_NNNNN_depth.png`: depth, log-scaled, background black;
- `frame_NNNNN.txt`: the user, poses, fields of view, near and far, and the depth encoding.

| Color | Depth |
| --- | --- |
| ![Left and right eye of the test scene: an orange cube in a ring of light-blue points](media/test-scene-color.png) | ![The same frame's depth, near surfaces bright](media/test-scene-depth.png) |

Several simulated users, each with their own stream and the frame stamp (`U<user> F<frame> L|R`) in every eye:

```sh
./target/release/qaros-rerun --headless 1600x1000 --users 2 --join-interval 30 \
    --frames 31 --write-every 30 --frame-stamp --out out/two-users --quit-when-done &
./target/release/log-test-scene 300
```

User k joins `--join-interval` viewer frames after user k-1 and leaves after `--frames` shown frames; users are spread evenly around the walk. `--max-users` (default 2) caps how many are rendered at once.

## QAROS sink against the stub

`qaros-stub` is a dev-dependency of `qaros-sink`, so building that crate's tests builds it. Its simulated visualizer asks for a stream as soon as one is offered, at 64x64 per eye:

```sh
cargo test -p qaros-sink --no-run
export QAROS_DLL="$(ls -t target/debug/deps/libqaros_stub-*.dylib | head -n 1)"
./target/debug/qaros-rerun --headless 1600x1000 --sink qaros --pairing-code ANY-CODE \
    --frames 150 --quit-when-done &
./target/debug/log-test-scene 300
```

This runs the whole QAROS flow: onboarding (the id lands in `qaros-rerun.onboarding-id.txt`, so the next start rejoins without a code), the app volume, the visualizer found in the peer list, its render sender, 150 frames, and an ordered teardown. The stub has no video senders, so panels are checked on a QAROS PC.

The stub can also script peers joining and leaving, and check every shown frame's stamp against the frame its sender issued:

```sh
QAROS_STUB_SCENARIO="join:headset-b@20,leave:visualizer@100" QAROS_STUB_REPORT=out/stub-report.txt \
./target/release/qaros-rerun --headless 1280x800 --sink qaros --pairing-code ANY-CODE \
    --frame-stamp --frames 160 --quit-when-done &
./target/release/log-test-scene 300
grep -c '^paired' out/stub-report.txt; grep -c '^ERROR' out/stub-report.txt
```

## Vulkan on the Mac

With Homebrew's `vulkan-loader` and `molten-vk` and `DYLD_FALLBACK_LIBRARY_PATH=/opt/homebrew/lib`, the viewer runs on wgpu's Vulkan backend (`--graphics-backend vulkan`) and on a Vulkan device it creates itself (`--vulkan-device`). The QAROS Vulkan path then runs against the stub too (`--sink qaros --qaros-graphics vulkan`). Commands: [vulkan-device.md](vulkan-device.md#running-on-the-mac) and [vulkan-path.md](vulkan-path.md#running-it-on-the-mac).
