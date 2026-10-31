---
id: ADR-0049
title: Panels posed by their top-edge midpoint, turned half a turn to face the viewer, named by the LAN address
status: superseded
superseded_by: ADR-0070
date: 2026-10-14
repo: qaros-rerun
---

# ADR-0049: Panels posed by their top-edge midpoint, turned half a turn to face the viewer, named by the LAN address

> The LAN-address base URL is superseded by ADR-0076: panels are video streams of the viewer and need no URL; the web route, `--panel-base-url` and the LAN address lookup were removed. The pose convention stands through ADR-0070.

## Context

The `b1338df` header defines panel space ([HDR] L781-800): the pose origin is the midpoint of the panel's top edge, +X runs along the top edge to the viewer's left, +Y up, and -Z is the front-face normal pointing at the viewer; the surface covers x in ±width/2 and y in [-height, 0]. The Coordinate Systems page repeats it and warns that a volume's pose is its centre while a panel's is its top-edge midpoint ([CS], "GUI panels"). A panel posed as its top-left corner with the volume's orientation sits half a width (0.3 m) too far left, overlapping the volume by 0.2 m, and its front normal points along the volume's -Z.

Neither the header nor the docs name a front for an app volume; app volume space only has +Z as the horizontal depth ([HDR] L949-958). Where the viewer stands comes from the room convention and from where this viewer puts the volume:

- Room space follows OpenXR: right-handed, Y up, -Z forward ([CS], "Room space").
- The viewer's volume defaults to the room position (0, 0.5, -1.6) with an identity orientation; `--yaw` turns the app pose inside the volume, not the volume ([OPT] `placement`, [PLC] `with_up_axis_rotation`). The demo launcher places its data at `--origin 0,1.1,-1.2`, along -Z from the room origin ([DEMO]).

So the viewer stands near the room origin, on the volume's +Z side, looking along -Z. A panel with the volume's orientation points its -Z front away from that viewer, the same way they look.

The panel URL needs the same care: with `--panels 0.0.0.0:<port>`, a base URL of `http://127.0.0.1:<port>` reaches the panel server only from the machine the viewer runs on. Whether the headset or the Hub fetches the page is not documented.

## Decision

Each panel's pose is the volume's pose times a panel offset in volume space: the top-edge midpoint at (width/2 + 0.1 + 0.3 + slot × 0.7, height/2, 0), turned half a turn (π) about the volume's +Y. Panel -Z then points along the volume's +Z at the viewer, and panel +X along the volume's -X, the viewer's left, as the header requires. The row stays to the viewer's right (the volume's +X side), top edges level with the volume's top, at the volume's centre depth. For the default volume (2 m cube at (0, 0.5, -1.6)) slot 0 is at (1.4, 1.5, -1.6) and slot 1 at (2.1, 1.5, -1.6), both with orientation (x, y, z, w) = (0, 1, 0, 0).

With `--panels 0.0.0.0:<port>` and no `--panel-base-url`, the base URL is named by this machine's LAN IPv4 address: the default route's local address (a UDP socket connected to a TEST-NET-1 address, which sends nothing), else the first interface address that is neither loopback nor link-local, else 127.0.0.1 with a warning. The log names the address and how it was found. `--panel-base-url` overrides it.

## Alternatives considered

- Keep the volume's orientation: contradicts the header's front normal for a viewer on the volume's +Z side.
- Turn each panel towards the viewer's head: needs a viewer position, which differs per user; panels are shared session state ([GP]) at one pose for everyone.
- Name the Hub's route: the API does not expose the Hub's address to the application.

## Consequences

- Positive: panels sit beside the volume with a 0.1 m gap and face a viewer standing in front of it; stub tests check the exact poses for slots 0 and 1 and a turned volume.
- Positive: headsets on the LAN can load the panel pages whichever side fetches them.
- Negative: a viewer standing behind the volume (its -Z side) sees the panels' backs; this follows from the assumption above and is unverified on headsets.
- Negative: a machine with several networks (VPN, virtual adapters) may name the wrong one when it has no default route; the log line shows the choice and `--panel-base-url` corrects it.
- Superseded by ADR-0070 for the pose: new panels open within reach of a headset user's head, and the mixer turns every panel toward each viewer's head. The pose convention (top-edge midpoint, -Z front) stands, and ADR-0070 keeps it. `panel_pose_beside_volume` remains as the pose a panel config carries before any head is known; no new panel opens before then, and the Hub ignores it for a panel that exists.

## Evidence and sources

- [HDR] QAROS header at `b1338df`, `qar-streaming-c/include/qar_streaming.h` L781-800 (panel space), L939-966 (app volume space).
- [CS] QAROS docs at `b1338df`, `docs/website/docs/developer-guide/coordinate-systems.md`, "Room space" and "GUI panels".
- [GP] QAROS docs at `b1338df`, `docs/website/docs/developer-guide/gui-panels.mdx`, "Attach a panel to your app volume".
- [OPT] `crates/qaros-rerun/src/options.rs` (`--origin` default, `Options::placement`).
- [PLC] `crates/xr-math/src/placement.rs` (`with_up_axis_rotation` turns the app pose).
- [DEMO] `demos/launcher.py` (the demo's `origin`).

[HDR]: https://github.com/Quaternar/qaros/blob/b1338df/qar-streaming-c/include/qar_streaming.h "QAROS C header"
[CS]: https://github.com/Quaternar/qaros/blob/b1338df/docs/website/docs/developer-guide/coordinate-systems.md "Coordinate Systems"
[GP]: https://github.com/Quaternar/qaros/blob/b1338df/docs/website/docs/developer-guide/gui-panels.mdx "GUI Panels"
[OPT]: ../../crates/qaros-rerun/src/options.rs "Viewer options"
[PLC]: ../../crates/xr-math/src/placement.rs "App placement"
[DEMO]: ../../demos/launcher.py "Demo launcher"
