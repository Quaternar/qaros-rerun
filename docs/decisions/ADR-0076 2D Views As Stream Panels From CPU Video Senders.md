---
id: ADR-0076
title: 2D views as stream panels fed by the viewer's own CPU video senders, behind --panel-route stream
status: accepted
date: 2026-10-29
repo: qaros-rerun
---

# ADR-0076: 2D views as stream panels fed by the viewer's own CPU video senders, behind `--panel-route stream`

## Context

ADR-0019 brings each 2D view to a GUI panel as a web page: the viewer renders the view, encodes it as PNG and serves it, and the mixer's embedded browser decodes it, lays out a page and renders it again. Input comes back through image loads (ADR-0072). Every panel is a browser, and the browser's navigations and reloads have been the mixer's most fragile path ([known issues](../known-issues.md#mixer-crash-when-panels-close-or-navigate)).

QAROS has a second kind of GUI panel. A panel created with `QarGuiPanelStreamIdExt` shows the frames of a video stream ([HDR] L2122-2134, L4194-4206), and the header names that use: "Use QarVideoSender for application-owned video such as GUI panel content" ([HDR] L4119-4131). A `QarVideoSender` with CPU ingress exists before any receiver, and its frame cycle is `try_begin_frame`, `frame_cpu`, `show_frame` ([HDR] L4109-4183, `QarVideoFrameCpu` L971-985). Taps and drags on any panel arrive as typed gesture events with kind, phase, position, start position and a per-peer lifecycle key ([HDR] L4301-4482), in the sender's pixels ([HDR] L1180-1197). The mixer at `c6e052d3` creates one Vulkan receiver per stream panel, sized from the sender's layout ([BC] `EnsureStreamReceiver` L790-866), receives `B8G8R8A8` ([BC] L73), draws the video at full content width keeping its aspect ([BR] L1630-1637, L1657-1696), and recognises gestures on stream panels like on any other ([BC] L1297-1325, [BR] L320-330).

Measured in the glasses:

- **0.9.0-dev.3137.** Within a run the route works: the panels showed live content about 50 ms after the claim, and taps and drags arrived as gesture events. Across runs it fails: the mixer crashed in its Ultralight renderer at the next Start ([ADR-0079](<ADR-0079 Stop Order With Stream Panels.md>)).
- **0.9.0-dev.3177.** Stream panels shown, gestures arriving, two clean restarts, Stop between 244 and 261 ms, no crash dumps. This build's mixer shares one Ultralight renderer across its panels.

## Decision

- **Pixels.** Each non-3D view keeps the existing capture (a window screenshot, or headless the harness's off-screen render) and is cropped per view. On the stream route the crop goes into `PanelImages` instead of the panel server: capped at 1280 px a side by halving with a 2x2 box filter, swizzled to BGRA with alpha 255 on the UI thread, and its sequence raised only when the pixels or the size changed (`crates/qaros-rerun/src/panel_capture.rs`, `crates/qaros-sink/src/panel_images.rs`).
- **Senders.** One CPU video sender per panel, one `B8G8R8A8` texture at the crop's size (`crates/qaros-sink/src/video_sender.rs`). The panel hook runs on the thread that owns the QAROS session and shows at most one due image per call, round robin: an image whose sequence changed, or an unchanged one once a second, so a receiver that connects late gets a frame. The size is compared before `try_begin_frame`, since a begun cycle cannot be abandoned; statuses 602, 603 and 604 mean "not now" ([HDR] L381-383). A view whose size stays changed gets a new sender, and the old one lives until the new one's first ready begin, at most 10 s, because the mixer keeps the old receiver until the new stream's layout is registered ([BC] L793-829, L1043-1070).
- **Settling.** A view gets a panel once it has been published for 3 s and still is. On the stream route "published" is read from each image's publish time, not its sequence, so a paused recording settles too.
- **Route flag.** `--panel-route web|stream` picks the route for every panel of a run; there is no mixed mode. The stream route binds no port: `--panels` only turns panels on, and `--panel-base-url` and `--dead-panels` do not apply.
- **Default.** `web` stays the default, and the help text and docs recommend `stream` from 0.9.0-dev.3177 on. The viewer learns the runtime's version only when it loads the QAROS library inside onboarding, after the panel capture has decided whether a panel server starts; choosing the route by version would mean deciding the panel route after the session exists, which this decision does not take on.

## Alternatives considered

- **Vulkan ingress on the viewer's device.** It saves one upload, but only once views render straight to GPU textures; today their pixels are on the CPU after the screenshot readback anyway.
- **`--panel-route stream` as the default.** On 3137 it crashes the mixer at the second Start, and a viewer cannot tell 3137 from 3177 before its route is set (see Default).
- **Removing the panel server now.** It keeps a working fallback for builds before 3177 and for a session where stream panels misbehave ([known issues](../known-issues.md)); removal waits for stream-route sessions that needed no fallback.

## Consequences

- Positive: the mixer samples a texture instead of rendering a page, no navigation or page load is involved for our panels, and input arrives as typed events. A panel's height follows the content area the mixer draws into, `0.37 * h/w + 0.105` m on a 0.45 m wide panel, so a view fits whole with no scroll bar.
- Positive: the route needs no open port, no firewall rule and no LAN address.
- Negative: four views mean four streams from one PC; the copy and show run on the QAROS thread, the render thread with `--decoupled-render`.
- Negative: on builds before 3177 the route is not usable across restarts, and two QAROS issues seen on 3177 can close a stream panel for every viewer during a run ([Quaternar#343], [Quaternar#344]).
- ADR-0019 stands for the web route.

## Evidence and sources

- [PC] `crates/qaros-rerun/src/panel_capture.rs` (`PanelPublisher`, `fit_within_cap`), `crates/qaros-rerun/src/qaros_setup.rs` (`PanelHook`), `crates/qaros-sink/src/stream_panels.rs`, `video_sender.rs`, `panel_images.rs`.
- [HDR] QAROS header at `ebfe3bf`, `qar-streaming-c/include/qar_streaming.h`, byte-identical to the 0.9.0-dev.3137 package's header: L971-985, L1180-1197, L2122-2134, L4109-4183, L4194-4206, L4301-4482.
- [BC] Mixer source at `c6e052d3`, `StereoKitGUI/src/Browser/BrowserControl.hpp` L73, L790-866, L1043-1070, L1297-1325.
- [BR] Mixer source at `c6e052d3`, `StereoKitGUI/src/Browser/Browser.hpp` L320-330, L1630-1637, L1657-1696.
- [Quaternar#343], [Quaternar#344] QAROS issues seen on 0.9.0-dev.3177.

[PC]: ../../crates/qaros-rerun/src/panel_capture.rs "Panel capture"
[HDR]: https://github.com/Quaternar/qaros/blob/ebfe3bf/qar-streaming-c/include/qar_streaming.h "QAROS C header"
[Quaternar#343]: https://github.com/Quaternar/Quaternar/issues/343 "Mixer: VulkanDeviceLost when a GUI stream panel connects its replaced stream"
[Quaternar#344]: https://github.com/Quaternar/Quaternar/issues/344 "A GUI panel failure in one viewer's mixer closes the panel for every viewer"
