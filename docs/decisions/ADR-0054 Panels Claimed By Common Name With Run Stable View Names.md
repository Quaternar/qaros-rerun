---
id: ADR-0054
title: Panels claimed by common name, stable across runs, hidden at Stop instead of closed
status: accepted
date: 2026-10-19
repo: qaros-rerun
---

# ADR-0054: Panels claimed by common name, stable across runs, hidden at Stop instead of closed

## Context

QAROS keeps GUI panels and app volumes in the session after the app that made them is gone; both are identified by stable common names and created idempotently by them ([CON] "Identity, naming, and persistence"). `get_or_create` returns the existing panel or creates it, and the panel's id is derived from its common name, so "restarting your app reattaches to the same panel instead of spawning duplicates" ([GP] L18-20, L101). The Hub keeps a panel's place, or wherever a user moved it, across runs (ADR-0070).

A panel snapshot carries its id, display name, pose, size, content URI, state and visible peers; there is no common-name getter ([QP] `GuiPanelInfo`). Navigating a panel makes the mixer replace its web page. On QAROS builds up to 3137 the mixer crashed when an app closed its panels or set new URLs on reused ones ([KI] "Mixer crash when panels close or navigate"). A panel whose document failed to load stays white until it is navigated ([KI] "White panel after a Hub restart").

## Decision

- **Stable view names.** A view's panel name is the same on every run: the view's name reduced to `[a-z0-9_]` (at most 40 characters, `view` when nothing is left); views sharing a name get `-2`, `-3` and so on in screen order, top to bottom, then left to right. A view keeps its name while it stays on screen, and one that leaves the screen and returns gets its name back if no other view took it meanwhile, so same-named views that return in another order do not swap panels within a run. Panel capture maps view ids to names, so the panel server routes by the stable name.
- **Claim by common name.** The panel's common name is `<view name>.<volume common name>.rerun`, its URL `<base URL>/views/<view name>`. A run claims the panel by common name and calls `navigate_to_uri` only when the panel does not already show that URL (a new panel, or a changed panel server address or port). A panel the user closed counts as absent and is claimed again by the same name (ADR-0075).
- **Hide at Stop.** Stop does not close panels. By default it sets them `QAR_GUI_PANEL_STATE_HIDDEN`, and the next run's claim sets a panel it finds hidden back to `VISIBLE` after its size and any navigation; a panel a user minimised stays minimised. `--panels-at-stop keep` makes no panel call at Stop. `QarosPanels::sync` never closes a panel, and the app volume is closed at Stop after the panels.
- **Leftovers.** The display name repeats the common name, since a listing shows display names. A listed panel is a leftover of an earlier run when its display name is `<name>.<volume common name>.rerun` with a name holding no dot, this run does not hold it, and this run publishes no view of that name (one that publishes but has not settled claims its panel later). Once a panel opens, visible leftovers are hidden, never closed: a view this run publishes later (a background tab, entities arriving after another view settled) is not yet among the published names, and its later claim by name shows the hidden panel again with its page in place. A leftover a user minimised stays minimised. Panels of other apps and other volumes are left alone whatever their URL.
- **Dead pages.** The panel page asks for a missing frame again with back-off (0.5 s doubling to 4 s, reset by a loaded frame) and never stops, so a page that loaded resumes by itself once the viewer's panel server is back. A document that failed to load runs no script and recovers only by a navigation. The panel server notes, per view, the last request for its page or a frame. A browser that runs no timers (the headset off or asleep at viewer start, the mixer not rendering panels) leaves a live page as silent as a dead one, so silence counts only once the run has seen the browser running: a user stream has started in this run, or any view page has asked this run's panel server for anything. A view panel claimed without navigation that is visible after the claim (not minimised), whose view has had no request since the panel server started, and that stays silent 10 s (`PAGE_SILENCE_LIMIT`) after both its claim and the first sight of the running browser is navigated once in that run, to the URL it already shows. `--dead-panels leave` makes no navigation.

Ways to hide a panel without closing it:

| Candidate | Fits | Why |
| --- | --- | --- |
| `set_state(HIDDEN)` | yes, chosen | A documented panel state ([HDR] L1142-1147, [GP] L136) set through the safe API; no URL, size or pose change. |
| `MINIMIZED` | no | The state a user gives a panel; the C example sets it as a user action would ([EX] L178). Overriding it at Stop and restoring it would undo a user's choice. |
| Empty visible-to set | no | A count of 0 means visible to every peer ([HDR] L4572-4578); a set naming only an absent peer would hide it by misuse, and the safe API has no visible-to setter. |
| Size or content scale of zero | no | Limits undocumented; `content_scale` is the page's zoom and reflows it ([HDR] `QarGuiPanelSize`), which exercises the page. |
| Moving it out of view | no | The Hub ignores the pose of an existing panel and keeps the user's placement (ADR-0070). |
| Navigating to a blank page | no | A navigation replaces the page, the call that crashed the mixer on builds up to 3137. |

## Alternatives considered

- Keep the view id in the URL and skip navigation only when the URL matches: the id changes every run, so it never matches.
- Map stable paths to current view ids inside the panel server: the same result with routing state in a second place.
- Recognise own panels by computing the panel id from the common name, or by URL host: the derivation is the library's and undocumented, and a changed address is the case to survive.
- Keep panels visible at Stop by default: no extra state change, but a frozen or white page hangs in the room while the app is not running; kept as `--panels-at-stop keep`.

Dead pages:

| Candidate | Fits | Why |
| --- | --- | --- |
| Page retries with back-off | yes, kept | Recovers a page that loaded; cannot reach a document that never loaded. |
| One navigation, gated on the page's silence | yes, chosen | The only call that replaces a failed document. It reaches only pages shown to be dead, once per run, at an unchanged URL, so the next run still finds "already showing". |
| Silence counted from the claim alone | no | A headset off or asleep at viewer start runs no page timers, so every live page would look dead and be navigated. |
| One navigation only for a panel hidden by an earlier run | no | A run ended without its Stop (a forced end) never hides its panels, and the docs do not say whether a hidden state survives a Hub restart. |
| A cache-busting redirect served by the page | no | A document that failed to load has no page to run it. |
| Navigate every reattached panel | no | Every restart would replace every page in the mixer. |

## Consequences

- Positive: Stop then Start reattaches every panel with no close and no navigation; stub tests assert the call log (one `set_state` per panel at Stop and at the restart, no `navigate_to_uri`, no `close_panel`) and the panel states (`a_stop_then_start_shows_the_hidden_panels_again_without_navigating_them`, `panels_kept_at_stop_get_no_call_and_are_reattached_as_they_are`).
- Positive: panels left on another address are found by name and navigated once (`a_changed_panel_server_address_still_finds_the_panels_by_name`); panels of other apps stay. A view that publishes after another claimed its panel finds its hidden panel with no close and no navigation (`a_view_that_publishes_late_after_a_restart_reclaims_its_hidden_panel`).
- Positive: a reattached white panel comes back in its run about 10 s after its claim or after the first stream, whichever is later. Stub tests assert no navigation while the headset is off, one navigation per silent visible page 10 s after a stream starts, and, beside a live page, one navigation for the silent page, none for the live and minimised ones, none on a repeat and none in a next run where the page is no longer silent. A Node test runs the page's script through an outage.
- Negative: the docs do not say whether a hidden panel keeps its page, whether an app may toggle hidden and visible freely, whether a user can unhide it, whether the hidden state survives a Hub restart, or whether closing an app volume closes its panels; `--panels-at-stop keep` is the fallback should hiding turn out to tear the page down like a close.
- Negative: a true leftover (a view no later run publishes) stays in the session, hidden, where it stands.
- Negative: two same-named views swap names if their screen order swaps between runs.
- Negative: a page that never recovers is navigated once in every run, since each run starts with no record of the last; a browser on the LAN that opens a view's page makes that page count as alive, and a minimised white panel stays white.

## Evidence and sources

- [PN] `crates/qaros-sink/src/panels.rs` (`QarosPanels::sync`, `open_panel`, `is_left_by_earlier_run`, `hide_panels_left_by_earlier_runs`, `reload_dead_pages`, `PAGE_SILENCE_LIMIT`, `PanelsAtStop`, `DeadPages`).
- [PC] `crates/qaros-rerun/src/panel_capture.rs` (`readable_panel_name`, stable names), `crates/qaros-rerun/src/qaros_setup.rs` (`PanelServerActivity`).
- [PS] `crates/panel-server/src/server.rs` (`last_page_request`, `any_page_request`), `crates/panel-server/src/page.html`, `crates/panel-server/tests/page_retry.mjs`.
- [TS] `crates/qaros-sink/tests/stub.rs` (the tests named above, `a_reattached_page_is_not_reloaded_while_the_headset_is_off`, `a_page_still_silent_after_a_stream_started_is_loaded_again_once_at_the_same_url`, `a_silent_page_beside_a_live_one_is_loaded_again_once`).
- [KI] [Known issues](../known-issues.md): the mixer crash when panels close or navigate, and the white panel after a Hub restart.
- [CON] QAROS docs at `ebfe3bf`, `docs/website/docs/developer-guide/concepts.md`, "Identity, naming, and persistence".
- [GP] QAROS docs at `ebfe3bf`, `docs/website/docs/developer-guide/gui-panels.mdx` L18-20 (`get_or_create`), L101 (id from the common name), L136 (panel states).
- [HDR] QAROS header at `ebfe3bf`, `qar-streaming-c/include/qar_streaming.h` L1116-1130 (`QarGuiPanelSize`), L1142-1147 (`QarGuiPanelState`), L4572-4578 (visible-to count).
- [QP] The `qaros` crate at `ebfe3bf`, `qar-streaming-rust/crates/qaros/src/panels.rs` L77-85 (`GuiPanelInfo`).
- [EX] QAROS example at `ebfe3bf`, `qar-streaming-c/examples/gui_panel_operations.c` L178.

[PN]: ../../crates/qaros-sink/src/panels.rs "QAROS panels"
[PC]: ../../crates/qaros-rerun/src/panel_capture.rs "Panel capture"
[PS]: ../../crates/panel-server/src/server.rs "Panel server"
[TS]: ../../crates/qaros-sink/tests/stub.rs "Sink stub tests"
[KI]: ../known-issues.md "Known issues"
[CON]: https://github.com/Quaternar/qaros/blob/ebfe3bf/docs/website/docs/developer-guide/concepts.md "Concepts"
[GP]: https://github.com/Quaternar/qaros/blob/ebfe3bf/docs/website/docs/developer-guide/gui-panels.mdx "GUI Panels"
[HDR]: https://github.com/Quaternar/qaros/blob/ebfe3bf/qar-streaming-c/include/qar_streaming.h "QAROS C header"
[QP]: https://github.com/Quaternar/qaros/blob/ebfe3bf/qar-streaming-rust/crates/qaros/src/panels.rs "qaros GUI panels"
[EX]: https://github.com/Quaternar/qaros/blob/ebfe3bf/qar-streaming-c/examples/gui_panel_operations.c "GUI panel example"
