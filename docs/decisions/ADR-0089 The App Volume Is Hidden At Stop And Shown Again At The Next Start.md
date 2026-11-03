---
id: ADR-0089
title: The app volume is hidden at Stop by removing every peer that uses it, and shown again at the next start when the run's render senders attach their targets
status: proposed
date: 2026-11-02
repo: qaros-rerun
---

# ADR-0089: The app volume is hidden at Stop by removing every peer that uses it, and shown again at the next start when the run's render senders attach their targets

> Amends ADR-0079 (the Stop order: the volume is hidden, never closed) and ADR-0085 (the remembered place applies to a volume created afresh; a volume taken over keeps the place QAROS holds).
>
> Amended by ADR-0092: the app scale a taken-over volume keeps is the user's zoom, which is correct to keep; one outside 0.1 to 10, or equal to `--scale` as an earlier build set it, is set to the zoom the start asks for. The spread that left 0.001 no longer carries a data scale.
>
> Amended by ADR-0093: the used-by peers are followed during the run and Stop asks QAROS nothing; the peers are removed one at a time and the hide is confirmed by the Hub's bake within 180 ms; the volume is never closed, and a hide left unfinished is finished by the next start.

## Context

The product owner's request (2026-11-02): "Add hide/show to app volumes in the same way we have for panels." Panels are hidden at Stop, kept by the Hub with their place, and claimed again by common name and shown at the next start (ADR-0054, ADR-0075, ADR-0077). The viewer closed its volume at Stop instead, so an empty box would not stay in the room ([CN] `Drop for QarosConnection`), and ADR-0085 remembered the volume's place in a local file because QAROS rebuilds a closed volume from the init.

What QAROS allows, from its header, its source and the headset renderer's source:

- **No hidden lifetime state.** A volume is `QAR_APP_VOLUME_ACTIVE` or `QAR_APP_VOLUME_CLOSED` ([HDR] L1299-1303; `AppVolumeLifetimeStatus` in [QS] `AppVolume.hpp` L15-19). A GUI panel has `QAR_GUI_PANEL_STATE_HIDDEN` ([HDR] L1163-1169); there is no volume counterpart.
- **Used-by peers decide what a headset draws.** The header only says "Update which peers use the volume" ([HDR] L4950-4958) and "Get number of peers currently using this volume" (L5354-5364). The headset's mixer keeps a table (wireframe box, name tag, handles) and a mixer state (the app's content) only for a volume whose `Peers` lists its own peer; when the peer leaves the list it erases both ([SK] `TableControl.hpp` `UpdateTablesFromAppVolumes` L99-128). The box and the name tag are drawn every frame for a listed, active volume with a size, not only in edit mode ([SK] `TableView.hpp` `Render` L392-401; handles only while editing). The headset's hand menu offers "Hide Volume for Me" and "Unhide Volume for Me", which are exactly `RemovePeer` and `AddPeer` of its own peer ([SK] `AppVolumesHandMenu.hpp` L120-130).
- **A render sender lists its target.** Creating a sender attaches its target to the app's volume before it serves frames ([QS] `RenderingStreamSenderImpl.hpp` `EstablishServingPreconditions` L1963-1971, `AddPeerWhenAppVolumeReplicated` L1863-1868; QuaternAR ADR-0273). Nothing removes a peer when a sender is destroyed: the only callers of `RemovePeer` are the C API, the extension API and the hand menu ([QS] search of `libs/`).
- **An open volume outlives its app.** A session that ends without closing its volume leaves it in the room, still listing its peers, so every headset keeps drawing its box ([CN]; the Hub's recovery restores "volumes whose owner is gone", [QS] `AppVolumeProcessor.hpp` L613-615).
- **An open volume is taken over with its place.** `get_or_create` ignores the init for a volume that exists and is not closed and takes it over; a closed one is rebuilt from the init at the same id ([HDR] L4916-4934; [QS] `AppVolumeImpl.cpp` L638-653). The volume keeps its pose, app pose and app scale.
- **The binding has no used-by calls.** `qaros::AppVolumes` has get-or-create, the setters, close, list and subscribe; `qaros-stub` does not implement the used-by functions (they answer `QAR_STATUS_NOT_IMPLEMENTED`).

## Decision

At Stop the viewer hides its app volume instead of closing it, through the used-by peers; at the next start it takes the volume over, and its render senders show it again.

- **Hide at Stop.** After the senders and the panels, before the session (the `HideVolume` teardown step), the viewer removes every used-by peer of the volume (`qar_app_volumes_update_used_by_peers`; how the peers are known and removed within the stop budget is ADR-0093). The volume stays active and lists no peer, as a volume does before any sender attached a target: no headset draws its box or its content, and the Hub keeps it with its pose, app pose and app scale. A volume that lists no peer is left as it is. The log names the volume and each removed peer: `qaros: hid app volume "rerun-air_traffic" from every peer that used it (...); QAROS keeps it with its place, and the next run takes it over and shows it again`.
- **Never closed.** The viewer never closes its volume, at Stop or on any failure. A hide that cannot be completed leaves the volume as it is, with a warning, and the next start hides it from the peers it still lists before its senders list their headsets (ADR-0093).
- **Show at start.** `get_or_create` takes the open volume over by common name ([HDR] L4916-4934). Each render sender of the new run lists its target as it is created, which shows the volume to that headset with its new content; a headset the viewer does not serve does not see it, as with a volume created afresh. The viewer adds no peer itself: a peer added without a sender would see an empty box.
- **Where a taken-over volume stands.** It keeps the transform QAROS holds, as a reattached panel keeps its place (ADR-0070, "existing panels never re-posed"), and the viewer follows it as before (ADR-0066). `--reset-placement` is the exception: it sets every part of the transform that differs from the flags' placement (pose, app pose, app scale), and logs it.
- **The remembered place stays as the fallback.** The remembered-placement file is still written within a second of every move and at Stop. It decides where a volume starts only when QAROS creates the volume afresh: the first run, a volume closed in the Hub, or a Hub reset.
- **Raw calls.** The used-by calls go through `library.raw_api()` in `volume_visibility.rs` ([VV]), as the stream panel and gesture calls do; the hide sits behind a small trait so it is tested without QAROS.

## Alternatives considered

- **Keep closing the volume.** Every Stop loses the volume, and its place survives only through the remembered-placement file, which misses a move made in the Hub while the viewer is stopped and is per machine. Rejected: the owner asked for the panels' behaviour, and QAROS offers a per-peer way to hide a volume.
- **Leave the volume open at Stop without touching its peers.** Takes over the place for free, but every headset that was served keeps drawing the box and name tag of an app that is gone ([SK] `TableView.hpp` L392-401). Rejected, as in the remembered-placement decision.
- **Ask QAROS for a hidden state.** The right long-term shape (a `HIDDEN` lifetime or state for app volumes, like GUI panels), but it does not exist at `e0c0195`. The used-by route gives the same result today with calls QAROS already exposes and its own headset menu already uses.
- **Restore the startup transform on every take-over.** Would keep the remembered-placement precedence (changed flags win, the app scale always starts at `--scale`) for taken-over volumes too, but it would undo a move made in the Hub while the viewer was stopped, and it treats the volume unlike the panels. `--reset-placement` remains the way back to the flags' place.
- **Add the earlier run's peers back at start.** Would show the volume before any sender exists, as an empty box, and to a headset the new run may not serve.

## Consequences

- Positive: the volume disappears from every headset at Stop and reappears at the next start where it was left, including a move or resize made in the Hub meanwhile; the Hub's App Volumes tab keeps listing it, as it keeps the panels.
- Positive: unit tests assert that every used-by peer is removed and the volume is never closed, and that a volume listing no peer gets no call ([VV] tests); a unit test asserts which parts `--reset-placement` sets ([VT] `StartupCorrection`). Stub tests assert the Stop calls in order, that the volume is never closed, a volume listing no peer left open with every handle released, and `--reset-placement` setting a taken-over volume to the requested placement ([ST]; ADR-0093 lists the tests of the timing).
- Negative: a spread that left the app scale at 0.001 (item 14) is kept by the next run too; `--reset-placement` restores `--scale`.
- Negative: a person who chose "Hide Volume for Me" in the hand menu sees the volume again at the next start, when the sender lists them again.
- Negative: two viewers started under one volume name share the volume; the first to stop hides it from the other's headsets until that viewer's senders are created again.
- Open: whether the Hub's validator accepts the viewer's removal of peers it did not add. The shared-object design lets each kind plug in its own validator over an ACL whose editors default to the used-by peers ([QS] `AppVolumeProcessor.hpp` `BuildDefaultAcl` L669-680; QuaternAR `docs/Shared Object Synchronization.md` §6). The viewer already adds peers through its senders, and the removal is a proposal: a rejection would not reach the log. The owner's check below shows it.
- Open: not yet tried in the glasses. The owner's check: start a demo, Stop it; the box and content disappear from the glasses and the Hub's App Volumes tab still lists the volume with its place; start the demo again; the volume reappears at the same place with the new run's content. If the box stays at Stop, the log line above names the peers that were removed.

## Feature request for QuaternAR

A hidden state for app volumes, like GUI panels: `qar_app_volumes_set_state(session, id, QAR_APP_VOLUME_STATE_VISIBLE | QAR_APP_VOLUME_STATE_HIDDEN)` (or a `HIDDEN` lifetime status), under which every mixer drops the volume's table and mixer state while the Hub keeps it with its transform and its used-by peers, and `get_or_create` takes a hidden volume over and leaves showing it to the app. Today an app has to remove every used-by peer to hide its volume and rely on its render senders to list them again; the hidden state would keep the headsets' own "Hide Volume for Me" choices across an app restart and make the intent visible in the Hub.

## Evidence and sources

- [CN] `crates/qaros-sink/src/connection.rs` (`Drop for QarosConnection`, `reset_taken_over_volume`, `list_and_warn_if_closed`).
- [VV] `crates/qaros-sink/src/volume_visibility.rs` (`hide_at_stop`, `SessionVolume`, tests).
- [VT] `crates/qaros-sink/src/volume_transform.rs` (`StartupCorrection`, tests).
- [ST] `crates/qaros-sink/tests/stub.rs` (`a_volume_whose_peers_were_never_reported_is_left_open_at_stop_without_a_listing`, `a_volume_no_peer_uses_stays_open_at_stop`, `reset_placement_sets_a_taken_over_volume_to_the_requested_placement`, `a_volume_held_with_another_transform_maps_eyes_with_the_live_one`).
- [HDR] QAROS header at `e0c0195`, `qar-streaming-c/include/qar_streaming.h`: `QarGuiPanelState` (L1163-1169), `QarAppVolumeLifetimeStatus` (L1299-1303), the session lifecycle (L3582-3619), `qar_app_volumes_get_or_create` (L4916-4934), `qar_app_volumes_update_used_by_peers` (L4950-4958), `qar_app_volume_get_used_by_peers_count` and `qar_app_volume_get_used_by_peers` (L5354-5364).
- [QS] QAROS source, `Quaternar/qar_streaming` at `e487a31`, `libs/qar-streaming/`: `include/qar_streaming/AppVolume.hpp` (L15-19, `AppVolume::Peers` L294), `src/AppVolumeImpl.cpp` (`GetOrCreateAppVolume` L628-653, `AddPeer` and `RemovePeer` L700-740, `CloseAppVolume` L849-864), `src/AppVolumeProcessor.hpp` (L613-615, `BuildDefaultAcl` L669-680), `src/RenderingStreamSenderImpl.hpp` (L1840-1871, L1954-1972).
- [SK] QuaternAR headset renderer, `Quaternar/Quaternar` at `75daaa9`, `StereoKitGUI/src/`: `Table/TableControl.hpp` (`UpdateTablesFromAppVolumes` L99-205), `Table/TableView.hpp` (`Render` L382-416), `Menu/AppVolumesHandMenu.hpp` (L88-131), `StreamSenderRequest.hpp` (L125); `docs/adr/hub/0273-a-source-app-chooses-its-targets-from-the-peer-list-and-its-sender-attaches-them.md`.

[CN]: ../../crates/qaros-sink/src/connection.rs "QAROS connection"
[VV]: ../../crates/qaros-sink/src/volume_visibility.rs "App volume visibility"
[VT]: ../../crates/qaros-sink/src/volume_transform.rs "Followed app volume transform"
[ST]: ../../crates/qaros-sink/tests/stub.rs "Stub tests"
[HDR]: https://github.com/Quaternar/qaros/blob/e0c0195/qar-streaming-c/include/qar_streaming.h "QAROS C header"
[QS]: https://github.com/Quaternar/qar_streaming/tree/e487a31158caca9a9cd2fcf6073456b316d8b7c6/libs/qar-streaming "QAROS streaming library source (private)"
[SK]: https://github.com/Quaternar/Quaternar/tree/75daaa96afcc325e4aa084700f0edecf98fb315c/StereoKitGUI/src "QuaternAR headset renderer source (private)"
