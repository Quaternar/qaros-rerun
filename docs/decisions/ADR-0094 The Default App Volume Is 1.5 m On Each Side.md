---
id: ADR-0094
title: The default app volume is 1.5 m on each side, the demos fit it, and a taken-over volume gets the requested size
status: proposed
date: 2026-11-03
repo: qaros-rerun
---

# ADR-0094: The default app volume is 1.5 m on each side, the demos fit it, and a taken-over volume gets the requested size

> Amends ADR-0089 and ADR-0092 (what a taken-over volume keeps).

## Context

The viewer asked QAROS for a 2 m cube by default (`--volume-size 2,2,2`), and the demos placed their data at 1 m to 1.7 m across, the air traffic demo in a 2.4 m volume of its own. In the owner's test of build `test/combined-1102d` on METALENSE 2 (2026-11-03, run 1), a scene that size does not fit the headset's field of view from where the user stands at the table; the owner asked for a default volume of 1.5 m on each side.

The auto-fit sizes each recording to the volume it requests: largest extent 0.7 of the volume's smallest side (ADR-0092). The demos place their first recording with hand-tuned `--center` and `--scale` instead, so they do not follow the volume's size by themselves.

A volume an earlier run left open is taken over by its common name. `qar_app_volumes_get_or_create` ignores the init for it, size included, and hands it over as QAROS holds it (ADR-0089). Without a resize, a volume first created at 2 m stays at 2 m whatever the next run asks for, and the 2.4 m air traffic volume stays at 2.4 m.

## Decision

- **The default is 1.5 m on each side.** `--volume-size` defaults to `1.5,1.5,1.5` (`DEFAULT_VOLUME_SIDE_METERS`), for the volume QAROS creates and for the auto-fit alike.
- **The demos shrink with it.** Each demo's `--scale` is three quarters of what it was, the ratio of 1.5 m to 2 m, so each scene keeps its share of the volume: structure from motion 0.135 (the car about 0.75 m long), drone lidar 0.018 (the basement about 1.3 m by 0.85 m), robot arm 1.5 (the sweep about 1.05 m wide). Air traffic leaves its 2.4 m volume for the default one at `--scale 1.125e-6`, 1.5/2.4 of its earlier scale. `--origin` stays: the robot arm's base, 0.45 m below its sweep's middle, still clears the table.
- **A taken-over volume gets the requested size.** After the take-over, the viewer reads the volume's size (`qar_app_volume_get_latest_size`) and, when any side differs from `--volume-size` by more than 1 mm, sets it (`qar_app_volumes_change_size`) and logs `qaros: took over app volume "<name>" at 2.00x2.00x2.00 m (width, length, height); resized to the requested 1.50x1.50x1.50 m (width, length, height)`. The place and the user's zoom stay as QAROS holds them (ADR-0089, ADR-0092). A failed read or resize is logged and the volume keeps the size QAROS holds.

## Consequences

- Positive: every demo and every auto-fitted recording is three quarters of its earlier size and fits a headset's view at the table; the first 1102e run resizes the volumes earlier builds left at 2 m and 2.4 m.
- Positive: `--volume-size` always holds: the size the data is fitted to is the size QAROS clips to.
- Negative: a volume a person resized in the Hub is set back to `--volume-size` at the next start. The viewer has no way to tell such a resize from an earlier run's request; the place and the zoom, which a person changes in the glasses, are kept.
- Negative: data that filled the 2 m volume with explicit `--center` and `--scale` and the default size is now cut at 1.5 m; pass `--volume-size 2,2,2` to keep the earlier box.
