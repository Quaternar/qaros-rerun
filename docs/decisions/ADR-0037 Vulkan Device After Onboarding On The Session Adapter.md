---
id: ADR-0037
title: Vulkan device created after onboarding on the session's adapter, with a discrete-GPU fallback
status: accepted
date: 2026-10-13
repo: qaros-rerun
---

# ADR-0037: Vulkan device created after onboarding on the session's adapter, with a discrete-GPU fallback

## Context

The session's devices and every sender's images live on the session's adapter, which the app reads after onboarding with `qar_session_get_graphics_device_id` ([HDR] L42-50, L2347-2362). An API app that QAROS launches is told to "Onboard first, without chaining a `QarGraphicsDeviceId`. Then read the session's adapter ... and create your graphics device on it" ([OS] L608). Where the adapter comes from depends on how the app joined: "Absent, QAROS selects one: for an app onboarded with a launch invite, its target's GPU; otherwise the library picks one. Only NVIDIA adapters are supported" ([OS] L527). On Windows a launched app joins as a local app; elsewhere it gets a launch invite, which carries a GPU only "when the target advertised one" ([OS] L601, L617-619).

## Decision

With `--qaros-graphics vulkan` the viewer onboards (or rejoins) without an adapter, reads the session's adapter, and creates its Vulkan device on that LUID (Windows) or UUID. If the session names none, the device goes on the first discrete NVIDIA GPU, else the first discrete GPU, else the first device (`PhysicalDeviceSelector::Discrete`), with a warning, and the sink accepts it. A device on an adapter other than the session's is still refused. `--vulkan-device` stays as an explicit override: that device is created first and its adapter chained into onboarding.

## Alternatives considered

- Device first, its adapter chained into onboarding: wrong on hybrid laptops that list the integrated GPU first, and a chained adapter always wins over the one QAROS would select ([OS] L608).
- Refuse a session without an adapter: rejected; a launch invite legitimately may carry none.

## Consequences

- Positive: an API app on a hybrid laptop follows QAROS's GPU with no flags; on an NVIDIA and Intel laptop the session reported the NVIDIA LUID, which matched, and the integrated GPU did not.
- Negative: the discrete fallback is a guess where QAROS names nothing; the docs say only that the library picks one.

## Evidence and sources

- [HDR] QAROS header at `b1338df`, `qar-streaming-c/include/qar_streaming.h` L42-50, L2347-2362.
- [OS] QAROS docs at `ebfe3bf`, `docs/website/docs/developer-guide/onboarding-and-sessions.mdx` L527 ("Choosing the GPU"), L601-619 ("Launched by QAROS").
- [VP] `docs/vulkan-path.md`, "Startup: session, then device".
- [CODE] `crates/qaros-rerun/src/qaros_setup.rs`, `crates/qaros-sink/src/connection.rs` (`graphics_adapter`), `crates/vulkan-device/src/picker.rs` (`PhysicalDeviceSelector`).

[HDR]: https://github.com/Quaternar/qaros/blob/b1338df/qar-streaming-c/include/qar_streaming.h "QAROS C header"
[OS]: https://github.com/Quaternar/qaros/blob/ebfe3bf/docs/website/docs/developer-guide/onboarding-and-sessions.mdx "Onboarding and sessions"
[VP]: ../vulkan-path.md "The QAROS Vulkan path"
[CODE]: ../../crates/vulkan-device/src/picker.rs "Physical device picker"
