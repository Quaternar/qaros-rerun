# Changelog

All notable changes to this project are documented here, following [Keep a Changelog](https://keepachangelog.com/).

## [0.1.0]

Initial public release.

### Added

- `qaros-rerun`: the Rerun 0.40.0 viewer with a headset renderer. The 3D view is rendered once per eye of every user, with the eye poses and fields of view each user's QAROS render stream asks for, and handed over as color plus depth; pixels where nothing was drawn are transparent. Includes a headless mode and `log-test-scene`.
- QAROS sink (`--sink qaros`) over the public `qar-streaming-c` API: onboarding with a pairing code, as a local app (`--local-app`) or with a launch invite (`--launched-by-qaros`), an app volume with placement and scale, one render sender per target app found in the peer list, a render policy capping the users rendered at once (`--max-users`, default 2), and an orderly stop on a QAROS Stop, Ctrl+C or a closed console.
- Vulkan path (`--qaros-graphics vulkan`, the default on Windows): the viewer creates its own Vulkan device on the session's adapter and renders each eye straight into the images of the user's Vulkan render sender. The CPU path (`--qaros-graphics cpu`) reads the eyes back and copies them into a CPU render sender.
- 2D views as QAROS GUI panels (`--panels`): views are cropped from the viewer's UI, served by a local panel server and opened as panels within reach of the first viewer near the app volume, 0.8 m away and to the right of the gaze, with stable names across runs; after that the Hub keeps each panel where it is, or where a user moved it.
- Decoupled render loop (`--decoupled-render`, experimental): the eyes are drawn on a render thread from the last finished scene, at the sink's rate.
- `xr-math`: per-eye cameras from OpenXR poses and fields of view, the off-axis projection decomposed for `re_renderer`, depth encodings, view coordinates to room axes and app-volume placement.
- `frame-sink`: the `FrameSink` seam, users and render policy, the frame stamp, and a PNG image sink with simulated users and scripted head motion for runs without QAROS: a level circle walk, an orbit that keeps looking at the data, or standing still, with eye height, period and an optional camera-like field of view (`--motion`, `--eye-height`, `--period`, `--fov`).
- `vulkan-device`: an app-created Vulkan device with a physical device picker, extra device extensions, external image wrapping, semaphore staging and image hand-over batches.
- `panel-server`: a local HTTP server and page that show the latest frame of each 2D view.
- Patched copies of Rerun's `re_view_spatial` (a 3D view observer hook) and `re_renderer` (picking depth kept on the GPU, a second render context on a shared device, a queue submission lock).
- Demo launchers for Rerun examples (structure from motion, drone lidar, air traffic, robot arm) that record once and replay in a loop, placed and coloured for see-through glasses: centered on their data 0.5 m above a room origin on a table (`--origin` for an origin on the floor), sized to the app volume, light colours for what matters, the drone flight over a dim backdrop of the whole scan, the air traffic altitudes exaggerated 20x.
- Integration guide, design notes and architecture decision records under `docs/`.
