# The QAROS Vulkan path

`--sink qaros --qaros-graphics vulkan` (the default for `--sink qaros` on Windows) renders each eye of every user straight into the images of that user's QAROS Vulkan render sender, on a Vulkan device the viewer creates on the GPU the session reports; no pixel goes through the CPU on its way out. It follows the public QAROS header and Vulkan cube example at `b1338df` ([CUBE]: header `qar-streaming-c/include/qar_streaming.h`, example `qar-streaming-c/examples/vulkan_cube/`), through the `vulkan` feature of the `qaros` crate ([API]: `qar-streaming-rust/docs/vulkan-api.md` in the QAROS repository). The device side is [vulkan-device.md](vulkan-device.md).

## Startup: session, then device

QAROS creates the sender's images on the application's device, and the session's devices on the adapter it chose for the session, so the order follows the header's "Basic flow" at `b1338df` (`qar_streaming.h` L39-64; `crates/qaros-rerun/src/main.rs`, `device_setup.rs`, `qaros_setup.rs`):

1. **Session.** `connect` onboards (or rejoins) without an adapter: QAROS puts the session on the GPU carried in the launch invite, else the library picks one. With `--launched-by-qaros` it onboards with the launch invite (below).
2. **Adapter.** `qar_session_get_graphics_device_id` (`Session::graphics_adapter`) names the session's adapter; `QarosConnection::adapter` keeps it. The launch invite carries a GPU only when the target advertised one (`onboarding-and-sessions.mdx` L250), so a session may name none: the device then goes on the first discrete NVIDIA GPU, else the first discrete GPU, else the first device (`PhysicalDeviceSelector::Discrete`), with a warning, and the sink takes it as it is.
3. **Device.** `VulkanDevice::create` on the physical device with that LUID (Windows) or UUID, with `VK_KHR_external_memory_win32` and `VK_KHR_external_semaphore_win32` added on Windows (`QAROS_WINDOWS_EXTENSIONS`, the cube's `renderer.cpp` L128-129) and Rerun's own features and limits. An adapter that is not there stops the viewer with the list of candidates. egui gets the same device as `WgpuSetup::Existing`.
4. **Sink.** `QarosVulkanStream::on_device` checks that the device is on the session's adapter and has the extensions QAROS lists (`qaros::vulkan::required_device_extensions`), then `VulkanSink` wraps it. One render sender is created per target the viewer renders for (targets from the peer list, see the README), with the device's `VkPhysicalDevice` and `VkDevice` (`QarStreamParamsVulkan`), separated textures (one color and one depth image per eye, as the cube requests), the app volume, `--color-format` and `D32_FLOAT` depth.

`--vulkan-device SELECTOR` overrides the order: the device is created first on the named physical device, and its adapter is chained into onboarding as `QarGraphicsDeviceId` (its LUID on Windows, its UUID elsewhere; the header accepts LUIDs on Windows only).

## One frame

| Step | Where | What |
| --- | --- | --- |
| Begin | `QarosVulkanStream::try_begin_frame` | Per user without an open frame: `qar_render_sender_try_begin_frame` on that user's sender (non-blocking, as in the cube), then the frame's Vulkan record: views, images with their current layout and queue family, the semaphore. Each eye's color view index gives its pose and field of view. |
| Wrap | `VulkanSink::begin_frames` | Each image becomes a `wgpu::Texture` (`texture_from_vk_image`, render attachment use, empty drop callback since QAROS owns image and memory), again every frame. Frames are keyed by user and a per-user frame id. |
| Render | `HeadsetRenderer` | During the 3D view's pass the two eye views of every begun frame are built from the same draw data; after the UI pass they are drawn and composited into a texture of the viewer's output format, in a submission of their own on the viewer's queue (`draw_and_submit`, `eye_render.rs`). Each eye's picking layer keeps its depth on the GPU (`ViewBuilder::new_with_picking_depth_on_gpu` in the patched `re_renderer`); nothing is read back. |
| Output | `EyeOutput` (`eye_output.rs`, `.wgsl`) | In the same viewer frame: one fullscreen pass per eye writes the composited color into the sender's color image, transparent where nothing was drawn (with `--frame-stamp`, a small stamp texture over the top rows wins there, at 1 m), and samples the picking layer's reverse-Z depth, converts it to the `--depth` encoding and writes it into the depth image through `frag_depth`; a depth image of another size than color gets a pass of its own ([below](#depth-at-a-lower-resolution)). |
| Hand over | `HandoverQueue::submit_handover` (`vulkan-device`) | Three batches on wgpu's queue, chained by binary semaphores of our own: a raw acquire batch moves each image from the layout and queue family QAROS reported to the attachment layout on wgpu's family; wgpu's submission renders; a raw release batch moves each image to `TRANSFER_SRC_OPTIMAL`, releases it to `VK_QUEUE_FAMILY_EXTERNAL` and signals the frame's semaphore. |
| Write back, show | `QarosVulkanStream::show_frame` | Every image is recorded as `TRANSFER_SRC_OPTIMAL` on `VK_QUEUE_FAMILY_EXTERNAL`, the fence as null, then `qar_render_sender_show_frame` with the near and far planes used, in app units (room meters divided by the app scale) with `--pose-space app`, the default. |
| Discard | `VulkanSink::abandon_frame` | A begun frame that is not shown (no 3D view drew it, an eye failed to draw, the viewer stops, its user leaves) gets its semaphore signalled by an empty batch before the frame is dropped, as the cube's `Discard` does (`renderer.cpp` L798-814). A user's sender is dropped only after the device is idle. |

The barriers are the cube's (`renderer.cpp` L635-675 and L757-793): same layouts, same queue family transfers, same order of submit, write-back and show. Two things differ from the cube, both because wgpu sits in between:

- **Wrapping per frame.** wgpu assumes a wrapped image starts `UNDEFINED` and tracks its layout from there; QAROS hands the images back each frame in the layout the release left them. Wrapping again every frame keeps wgpu's view right: its first use discards the content (every pixel is rewritten anyway), and the acquire batch has already taken the image over. A wrapped texture costs a hal texture object per image per frame, nothing else.
- **Final layout from wgpu's tracking.** The release barrier's old layout is the one wgpu-hal leaves after a render pass, `COLOR_ATTACHMENT_OPTIMAL` or `DEPTH_STENCIL_ATTACHMENT_OPTIMAL` (`wgpu-hal-30.0.0/src/vulkan/conv.rs` L240-260); wgpu does not transition at the end of a submission.

The frame is shown right after its batches are submitted, as the cube shows right after its submission (`main.cpp` L201-214); the semaphore orders QAROS's reads after them, and the cube's fence only guards the reuse of its own command slot (`renderer.cpp` L775-788).

Teardown: the headset releases its open frames (each discarded, semaphore signalled) before the sink goes; the sink waits for the device before the sender, and with it the images, is destroyed.

### Why the extra pass

`re_renderer` 0.40.0 composites a view only into a render pass of its output format (`ViewBuilder::composite`, the window's format), and keeps its depth buffers private. So color goes through a composited texture, and depth comes from the picking layer, the same depth the CPU path reads back.

The main depth buffer is not used: it is multisampled (4x), created as a render attachment only and discarded after the main pass, so reaching it would need a sampleable usage, a stored result and a choice of sample per pixel, and its coverage at silhouettes would no longer match the CPU path's. The picking layer is single-sampled, at the eye's full resolution, and stored. The patch to `re_renderer` adds a constructor that draws it without scheduling the readback (no copy of 20 bytes per pixel, no readback buffer) and makes its depth target sampleable, plus an accessor; `ViewBuilder::new` behaves as published.

The output pass then writes any color format QAROS hands out, converts reverse-Z to the `--depth` encoding in the shader (forward or reverse normalized; linear meters cannot go through a depth attachment and is refused, as before), writes it with any depth format, and clears uncovered pixels to transparent from that same depth, as `clear_uncovered_pixels` does on the CPU. Nothing goes through the CPU, and a frame is shown in the viewer frame its eyes were drawn in.

### Depth at a lower resolution

QAROS reprojects every received frame to the viewer's latest head pose using the frame's depth ([MIXER]), so every frame carries it. The header lets a frame carry depth at a lower resolution than color (every `QarVideoTextureVulkan` has its own size), but gives the application no way to ask for it: the init has no sizes, and whether `qar_render_sender_change_layout` accepts a smaller depth texture is not said. So the viewer follows whatever the sender hands out:

- depth of the color's size is written in the color's pass, pixel for pixel, as above;
- depth of another size gets a pass of its own after the color pass. Each depth pixel takes the nearest depth (largest reverse-Z) of the eye pixels it covers, then the same conversion to the `--depth` encoding. Nearest rather than averaged, so drawn content is never pushed behind what another app volume shows, and an edge pixel half covered counts as covered.

`--depth-scale-down N` (with `--image-targets gpu`) makes the image sink's targets hand out depth N times smaller in each dimension, to test and time that pass on the Mac. The written PNGs and raw dumps repeat each depth pixel N x N times to keep the eye's size.

## Launched by QAROS

`--launched-by-qaros` implements the API app contract of [LAUNCH] as of `b1338df`: the first line of stdin is the JSON launch invite (`qar_onboarding_invite_deserialize`, then onboarding with it), or a code when it does not start with `{`; it is used for every start (never stored, never logged, no rejoin); the GPU travels in the invite, and the viewer creates its device on the adapter the session then reports; the viewer creates its own app volume "rerun"; end of stdin closes the viewer, windowed or headless. The operator enters the viewer's other arguments in the visualizer's Source Applications panel, one per line, for example `--sink`, `qaros`, `--qaros-graphics`, `vulkan`, `--launched-by-qaros`.

## Running it on the Mac

As in [vulkan-device.md](vulkan-device.md#running-on-the-mac): Homebrew's `molten-vk` and `vulkan-loader`, `DYLD_FALLBACK_LIBRARY_PATH=/opt/homebrew/lib`, and the toolchain's own `cargo` for tests. `qaros-stub` emulates the Vulkan sender on the caller's device: plain images and a binary semaphore, a check of the write-back at every show and a wait on the semaphore ([API], "Against `qaros-stub`").

```sh
export DYLD_FALLBACK_LIBRARY_PATH=/opt/homebrew/lib
PATH="$(dirname "$(rustup which cargo)"):$PATH" cargo test -p vulkan-device -p qaros-sink --features qaros-sink/vulkan

# The viewer against the stub, as QAROS would launch it: code on stdin. --vulkan-device chains
# the device's adapter into onboarding, which the stub logs; without it the device goes on the
# discrete fallback, since the stub reports no adapter. The tests above built the stub.
(echo "qar-code:test"; sleep 20) | QAROS_DLL="$(ls -t target/debug/deps/libqaros_stub-*.dylib | head -n 1)" \
    QAROS_STUB_REPORT=stub-report.txt \
    ./target/debug/qaros-rerun --headless 1600x1000 --sink qaros --qaros-graphics vulkan \
    --vulkan-device default --launched-by-qaros --color-format rgba &
./target/debug/log-test-scene 300

# The same renderer path at full size into PNGs: textures of the sink, read back after each frame.
# --raw-depth adds frame_N_depth.f32 for an exact comparison with --image-targets cpu.
./target/debug/qaros-rerun --headless 1600x1000 --out out/vk-gpu --frames 60 --write-every 30 \
    --motion still --quit-when-done --vulkan-device default --image-targets gpu --raw-depth &
./target/debug/log-test-scene 300

# Any of the above under the Khronos validation layer (brew install vulkan-validationlayers).
export VK_LAYER_PATH=/opt/homebrew/opt/vulkan-validationlayers/share/vulkan/explicit_layer.d
export VK_INSTANCE_LAYERS=VK_LAYER_KHRONOS_validation VK_KHRONOS_VALIDATION_REPORT_FLAGS=error,warn
export VK_KHRONOS_VALIDATION_DEBUG_ACTION=VK_DBG_LAYER_ACTION_LOG_MSG VK_KHRONOS_VALIDATION_LOG_FILENAME=validation.txt
```

What the Mac shows ([evidence/vulkan-path/runs.log](evidence/vulkan-path/runs.log)):

- **Viewer against the stub:** the device is created on the MoltenVK device, the session is onboarded with the launch code and the device's UUID chained (`adapter uuid=...` in the stub's log), the sender gets four 64x64 images, 1433 frames are shown with every write-back accepted and every semaphore consumed, no stub error, each in the viewer frame its eyes were drawn in; closing stdin stops the viewer with exit status 0 and teardown runs in order.
- **GPU targets into PNGs** (`--image-targets gpu`, [evidence/vulkan-path](evidence/vulkan-path)): the color PNGs of frames 0 and 30 are byte-identical to the CPU path's on the same device, and for frame 0 to the Metal evidence in [evidence](evidence); the output pass reproduces Rerun's screenshot path exactly, transparency included. Raw depth (`--raw-depth`) differs from the CPU path's in about 1.3% of the pixels, by at most 3 ULPs (1.8e-7); with MoltenVK's fast math off (`MVK_CONFIG_FAST_MATH_ENABLED=0`) for both runs, color, depth PNG and raw depth are byte-identical, so the difference is the fast-math division of the conversion in the shader, not the depth itself. With `--depth-scale-down 2`, color is still byte-identical, and every depth pixel equals the smallest forward depth of its 2x2 block in the CPU run, within 1.8e-7.
- **Per-frame time** (release builds, same scene, 300 frames, three runs each; [runs.log](evidence/vulkan-path/runs.log) section 4): with the readback, 28.8 ms per frame at 960x1000 per eye, 61.6 ms at 1832x1920 and 18.2 ms against the stub (64x64); with depth on the GPU, 16.6, 45.2 and 9.7 ms. Begin-to-show drops by about one viewer frame. With depth at half the resolution (`--depth-scale-down 2`), 15.6 and 39.3 ms; at a quarter, 15.8 and 33.7 ms (section 5). The image-sink rows include that sink's own readback of its targets with a device wait per frame, which shrinks with the depth too, so they overstate what the renderer itself saves. These are Apple GPU numbers through MoltenVK and do not predict an NVIDIA GPU.
- **Tests:** `HandoverQueue` acquires an image from `UNDEFINED` and then from the external family, renders into it with wgpu and releases it, three frames in a row, with a consumer batch that acquires, copies and gives the image back (`crates/vulkan-device/tests/handover.rs`); `VulkanSink` over a local stream with its own images and semaphore checks pixels, depth values, written-back states, one frame in flight, the stop after N frames, the semaphore of an abandoned frame and of a frame open at drop, and the refusal of linear depth (`crates/qaros-sink/tests/vulkan_sink.rs`); `QarosVulkanStream` against the stub checks the adapter on rejoin, the refusal of a session without adapter, three frames' pixels read back from the sender's images, the stub's write-back check, a discarded frame and a clean teardown (`crates/qaros-sink/tests/stub.rs`); `EyeOutput` on the Vulkan device writes depth within 1e-6 of `xr_math`'s conversion in both normalized encodings, keeps the nearest depth of each footprint for a smaller depth target, clears uncovered pixels unless the background is opaque, and refuses linear depth for a depth target (`crates/qaros-rerun/src/eye_output.rs`); `VulkanSink` hands out depth images smaller than color at their own size.
- **Khronos validation layer** (Homebrew's `vulkan-validationlayers` 1.4.363, `VK_INSTANCE_LAYERS=VK_LAYER_KHRONOS_validation`, messages to a file): the handover, sink and stub tests report nothing, and the viewer against the stub and the image sink with `--image-targets gpu` report only `VUID-StandaloneSpirv-None-10684`, from the SPIR-V naga generates for Rerun's shaders, the same count as a run with `--image-targets cpu`. With the release barrier's old layout deliberately wrong the layer reports the mismatch, so it does track these images. MoltenVK ignores most layout errors on its own, so this is the check that matters on the Mac.

## What the Mac does not exercise

- **Real external memory.** On MoltenVK the stub's images are plain device-local images and its semaphore a plain binary semaphore; the release to `VK_QUEUE_FAMILY_EXTERNAL` and the acquire back are legal there but exercise none of the Win32 export. On Windows the images are shared with QAROS's encoder through `VK_KHR_external_memory_win32`.
- **The adapter by LUID.** The LUID `qar_session_get_graphics_device_id` reports, matched against `deviceLUID`, and the device created there.
- **Image usage and formats.** The header reports format features but not usage flags; the sink wraps the images for render attachment use only. The stub creates them with attachment usage.
- **Queue access.** wgpu and the release batches submit from the viewer's UI thread to queue 0 of family 0. If QAROS submits to the same `VkQueue` from its own threads (the stub does so inside `show_frame`, on the calling thread), the two must not overlap.
- **Depth size.** The "streaming to peer ... views" line lists each image's size, so the log shows whether QAROS hands out depth at the color's resolution or smaller (the resampled pass then runs).
- **The viewer build itself.** `vulkan-device`, `frame-sink` with `gpu` and `qaros-sink` with `vulkan` check and pass clippy for `x86_64-pc-windows-msvc` from the Mac; the viewer crate is built and checked on Windows.

To run the path on Windows, see the [integration guide](integration-guide.md#choose-the-path-vulkan-or-cpu).

## Code

| Piece | File |
| --- | --- |
| QAROS's extensions, the named device, egui's setup | `crates/qaros-rerun/src/device_setup.rs`, `main.rs` |
| Launch invite, device on the session's adapter, stdin watcher, sink choice | `crates/qaros-rerun/src/qaros_setup.rs`, `viewer_app.rs`, `headless.rs` |
| The session's adapter, targets from the peer list | `crates/qaros-sink/src/connection.rs`, `users.rs` |
| Eye views composited, picking depth on the GPU, output pass | `crates/qaros-rerun/src/headset.rs`, `eye_render.rs`, `eye_output.rs`, `eye_output.wgsl` |
| Picking layer drawn without readback, its depth sampleable | `patches/re_renderer/src/view_builder.rs`, `src/draw_phases/picking_layer.rs` |
| `GpuFrameSink`, `GpuReadbackSink` | `crates/frame-sink/src/gpu.rs`, `gpu_readback.rs` |
| `VulkanSink` over a per-user `VulkanFrameStream`, `QarosVulkanStream` with a sender per user | `crates/qaros-sink/src/vulkan/` |
| Acquire and release batches | `crates/vulkan-device/src/handover.rs` |

[MIXER]: https://github.com/Quaternar/qaros/blob/b1338df/docs/website/docs/operator-guide/the-mixer.md "QAROS mixer: late-stage reprojection"
[CUBE]: https://github.com/Quaternar/qaros/tree/b1338df/qar-streaming-c/examples/vulkan_cube "QuaternAR qaros at b1338df, the Vulkan cube example"
[API]: https://github.com/Quaternar/qaros/blob/442a98026ec9c0748127a7c52b24f2174a5593e7/qar-streaming-rust/docs/vulkan-api.md "QAROS Rust binding: the Vulkan path in the qaros crate"
[LAUNCH]: https://github.com/Quaternar/qaros/blob/b1338df/docs/website/docs/developer-guide/onboarding-and-sessions.mdx "Onboarding and Sessions at b1338df, section Launched by QAROS: QAROS API apps"
