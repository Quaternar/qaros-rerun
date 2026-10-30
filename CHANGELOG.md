# Changelog

All notable changes to this project are documented here, following [Keep a Changelog](https://keepachangelog.com/).

## [Unreleased]

### Added

- Stream panels: each 2D view's GUI panel shows a CPU video stream the viewer owns (`QarVideoSender` with `QarGuiPanelStreamIdExt`), so the mixer samples the viewer's pixels as they are and no port is opened. Views are capped at 1280 px a side, padded to a width that is a multiple of 64 and a height that is a multiple of 8 (a test for Quaternar#343), and shown when they change, and once a second otherwise. Panels take names of their own (`<view>.stream.<volume>.rerun`) and are pointed at each run's streams with `qar_gui_panels_set_stream` on reattach (ADR-0076, ADR-0077).
- A timeline panel per viewer with `--panels`, in every demo whether or not it has 2D views, opening 0.5 m in front of the head that places the panels and 0.2 m below its eyes, tilted to face them. The viewer draws it itself, with Rerun's style and icons, and takes taps and drags on it as QAROS panel gestures: a tap seeks and pauses, a drag scrubs under the finger, and the button plays or pauses and shows the time control's play state. The thumb follows the viewer's time cursor, during playback and when the desktop time bar moves, and stays under the finger for a second after the last seek. One drag owns the cursor at a time, per peer and gesture id. The timeline is drawn only when it changes, and its GPU readback is collected on a later frame, so the UI thread never waits for the GPU (ADR-0078).
- Stop logs every teardown step's duration, and a step of its own closes the stream panels' gesture subscriptions and senders after the hide and before the session (ADR-0079).
- `docs/known-issues.md`: why panels need 0.9.0-dev.3177, three QAROS issues seen on that build (Quaternar#343, Quaternar#344, Quaternar#345), and GUI panel queries that block for about 2 s there. The viewer lists panels only to claim them, and one listing serves every claim for 30 s.
- A warning at start when the QAROS runtime is older than 0.9.0-dev.3177, the oldest the viewer supports.
- `docs/known-issues.md`: QAROS behaviour the viewer works around, with symptom, cause, workaround, the build it was seen on and its status.
- Architecture decisions on panel placement within reach of a headset (ADR-0070), the headset chosen by device category (ADR-0071), the timeline panel (ADR-0072), the panel listing retry (ADR-0073), local-app onboarding (ADR-0074) and closed panels claimed again by name (ADR-0075); panels claimed by common name (ADR-0054), the user lifecycle (ADR-0055) and the pairing-code fallback (ADR-0056) are published. ADR-0049's pose rule is superseded by ADR-0070, and the ADRs on depth and color, URI panels, the Vulkan path, the launch contract, target discovery, the session adapter, begin-frame polling, sender creation and the queue lock cite the current QAROS headers and docs.

### Changed

- Panels are listed through a raw listing that reads each panel's content type first, so a stream panel in the session no longer makes the listing fail.
- New panels open within reach of a headset: the viewer asks QAROS for each streaming peer's device category (`qar_peer_spec_get_category`) and places panels by the first `QAR_DEVICE_CATEGORY_HEADSET` head near the app volume. A Display Receiver (a Visualizer receiver) or a viewer whose category QAROS cannot say places them only when no headset streamed within 60 s of the first head near the volume. The log names the viewer, its peer and its device.
- `--launched-by-qaros` on Windows joins as a local app, as QAROS expects of an app it launches there, and reads no launch invite from stdin; stdin still signals Stop. Other platforms keep the stdin launch invite.
- The stop watchdog ends the process 600 ms after a stop request when the teardown has not, so with its bounded flush of at most 200 ms the exit and its log line land before QAROS kills the app, about 1 s after closing stdin.
- A failed panel listing opens no panel and is retried after 0.5 s, then every 30 s, instead of at every sync.

- `--panels` is a plain switch and needs `--sink qaros`; panels need QAROS 0.9.0-dev.3177 or later.

### Removed

- The web panel route and the panel server; panels are video streams. `--panel-route`, `--panel-base-url` and `--dead-panels` are gone, and `--panels` takes no address. QAROS 0.9.0-dev.3177 or later.

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
