---
id: ADR-0047
title: The QAROS sink takes the Vulkan path by default on Windows and the CPU path elsewhere
status: accepted
date: 2026-10-13
repo: qaros-rerun
---

# ADR-0047: The QAROS sink takes the Vulkan path by default on Windows and the CPU path elsewhere

## Context

A `--qaros-graphics` default of `cpu` gives an operator who leaves the flag out the readback path, which cannot reach headset-resolution rates (ADR-0013, ADR-0026), on the only platform QAROS runs on. On macOS the Vulkan path needs MoltenVK and is a development aid. The image sink, not the QAROS sink, is the viewer's default sink, and an explicit `--qaros-graphics vulkan` requires `--sink qaros`.

## Decision

`--qaros-graphics` has a platform default, `QarosGraphics::PLATFORM_DEFAULT`: `vulkan` on Windows, `cpu` elsewhere. The default applies only with `--sink qaros`; the flag is stored as `Option<QarosGraphics>` and resolved by `QarosOptions::graphics`, so the check that `vulkan` needs `--sink qaros` applies only when the flag is given. Errors the default can trigger (another `--graphics-backend`, linear depth) name `--qaros-graphics cpu`. The viewer's e2e test always passes `--qaros-graphics`, so its CPU case stays the CPU path on Windows.

## Alternatives considered

- Vulkan by default on every platform: would make every plain run with the image sink or on a Mac without MoltenVK fail.
- Default to `cpu` and document the flag: leaves the slower path as what an operator gets by default.

## Consequences

- Positive: `--sink qaros` alone streams through the Vulkan render senders on Windows; on a Windows laptop with an RTX-class GPU the launched viewer created its device on that GPU and streamed to two targets, and `--qaros-graphics cpu` took the CPU path.
- Negative: the same command line behaves differently on macOS and Windows; logs name the path taken ("frames go straight into the Vulkan render senders' images").
- Negative: documented commands for the CPU path must say `--qaros-graphics cpu`.

## Evidence and sources

- [CODE] `crates/qaros-rerun/src/options.rs` (`QarosGraphics::PLATFORM_DEFAULT`, `QarosOptions::graphics`, `check_qaros_flags`).
- [TEST] `options::tests::qaros_graphics_defaults_to_vulkan_on_windows_and_cpu_elsewhere`, green on macOS and Windows.

[CODE]: ../../crates/qaros-rerun/src/options.rs "Viewer options"
[TEST]: ../../crates/qaros-rerun/src/options.rs "Options tests"
