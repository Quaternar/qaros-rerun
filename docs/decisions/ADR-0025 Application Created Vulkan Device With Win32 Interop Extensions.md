---
id: ADR-0025
title: The viewer creates its own Vulkan device, with the Win32 interop extensions
status: accepted
date: 2026-10-06
repo: qaros-rerun
---

# ADR-0025: The viewer creates its own Vulkan device, with the Win32 interop extensions

## Context

In the QAROS Vulkan model, as the public StereoKit branch `feature/qar-vulkan-0.4` and the Vulkan cube example show it, QAROS adopts the application's Vulkan device, on the adapter the session picked. Rerun requests no device features, keeps `wgpu_options` crate-private, and wgpu never enables `VK_KHR_external_semaphore_win32` or `VK_KHR_win32_keyed_mutex`.

## Decision

A `vulkan-device` crate creates a wgpu device on a `VkDevice` the application opens: wgpu creates the instance; a picker chooses the physical device by name, UUID, LUID or index (`VkPhysicalDeviceIDProperties`); extra extensions are checked first, then added through `open_with_callback` with Rerun's own features and limits; the device goes to eframe and the headless harness as `WgpuSetup::Existing`. The crate exposes raw handles, wraps foreign `VkImage`s as wgpu textures, and stages semaphores or raw batches. On Windows the QAROS set is `VK_KHR_external_memory_win32` and `VK_KHR_external_semaphore_win32` (`QAROS_WINDOWS_EXTENSIONS`, from the cube's `renderer.cpp` L128-129).

## Alternatives considered

- egui's own device creation: cannot add extensions and cannot be handed to QAROS.
- Rerun's `force_wgpu_backend`: refuses `dx12` and does not expose the device.

## Consequences

- Positive: byte-identical renders on MoltenVK; on Windows the device matches the session's LUID and the validation layer stays silent.
- Negative: bypasses egui's surface-aware adapter choice; a windowed viewer needs the display GPU.
- Negative: wgpu-hal opens only queue 0 of family 0, shared with QAROS; an unsupported extension panics inside wgpu-hal unless checked first.

## Evidence and sources

- [VD] `docs/vulkan-device.md`; `crates/vulkan-device/src/device.rs`, `picker.rs`; `crates/qaros-rerun/src/device_setup.rs`.
- [EV] `docs/evidence/vulkan-device-run.log`.
- [SKQ] StereoKit branch `feature/qar-vulkan-0.4`, `StereoKitC/qar_vulkan.h`.
- [CUBE] QAROS Vulkan cube example at `b1338df`.

[VD]: ../vulkan-device.md "Vulkan device"
[EV]: ../evidence/vulkan-device-run.log "Vulkan device run"
[SKQ]: https://github.com/Quaternar/StereoKit/blob/feature/qar-vulkan-0.4/StereoKitC/qar_vulkan.h "qar_vulkan.h"
[CUBE]: https://github.com/Quaternar/qaros/tree/b1338df/qar-streaming-c/examples/vulkan_cube "QAROS Vulkan cube example"
