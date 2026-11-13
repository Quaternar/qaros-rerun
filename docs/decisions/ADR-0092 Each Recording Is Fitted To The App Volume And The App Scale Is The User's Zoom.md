---
id: ADR-0092
title: Each recording is fitted to the app volume by the viewer, and QAROS's app scale is only the user's zoom
status: proposed
date: 2026-11-02
repo: qaros-rerun
---

# ADR-0092: Each recording is fitted to the app volume by the viewer, and QAROS's app scale is only the user's zoom

> Amends ADR-0066 (the app scale QAROS holds is the user's zoom; the data scale and center are the viewer's own data fit), ADR-0080 (a spread scales around 1), ADR-0084 (the reverse-infinite marker is divided by the zoom only), ADR-0085 (the zoom is remembered with the place) and ADR-0089 (a taken-over volume keeps its zoom, unless it cannot be one).

## Context

Two problems from the 1102c build in the glasses (2026-11-02), with one cause: the viewer's placement of the data is fixed at start and handed to QAROS.

- **A recording opened later keeps the start recording's placement.** The world-to-room mapping came from `--center`, `--scale`, `--up`, `--origin` and `--yaw` once, at start ([OP] `Options::placement`, [PL] `AppPlacement`). The owner ran the structure from motion demo and opened air traffic and the robot arm with File > Open. Air traffic (map coordinates around 900 km by 5,520 km) landed thousands of kilometers from the volume: tiny, without parallax, like a parallel projection. The robot arm came out small and off-centre. The patched 3D view already handed the scene bounds to the observer every frame ([OB] `View3DFrame::scene_bounding_box`), unused for placement.
- **The data scale was QAROS's app scale (item 14).** `--scale` went to QAROS as the volume's app scale. QAROS's two-hand spread treats the app scale as the user's zoom: on air traffic (`--scale 1.8e-6`) a spread set it to 0.001, a 555x jump (air traffic `viewer.log` on Legion, 2026-11-02 around 11:17:45 UTC); the Hub clamps the app scale to at least 0.001 (ADR-0066). Since ADR-0089 a taken-over volume keeps the app scale QAROS holds, so a spread's scale carried into later runs.

## Decision

**Room from world = volume pose x app pose x app scale x view coordinates x data fit**, where the data fit is a center (the Rerun-world point on the app origin) and a data scale (app meters per Rerun unit), chosen by the viewer per recording ([PL] `DataFit`, `AppPlacement::room_from_world`). QAROS never sees the data fit. The app scale starts at 1 and is the user's zoom.

### Every consumer of the placement

- **Eye poses.** The sink maps each eye pose into the room with the volume pose, app pose and app scale, unchanged ([SK] `PoseMapping`). The renderer turns the room pose into a Rerun-world camera with zoom times data scale ([PL] `world_from_scaled_eye`); the near plane in Rerun-world units is the room near plane divided by the same product ([EY] `EyeCamera::new`).
- **Depth near and far (ADR-0084).** Rerun's reverse-Z holds `near / distance`, a ratio the data scale cancels out of, so the depth pixels need no change. The declaration goes to QAROS in app units, divided by the app scale alone, now the zoom: at zoom 0.1 to 10 the reverse-infinite marker is 1e31 to 1e29, finite and above QAROS's 1e20 threshold, and the declared far plane is the near plane over the zoom ([SK] `reverse_infinite_depth_stays_valid_for_any_user_zoom`). Before, the data scale entered the marker (5.6e35 at air traffic's scale).
- **Panels near the volume** are placed in room meters from the volume's pose and size, or from the user's head ([PN] `panel_pose_beside_volume`); neither depends on the app scale or the data fit.
- **Remembered placement (ADR-0085).** Per volume common name: the volume pose, the app pose and now the user zoom, which is no longer the data scale. The zoom is stored only inside 0.1 to 10 (`USER_ZOOM_RANGE`) and restored only from there; entries written before this decision hold no zoom and start at 1. Nothing is stored per recording: each run fits every recording again, the flags' fit and the auto-fit being functions of the flags and the data.
- **Volume hide and show (ADR-0089).** A taken-over volume keeps the app scale QAROS holds: that is the user's zoom, and keeping it is the point. Two app scales are not a zoom and are set to the zoom this start asks for (1, or the remembered one), with a log line: one outside 0.1 to 10 (the Hub's clamp at 0.001), and one equal to `--scale` other than 1, which only a build before this decision sets: a spread never lands on it to the bit ([CN] `keep_only_a_user_zoom`, [VT] `is_user_zoom`). `--reset-placement` sets the zoom to 1 with the rest of the requested placement.
- **Gestures (ADR-0080).** Unchanged in the viewer: QAROS applies grab and spread, and the viewer follows the live transform. A spread now scales around 1: going from 1 to 2 doubles the scene in the room at any data scale ([PL] `a_spread_from_one_to_two_doubles_the_room_size_at_any_data_scale`), and the Hub's 0.001 clamp is a 1000x zoom-out instead of a jump.

### The fit per recording

- **Which recording.** The store id (application id and recording id) of the recording the streamed 3D view shows, read from `ViewerContext::store_id` every frame the view is drawn ([HS] `shown_recording`). A File > Open, a recording selected in the recordings panel, or a new stream changes it.
- **Its fit, chosen once** ([RF] `RecordingFits`):
    - With `--auto-fit off`, every recording takes the flags' fit (`--center` and `--scale`, else Rerun's origin at one meter per unit), as before this decision.
    - Otherwise, when `--center` or `--scale` is given, the first recording that shows content takes the flags' fit: the recording the viewer was started for. Waiting for content keeps an empty recording (one the launcher's own stream creates before the replay arrives) from taking it.
    - Every other recording is auto-fitted: centered on its scene bounds, its largest extent 0.7 of the volume's smallest side (`AUTO_FIT_FILL`, below 1/sqrt(2) so a square footprint turned by any `--yaw` stays inside). The bounds are the 3D view's region of interest, which leaves outliers out as Rerun's own camera framing does, else the full bounds ([OB] `scene_region_of_interest`).
    - Bounds that are empty, not finite, or no larger than 1e-5 of their center's largest coordinate, taken as at least 1 (a single point, or float noise around a point at map coordinates) are not fitted. Bounds are fitted once they stood within 5 % of their largest extent for 0.5 s, or 3 s after they first meant something while data keeps streaming in, with what they hold then.
    - A recording keeps its fit as data streams in and gets it back when it is shown again. Until it is fitted it is drawn with the fit in effect before it. The up axis is not part of the fit: it follows the view's coordinates every frame, or `--up`.
- **Every fit is logged** with its recording, source (flags, auto-fit, origin) and numbers, and the auto-fit with its bounds: `headset: recording air_traffic/<id> placed with auto-fit to the scene bounds: center [...], data scale 1.4e-6 m per unit, from scene bounds [...] to [...]`.
- **The demo launcher** passes its demo's `--center` and `--scale` as before; they now place the demo's own recording only.

### Flags

`--center` and `--scale` become optional (default: auto-fit) and place the start recording. `--auto-fit on|off` (default on) is the switch back to flag placement for every recording. `--origin`, `--yaw`, `--up`, `--volume-size` (which the auto-fit sizes to) and `--reset-placement` keep their meaning.

## Alternatives considered

- **A placement hint logged into each demo recording** (a recording property the launcher writes, read back by the viewer), so a demo `.rrd` opened with File > Open fits as its own demo. Needs every cached recording re-recorded and a store query for the property; the auto-fit already brings a demo opened that way to table size and centre. Revisit if a demo's hand-tuned fit (structure from motion's median center) is wanted under File > Open too.
- **Re-fit as data streams in.** The scene would jump in size and place while a person looks at it. A fit once per recording, with a settle rule, keeps it still.
- **Keep the data scale in the app scale and only re-fit per recording.** A spread would still start from the data scale and the Hub's clamp would still jump; QAROS would keep the previous recording's scale on take-over.
- **Remember fits per recording across runs.** Recording ids of live streams are new every run, and a fit computed again from the same data and flags comes out the same.
- **Reset the app scale to 1 on every take-over.** Simplest migration from the old builds, but it throws away a zoom the person set, which ADR-0089 exists to keep.

## Consequences

- Positive: a recording opened with File > Open, or streamed after the start one, lands centred in the volume at table size with full parallax; the start recording keeps its hand-tuned flags.
- Positive: a spread zooms around the scene's fitted size, at any data scale, and the zoom survives Stop and Start.
- Positive: unit tests assert room from world with a data fit and a zoom, a spread from 1 to 2 doubling the room size at data scales from 1.8e-6 to 2, room-distance depth at any zoom and data scale, the reverse-infinite declaration at zoom 0.1 to 10, an auto-fit of air traffic-like bounds landing inside the volume 1.4 m across, the flags for the start recording and an auto-fit for the next, fits switching with the recording, the settle rule, the zoom remembered and restored only inside its range, and an older entry without a zoom; a stub test asserts that a taken-over app scale of 0.001, or of `--scale`, is set back and a zoom of 4 is kept ([PL], [EY], [SK], [RF], [RP], [VT], [ST]).
- Negative: an auto-fit frames by bounds, not by what a person finds interesting: structure from motion opened with File > Open fits its point cloud's region of interest, not the median its demo centres on.
- Negative: a stream whose content keeps growing past 3 s is fitted on what it showed then.
- Negative: a volume an older build left at a `--scale` between 0.1 and 10 is kept as a zoom by a run started with another `--scale`; `--reset-placement` clears it.
- Open: not yet tried in the glasses. The owner's check: start the structure from motion demo, open air traffic and the robot arm with File > Open; each lands centred at table size with parallax when walking around; a two-hand spread scales around 1 without a jump; Stop and Start keep the zoom. The `headset: recording ... placed with` lines in `viewer.log` give each fit.

## Evidence and sources

- [PL] `crates/xr-math/src/placement.rs` (`DataFit`, `AppPlacement`, `AUTO_FIT_FILL`, `bounds_are_meaningful`, tests).
- [EY] `crates/xr-math/src/eye.rs` (`EyeCamera::new`, `depth_reports_room_distance_regardless_of_scale`).
- [RF] `crates/qaros-rerun/src/recording_fit.rs` (`RecordingFits`, the settle rule, tests).
- [HS] `crates/qaros-rerun/src/headset.rs` (`shown_recording`, `scene_bounds_to_fit`, `fit_for_view`, `placement_for_view`).
- [OP] `crates/qaros-rerun/src/options.rs` (`Options::placement`, `flag_fit`, `--auto-fit`).
- [OB] `patches/re_view_spatial/src/view_3d_observer.rs` (`View3DFrame::scene_bounding_box`, `scene_region_of_interest`).
- [SK] `crates/qaros-sink/src/sink.rs` (`PoseMapping`, `reverse_infinite_depth_stays_valid_for_any_user_zoom`).
- [CN] `crates/qaros-sink/src/connection.rs` (`keep_only_a_user_zoom`).
- [VT] `crates/qaros-sink/src/volume_transform.rs` (`USER_ZOOM_RANGE`, `is_user_zoom`).
- [RP] `crates/qaros-sink/src/remembered_placement.rs` (`user_zoom`, tests).
- [PN] `crates/qaros-sink/src/panels.rs` (`panel_pose_beside_volume`, `consider_head`).
- [ST] `crates/qaros-sink/tests/stub.rs` (`a_taken_over_app_scale_that_is_no_user_zoom_is_set_to_the_requested_zoom`, `a_moved_volume_is_created_where_it_was_last_in_the_next_run`).

[PL]: ../../crates/xr-math/src/placement.rs "App placement"
[EY]: ../../crates/xr-math/src/eye.rs "Eye camera"
[RF]: ../../crates/qaros-rerun/src/recording_fit.rs "Recording fits"
[HS]: ../../crates/qaros-rerun/src/headset.rs "Headset renderer"
[OP]: ../../crates/qaros-rerun/src/options.rs "Options"
[OB]: ../../patches/re_view_spatial/src/view_3d_observer.rs "3D view observer"
[SK]: ../../crates/qaros-sink/src/sink.rs "QAROS sink"
[CN]: ../../crates/qaros-sink/src/connection.rs "QAROS connection"
[VT]: ../../crates/qaros-sink/src/volume_transform.rs "Followed app volume transform"
[RP]: ../../crates/qaros-sink/src/remembered_placement.rs "Remembered app volume placement"
[PN]: ../../crates/qaros-sink/src/panels.rs "Panels"
[ST]: ../../crates/qaros-sink/tests/stub.rs "Stub tests"
