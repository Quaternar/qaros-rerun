---
id: ADR-0098
title: A taken-over app volume keeps its size unless --volume-size asks for one, and the data is fitted to the size the volume holds
status: proposed
date: 2026-11-03
repo: qaros-rerun
---

# ADR-0098: A taken-over app volume keeps its size unless --volume-size asks for one, and the data is fitted to the size the volume holds

> Amends ADR-0094 (what a taken-over volume's size becomes) and ADR-0092 (the size the auto-fit uses).

## Context

ADR-0094 sets every taken-over volume to `--volume-size`, which defaults to 1.5 m on each side. A volume a person set to another size in the Hub is set back at the next start, since the viewer cannot tell that size from one an earlier run asked for. The owner wants the viewer to keep what a person set up, as it keeps the volume's place and the user's zoom (ADR-0089, ADR-0092).

The auto-fit sizes each recording to the volume's smallest side (ADR-0092). Keeping a size other than the one the data is fitted to would leave the data larger than the box the mixer clips to, or smaller than the box. The connection is made before the headset renderer exists, on the UI thread and on the render thread alike, so the size the volume holds is known before any recording is fitted.

## Decision

- **`--volume-size` given: the volume gets that size.** A taken-over volume of another size is set to it, as ADR-0094 decided, and the log says `resized to the requested`.
- **No `--volume-size`: a taken-over volume keeps its size.** The viewer reads it (`qar_app_volume_get_latest_size`), makes no call, and logs `qaros: took over app volume "<name>" at 2.00x2.00x2.00 m (width, length, height); kept its size, the data fits to it (--volume-size sets one)`. A volume QAROS creates at this start is 1.5 m on each side (`DEFAULT_VOLUME_SIDE_METERS`).
- **The data is fitted to the size the volume holds.** The connection reports the size the volume ends the start with; the auto-fit and the panels beside the volume use it. A size that cannot be read is logged, and the data is fitted to the requested size.
- **The demos pass `--volume-size 1.5,1.5,1.5`.** Their `--center` and `--scale` are tuned for that box, so a demo always sets it.

## Consequences

- Positive: a size a person set in the Hub survives the next start, as the place and the zoom do.
- Positive: the data always fits the box the mixer clips to, whichever size the volume holds.
- Negative: a volume an earlier build left at 2 m keeps 2 m for a viewer started without `--volume-size`; pass `--volume-size 1.5,1.5,1.5` once to set it back.
- Negative: the start recording's `--center` and `--scale` are not scaled with a kept size; a run that gives them and no `--volume-size` places the data by hand in whatever box it takes over.
