---
id: ADR-0085
title: The app volume starts where it was last placed, from a local file per volume name, while the startup placement is the one it was moved from
status: proposed
date: 2026-11-02
repo: qaros-rerun
---

# ADR-0085: The app volume starts where it was last placed, from a local file per volume name, while the startup placement is the one it was moved from

> Amends ADR-0066: the transform the viewer asks for at start, and falls back to when the live one cannot be read, is the remembered place when there is one, not always the flags' placement. Following the live transform during the run is unchanged. The Stop order (ADR-0079) gains a read of the volume's transform before the volume closes.

## Context

The product owner's requirement (2026-11-02): "App volume position shouldn't be reset, in the same way panels aren't." A user grabs the scene in the glasses and moves it; at the next run it is back at the demo's default place.

- **The viewer closes its volume at Stop.** `Drop for QarosConnection` closes it while the session is alive, because a session that just goes away leaves its volume in the room ([CN]).
- **A closed volume is created afresh.** `qar_app_volumes_get_or_create` ignores the init and takes the volume over only "when it already exists and is not closed" ([HDR] L4916-4934), so a closed volume comes back from the init, which the viewer builds from `--origin`, `--yaw` and `--scale` ([CN] `volume_config`). A volume is active or closed; there is no hidden state ([HDR] L1299-1303). Panels keep their place because the viewer hides them instead of closing them (ADR-0054).
- **The viewer already follows the live transform.** It reads the volume pose, app pose and app scale at connect, on every update, every frame while a gesture runs and at least every 100 ms, and logs each change ([VT], ADR-0066, ADR-0080). A one-hand grab moves the app pose and a spread sets the app scale ([HDR] L1437-1523).
- **The app scale carries the data scale.** `--scale` goes to QAROS as the volume's app scale ([OP] `Options::placement`), and a two-hand spread can leave it at 0.001, which shrinks the data out of sight (tracked separately as item 14).
- **The demo launcher passes placement flags on every start**: `--center`, `--origin`, `--scale` and `--yaw` from each demo's `Placement` ([LA] `Placement.viewer_arguments`), under a volume name of its own per demo (`rerun-<demo>`).

## Decision

The viewer remembers where each app volume was last placed and asks QAROS for the volume there at the next start.

- **What is remembered.** The volume pose and the app pose of the followed transform, per volume common name. Not the app scale: it starts at `--scale` every run, until the data scale moves out of the app scale (item 14). The app pose is in unscaled volume meters, so it keeps its meaning at any app scale.
- **When it is written.** Whenever the followed transform differs from what the file holds, at most once a second (`PLACEMENT_SAVE_PERIOD`), so a crash loses at most a second of a move; and at Stop, from a fresh read of the latest values, before the volume closes (`LiveVolume::remember_before_close`). A volume taken over at start with a place of its own, after a run that did not close it, is written as followed.
- **Where.** `qaros-rerun.volume-placements.json` next to the onboarding id file, as `qaros-rerun.unanswered-peers.txt` is (`QarosOptions::volume_placement_file`). One JSON object, `volumes`, maps each common name to `requested` (the startup volume pose and app pose the volume was moved from) and `last_known` (the last followed volume pose and app pose), each pose `{ "position": [x, y, z], "orientation_xyzw": [x, y, z, w] }` in the spaces of `QarAppVolumeInit`. It is deserialized into typed structs with serde ([RP]). Each write reads the file again and replaces only its own entry, so viewers with other volume names keep theirs, and lands through a temporary file and a rename ([SF], shared with the unanswered peers).
- **Precedence.** At start, the remembered place applies when its `requested` equals the startup placement from the flags (the volume pose and app pose `--origin` and `--yaw` make, within 1e-4). The demo launcher passes the same flags every run, so a demo starts where it was left. Other flags (a demo whose default placement changed, an `--origin` typed by hand) start at the new place, which becomes the new baseline. `--reset-placement` starts at the flags' place and forgets the remembered one at once. `--center` and `--scale` do not take part: neither moves the volume.
- **Errors.** A file that cannot be read or parsed is logged with its path and the parse error and ignored; the run starts at the flags' place and the file is replaced at the next write. A remembered pose that is not finite or not a unit rotation is logged and ignored. A failed write is logged once, and once more when writes succeed again; the run goes on. No error ends the run.
- **The log** names the place a volume starts at: `app volume "rerun-air_traffic" starts where it was last placed, from <file>: pose ..., app pose ..., app scale 0.02; the app scale is the requested one`.

## Alternatives considered

- **Leave the volume open at Stop.** QAROS would keep its place, and `get_or_create` would take it over at the next start with no file of our own. Rejected: while the viewer is not running an empty box stays in the room, and every viewer it is shared with keeps seeing it; a volume a crashed or stopped viewer leaves behind has no owner to close it. Closing at Stop stays the viewer's contract (ADR-0079).
- **Explicit flags always win.** Simple to explain, but the launcher passes placement flags on every start, so the memory would never apply to a demo.
- **The remembered place always wins.** A changed demo placement or a hand-typed `--origin` would be silently ignored until someone found `--reset-placement`. Comparing against the request the volume was moved from keeps both: the same flags restore, new flags win.
- **Restore the app scale too.** Restores a spread, but also the 0.001 a spread can leave (item 14), and would override `--scale`, the data's fit to the table. Revisit once the data scale is no longer the app scale.
- **One file per volume name.** Avoids reading the file again before each write, but needs the name, which may hold any character, turned into a file name, and leaves a file per demo next to the onboarding id. One small file with an entry per name reads as one piece of state.
- **Write on every followed change.** A grab changes the transform every frame; a write per frame would cost a file replace per frame for no gain over a write a second plus one at Stop.

## Consequences

- Positive: a volume grabbed and moved starts where it was left at the next run, in the demos and by hand, without flags.
- Positive: unit tests assert the stored then restored pose and app pose, the app scale left at the request, a changed request winning and becoming the baseline, `--reset-placement` forgetting, a corrupt file reported and ignored then replaced, an unusable pose ignored, the once-a-second throttle and other names kept ([RP] tests); a stub test moves the volume, stops, has the stub forget the closed volume as QAROS does, and asserts the next run's init and live transform carry the moved poses and the requested app scale ([ST] `a_moved_volume_is_created_where_it_was_last_in_the_next_run`).
- Negative: a spread is not kept: the next run starts at `--scale`.
- Negative: a hard kill loses up to a second of the last move.
- Negative: the place is per machine and working directory, not per room: a viewer started on another PC, or with another `--onboarding-id-file` directory, starts at the flags' place.
- Open: not yet tried in the glasses. The owner's check: move the volume, stop, start again, and it stands where it was left; `--reset-placement` puts it back at the demo's default.

## Evidence and sources

- [CN] `crates/qaros-sink/src/connection.rs` (`Drop for QarosConnection`, `open_placement_memory`, `volume_config`).
- [VT] `crates/qaros-sink/src/volume_transform.rs` (`FollowedVolume`, `LiveVolume::refresh`, `LiveVolume::remember_before_close`).
- [RP] `crates/qaros-sink/src/remembered_placement.rs` (`PlacementMemory`, `PLACEMENT_SAVE_PERIOD`, the file format, tests).
- [SF] `crates/qaros-sink/src/state_files.rs` (`replace_file_contents`).
- [ST] `crates/qaros-sink/tests/stub.rs` (`a_moved_volume_is_created_where_it_was_last_in_the_next_run`).
- [OP] `crates/qaros-rerun/src/options.rs` (`--reset-placement`, `QarosOptions::volume_placement_file`, `Options::placement`).
- [LA] `demos/launcher.py` (`Placement.viewer_arguments`, `--volume-name rerun-<demo>`).
- [HDR] QAROS header at `e0c0195`, `qar-streaming-c/include/qar_streaming.h`: `QarAppVolumeLifetimeStatus` (L1299-1303), the gesture rules (L1437-1523), `qar_app_volumes_get_or_create` (L4916-4934).

[CN]: ../../crates/qaros-sink/src/connection.rs "QAROS connection"
[VT]: ../../crates/qaros-sink/src/volume_transform.rs "Followed app volume transform"
[RP]: ../../crates/qaros-sink/src/remembered_placement.rs "Remembered app volume placement"
[SF]: ../../crates/qaros-sink/src/state_files.rs "State files"
[ST]: ../../crates/qaros-sink/tests/stub.rs "Stub tests"
[OP]: ../../crates/qaros-rerun/src/options.rs "Options"
[LA]: ../../demos/launcher.py "Demo launcher"
[HDR]: https://github.com/Quaternar/qaros/blob/e0c0195/qar-streaming-c/include/qar_streaming.h "QAROS C header"
