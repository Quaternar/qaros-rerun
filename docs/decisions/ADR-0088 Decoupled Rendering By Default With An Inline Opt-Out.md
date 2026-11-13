---
id: ADR-0088
title: The eyes render on a render thread of their own by default, inline where that cannot run or with --inline-render
status: proposed
date: 2026-11-02
repo: qaros-rerun
---

# ADR-0088: The eyes render on a render thread of their own by default, inline where that cannot run or with `--inline-render`

> Amends ADR-0058: the decoupled render loop is the default instead of an opt-in. Amends ADR-0081: on a one-queue GPU in a window the default renders inline instead of refusing; only `--decoupled-render` keeps the refusal.

## Context

ADR-0058 put eye rendering on a render thread of its own behind `--decoupled-render`, off by default, so head motion in the glasses no longer waits for Rerun's scene update. ADR-0081 made it run in a window with the QAROS Vulkan sink on GPUs whose graphics queue family has a second queue, and parks the render thread across a window resize. The owner's request (2026-11-02): decoupled rendering becomes the default, with a flag to opt out, existing scripts that pass `--decoupled-render` keep working, and a fallback from the default is loud: a warning, and the render mode in the startup log of every run.

Two combinations cannot run decoupled: no sink (`--sink off`, nothing to render for), and the QAROS Vulkan sink in a window on a GPU whose family 0 has one queue (MoltenVK, the Legion's Intel iGPU; ADR-0081 [VKI]), where eframe's submits and presents would race the render thread's raw batches. The sink, the QAROS session and the panel hook are built on the thread that renders and stay there (ADR-0060), so the choice must be made before anything is built. The viewer's Vulkan device is created only after onboarding, on the adapter the session names ([CODE] `qaros_setup::build`).

## Decision

- **Three states.** `RenderLoop` in `options.rs`: `DecoupledWherePossible` without a flag, `Decoupled` with `--decoupled-render`, `Inline` with `--inline-render`. The two flags exclude each other; either may be repeated.
- **The default falls back before anything is built.** `render_loop::settle` runs in `main` after logging is set up. Without a sink, or for the QAROS Vulkan sink in a window on a GPU whose wgpu queue family has one queue, it switches to `Inline` and logs a WARN line that names the reason and the flags that choose a mode (`--inline-render` without the warning, `--decoupled-render` to refuse instead). The GPU is forecast without creating a device (`vulkan_device::RawQueueForecast`: the picker, adapter and family-0 queue count that `VulkanDevice::create` would use) on `--vulkan-device`, else on the guess `qaros_setup` makes for a session that names no adapter (the first discrete NVIDIA GPU, else the first discrete GPU, else the first device). A GPU whose queues cannot be read also falls back.
- **Every run says how it renders.** Once the sink and the viewer's device exist, one INFO line gives the loop and where raw batches go, or the fallback's reason: `render: decoupled (raw batches on queue 1)`, `render: decoupled (raw batches share wgpu's queue under the submission lock)`, `render: decoupled` (no device of the viewer's own), `render: inline (--inline-render)`, `render: inline, because ...` (`render_loop::summary`).
- **`--decoupled-render` insists.** It keeps today's refusals: at parse time without a sink, and at sink creation on a one-queue GPU in a window (`qaros_setup::check_decoupled_window`), whose message now names `--inline-render` too.
- **A forecast that missed.** When the session names a one-queue GPU other than the one forecast, the default meets the sink-creation check after the session is joined; it refuses there with a message that names `--inline-render`, `--headless` and `--vulkan-device`, since the session and the sink cannot move to the UI thread once built.

## Alternatives considered

- **Fall back at sink creation.** The session, the sink and the panel hook already live on the render thread then, bound to it by QAROS's single-thread rule; falling back would mean leaving the session and onboarding again on the UI thread in the same process, which the viewer never does: its QAROS teardown ends the process, and the QAROS libraries stay loaded until then ([CODE] `main.rs`, `exit_code_after_teardown`).
- **Forecast every GPU and fall back if any has one queue.** The Legion's Intel iGPU has one queue, so the Legion, the machine the windowed headset runs happen on, would never run decoupled.
- **Keep the opt-in and change the scripts.** Leaves every other launch (Hub launches, the integration guide's commands) on the slower path the owner asked to leave.

## Consequences

- Positive: head motion stays smooth by default while the scene updates slowly; scripts with `--decoupled-render` parse as before.
- Positive: a one-queue GPU in a window, the Mac's `--sink off` viewer and the default CPU sink keep starting, with a warning saying why the eyes are drawn inline.
- Negative: every start with the QAROS Vulkan sink in a window creates a Vulkan instance once more, before onboarding, to read the GPU's queues.
- Negative: the forecast is a guess at the session's GPU; when the session names a different one-queue GPU, the default refuses after joining the session instead of falling back.
- Negative: the image sink and headless runs are decoupled by default too, so their eye frames come from the render thread's loop, as with `--decoupled-render` before.

## Evidence and sources

- [CODE] `crates/qaros-rerun/src/render_loop.rs` (`settle`), `crates/qaros-rerun/src/options.rs` (`RenderLoop`), `crates/qaros-rerun/src/qaros_setup.rs` (`build`, `check_decoupled_window`), `crates/vulkan-device/src/device.rs` (`RawQueueForecast`).
- [VKI] `vulkaninfo` on the Legion and on an Apple M5 Pro with MoltenVK, 2026-11-02, as cited in ADR-0081: the RTX 5070 Laptop GPU's family 0 has 16 queues, the Intel iGPU's and MoltenVK's one.

[CODE]: ../../crates/qaros-rerun/src/render_loop.rs "render_loop"
