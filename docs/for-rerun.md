# For the Rerun team

`qaros-rerun` streams Rerun's 3D view into AR glasses: every headset gets its own stereo stream, drawn by `re_renderer` from that headset's head pose, and Rerun's 2D views open as panels next to it. Everything else is stock Rerun 0.40.0: the store, the time control, blueprints, the SDKs and the desktop viewer work as they do in `rerun`.

To get there we carry two small patches to Rerun crates, vendored under `patches/` and applied with `[patch.crates-io]`. We would like to upstream them, or whatever API you prefer that makes them unnecessary, so that integrations like this one build against published crates.

## What the patches enable

- **XR and AR output for Rerun.** Any headset runtime (QAROS here, OpenXR or another streaming runtime elsewhere) can draw the 3D view once per eye, per user, from its own poses, without forking the viewer.
- **Any external renderer of the 3D view.** The same hook serves anything that wants Rerun's scene from cameras other than the desktop one: a second monitor or projector, a stereo display, an offscreen camera path for video, a remote render service.
- **Depth without a CPU round trip.** An embedding application can composite Rerun's output with its own content, or hand it to a compositor that reprojects with depth, entirely on the GPU.

All of it stays opt-in: with no observer registered and no new constructor called, both crates behave exactly as published.

## Patch 1: a 3D view observer in `re_view_spatial`

59 lines added, 1 removed, in 4 files.

| File | Change |
| --- | --- |
| `src/view_3d_observer.rs` | new module, 44 lines: the observer type, `set_view_3d_observer`, `is_set`, `notify` |
| `src/ui_3d.rs` | +10/-1: collect the draw data, notify the observer, then queue it as before |
| `src/eye.rs` | +4: while an observer is set, `ExtendedEyeFrustum::last_frame` returns `None` |
| `src/lib.rs` | +1: `pub mod view_3d_observer` |

**The problem.** The draw data the 3D view builds each frame is exactly what a second renderer needs, and 0.40.0 offers no way to reach it from outside the crate. A wrapper view class cannot reuse the `3D` identifier the spatial visualizers check for, visualizers cannot see each other's output, and the data is consumed by the view's own `ViewBuilder`.

**The change.** Right before the 3D view queues its draw data, it hands an optional observer a frame record by reference, on the UI thread:

```rust
pub struct View3DFrame<'frame, 'viewer> {
    pub viewer_ctx: &'frame ViewerContext<'viewer>,
    pub query: &'frame ViewQuery<'frame>,
    pub view_coordinates: ViewCoordinates,
    pub scene_bounding_box: re_math::BoundingBox,
    pub world_from_desktop_view: re_math::IsoTransform,
    pub draw_data: &'frame [QueueableDrawData],
}

pub type View3DObserver = Box<dyn Fn(&View3DFrame<'_, '_>) + Send + Sync>;

pub fn set_view_3d_observer(observer: View3DObserver) -> Result<(), View3DObserver>;
```

The observer clones what it needs (draw data is cheap to clone: it holds its GPU resources by `Arc`) and draws it into its own `ViewBuilder`s after the UI pass.

The `eye.rs` line exists because the 3D view skips encoded images and video frames that lie outside the desktop camera's frustum. Another camera may well see them, so while an observer is registered that culling is off.

## Patch 2: picking depth kept on the GPU in `re_renderer`

55 lines added, 18 removed, in 2 files.

| File | Change |
| --- | --- |
| `src/view_builder.rs` | +29/-1: `ViewBuilder::new_with_picking_depth_on_gpu` and `ViewBuilder::picking_depth_target`; `ViewBuilder::new` unchanged |
| `src/draw_phases/picking_layer.rs` | +26/-17: the readback identifier becomes optional; without it, no readback buffer is allocated and the depth target is created with `TEXTURE_BINDING` |

**The problem.** A headset compositor needs depth for every frame, to reproject to the latest head pose. `ViewBuilder` keeps its depth buffers private. The main depth buffer is multisampled, a render attachment only, and discarded after the main pass. The picking layer's depth is single-sampled, full resolution and stored, which is what we need, but `ViewBuilder` always schedules its readback to the CPU.

**The change.** A second constructor draws the picking layer without scheduling the readback (no readback buffer, no copy of 20 bytes per pixel) and makes its depth target sampleable, and an accessor returns it. Our output pass then samples it in a shader, converts reverse-Z to the encoding the headset runtime wants, and writes it into the runtime's depth image. Nothing goes through the CPU.

### The shader path line

`src/file_server.rs`, +1. `include_file!` strips Rerun's workspace prefix (`crates/viewer_support/re_renderer`) from `file!()` to find embedded shaders. Vendored at `patches/re_renderer`, the prefix differs and release builds panic on the first shader import, so we strip that prefix too. The line is only needed because the crate is vendored; it disappears with upstreaming. A general fix would derive the prefix from the crate's own location rather than a fixed path, so any `[patch]`ed copy works.

## Also carried, separable

These live in the same vendored `re_renderer`. They are smaller asks or specific to us, and we list them so the full diff holds no surprises.

| Change | Size | Why |
| --- | --- | --- |
| Pool reuse grace of 4 frames instead of 1 (`wgpu_resources/dynamic_resource_pool.rs`, `REUSE_GRACE_FRAMES`) | +87/-33, about half of it tests | Eye views are drawn every second to fourth viewer frame per user, so with one frame of grace their full-resolution targets were destroyed and created again almost every eye frame. A configurable grace would serve any renderer with views that are not drawn every frame. |
| `RenderContext::new_sharing_device` and `queue_submission_lock()` (`context.rs`) | part of +75/-10 | A second context on the same device for a render thread, without replacing the first context's uncaptured error handler; a lock so raw Vulkan submissions from another thread never interleave with Rerun's. Used only by our experimental `--decoupled-render`. |
| Hand-written `Clone` for `PointCloudDrawData` and `GaussianSplatDrawData` | +14/-1, +13/-1 | A clone shares its `SortedDrawables` (`Arc<Mutex<_>>`) with the original, so two contexts drawing clones clear and refill one list. A clone that starts its own list fixes that; this may be worth having regardless. |
| `POLL_DEVICE_WAIT_MICROS`, `POOL_RESOURCES_CREATED` counters | a few lines | Our frame timing log. Not proposed. |

The decoupled render loop that uses the second context is described in [decoupled-render.md](decoupled-render.md).

## What an upstream API could look like

A sketch, to start the conversation; we would adapt to whatever shape fits Rerun.

**A registered 3D view observer** instead of a process-wide `OnceLock`, owned by the viewer, for example through `re_viewer::StartupOptions` or a method on `App`:

```rust
pub trait View3DObserver: Send + Sync {
    /// Called on the UI thread right before a 3D view queues its draw data.
    fn on_view_3d_frame(&self, frame: &View3DFrame<'_, '_>);

    /// Whether the view must keep content outside the desktop camera's frustum.
    fn needs_unculled_scene(&self) -> bool {
        true
    }
}
```

The frame record could carry the view id as well, so an observer can pick one of several 3D views (today we stream the first one drawn).

**GPU-side picking depth** as an option of the existing picking configuration rather than a second constructor, for example a readback mode on `ViewPickingConfiguration` (`Cpu(GpuReadbackIdentifier)` as today, or `GpuOnly`), plus the `picking_depth_target` accessor. Alternatively, a single-sampled resolved depth of the main pass, exposed as a sampleable texture, would serve the same purpose without drawing the picking layer at all.

With these two in place, an integration like ours would depend on stock `re_viewer`, `re_view_spatial` and `re_renderer`, and the shader path line would go away with the vendoring.

## Looking at the code

The first commit under `patches/` vendors both crates unmodified from crates.io 0.40.0, so the patch is one diff:

```sh
base=$(git log --format=%h -1 --grep='0.40.0 unmodified')
git diff $base -- patches/re_view_spatial/src patches/re_renderer/src
```

The vendored crates keep Rerun's license, MIT OR Apache-2.0, and we are happy to contribute the changes under it.

To see the result, the [demos](demos/README.md) run Rerun's own examples in the glasses; without a headset, [Developing on a Mac](mac.md) renders the eyes to PNGs on any machine.
