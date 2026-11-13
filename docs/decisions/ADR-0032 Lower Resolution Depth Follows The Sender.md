---
id: ADR-0032
title: Depth at a lower resolution than color follows what the sender hands out
status: accepted
date: 2026-10-08
repo: qaros-rerun
---

# ADR-0032: Depth at a lower resolution than color follows what the sender hands out

## Context

The header lets a frame carry depth of its own size, but the sender init takes no sizes, and the header does not say whether `change_layout` accepts a smaller depth texture. A smaller depth texture costs less per frame.

## Decision

The viewer follows whatever depth size the sender hands out. Depth of the color's size is written in the color pass. Depth of another size gets a pass of its own: each depth pixel takes the nearest depth (largest reverse-Z) of the eye pixels it covers, then the same encoding conversion. `--depth-scale-down N` on the GPU image targets tests and times the pass on a Mac.

## Alternatives considered

- Average depth per footprint: rejected so drawn content is never pushed behind what another app volume shows; a half-covered edge pixel counts as covered.
- Request a smaller depth size from QAROS: the API offers no way to.

## Consequences

- Positive: works with any depth size the sender hands out; on a Mac half-resolution depth cuts frame time further (45.2 to 39.3 ms at 1832x1920).
- Negative: the nearest-depth rule is ours; the documentation does not define how the mixer maps depth onto color. On the Windows QAROS package the sender hands out full-size depth, so the resampling pass has not run there.

## Evidence and sources

- [VP] `docs/vulkan-path.md`, "Depth at a lower resolution".
- [CODE] `crates/qaros-rerun/src/eye_output.rs`, `eye_output.wgsl`; `--depth-scale-down` in `crates/qaros-rerun/src/options.rs`.

[VP]: ../vulkan-path.md "The QAROS Vulkan path"
[CODE]: ../../crates/qaros-rerun/src/eye_output.rs "Eye output pass"
