# Vulkan device: Rerun on a device the viewer creates

QAROS's Vulkan API adopts the application's Vulkan device for streaming interop, and QuaternAR's StereoKit fork picks the GPU through a callback that receives the `VkInstance` and returns the `VkPhysicalDevice`, "because every shared texture and semaphore is bound to the adapter the session already picked" ([SKQ]). This note covers the part of that path which does not need QAROS itself, and runs on an Apple-silicon Mac through MoltenVK: Rerun renders on a Vulkan device the viewer creates, on a physical device it chooses, with device extensions wgpu would not enable, and the raw handles, image wrapping and semaphores the QAROS sink uses are in place and tested. The code is `crates/vulkan-device` and `crates/qaros-rerun/src/device_setup.rs`.

Sources: the crates as published on crates.io, wgpu 30.0.0, wgpu-hal 30.0.0, ash 0.38.0, egui-wgpu 0.36.2, re_renderer 0.38.1 and re_viewer 0.38.1, and [SKQ]. The line references below are to those versions (Rerun 0.38.1, egui 0.36.2), not to the Rerun 0.40.0 the viewer builds against.

## Running on the Mac

wgpu's Vulkan backend on macOS is compiled in already (Rerun enables wgpu's `vulkan-portability`), but it needs a Vulkan loader at run time: wgpu-hal calls `ash::Entry::load()` (`wgpu-hal-30.0.0/src/vulkan/instance.rs` L583), which opens `libvulkan.dylib` by leaf name (`ash-0.38.0+1.3.281/src/entry.rs` L80).

```sh
brew install molten-vk vulkan-loader vulkan-tools    # vulkan-tools only for vulkaninfo
export DYLD_FALLBACK_LIBRARY_PATH=/opt/homebrew/lib
./target/debug/qaros-rerun --headless 1600x1000 --out out/vk --frames 60 --write-every 30 \
    --motion still --quit-when-done --vulkan-device name:apple --vulkan-extension VK_EXT_metal_objects &
./target/debug/log-test-scene 300
```

- `DYLD_FALLBACK_LIBRARY_PATH=/opt/homebrew/lib` is the only variable needed. Homebrew's loader finds MoltenVK through `/opt/homebrew/etc/vulkan/icd.d/MoltenVK_icd.json` on its own; `VK_ICD_FILENAMES` (or `VK_DRIVER_FILES`) pointing at that file is only needed with another loader.
- Homebrew's `cargo` is a shell script, and macOS strips `DYLD_*` variables when it starts `/bin/bash`, so `cargo test` and `cargo run` never pass the variable on. Use the toolchain's own binaries: `PATH="$(dirname "$(rustup which cargo)"):$PATH"`. Without a loader the Vulkan tests print `skipped` and pass.
- `--graphics-backend vulkan` alone keeps egui's device creation and only narrows the instance to Vulkan; `--vulkan-device SELECTOR` creates the device here (selectors: `default`, `name:TEXT`, `uuid:HEX`, `luid:HEX`, `index:N`), and `--vulkan-extension NAME` adds device extensions, `windows-interop` standing for the QAROS Windows set.

### What the Mac shows

- **Headless**, on the app-created device (log excerpt in [evidence/vulkan-device-run.log](evidence/vulkan-device-run.log)): the picker lists the MoltenVK device, the device is created with `VK_EXT_metal_objects` added to wgpu's own extensions, egui reports `backend: Vulkan ... driver: "MoltenVK", driver_info: "1.4.2"`, and the image sink shows 60 of 60 stereo frames. The color and depth PNGs of frames 0 and 30 are byte-identical to a Metal run with the same arguments and to the README's Metal evidence for frame 0, and so are those of `--graphics-backend vulkan` with egui's own device.
- **Windowed**, on the app-created device: the window presents, and 90 of 90 frames are shown with the same eye PNGs. No surface error, so MoltenVK's `VK_EXT_metal_surface` swapchain works on a device egui did not choose.
- **Tests** (`crates/vulkan-device/tests`, run on MoltenVK): the picker receives the instance and its choice becomes the device; handles read back from the wgpu device match; selection by name; an unsupported extension and an unmatched LUID are refused before `vkCreateDevice`; a `VkImage` created with ash on the device is wrapped, cleared by a wgpu render pass, read back pixel by pixel, and its owner's drop callback runs once wgpu drops it; wgpu signals a staged timeline semaphore; a staged wait holds wgpu work until the host signals; raw batches chain before and after a wgpu submission through binary semaphores.
- MoltenVK logs pipeline warnings Rerun's shaders trigger (no primitive restart disable on Metal, blending declared on integer attachments); rendering is unaffected.
- MoltenVK 1.4 reports a valid LUID (`000000010000074b` on the Mac measured), so the LUID path of the picker runs here too.

## How the device is made

`VulkanDevice::create` (`crates/vulkan-device/src/device.rs`) follows the order the QAROS model implies:

1. **Instance.** wgpu creates it from Rerun's instance descriptor with the backends narrowed to Vulkan; `wgpu::Instance::as_hal::<Vulkan>()` gives the `ash::Instance`.
2. **Picker.** `PhysicalDevicePicker` receives that instance and returns `Ok(Some(VkPhysicalDevice))`, `Ok(None)` for the default (wgpu's first Vulkan adapter), or an error. `PhysicalDeviceSelector` builds pickers from name, UUID, LUID or index, using `vkGetPhysicalDeviceProperties2` with `VkPhysicalDeviceIDProperties`. The picked handle is matched against `wgpu::Adapter::as_hal::<Vulkan>().raw_physical_device()`; a device wgpu does not expose is an error, not a silent fallback.
3. **Extensions.** Each extra extension is checked with `PhysicalDeviceProperties::supports_extension` first, because an unsupported one makes `vkCreateDevice` fail with `VK_ERROR_EXTENSION_NOT_PRESENT`, which wgpu-hal turns into a usage-error panic (`adapter.rs` L3044-3050, `hal_usage_error` in `lib.rs` L493-495).
4. **Device.** `wgpu_hal::vulkan::Adapter::open_with_callback` (`adapter.rs` L2995) with Rerun's own features, limits and memory hints (`DeviceCaps::from_adapter_without_validation(adapter).device_descriptor()`), the callback appending to `CreateDeviceCallbackArgs::extensions` (`mod.rs` L1703-1724); then `wgpu::Adapter::create_device_from_hal` (`wgpu-30.0.0/src/api/adapter.rs` L77) with the same descriptor.
5. **Hand-over to egui.** `egui_wgpu::WgpuSetup::Existing { instance, adapter, device, queue }`, for eframe (`re_viewer::native::eframe_options` with the setup replaced, then `re_viewer::customize_eframe_and_setup_renderer`) and for the headless harness alike.

What egui's own setup does that this one does not:

- **Surface-aware adapter choice.** Rerun's selector drops adapters that cannot present to the window (`re_renderer-0.38.1/src/device_caps.rs` L449-467). With an existing device, egui creates the surface afterwards and asks the given adapter for its formats (`egui-wgpu-0.36.2/src/lib.rs` L261-278); an adapter that cannot present fails there, at startup. The picker must name the GPU that drives the display, or the viewer must run headless.
- **Rerun's adapter ranking** (`select_adapter`, same file from L436) only applies with `default`, and then as wgpu's order, not Rerun's.
- **One queue.** wgpu-hal opens queue 0 of queue family 0 and nothing else (`adapter.rs` L3005, `let family_index = 0; //TODO`). Family 0 is the graphics queue on NVIDIA and MoltenVK; QAROS gets that same queue, not a dedicated one.

## The handles and helpers the QAROS sink uses

| Need | API | Tested on the Mac |
| --- | --- | --- |
| Raw objects to give QAROS | `VulkanDeviceHandles` (the `VulkanDevice::handles` field, or `VulkanDeviceHandles::of(&wgpu::Device)` for any Vulkan device): `instance`, `get_instance_proc_addr`, `instance_api_version`, `physical_device`, `device`, `queue_family_index`, `queue_index`, `queue`, enabled extensions | yes, against the device's own hal handles |
| A QAROS-owned `VkImage` as a render target | `texture_from_vk_image(device, image, descriptor, on_drop)`: wgpu-hal `texture_from_raw` with external memory and a drop callback (`wgpu-hal-30.0.0/src/vulkan/device.rs` L296), then `create_texture_from_hal` | yes: render, readback, drop callback |
| Wait for QAROS before rendering, signal QAROS after | `stage_wait_semaphore`, `stage_signal_semaphore`: wgpu-hal's `Queue::add_wait_semaphore` and `add_signal_semaphore` (`mod.rs` L1524-1572), binary or timeline, consumed by wgpu's next submission | yes |
| Keyed mutex or anything else wgpu cannot put on its submission | `submit_sync_batch`: an empty raw `vkQueueSubmit` on wgpu's queue with waits, signals and `VkWin32KeyedMutexAcquireReleaseInfoKHR`, chained to wgpu's work through a semaphore | the semaphore chain yes; the keyed mutex compiles for Windows only |
| Take images over from their owner and hand them back | `HandoverQueue::submit_handover`: a raw acquire batch (reported layout and queue family to the attachment layout), wgpu's submission, a raw release batch (final layout, release to the owner's family, the owner's semaphores signalled), chained by binary semaphores; slots with fences so command buffers are reused safely | yes: three frames from `UNDEFINED` and from `VK_QUEUE_FAMILY_EXTERNAL`, pixels checked by a consumer batch |

Rules that follow from Vulkan and wgpu, documented on the API:

- A semaphore wait only orders the batch it is part of. A raw batch that acquires a keyed mutex therefore signals a binary semaphore that wgpu's submission waits on, and the release batch waits on a semaphore wgpu's submission signals; the third Mac test runs exactly that chain with a host-signalled gate in place of the keyed mutex.
- Staged semaphores attach to whichever `wgpu::Queue::submit` comes next, including egui's and Rerun's, and queue access must be externally synchronized. Stage right before the sink's own submit, on the UI thread that submits, and do not let QAROS submit to the same `VkQueue` from its own thread unless it serializes with the viewer.
- A wrapped image starts in `VK_IMAGE_LAYOUT_UNDEFINED` for wgpu (content discarded on first use), and wgpu then tracks its layout. An image QAROS hands out again every frame either comes back in the layout wgpu left it in, or is wrapped again per frame.

## Windows and NVIDIA

- No portability layer: the NVIDIA ICD is a native Vulkan driver, `vulkan-portability` and `VK_KHR_portability_subset` play no part, and the loader ships with the driver.
- `--vulkan-extension windows-interop` adds `VK_KHR_external_memory_win32`, `VK_KHR_external_semaphore_win32`, `VK_KHR_win32_keyed_mutex` and `VK_KHR_timeline_semaphore` (`WINDOWS_INTEROP_EXTENSIONS`). wgpu already enables `VK_KHR_external_memory_win32` when offered (`adapter.rs` L1340-1341); the others only this path enables. Timeline semaphores are core in Vulkan 1.2 and wgpu turns the feature on when supported, so the extension name is redundant on current NVIDIA drivers but harmless.
- Picking by LUID: `luid:HEX` matches `VkPhysicalDeviceIDProperties.deviceLUID`, the same eight bytes as DXGI's adapter LUID and QAROS's `QarGraphicsDeviceId` (LUID variant). On a hybrid laptop this is what keeps the viewer on the GPU QAROS's session uses; on a one-GPU VM `default` gives the same device.
- D3D11 textures imported through wgpu-hal's `texture_from_d3d11_shared_handle` (`device.rs` L453) get dedicated memory, which `Texture::memory()` exposes (`mod.rs` L843): that `VkDeviceMemory` is what a keyed mutex acquire in `submit_sync_batch` names.
- `vulkan-device`, `frame-sink` with `gpu` and `qaros-sink` with `vulkan` are checked for `x86_64-pc-windows-msvc` from the Mac (check and clippy, all targets); the viewer itself is built and checked on Windows.

## How the QAROS Vulkan path uses this

[vulkan-path.md](vulkan-path.md) describes the path; this is where it meets the crate.

| QAROS (`qar_streaming.h` and the `vulkan_cube` example at `b1338df`) | Here |
| --- | --- |
| The session's GPU, read with `qar_session_get_graphics_device_id` after onboarding, or chained as `QarGraphicsDeviceId` on onboard or rejoin | `PhysicalDeviceSelector::Luid` (Windows) or `Uuid` picks the device on the session's adapter; with `--vulkan-device` the named device comes first and its LUID or UUID goes to the session |
| The application's own `VkInstance` and `VkDevice`, with the Win32 external memory and semaphore extensions | `VulkanDevice::create` with `QAROS_WINDOWS_EXTENSIONS` on Windows; wgpu creates the instance, which QAROS never sees (it takes only the physical device and the device) |
| `QarStreamParamsVulkan { physical_device, device }` | `VulkanDeviceHandles` of the viewer's device |
| Sender-owned images, re-read every frame | `texture_from_vk_image` per image and frame, with an empty drop callback |
| Transition from the reported layout and family, release to `VK_QUEUE_FAMILY_EXTERNAL` in `TRANSFER_SRC_OPTIMAL`, signal the frame's binary semaphore | `HandoverQueue::submit_handover` |
| A discarded frame still signals the semaphore | `submit_sync_batch` with the signal only |

What the header leaves open for this crate: whether QAROS submits to the adopted device's queues from its own threads (wgpu-hal opens one queue and the viewer submits to it from the UI thread; the stub waits on the semaphore inside `show_frame`, on the caller's thread), the usage flags of its images (not reported), and device features beyond the two extensions (none requested so far). QAROS must stop using the device before the viewer drops it; the sink waits for the device and destroys the sender first.

[SKQ]: https://github.com/Quaternar/StereoKit/blob/feature/qar-vulkan-0.4/StereoKitC/qar_vulkan.h "QuaternAR StereoKit fork, qar_vulkan.h"
