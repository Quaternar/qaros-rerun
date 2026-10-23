---
id: ADR-0037
title: Vulkan device created after onboarding on the session's adapter, with a discrete-GPU fallback
status: accepted
date: 2026-10-13
repo: qaros-rerun
---

# ADR-0037: Vulkan device created after onboarding on the session's adapter, with a discrete-GPU fallback

## Context

At `b1338df` QAROS puts the session on its target's GPU, carried in the launch invite, and the app reads it after onboarding with `qar_session_get_graphics_device_id`; the session's devices and every sender's images live on that adapter ([HDR] L42-50, L2347-2362). The invite carries a GPU only "when the target advertised one".

## Decision

With `--qaros-graphics vulkan` the viewer onboards (or rejoins) without an adapter, reads the session's adapter, and creates its Vulkan device on that LUID (Windows) or UUID. If the session names none, the device goes on the first discrete NVIDIA GPU, else the first discrete GPU, else the first device (`PhysicalDeviceSelector::Discrete`), with a warning, and the sink accepts it. A device on an adapter other than the session's is still refused. `--vulkan-device` stays as an explicit override: that device is created first and its adapter chained into onboarding.

## Alternatives considered

- Device first, its adapter chained into onboarding: wrong on hybrid laptops that list the integrated GPU first, and the adapter comes from the invite, not from the app.
- Refuse a session without an adapter: rejected; a launch invite legitimately may carry none.

## Consequences

- Positive: an API app on a hybrid laptop follows QAROS's GPU with no flags; on an NVIDIA and Intel laptop the session reported the NVIDIA LUID, which matched, and the integrated GPU did not.
- Negative: the discrete fallback is a guess where QAROS names nothing; the header does not say which adapter such a session uses.

## Evidence and sources

- [HDR] QAROS header at `b1338df`, `qar-streaming-c/include/qar_streaming.h` L42-50, L2347-2362.
- [VP] `docs/vulkan-path.md`, "Startup: session, then device".
- [CODE] `crates/qaros-rerun/src/qaros_setup.rs`, `crates/qaros-sink/src/connection.rs` (`graphics_adapter`), `crates/vulkan-device/src/picker.rs` (`PhysicalDeviceSelector`).

[HDR]: https://github.com/Quaternar/qaros/blob/b1338df/qar-streaming-c/include/qar_streaming.h "QAROS C header"
[VP]: ../vulkan-path.md "The QAROS Vulkan path"
[CODE]: ../../crates/vulkan-device/src/picker.rs "Physical device picker"
