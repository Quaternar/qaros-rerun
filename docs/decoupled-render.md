---
title: Decoupled render loop
description: Keep head motion smooth while the scene updates slowly, by drawing the eyes on a render thread of their own from the last finished scene.
sidebar_position: 7
---

# Decoupled headset render loop

Experimental, opt-in with `--decoupled-render`; without the flag the eyes are rendered inside the viewer frame.

## What it does

By default the eyes are rendered inside the viewer frame: the 3D view builds its draw data in Rerun's UI pass, the observer copies it per eye, and the eyes are drawn after the pass. Every eye frame waits for a whole viewer frame, so a slow scene update sets the headset rate, and head motion stutters with it.

With `--decoupled-render` a render thread draws the last finished scene for every user and eye at its own rate, from fresh head poses, while the UI thread rebuilds the scene at its own pace and swaps it in when it is complete. Head motion stays smooth; the data itself updates at the scene's rate.

The same idea applies to any application whose scene update is slower than the headset needs: keep the last complete scene, and draw it from new poses as often as the headset asks.

## When to use it

Try it when your scene updates slowly: many entities, large point clouds, long time windows. Measured on a Mac (Apple M5 Pro, Metal, release build, Rerun 0.40.0, one user standing still, 960x1000 per eye, image sink, two runs each):

| Scene | Scene update | Eye frames per second, flag off | Eye frames per second, flag on |
| --- | --- | --- | --- |
| Test scene with a 100 ms delay after every UI pass, standing in for a slow update | about 114 ms | 4.3 to 4.4 | 247 to 252 |
| Test scene with 5000 extra point cloud entities | about 40 ms | 12.2 to 13.1 | 64.6 to 64.8 |

With the flag on, the eye rate no longer depends on how long the scene update takes, only on how much the scene costs to draw: with 5000 entities each eye frame still collects and encodes thousands of draw calls. Your rates on a Windows GPU differ; measure them with `QAROS_RERUN_PERF_LOG` (below).

Limits:

- With the QAROS Vulkan path it needs a headless viewer (`--headless WxH`, or `--launched-by-qaros` without `--window`); in a window, use `--qaros-graphics cpu`.
- Videos and other textures Rerun rewrites in place can show newer content than the rest of a held scene.
- Both threads share the GPU and the queue, so the eye rate is decoupled from the scene's CPU update time, not from its GPU time.
- Shaders and pipelines compile twice, and the render thread's upload belt takes its own 32 MiB host-visible chunks.
- A failure on the render thread ends the viewer with exit code 1.

## Design

### Thread model

- **UI thread.** Runs Rerun unchanged. The 3D view observer (`SceneCapture::on_view_3d_frame`) claims the streamed view, computes the placement and copies the draw data once (`draw_data_copy::copy_all`, `Arc` clones). After the UI pass, `DecoupledHeadset::end_viewer_frame` publishes the copy as a `RetainedScene` (draw data, placement, view seed, sequence number). A viewer frame that did not draw the view withdraws the scene. The UI repaints continuously while the render thread reports streaming, so the scene follows new data as fast as the UI allows.
- **Render thread** (`headset render`). Owns a second `re_renderer::RenderContext` on the viewer's device and queue (`RenderContext::new_sharing_device`, Rerun's output format and render config), the sink, the QAROS GUI panel hook and its own `HeadsetRenderer`. One loop iteration is one eye frame (`run_eye_frame`): `begin_frame` on its own context, take the newest scene, show finished frames, begin the next frame of every user without one in flight, build two `ViewBuilder`s per user from per-eye copies of the scene's draw data, `before_submit`, draw, submit, show, run the panel hook, publish its status. It never waits for the UI thread.
- **Setup.** The whole sink setup (`build_sink`: QAROS onboarding, app volume, the viewer's Vulkan device on the session's adapter, the sink, the panel hook; or the image sink) runs on the render thread, since QAROS is used from one thread. The stop request and the viewer's Vulkan device go back to the UI thread, which starts eframe on that device. Once Rerun has created its context, the UI thread hands device, queue, adapter, output format and render config to the render thread (`share_device`), which then creates its own context.

### Hand-off

- `SceneHandoff`: a mutex over `Option<Arc<RetainedScene>>`, a generation counter and a condvar. The UI publishes or withdraws; the render thread clones the `Arc` once per eye frame. Neither side holds the lock while it works.
- `RenderStatus`: atomics the render thread updates every eye frame (streaming, finished, frames open) and a statistics snapshot. The UI reads them for repaint decisions, `--quit-when-done` and the exit summary, without waiting for the render thread.
- QAROS callbacks wake both the UI (egui repaint) and the render thread (`SceneHandoff::wake`).
- Pacing: with nothing begun and nothing in flight the render thread waits on the condvar for a new scene, a wake or a stop, 1 ms while streaming and 20 ms otherwise (QAROS frames are polled). With frames in flight but none to begin it waits up to 8 ms for the GPU, so readbacks map.

### Retained draw data and resource lifetimes

`re_renderer` 0.40.0 recycles pooled resources by reference count: `DynamicResourcePool::begin_frame` reclaims a buffer, texture or bind group only once the pool holds its last `Arc`. Every draw data type keeps its GPU resources through `GpuBindGroup`, which owns strong handles of what it binds, or through `GpuBuffer`, `GpuTexture` and `Arc<GpuMesh>` fields. The CPU write belt is only staging, copied into pool resources in the frame's "before view builder" encoder, which `before_submit` submits. A held copy therefore keeps everything it draws alive and out of the pools.

Rules:

1. A scene is published only after Rerun's `before_submit`, so every upload it depends on is on the queue before any eye frame that draws it.
2. The render thread holds its `Arc` for a whole eye frame and drops it after that frame's submissions. A replaced scene's resources return to Rerun's pools only then, and any later write to them is a later submission on the same queue.
3. Per eye frame state (camera uniforms, frame bind group, eye targets, picking and screenshot readbacks, transparency lookup textures) lives in the render thread's own context, pools and belt.
4. `PointCloudDrawData` and `GaussianSplatDrawData` copies collect their drawables on their own (patch below), so the desktop view and the eye views never clear one shared list from two threads.
5. Resources that Rerun rewrites in place (video frame textures) can change under a retained scene; the eye then shows newer content than the rest of the scene.

Drawing another context's draw data needs nothing else: `QueueableDrawData::renderer_key` creates the renderer in the context it is queued into, `Renderer::draw` sees only its own pipelines and the draw data's bind groups, and wgpu-core deduplicates bind group layouts with equal entries (`Device::create_bind_group_layout_inner`, `bgl_pool`), so a bind group made with Rerun's layout is compatible with the render thread's pipelines.

### Queue access

wgpu's own submissions are thread-safe; the QAROS Vulkan path's raw submits and the semaphores it stages on wgpu's queue are not. `re_renderer::queue_submission_lock()` covers Rerun's `before_submit`, the eye submit, the headless panel capture's two queue submits, and on the render thread the sink calls (begin, show, abandon, user changes, drop) of a GPU sink that submits outside wgpu (`GpuFrameSink::submits_outside_wgpu`, today the QAROS Vulkan sink). Sinks that submit through wgpu alone take no lock. Building the eye views and queuing the scene's draw data run outside it. eframe's window presentation does not take it, so the QAROS Vulkan sink with `--decoupled-render` is refused unless the viewer is headless, which is the launched-by-QAROS default.

### Eye poses, panels, users

- Poses: `begin_frames` runs on the render thread right before the eye views are built, so every eye frame uses the newest poses, whatever the scene's age. The perf log records the scene's age at every eye frame (`scene_age_at_eye_frame`).
- Eye output resources (composite targets, output uniforms) belong to the render thread's `HeadsetRenderer` and are reused across frames.
- Panels: capture (cropping the UI image, the panel server) stays on the UI thread; the QAROS GUI panel hook runs once per eye frame on the render thread and syncs once a second.
- Users: joins, leaves and the user cap work as without the flag, inside the sink on the render thread; with two users each gets a frame per eye frame.

### Stop and teardown

`Drop` of `DecoupledHeadset`, reached from `leave_qaros` (Stop, Ctrl+C, `--quit-when-done`) and `on_exit`: request the stop, wake the condvar, join the render thread. The thread finishes its eye frame, abandons every open frame on the sink, drops the sink (senders), then the panel hook, which holds the last reference to the QAROS connection, so session and library are released on the thread that created them; then its own context. The UI then clears its own frame hooks and ends the process as without the flag; the stop watchdog bounds the whole teardown. Dropping the QAROS sink first lets its senders' begin-frame waits end, for at most 500 ms. A panic anywhere after the sink was built is caught: the teardown runs as above, and the render thread marks itself failed, which the UI thread treats as a stop request and ends the run with an error. A panic in the teardown is caught too; the submission lock is taken before the sink is unpacked, so the sink drops under it even then.

## Changes to Rerun crates

`re_renderer` only; `re_view_spatial` needs nothing for this mode. These changes are listed with the others in [for-rerun.md](for-rerun.md).

| File | What and why |
| --- | --- |
| `src/context.rs` | `RenderContext::new_sharing_device`: a second context on a device another context renders with, which leaves the device's uncaptured error handler to the first (`new` replaces it). `queue_submission_lock()`: a process-wide mutex held around `before_submit`'s submit. |
| `src/lib.rs` | exports `queue_submission_lock`. |
| `src/renderer/point_cloud.rs` | hand-written `Clone`: a copy starts its own `SortedDrawables` instead of sharing the original's `Arc<Mutex<_>>`. |
| `src/renderer/gaussian_splat.rs` | the same for gaussian splats. |

Carrying them across a Rerun upgrade: re-apply the changes to the new vendored copy (each is local), then re-check the properties the hand-off rests on, which no API promises: pools recycle by reference count, draw data holds its resources by strong handles, renderers draw with nothing but their pipelines and the draw data's bind groups, and no other draw data type shares per-frame state between clones (search the renderers for `Arc<Mutex` and `Cell` fields). The GPU test `retained_draw_data_draws_unchanged_from_a_second_context_on_another_thread` (`decoupled::tests`) fails if a retained point cloud is recycled or drawn differently; extend it to any new draw data type. `draw_data_copy.rs` must list every new renderer.

## Alternatives

- **One context, shared under a lock.** The UI pass holds Rerun's context from `begin_frame` to `before_submit`, so the render thread would wait for the whole scene update.
- **Several eye-only frames per scene on Rerun's context.** Reuses a scene for several eye frames, but still on the UI thread, before or after the UI pass, never during it.
- **A minimal draw path of our own for retained scenes.** Draw data contents are private to `re_renderer`; it would mean reimplementing every renderer.
- **Persistent variants of draw data or of the belt in `re_renderer`.** A patch across every renderer; unnecessary while pools recycle by reference count.
- **Sink on the UI thread, proxied from the render thread.** Every begin and show would wait for the UI thread again.
- **A second queue for the render thread.** wgpu 30 exposes one queue per device.

## Measuring

```sh
QAROS_RERUN_PERF_LOG=runs/NAME.perf.csv \
    ./target/release/qaros-rerun --headless 1280x800 --out runs/NAME.images --frames N \
    --write-every 100000 --motion still --quit-when-done --decoupled-render &
sleep 3; ./target/release/log-test-scene 300 rerun+http://127.0.0.1:9876/proxy 5000
```

`QAROS_RERUN_PERF_LOG` writes a CSV with the timing of each phase of a frame; the gaps between a user's `frame_shown` events are the eye intervals. `log-test-scene`'s third argument adds that many small point cloud entities for a scene that updates slowly; `QAROS_RERUN_SCENE_DELAY_MS` sleeps after every UI pass as a stand-in for a slow update. The exit summary's per-user frames in flight are counted in eye frames that drew, not in viewer frames, so that column does not compare between the flag off and on; begin to show in milliseconds does.
