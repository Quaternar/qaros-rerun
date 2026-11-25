---
id: ADR-0104
title: A viewer frame without the streamed view streams an empty scene
status: proposed
date: 2026-11-25
repo: qaros-rerun
---

# ADR-0104: A viewer frame without the streamed view streams an empty scene

> Amends ADR-0059 (a viewer frame that did not draw the view withdrew the scene) and ADR-0099 (a shown window that drew no view withdrew it).

## Context

- **No recording, no frame.** A viewer started from the QAROS Visualizer with no recording loaded shows Rerun's welcome screen, which draws no 3D view. The observer captured nothing, `end_viewer_frame` withdrew the scene, and the render thread began no frame. The sender and the mixer connected, the stream ran at 0 fps, and the mixer showed **Connecting...** in the app volume for as long as the viewer ran.
- **The same for any layout without the 3D view.** A 2D-only blueprint or a 3D view in a background tab left the last frame in the glasses or, before the first one, the same **Connecting...**.
- **The user cannot tell this from a broken stream.** An empty viewer is the first thing a new user sees after installing the viewer from the Visualizer's Sources catalog.

## Decision

- **A viewer frame that drew no streamed view publishes an empty scene**: no draw data, the settings' placement in the override's coordinates (Z up without one), a fixed view seed (`SceneCapture::empty_scene`, `decoupled.rs`). The render thread begins and submits frames for it like any scene, so the headset shows an empty app volume.
- **Once, not every viewer frame.** While the empty scene is in the handoff, further frames without a view publish nothing, so the render thread is not woken per UI pass.
- **A minimised window keeps the last scene (ADR-0099)**, unless there is none yet: a window minimised from the start streams the empty scene too.
- **The inline render (`--inline-render`) is unchanged.** It begins frames only from the 3D view observer; it is the opt-out path, not what the Visualizer starts.

## Alternatives considered

- **A "waiting for the first frame" state in the mixer.** Needs a QAROS change, and the viewer would still send nothing while it has nothing to show. Rejected for the viewer; the mixer's state is QAROS's own matter.
- **Publish the empty scene every viewer frame.** Wakes the render thread per UI pass for an unchanged scene. Rejected.

## Consequences

- An empty viewer's stream runs at the eye frame rate with transparent eyes; its GPU cost is a clear per eye.
- A layout that drops the 3D view empties the glasses instead of freezing the last frame.
- The **scenes published** statistic counts captured scenes only.
