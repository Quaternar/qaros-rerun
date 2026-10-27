# Changelog

All notable changes to this project are documented here, following [Keep a Changelog](https://keepachangelog.com/).

## [Unreleased]

### Added

- A timeline panel per viewer with `--panels`, in every demo whether or not it has 2D views: a slider and a Play button filling a 0.45 m by 0.12 m GUI panel that opens 0.5 m in front of the head that places the panels and 0.2 m below its eyes, tilted to face them. Dragging the slider with a finger pauses the viewer and moves its time cursor to that point of the recording, and Play resumes playback. The page (`/timeline-panel` on the panel server) sends `/timeline?at=<fraction>` and `/timeline?play`; it takes the mouse input of QAROS's Ultralight panels and the touch input of its CEF panels.
- The timeline panel's thumb follows the viewer's time cursor, during playback and when the desktop time bar moves. The viewer publishes the cursor as a fraction of the recording's time range each frame, and the page polls `/timeline/position` every 250 ms, a PNG whose width is 1 plus the fraction in thousandths. Polled positions wait while a finger drags the thumb and for a second after the last seek.
- `docs/known-issues.md`: QAROS behaviour the viewer works around, with symptom, cause, workaround, the build it was seen on and its status.
- Architecture decisions on panel placement within reach of a headset (ADR-0070), the headset chosen by device category (ADR-0071), the timeline panel (ADR-0072), the panel listing retry (ADR-0073), local-app onboarding (ADR-0074) and closed panels claimed again by name (ADR-0075); panels claimed by common name (ADR-0054), the user lifecycle (ADR-0055) and the pairing-code fallback (ADR-0056) are published. ADR-0049's pose rule is superseded by ADR-0070, and the ADRs on depth and color, URI panels, the Vulkan path, the launch contract, target discovery, the session adapter, begin-frame polling, sender creation and the queue lock cite the current QAROS headers and docs.

### Changed

- New panels open within reach of a headset: the viewer asks QAROS for each streaming peer's device category (`qar_peer_spec_get_category`) and places panels by the first `QAR_DEVICE_CATEGORY_HEADSET` head near the app volume. A Display Receiver (a Visualizer receiver) or a viewer whose category QAROS cannot say places them only when no headset streamed within 60 s of the first head near the volume. The log names the viewer, its peer and its device.
- `--launched-by-qaros` on Windows joins as a local app, as QAROS expects of an app it launches there, and reads no launch invite from stdin; stdin still signals Stop. Other platforms keep the stdin launch invite.
- The stop watchdog ends the process 600 ms after a stop request when the teardown has not, so with its bounded flush of at most 200 ms the exit and its log line land before QAROS kills the app, about 1 s after closing stdin.
- A failed panel listing opens no panel and is retried after 0.5 s, then every 30 s, instead of at every sync.

### Fixed

- Both render senders declare premultiplied alpha (`ColorAlphaMode::Premultiplied`), so the mixer blends the stream by its coverage; an undeclared stream is drawn opaque, uncovered pixels included.

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
