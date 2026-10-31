---
title: Quick start
description: Your Rerun project in a QAROS headset in five steps, with no change to how you log.
sidebar_position: 0
---

# Quick start: your Rerun project in a QAROS headset

Your logging code stays as it is. You point it at this viewer instead of the stock one, and the viewer puts your 3D view on the table in every headset and your 2D views next to it as panels.

## 1. Build the viewer on the Hub machine

QAROS 0.9.0-dev.3177 or later installed, then build `qaros-rerun` ([Installation](INSTALLATION.md)):

```powershell
cargo build --release -p qaros-rerun
```

## 2. Start it

From the signed-in desktop session (the Hub asks once to allow the viewer):

```powershell
.\target\release\qaros-rerun.exe --sink qaros --local-app --panels
```

## 3. Log to it

Use the SDK of the viewer's version, 0.40.0, and connect as you would to any running Rerun viewer:

```python
import rerun as rr

rr.init("my_app")
rr.connect_grpc("rerun+http://127.0.0.1:9876/proxy")
```

| Your program runs | Use |
| --- | --- |
| On the Hub machine | `127.0.0.1` |
| On another machine | The Hub machine's address, with TCP 9876 allowed through its firewall ([Integration guide](integration-guide.md#what-you-need)) |
| Rust | `rerun = "=0.40.0"`, `connect_grpc_opts("rerun+http://<host>:9876/proxy")` |

Already have a `.rrd`? Replay it through the SDK ([Recordings](integration-guide.md#recordings-rrd)).

## 4. Choose what shows

| Desktop layout | In the headset |
| --- | --- |
| One 3D view | The scene on the table, in stereo, for every user |
| Image, time series and other 2D views | A panel each, next to the scene, following the time cursor |
| The time bar | The timeline panel: play and scrub by finger |

Send a blueprint with a single 3D view and the 2D views you want as panels. A view becomes a panel when it is visible in the viewer's layout, in a window or headless (`--headless WxH` renders the layout off screen; a larger size gives sharper panels).

## 5. Fit it to the table

The app volume is 2 m on each side by default; data outside it is cut.

1. Run once with `RUST_LOG=qaros_rerun=debug`; the log prints the scene bounds.
2. `--center` is the middle of those bounds.
3. `--scale` is room metres per unit of your data: the volume size divided by the data's largest extent.

```powershell
.\target\release\qaros-rerun.exe --sink qaros --local-app --panels --center 0,0,5 --scale 0.025
```

`--origin` moves the volume in the room, `--yaw` turns the data. [Place your data](integration-guide.md#place-your-data-in-the-app-volume) has a worked example.

## If something is missing

| Symptom | Check |
| --- | --- |
| Nothing in the headset | The viewer's log shows `rendering for user-...`; the headset's player app is running and joined to this Hub |
| Only part of the data | `--center` and `--scale` (step 5) |
| No panels | `--panels` is on; the 2D views are visible in the layout (not in a hidden tab); QAROS is 3177 or later |
| A view is not streamed | Left-handed view coordinates: set `--up` |
| Slow with large point clouds | Shorten time windows in the blueprint; compact recordings with `rerun rrd optimize` |

Everything else, every option and the troubleshooting list: the [Integration guide](integration-guide.md).
