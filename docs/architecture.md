---
title: Architecture
description: The integration pattern behind qaros-rerun, and how the viewer gets Rerun's 3D view and 2D views into QAROS.
sidebar_position: 4
---

# Architecture

How the viewer gets Rerun's 3D view and 2D views into QAROS, and the pattern to follow for your own application. The overview diagram and the crate table are in the [repository README](https://github.com/Quaternar/qaros-rerun#how-it-works).

## The integration pattern

`qaros-rerun` brings an existing desktop renderer to AR without changing what it draws. The same pattern fits any application that renders a 3D scene:

1. **Hook the renderer where the scene is ready.** Find the point where the application has built everything it draws in a frame, in a form that can be drawn again from another camera. Here a small patch to Rerun's 3D view hands its draw data to an observer ([The hook](#the-hook)).
2. **Render per eye, per user.** Every running target app in the QAROS session (a headset's player app, the Hub's visualizer) is a user with a render sender of its own. Each frame, QAROS hands out two eye poses and fields of view; the scene is drawn once per eye with an off-axis projection built from them (`crates/qaros-rerun/src/headset.rs`, `eye_render.rs`, the math in `crates/xr-math`).
3. **Hand color and depth to QAROS.** Color is transparent where nothing was drawn, so only your content reaches the room. Depth lets QAROS reproject the frame to the latest head pose. On Windows the eyes are drawn straight into the sender's Vulkan images on the application's own device ([vulkan-path.md](vulkan-path.md)); a CPU path copies them instead (`crates/qaros-sink`).
4. **2D UI as GUI panels.** Each 2D view is captured from the viewer's UI and shown on a QAROS GUI panel, placed once near the user's head; the Hub keeps it there. The panel shows a video stream the application owns and writes its pixels into, so the mixer samples the application's pixels as they are ([Stream panels](#stream-panels), `crates/qaros-rerun/src/panel_capture.rs`, `crates/qaros-sink/src/panels.rs`).
5. **Gestures through the panels.** A stream panel reports taps and drags as typed QAROS gesture events, in the stream's pixels. The application applies them on its own thread; the timeline panel's track and Play button work this way (`crates/qaros-rerun/src/timeline_panel.rs`, `crates/qaros-rerun/src/timeline_scrub.rs`, [The timeline panel](#the-timeline-panel)).

Everything that talks to QAROS sits behind one trait, `FrameSink` ([The seam](#the-seam)), so the renderer runs and is tested the same way with a PNG writer or QAROS's stub runtime in place of a headset.

## The hook

The 3D view's draw data is what the eyes need, and Rerun 0.40.0 offers no public way to get it: a wrapper view class cannot reuse the `3D` identifier the spatial visualizers check for, and visualizers cannot see each other's output. So a small patch to `re_view_spatial` (one new module and one call site) hands the data to an optional observer right before the 3D view queues it into its own `ViewBuilder`, together with the view coordinates, scene bounds and desktop camera (`patches/re_view_spatial/src/view_3d_observer.rs`, call site in `src/ui_3d.rs`). While an observer is registered, the view also keeps encoded images and video frames that lie outside the desktop camera, which Rerun would otherwise skip (`src/eye.rs`). The workspace applies the patch with `[patch.crates-io]`. The case for upstreaming it is in [for-rerun.md](for-rerun.md).

Per viewer frame, the headset renderer then:

1. while the 3D view draws (the sink is lent to the UI pass), shows the frames that are complete, then asks the sink for the next frame of every user that has none in flight (two poses and fields of view each, per-eye resolution);
2. builds one `ViewBuilder` per eye of each of those frames from copies of the same draw data, with the off-axis projection of that frame's own pose and a full-eye picking layer for depth (`re_renderer` does not expose the main depth buffer); for a CPU sink also a screenshot readback for color and a readback of the picking depth, for a GPU sink neither (the patched `re_renderer` keeps the picking depth on the GPU, [vulkan-path.md](vulkan-path.md#why-the-extra-pass));
3. after the UI pass, draws and submits all eyes on the viewer's queue, so streaming does not depend on the desktop window being visible; for a GPU sink it also writes color and depth into the sink's textures and shows each frame right away;
4. collects finished readbacks and shows them through a CPU sink, and releases frames that could not be set up or whose readback did not arrive.

Every frame is keyed by user and a per-user frame id from the sink's request to `show_frame`, and every readback carries that key, so a user's frame is shown only with the pixels rendered for its own pose. Users render independently: a user whose readback is late does not hold up the others.

The sink lives with the viewer app on the UI thread and is passed to the renderer per call: the renderer is shared with the 3D view observer, which must be `Send + Sync`, while QAROS handles are bound to one thread. Pixels where nothing was drawn are made transparent from the depth coverage. The viewer repaints continuously while at least one user is rendered, and otherwise behaves like the stock viewer, repainting every 0.5 s to poll the peer list; the QAROS peer update and frame callbacks wake it from library threads.

When the scene updates slowly, `--decoupled-render` moves eye rendering to a thread of its own: [decoupled-render.md](decoupled-render.md).

## The seam

```rust
pub trait FrameSink {
    fn begin_frames(&mut self) -> Result<Vec<FrameRequest>, SinkError>;
    fn submit_view(&mut self, key: FrameKey, view_index: usize, image: EyeImage)
        -> Result<(), SinkError>;
    fn show_frame(&mut self, key: FrameKey, show: ShowInfo) -> Result<(), SinkError>;
    fn abandon_frame(&mut self, key: FrameKey);
    fn is_streaming(&self) -> bool;
    fn depth_range(&self) -> DepthRange;
}
```

- `begin_frames` first applies user joins and leaves, then returns one request per rendered user without a frame in flight.
- `FrameRequest` carries the `FrameKey { user, frame_id }` (frame ids count per user from 0), two `ViewRequest { pose, fov }` (room-from-eye pose and OpenXR-style angles, like `QarPose` and `QarFov`) and the per-eye resolution.
- `EyeImage` is tightly packed RGBA8 color (sRGB) plus one `f32` per pixel in the sink's `DepthRange` encoding.
- `ShowInfo` is what `QarRenderFrameShow` carries: near and far, and `depth_scale`.

A GPU variant, `GpuFrameSink`, hands out textures to render into instead of taking images (`crates/frame-sink/src/gpu.rs`).

## Coordinates and depth

- Rerun's world is placed in the room by `AppPlacement`: a volume pose, `app_pose` and `app_scale` as in QAROS app volumes, and the view's own coordinates (for example `RFU`, Z up) rotated onto the room's Y-up axes. Left-handed view coordinates are rejected. The eye is shrunk into Rerun-world units with the content, which keeps `view_from_world` rigid and every angle intact.
- With `--sink qaros`, `--origin`, `--yaw` and `--scale` define the app volume "rerun" (center, `app_pose`, `app_scale`). Stream poses arrive in the volume's app content space: `--pose-space app` (default) maps them into the room with the volume's placement and hands near and far back in app units (room meters divided by the app scale); `--pose-space room` uses the poses as they come and reports room meters. Either way the renderer works in room space, and depth values are encoded from room meters.
- Depth goes out as D3D/OpenXR-style forward normalized depth with a finite far plane (0 at near, 1 at far, Rerun's background at far) and `depth_scale` 1.0. Reverse normalized and linear meters are one flag away (`--depth`). The reasoning, with references to the QAROS header, is in `crates/xr-math/src/depth.rs`.

## The QAROS sink

`--sink qaros` connects before the viewer opens, on the UI thread (with `--decoupled-render`, on the render thread):

1. **Library.** Loads `qar-streaming-c` from `--qaros-library` or `QAROS_DLL`, else with the binding's default lookup (a runtime already loaded, one beside the executable, `QAROS_RUNTIME_DIR`, the installed QAROS from `HKLM\SOFTWARE\Quaternar\QAROS`, then `PATH`), with the runtime binaries from the same directory (`QAROS_RUNTIME_DIR` overrides).
2. **Session.** Rejoins with the id in `--onboarding-id-file` (default `qaros-rerun.onboarding-id.txt`), or onboards with `--pairing-code` and stores the new id; with `--local-app`, as a local app of the Hub on this PC; with `--launched-by-qaros`, as a local app on Windows and with the launch invite from stdin elsewhere. It then reads the session's adapter (`qar_session_get_graphics_device_id`), on which `--qaros-graphics vulkan` (the default on Windows; `cpu` elsewhere) creates the viewer's device.
3. **App volume.** Gets or creates the app volume "rerun" (`--volume-name`, `--volume-size`, placement from `--origin`, `--yaw`, `--scale`). The demos name theirs `rerun-<demo>`, since two viewers asking for one name get one volume and one render stream. The label people see is `--display-name` (default "Rerun"), and the viewer's peer is the display name followed by "viewer".
4. **Callbacks.** Subscribes to peer updates, then to render stream requests; the callbacks forward into channels and wake the viewer.

On Stop (a QAROS Stop, Ctrl+C, Ctrl+Break or a closed console) the viewer lets its senders' begin-frame waits end (at most 500 ms), drops the senders, hides its panels, closes the volume, leaves the session and ends the process without unloading QAROS; a second Ctrl+C ends it at once. The [known issues](known-issues.md) page has the QAROS behaviour behind the drain and the exit.

### Users

- **Who is a user.** A peer for which `qar_peer_spec_is_target_app` is true and whose app state is running (headset, Android streamer, visualizer), as the QAROS header's "Basic flow" describes. `TargetPeers` lists the peers once and then follows every peer update; a peer that stops running or leaves the list has left, and so has a user whose stream reports closed. A target asks for content only once a sender attached it to the app volume, so the viewer does not wait for render requests; a request from an unlisted peer still adds it. `TargetPeers` sits behind the `UserEventSource` trait.
- **How many.** The `RenderPolicy` decides whom to render for; the default `MaxUsers` renders up to `--max-users` (default 2). Senders are created for every joined target at once, requesters first, and the first to answer take the slots; the others wait and are tried again when a slot frees.
- **Unanswered targets.** A target whose sender creation timed out (10 s) is held back for 10 minutes, also in later runs, and retried about once a minute; the viewer keeps these peers in `qaros-rerun.unanswered-peers.txt` next to the onboarding id file. Why: [ended target apps listed as running](known-issues.md#ended-target-apps-listed-as-running).
- **Leaving.** A user who leaves is released: their open frame is dropped, and their sender once its begin-frame wait has ended.

### Frames on the CPU path

On the CPU path (`--qaros-graphics cpu`), every rendered user gets a CPU render sender bound to the volume, with both eyes' color side by side in texture 0 and both eyes' depth in texture 1 (BGRA8 by default, D32 depth). For each user without an open frame the sink polls `qar_render_sender_try_begin_frame` once per viewer frame, the begin mode on both paths; once a frame is delivered, its two view poses and fields of view become that user's frame request. Each eye is written into the rectangles the frame reports (row pitch, flips, BGRA swizzle or RGBA copy, `f32` depth), and the frame is shown with the near and far planes used. The per-eye resolution comes from the negotiated layout. The Vulkan path is described in [vulkan-path.md](vulkan-path.md).

## Panels

With `--panels`, a GUI panel opens for every 2D view the viewer publishes: 0.45 m wide, with a content area of the view's aspect, showing a video stream of the viewer ([Stream panels](#stream-panels)). Panels need QAROS 0.9.0-dev.3177 or later; the viewer logs a warning on an older runtime.

**Placement.** The Hub owns a panel's place once the panel exists: it keeps the pose the panel was created with, or wherever a user moved it, across runs, and ignores later pose calls. So a new panel is created at its final place, within reach of one viewer's head:

- that viewer is the first headset whose head is within 4 m of the app volume's center on the floor plane (`qar_peer_spec_get_category` is `QAR_DEVICE_CATEGORY_HEADSET`), or, when no headset streams within 60 s of the first head near the volume, any viewer near it, such as a Display Receiver (`QAR_DEVICE_CATEGORY_VISUALIZER`); a camera across the room never counts;
- the log line `new panels open within reach of user-N (peer ..., <category> <device name>)` names that viewer;
- the panels stand on an arc 0.8 m from the head (on the floor plane), starting 0.25 m to the right of the gaze and going right, 0.1 m apart, top edges 0.1 m below the eyes, each upright and facing the head; the head is midway between that viewer's eyes, levelled to its heading;
- until such a viewer streams, a view without a panel waits; later viewers change nothing, and panels created later join the same row;
- each pose is the panel's top-edge midpoint, with the panel's -Z front facing the viewer, as the QAROS header defines panel space.

**Across runs.** A view's name, and so its panel's common name, is the same on every run. Panels are never closed at Stop: they are hidden (`--panels-at-stop keep` leaves them showing) and stay in the session, so the next start reattaches to them where they are and shows them again. Panels of this volume for views no longer published are recognised by display name and hidden. A panel closed in the Hub Display is claimed again by its common name, which revives it.

### The timeline panel

Every run with `--panels` also gets a timeline panel: a track and a Play button, 0.45 m wide. It opens by the same viewer's head, its center 0.5 m in front of the head along the levelled gaze and 0.2 m below the eyes, tilted back to face the eyes with its top edge level. Like the other panels, the Hub keeps its place across runs.

The viewer draws the row of controls itself, with Rerun's style and icons, in an egui context of its own rendered off screen on its wgpu device, and streams it like a view ([ADR-0078](decisions/ADR-0078%20A%20Timeline%20Drawn%20By%20The%20Viewer%20Driven%20By%20Panel%20Gestures.md)).

- A tap on the track seeks and pauses, a drag scrubs with the thumb under the finger, and the button toggles Play and Pause. The viewer applies each request on its next frame as time commands for Rerun's time control, at that fraction of the active timeline's data.
- The desktop time panel shows the same cursor, since both drive Rerun's one time control. The other way, the viewer publishes the cursor every frame, so the thumb follows playback and the desktop time bar; under the finger, and for a second after the last seek, the thumb stays where the finger put it.
- The button shows Pause while the time control plays, read from it once per viewer frame.
- One drag owns the cursor at a time, keyed by peer and gesture id, so a second user's touch does not move it.
- The panel is drawn in the app's `logic`, which runs also while the desktop window is minimised.

### Stream panels

Each view's crop, capped at 1280 px a side, goes into `PanelImages` as BGRA; the panel hook, on the thread that owns the QAROS session, copies at most one changed image per viewer frame into the CPU frame of that panel's `QarVideoSender` and shows it, and re-shows an unchanged one once a second so a mixer that connects late gets a frame ([ADR-0076](decisions/ADR-0076%202D%20Views%20As%20Stream%20Panels%20From%20CPU%20Video%20Senders.md)).

- **Names and reattach.** Stream panels have common names of their own, `<view>.stream.<volume>.rerun` and `timeline.panel-stream.<volume>.rerun`. A Start finds its panels by name, or by id when the listing lost the name, points them at this run's streams with `qar_gui_panels_set_stream` and shows them; web page panels left by earlier builds are left alone and logged ([ADR-0077](decisions/ADR-0077%20Stream%20Panels%20Claimed%20Under%20Names%20Of%20Their%20Own.md)).
- **Size.** A panel's height follows the content area the mixer draws the stream into, so a view fits whole. A view that changes size for good gets a new sender; the old one stays until the mixer has connected to the new one.
- **Stop.** Panels are hidden, then the gesture subscription is cancelled and the senders destroyed, all before the session goes; each step's duration is logged. On 0.9.0-dev.3177 a Stop takes about 250 ms in all ([ADR-0079](decisions/ADR-0079%20Stop%20Order%20With%20Stream%20Panels.md)).
- **QAROS builds.** 0.9.0-dev.3177 or later; the [known issues](known-issues.md) list the QAROS issues seen on that build.

## Tests

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo clippy -p qaros-rerun --no-default-features --all-targets -- -D warnings
```

- **`qaros-sink`**: unit tests for the pixel copy (BGRA swizzle, row pitch, view offsets, flipped views, depth floats, rejected sizes and formats) and the layout check; stub tests (`crates/qaros-sink/tests/stub.rs`) that load `qaros-stub` and run onboarding, sender creation, frames, users joining, waiting and leaving, frame-stamp pairing, app-space poses and an ordered teardown; unit tests of the panels over a fake backend (claim and reattach order, replacement and retirement of senders, round-robin shows, Stop order), of panel placement and sizes, and of the gesture decoding; stub tests (`crates/qaros-sink/tests/stream_stub.rs`) of the raw panel listing, the wait for a head, the shared listing and its retry, the teardown steps, and the graceful refusal on a runtime without video senders.
- **`frame-sink`**: the user roster and render policy, the image sink with users joining at their tick, and the frame stamp decoding back to user, frame and eye.
- **`qaros-rerun`**: each eye image carries the stamp of its own user, frame and eye; readback and view builder ids differ per user, frame and eye; the timeline scrub; the timeline panel's gesture mapping, and a tap on it reaching Rerun's command channel in one `logic` pass; the panels' pixel cap, mixer padding and settling; per-user statistics.
- **`xr-math`**: the projection decomposition equals an explicit off-axis matrix, a point straight ahead lands on each eye's principal point, scale does not change reported room depth, depth encodings round-trip. `qaros-rerun` checks the same decomposition against `re_renderer`'s own `Projection` and `RectTransform` matrices.
