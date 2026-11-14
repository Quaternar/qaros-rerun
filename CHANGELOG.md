# Changelog

All notable changes to this project are documented here, following [Keep a Changelog](https://keepachangelog.com/) and [Semantic Versioning](https://semver.org/). Versions are the viewer's own; each release names the QAROS version it needs in its `manifest.json`. How entries and releases are made: [AGENTS.md](AGENTS.md).

## [Unreleased]

### Features

- The Rerun 0.40.0 viewer with a headset renderer: the 3D view is drawn once per eye of every QAROS user, from that user's head pose, and streamed as color plus depth.
- QAROS sink (`--sink qaros`): joins as a local app (`--local-app`), with the Hub's pairing code (`--pairing-code`) or when started by the Visualizer (`--launched-by-qaros`), and streams to every running target app, up to `--max-users` at a time.
- Vulkan path (`--qaros-graphics vulkan`, the default on Windows) renders the eyes straight into QAROS's images; `--qaros-graphics cpu` copies them through the CPU instead.
- 2D views open as QAROS GUI panels next to the scene (`--panels`), each a video stream of the viewer's own pixels.
- A timeline panel in the glasses: drag along it to scrub the recording, tap Play to resume; it follows the desktop time slider and the desktop follows it.
- A one-hand grab moves the scene and a two-hand spread scales it.
- Each recording is fitted into the app volume by itself; `--center` and `--scale` place the first one by hand, `--auto-fit off` places every one by hand.
- The app volume and the panels come back where they were left at the next start; `--reset-placement` and `--reset-panels` start afresh.
- `--move-far-panels` moves a reattached panel that is farther than 1.5 m away to where new panels open.
- Panels closed while the viewer runs, in the Hub Display or by a restarting mixer, are reopened when a headset streams again.
- PNG image sink (`--sink image`) with simulated users and scripted head motion, for runs without QAROS.
- `--version` prints the viewer's version, which the viewer also logs at start and reports to QAROS.
- Prebuilt viewers for Windows x64 and Linux x64 on the Releases page, `qaros-rerun-<version>-<platform>.zip`, with a `manifest.json` the QAROS Visualizer installs them from.
- `scripts/register-qaros-app.ps1` and `scripts/register-qaros-app.sh` register a viewer you built yourself, so the Visualizer's Rerun tile shows it as Detected; `-Unregister` removes it.
- One-command demos with real Rerun data: structure from motion, drone lidar, air traffic and robot arm.

### Enhancements

- The eyes are drawn on a render thread of their own, so head motion no longer waits for the scene update; `--inline-render` draws them in the viewer frame instead.
- Depth goes to QAROS as Rerun draws it (infinite reverse-Z), so far content is no longer clipped at `--far`.
- The app volume is 1.5 m on each side by default (`--volume-size`), and a taken-over volume keeps its size unless one is asked for.
- New panels open for a standing user, between chest and waist, a step out where AR glasses with a narrow field of view see them; `--panel-posture sitting` keeps the desk placement.
- New panels open within reach of the first headset near the app volume.
- A reattached panel at the floor, too close to the eyes, or at another size is moved to where new panels open.
- Panels open about 2 s sooner: each QAROS listing is one query.
- 2D view panels update every 50 ms by default (`--panel-interval-ms`).
- The app volume is hidden at Stop and shown again at the next start, and is never closed.
- `--launched-by-qaros` on Windows joins as a local app and reads no launch invite from stdin.
- The viewer ends within about 1 s of a Stop, before QAROS would kill it.
- A warning at start when the QAROS runtime is older than 0.9.0-dev.3177, the oldest the viewer supports.
- Quick start, integration guide, demo pages and known QAROS issues with their workarounds under `docs/`.

### Bug fixes

- A run that ended with the desktop window minimised no longer hides the window at the next start.
- With the desktop window minimised or covered, the glasses keep the 3D content and the 2D view panels, and the recording keeps playing.
- Resizing the desktop window no longer blanks the panels in the glasses.
- A panel stream's first frame reaches the glasses within a few frames of the mixer connecting it.
- After a Stop and a Start in the same Hub session the view panels come back.
- A panel QAROS lists as closed when the viewer claims it is shown again.
- A Ctrl+C Stop removes the app volume's box and content from the glasses.
- Panels of views no longer in the layout are hidden instead of keeping their last image.
- A stream panel QAROS cannot read no longer keeps the other panels from opening.
- Uncovered pixels are transparent in the glasses: both render senders declare premultiplied alpha.

### Removed

- The web panel route and its local panel server: `--panel-route`, `--panel-base-url` and `--dead-panels` are gone, `--panels` takes no address, and panels need QAROS 0.9.0-dev.3177 or later.

[Unreleased]: https://github.com/Quaternar/qaros-rerun/commits/main
