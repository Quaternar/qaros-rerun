---
id: ADR-0102
title: A QAROS listing is one query with no count before it
status: proposed
date: 2026-11-03
repo: qaros-rerun
---

# ADR-0102: A QAROS listing is one query with no count before it

> Amends ADR-0073 (the panel listing) and ADR-0093 (the connect's app volume listing).

## Context

Seen on METALENSE 2 with QAROS 0.9.0-dev.3184, build `combined-1103` (viewer logs of the `structure_from_motion` demo, 2026-11-03 19:40 to 19:42 UTC): every panel listing took 4.0 s (`qaros: panel listing 1 of this run took 4.0013873s on the QAROS thread`, `4.0014777s`, `4.0024251s`), and every claim 2.0 s from `stays where it is held` to `reattached`. A run's panels opened about 15 s after its connect.

The waits come from QAROS's reconciliation query, read in the runtime's source ([QS], the `qar-streaming` commit the 3184 build `2ccfb349` pins):

- **Every query waits out its 2 s window.** `SharedObjectClient::Query` sends the mutation and snapshot queries and drains both until their reply channels close (`SharedObjectClient.hpp` L404-L425, `SharedObjectQueryHelpers.hpp` `DrainLatestSharedSnapshots`, `DrainSharedMutationsByObject`). With `ResendOnNewQueryHandler` the query re-sends to responders that appear until the deadline, so the channel closes only at `kSharedObjectReconcileQueryTimeoutMs` = 2000 ms (`SharedObjectTypes.hpp` L27; `ZenohMessaging.cpp` `QueryRetry` L4788-L4810). The comment at L407-L410 says so: "the query helpers never early-exit". Nothing in the C API sets the window or asks for the first reply.
- **A count is a full query.** `qar_impl_query_gui_panels_count` and `qar_impl_query_gui_panels` each call `GuiPanels().QueryPanels()`, which is `SharedObjectClient::Query` (`qar-streaming-c/src/v0/gui_panels.cpp` L1427-L1490, `qar-streaming/src/GuiPanelImpl.cpp` L716-L718); the app volume pair calls `QueryAppVolumes()` twice the same way (`app_volumes.cpp` L1012-L1090). The viewer called the count, then the query: two windows, 4 s.
- **The query alone holds every object when the buffer is large enough.** It writes `min(objects, buffer)` handles and reports how many (`gui_panels.cpp` L1493-L1498; `app_volumes.cpp` L1089-L1095).
- **The claim's 2 s is `set_stream`.** `qar_impl_gui_panels_get_or_create` reads the library's local cache (`GuiPanelImpl.cpp` L357-L380, `TryGetPanel`) and returns at once; `qar_impl_gui_panels_set_stream` first validates the panel with `QueryPanels()` (`gui_panels.cpp` L29-L36, L283), a full query.

## Decision

- **A listing is one query.** Panel and app volume listings call `qar_query_gui_panels` and `qar_query_app_volumes` with room for 64 handles and no count before them (`handle_query::query_all_handles`). A buffer the query fills is released and the query asked again with twice the room, so no object is missed however many there are.
- **The claim keeps `set_stream`.** A panel taken over from an earlier run still names that run's stream, and the listing is not trusted to show every panel (`StreamPanels::claim`), so the claim's 2 s stays until QAROS validates without a full query.

## Alternatives considered

- **Keep the count, ask the query with the counted size.** Exact, and twice as slow.
- **Skip `set_stream` for a panel the claim built afresh.** `get_or_create` builds a new or closed panel with the stream in its init, so the call is redundant there; but a reattached panel needs it, and every reattach of a run pays it. Left for QAROS: `set_stream` should validate from the local cache, as `get_or_create` reads it.
- **Shorten the window or ask for the first reply.** Not exposed by the C API.

## Consequences

- Positive: a panel listing takes about 2 s instead of 4, so a run's panels open about 2 s sooner, and the QAROS thread, which also shows panel frames and drives the eye frames on the inline path, is blocked 2 s less. The connect's app volume listing is 2 s shorter.
- Positive: ADR-0096 is unchanged: the listing still waits while a new sender's first frame is due, and it blocks the thread for less time.
- Negative: a session with 64 or more panels or volumes pays a second query. None has come near it.
- To report to QuaternAR: every reconciliation query waits its full 2 s window even when every responder has answered; the count calls repeat the whole query; and `qar_gui_panels_set_stream` runs a full query to validate a panel the library already holds. The same wait as Quaternar#345.

[QS]: https://github.com/Quaternar/qar_streaming/tree/e487a31158caca9a9cd2fcf6073456b316d8b7c6/libs "QAROS streaming library source (private)"
