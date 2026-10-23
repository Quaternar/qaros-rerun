# Decoupled headset render loop

Opt-in with `--decoupled-render`; without the flag the eyes are rendered inside the viewer frame. Decisions: ADR-0058 to ADR-0061.

## What it does

By default the eyes are rendered inside the viewer frame: the 3D view builds its draw data in Rerun's UI pass, the observer copies it per eye, and the eyes are drawn after the pass. Every eye frame waits for a whole viewer frame, so a slow scene update sets the headset rate.

With `--decoupled-render` a render thread draws the last finished scene for every user and eye at its own rate, from fresh poses, while the UI thread rebuilds the scene at its own pace and swaps it in when it is complete. On the Mac the eye rate stays at 248 frames per second while the scene update takes 50, 100 or 200 ms; with the flag off it is 8.1, 4.5 and 2.3 (measurements below).

## Design as built

### Thread model

- **UI thread.** Runs Rerun unchanged. The 3D view observer (`SceneCapture::on_view_3d_frame`) claims the streamed view, computes the placement and copies the draw data once (`draw_data_copy::copy_all`, `Arc` clones). After the UI pass, `DecoupledHeadset::end_viewer_frame` publishes the copy as a `RetainedScene` (draw data, placement, view seed, sequence number). A viewer frame that did not draw the view withdraws the scene. The UI repaints continuously while the render thread reports streaming, so the scene follows new data as fast as the UI allows.
- **Render thread** (`headset render`). Owns a second `re_renderer::RenderContext` on the viewer's device and queue (`RenderContext::new_sharing_device`, Rerun's output format and render config), the sink, the QAROS GUI panel hook and its own `HeadsetRenderer`. One loop iteration is one eye frame (`run_eye_frame`): `begin_frame` on its own context, take the newest scene, show finished frames, begin the next frame of every user without one in flight, build two `ViewBuilder`s per user from per-eye copies of the scene's draw data, `before_submit`, draw, submit, show, run the panel hook, publish its status. It never waits for the UI thread.
- **Setup.** The whole sink setup (`build_sink`: QAROS onboarding, app volume, the viewer's Vulkan device on the session's adapter, the sink, the panel hook; or the image sink) runs on the render thread through the tokio runtime's handle (ADR-0060). The stop request and the viewer's Vulkan device go back to the UI thread, which starts eframe on that device. Once Rerun has created its context, the UI thread hands device, queue, adapter, output format and render config to the render thread (`share_device`), which then creates its own context.

### Hand-off

- `SceneHandoff`: a mutex over `Option<Arc<RetainedScene>>`, a generation counter and a condvar. The UI publishes or withdraws; the render thread clones the `Arc` once per eye frame. Neither side holds the lock while it works.
- `RenderStatus`: atomics the render thread updates every eye frame (streaming, finished, frames open) and a statistics snapshot. The UI reads them for repaint decisions, `--quit-when-done` and the exit summary, without waiting for the render thread.
- QAROS callbacks wake both the UI (egui repaint) and the render thread (`SceneHandoff::wake`).
- Pacing: with nothing begun and nothing in flight the render thread waits on the condvar for a new scene, a wake or a stop, 1 ms while streaming and 20 ms otherwise (QAROS frames are polled, ADR-0039). With frames in flight but none to begin it waits up to 8 ms for the GPU, so readbacks map.

### Retained draw data and resource lifetimes

`re_renderer` 0.40.0 recycles pooled resources by reference count: `DynamicResourcePool::begin_frame` reclaims a buffer, texture or bind group only once the pool holds its last `Arc`. Every draw data type keeps its GPU resources through `GpuBindGroup`, which owns strong handles of what it binds, or through `GpuBuffer`, `GpuTexture` and `Arc<GpuMesh>` fields. The CPU write belt is only staging, copied into pool resources in the frame's "before view builder" encoder, which `before_submit` submits. A held copy therefore keeps everything it draws alive and out of the pools (ADR-0059).

Rules:

1. A scene is published only after Rerun's `before_submit`, so every upload it depends on is on the queue before any eye frame that draws it.
2. The render thread holds its `Arc` for a whole eye frame and drops it after that frame's submissions. A replaced scene's resources return to Rerun's pools only then, and any later write to them is a later submission on the same queue.
3. Per eye frame state (camera uniforms, frame bind group, eye targets, picking and screenshot readbacks, transparency lookup textures) lives in the render thread's own context, pools and belt.
4. `PointCloudDrawData` and `GaussianSplatDrawData` copies collect their drawables on their own (patch below), so the desktop view and the eye views never clear one shared list from two threads.
5. Resources that Rerun rewrites in place (video frame textures) can change under a retained scene; the eye then shows newer content than the rest of the scene.

Drawing another context's draw data needs nothing else: `QueueableDrawData::renderer_key` creates the renderer in the context it is queued into, `Renderer::draw` sees only its own pipelines and the draw data's bind groups, and wgpu-core deduplicates bind group layouts with equal entries (`Device::create_bind_group_layout_inner`, `bgl_pool`), so a bind group made with Rerun's layout is compatible with the render thread's pipelines.

### Queue access

wgpu's own submissions are thread-safe; the QAROS Vulkan path's raw submits and the semaphores it stages on wgpu's queue are not (ADR-0029). `re_renderer::queue_submission_lock()` covers Rerun's `before_submit`, the eye submit, the headless panel capture's two queue submits, and on the render thread the sink calls (begin, show, abandon, user changes, drop) of a GPU sink that submits outside wgpu (`GpuFrameSink::submits_outside_wgpu`, today the QAROS Vulkan sink). Sinks that submit through wgpu alone take no lock. Building the eye views and queuing the scene's draw data run outside it. eframe's window presentation does not take it, so the QAROS Vulkan sink with `--decoupled-render` is refused unless the viewer is headless, which is the launched-by-QAROS default (ADR-0061).

### Eye poses, eye output reuse, panels, users

- Poses: `begin_frames` runs on the render thread right before the eye views are built, so every eye frame uses the newest poses, whatever the scene's age. The perf log records the scene's age at every eye frame (`scene_age_at_eye_frame`).
- Eye output reuse (ADR-0057): the kept composite targets and output uniforms belong to the render thread's `HeadsetRenderer` and are reused as before.
- Panels: capture (cropping the UI image, the panel server) stays on the UI thread; the QAROS GUI panel hook runs once per eye frame on the render thread and syncs once a second as before.
- Users: joins, leaves and the user cap work as without the flag, inside the sink on the render thread; with two users each gets a frame per eye frame.

### Stop and teardown

`Drop` of `DecoupledHeadset`, reached from `leave_qaros` (Stop, Ctrl+C, `--quit-when-done`) and `on_exit`: request the stop, wake the condvar, join the render thread. The thread finishes its eye frame, abandons every open frame on the sink, drops the sink (senders), then the panel hook, which holds the last reference to the QAROS connection, so session and library are released on the thread that created them; then its own context. The UI then clears its own frame hooks and ends the process as without the flag; the stop watchdog bounds the whole teardown. Dropping the QAROS sink first lets its senders' begin-frame waits end, for at most 500 ms, so in this mode the drain runs on the render thread inside the join. A panic anywhere after the sink was built (the renderer's setup, the wait for the device, the render context, the eye loop) is caught: the teardown runs as above, and the render thread marks itself failed, which the UI thread treats as a stop request and ends the run with an error (ADR-0060). A panic in the teardown is caught too; the submission lock is taken before the sink is unpacked, so the sink drops under it even then.

## Changes to Rerun crates

`re_renderer` only, +92/-13 lines against the vendored 0.40.0; `re_view_spatial` needs nothing for this mode.

| File | Lines | What and why |
| --- | --- | --- |
| `src/context.rs` | +64/-10 | `RenderContext::new_sharing_device`: a second context on a device another context renders with, which leaves the device's uncaptured error handler to the first (`new` replaces it). `queue_submission_lock()`: a process-wide mutex held around `before_submit`'s submit (ADR-0061). |
| `src/lib.rs` | +1/-1 | exports `queue_submission_lock`. |
| `src/renderer/point_cloud.rs` | +14/-1 | hand-written `Clone`: a copy starts its own `SortedDrawables` instead of sharing the original's `Arc<Mutex<_>>` (ADR-0059). |
| `src/renderer/gaussian_splat.rs` | +13/-1 | the same for gaussian splats. |

Carrying them across a Rerun upgrade: re-apply the three changes to the new vendored copy (each is local), then re-check the properties the hand-off rests on, which no API promises: pools recycle by reference count, draw data holds its resources by strong handles, renderers draw with nothing but their pipelines and the draw data's bind groups, and no other draw data type shares per-frame state between clones (search the renderers for `Arc<Mutex` and `Cell` fields). The GPU test `retained_draw_data_draws_unchanged_from_a_second_context_on_another_thread` fails if a retained point cloud is recycled or drawn differently; extend it to any new draw data type. `draw_data_copy.rs` must list every new renderer, as before.

## Alternatives considered

- **One context, shared under a lock.** The UI pass holds Rerun's context from `begin_frame` to `before_submit`, so the render thread would wait for the whole scene update.
- **K eye-only frames per scene on Rerun's context.** Reuses a scene for several eye frames, but still on the UI thread, before or after the UI pass, never during it. Its held-draw-data idea is reused here.
- **Our own minimal draw path for retained scenes.** Draw data contents are private to `re_renderer`; it would mean reimplementing every renderer.
- **Persistent ("retained") variants of draw data or of the belt in `re_renderer`.** A patch across every renderer; unnecessary while pools recycle by reference count.
- **Sink on the UI thread, proxied from the render thread.** Every begin and show would wait for the UI thread again.
- **A second queue for the render thread.** wgpu 30 exposes one queue per device.

## Feasibility passes

1. **Retention.** Read the pools, bind groups, belt and every draw data type: retention by `Arc` holds. Confirmed by the GPU test and by byte-identical eye images with the flag off and on.
2. **Second context.** Read `RenderContext::new` for global side effects: it replaces the device's error handler (patched); the shader registry and file server are process-wide but idempotent. Renderer lookup, pipelines and bind group layout compatibility checked in `re_renderer` and wgpu-core.
3. **Shared state inside draw data.** Found the per-frame `SortedDrawables` list shared by clones of point clouds and splats; two contexts with different frame indices would corrupt each other's draws. Patched (copies own their list).
4. **Thread affinity of the sink.** QAROS is single-threaded per runtime and the panels share the session: the whole setup moves to the render thread rather than refusing the QAROS sink.
5. **Queue access on the Vulkan path.** Raw `vkQueueSubmit`, `vkQueueWaitIdle` and semaphores staged on wgpu's queue assumed one submitting thread: the process-wide submission lock, and a refusal for the one uncovered case (a window with the Vulkan sink).

No pass hit a wall that needed another route.

## Measurements on the Mac

Apple M5 Pro, Metal (MoltenVK for the GPU sink rows), release build, headless 1280x800, image sink, one user standing still (`--motion still`), 960x1000 per eye, `--write-every 100000`. Scene from `log-test-scene 300` (light) or `log-test-scene 300 <url> 5000` (5000 extra point cloud entities). `QAROS_RERUN_SCENE_DELAY_MS` sleeps after every UI pass as a stand-in for a slow scene update. Eye intervals are the gaps between a user's `frame_shown` events in the perf log (`QAROS_RERUN_PERF_LOG`), the first 2 s after the first shown frame skipped; the scene update is the headless loop's `step` span. Measured on a shared machine; read the rates as lower bounds.

| Scene | Scene update, median ms | Flag off: eye frames/s; interval median / p95 / max ms | Flag on: eye frames/s; interval median / p95 / max ms |
| --- | --- | --- | --- |
| light | 0.7 | 173.1; 5.2 / 7.9 / 8.6 | 209.2; 4.6 / 5.9 / 10.2 |
| +50 ms delay | 59 off, 56 on | 8.1; 124.1 / 129.1 / 133.6 | 248.1; 4.0 / 4.3 / 5.2 |
| +100 ms delay | 109 off, 106 on | 4.5; 224.7 / 230.1 / 233.3 | 247.6; 4.0 / 4.3 / 5.6 |
| +200 ms delay | 211 off, 206 on | 2.3; 427.7 / 429.8 / 431.3 | 247.7; 4.0 / 4.3 / 4.9 |
| 5000 entities | 45 off, 47 on | 11.1; 88.4 / 92.5 / 161.0 | 62.7; 15.9 / 16.6 / 20.0 |
| 5000 entities, 2 users | 51 off, 46 on | 10.1 per user; 98.5 / 101.6 / 106.4 | 33.6 per user; 29.7 / 30.7 / 34.6 |
| GPU sink (MoltenVK), +100 ms | 129 off, 111 on | 7.8; 129.4 / 132.3 / not recorded | 70.3; 14.2 / 15.1 / not recorded |
| GPU sink (MoltenVK), 5000 entities | 58 off, 47 on | 17.3; 57.5 / 59.0 / 63.7 | 43.0; 23.3 / 24.1 / 29.0 |

Reading:

- With the flag on the eye rate is flat against the scene update time: 248 frames per second at 50, 100 and 200 ms, with a p95 interval of 4.3 ms. With the flag off the CPU path shows a frame every second viewer frame.
- The eye rate now depends on the scene's size, not its update time: with 5000 entities each eye frame collects and encodes thousands of draw calls (encode p95 8.3 ms) and draws them twice per eye (main and picking pass).
- The scene's age at the eye frame is about half the scene update time (median 30 ms at 50 ms, 105 ms at 200 ms).
- On the GPU sink rows the UI's scene update is not slower with the flag on (111 against 129 ms off at a 100 ms delay, MoltenVK): `GpuReadbackSink` submits through wgpu alone, so its calls take no submission lock.
- Correctness: with the light scene and a still head, frame 399 of a run with the flag off and of one with it on are byte-identical in the color and the depth PNG (`cmp`).

Commands:

```sh
QAROS_RERUN_PERF_LOG=runs/NAME.perf.csv [QAROS_RERUN_SCENE_DELAY_MS=100] \
    ./target/release/qaros-rerun --headless 1280x800 --out runs/NAME.images --frames N \
    --write-every 100000 --motion still --quit-when-done [--decoupled-render] \
    [--users 2 --join-interval 1] [--vulkan-device default --image-targets gpu] &
sleep 3; ./target/release/log-test-scene 300 rerun+http://127.0.0.1:9876/proxy [5000]
```

N was 80 to 600 with the flag off and 3000 with it on, so every run lasted 10 to 90 s. The GPU sink rows need `DYLD_FALLBACK_LIBRARY_PATH=/opt/homebrew/lib`, set through `env` (SIP strips it from `/bin/sh` and the rustup cargo wrapper).

### Rerun 0.40.0 next to 0.38.1

The table above is Rerun 0.38.1. The runs below put a 0.38.1 build and a 0.40.0 build of this viewer side by side with the same commands, interleaved row by row, two runs each, in one session: Apple M5 Pro, macOS 26.5, Metal, release builds, headless 1280x800, image sink, one user standing still, 960x1000 per eye, `--port 9886`. Measured on a shared machine; read the rates as lower bounds. Each cell gives run 1, then run 2.

| Scene | Build | Scene update, median ms (off; on) | Flag off: eye frames/s; interval median / p95 / max ms | Flag on: eye frames/s; interval median / p95 / max ms |
| --- | --- | --- | --- | --- |
| +100 ms delay | 0.38.1 | 113.9, 114.2; 109.4, 110.3 | 4.3; 231.5 / 238.9 / 240.6 and 4.3; 232.0 / 238.6 / 239.5 | 258.4; 3.8 / 4.2 / 5.0 and 258.0; 3.8 / 4.2 / 5.3 |
| +100 ms delay | 0.40.0 | 114.1, 114.5; 110.8, 110.9 | 4.4; 231.4 / 238.4 / 239.6 and 4.3; 231.5 / 240.9 / 242.2 | 247.4; 4.0 / 4.3 / 5.3 and 251.9; 4.0 / 4.2 / 5.1 |
| 5000 entities | 0.38.1 | 44.9, 44.3; 45.8, 45.7 | 11.2; 88.2 / 95.5 / 109.2 and 11.5; 87.0 / 89.2 / 93.1 | 65.2; 15.3 / 15.9 / 21.1 and 64.5; 15.5 / 16.0 / 19.0 |
| 5000 entities | 0.40.0 | 41.0, 38.8; 38.8, 38.6 | 12.2; 82.1 / 89.8 / 104.6 and 13.1; 76.2 / 78.5 / 91.1 | 64.6; 15.4 / 16.1 / 23.1 and 64.8; 15.4 / 16.0 / 18.2 |

Pool creations per shown frame are the growth of `POOL_RESOURCES_CREATED` (`rr_resources_created` in the perf log) over the same window as the eye intervals, divided by the frames shown in it; the counter is process-wide, so with the flag on it counts both contexts. `view_builder_new` is the span around each eye's `ViewBuilder::new`, in microseconds.

| Scene | Build | Pool creations per shown frame (off; on) | `view_builder_new` median / p95 µs, off | `view_builder_new` median / p95 µs, on |
| --- | --- | --- | --- | --- |
| +100 ms delay | 0.38.1 | 4.20, 4.08; 0.10, 0.08 | 10 / 17 and 10 / 16 | 2 / 4 and 2 / 4 |
| +100 ms delay | 0.40.0 | 3.63, 3.55; 0.09, 0.10 | 15 / 23 and 14 / 21 | 3 / 6 and 3 / 5 |
| 5000 entities | 0.38.1 | 0.00, 0.00; 19.99, 147.45 | 4 / 6 and 4 / 6 | 6 / 8 and 5 / 8 |
| 5000 entities | 0.40.0 | 0.00, 0.00; 0.03, 2.41 | 6 / 9 and 6 / 9 | 9 / 14 and 9 / 13 |

Frame 399 of the light scene (`--frames 400 --write-every 399`), flag off against flag on: color and depth PNGs byte-identical (`cmp`) for both builds in both runs. Between the builds, with the flag off, 154 of the 1,920,000 color pixels (both eyes side by side) and 1 depth PNG pixel differ, the same in both runs; 0.40 spans each line's quad half a pixel wider so its antialiasing feather is drawn whole (`shader/lines.wgsl` L285-299), and the scene's arrows are lines.

Reading:

- The eye rate with the flag on stays flat against the scene update time on 0.40.0 (247 to 252 eye frames per second at +100 ms, against 258 on 0.38.1 in the same session), and the p95 interval stays at 4.2 to 4.3 ms.
- The 5000-entity scene updates faster on 0.40.0 (39 to 41 ms against 44 to 46 ms), and with the flag off the eye rate follows (12.2 and 13.1 against 11.2 and 11.5).
- `ViewBuilder::new` takes a few microseconds more on 0.40.0 (it also sets up the depth resolve), well below a millisecond either way at 960x1000.
- Pool creations with the flag off are the same within run to run spread. With the flag on the counter varies widely between runs of the same build (0.38.1: 20 and 147 per shown frame with 5000 entities), so these two runs settle nothing about it.

Commands: as above, with `--port 9886` on the viewer and `rerun+http://127.0.0.1:9886/proxy` for `log-test-scene`; N was 80 (+100 ms, off), 600 (5000 entities, off) and 3000 (on). The first 2 s after the first shown frame are skipped, as above.

## Tests

- `decoupled::tests`: the hand-off (the newest scene wins, a withdrawal clears it, a scene in use outlives its replacement, a publication or a stop from another thread ends the wait, otherwise the wait lasts its timeout) and, on the GPU, `retained_draw_data_draws_unchanged_from_a_second_context_on_another_thread`: a scene of an opaque point, sorted transparent points, sorted gaussian splats and a volume, built in one context and kept while that context runs 12 more frames that build and draw scenes of the same shape; then copies of it are drawn 24 times from a second context on another thread while the first context, on a third thread, draws copies of it and new scenes in turns, and every image must be byte-identical to the first context's reference. `a_volume_is_copied` checks the volume's copy on its own.
- `options::tests::decoupled_render_needs_a_sink_and_headless_for_the_qaros_vulkan_sink`.
- Workspace: `cargo test --workspace` with `DYLD_FALLBACK_LIBRARY_PATH=/opt/homebrew/lib`, cargo run as `$(rustup which cargo)`: everything passes and no test printed "skipped" (the Vulkan GPU tests ran on MoltenVK, the retained draw data test on Metal). `cargo clippy --workspace --all-targets -- -D warnings`, the same for `-p qaros-rerun --no-default-features`, and `cargo fmt --all --check` are clean.

## Known limits

- The eye rate is decoupled from the scene's CPU update time, not from its size or its GPU time: both threads share the GPU and the queue, and every eye frame collects and encodes the whole scene.
- Shaders and pipelines compile twice, and the render thread's belt takes its own 32 MiB host-visible chunks.
- With the QAROS Vulkan sink, the render thread's sink calls hold the submission lock, which can delay Rerun's `before_submit` on the UI thread.
- A window with the QAROS Vulkan sink is refused; a window with the CPU sinks works.
- Anything that calls into QAROS from the UI thread would be a second thread on one runtime; today nothing does.
- Video textures and other resources Rerun rewrites in place can be newer than the rest of a retained scene.
- `--frame-stamp` goes through the same code but was not run decoupled against `qaros-stub`; the QAROS CPU sink was, in `a_stop_tears_the_qaros_sink_and_session_down_on_the_render_thread_in_order` (`decoupled::tests`).
- The exit summary's per-user frames in flight are counted in eye frames that drew (`eye frames on average`), not in viewer frames, so that column does not compare between the flag off and on; begin to show in milliseconds does.
