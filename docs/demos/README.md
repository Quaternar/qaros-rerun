---
title: Rerun demos
description: Real Rerun examples in AR glasses through QAROS, each started with one command on the Hub PC.
sidebar_position: 3
---

# Rerun demos

Four of Rerun's own examples, shown in the glasses through QAROS. Each starts with one command, records itself once from the Rerun 0.40.0 example on the first start, and from then on replays the cached recording in a loop, with no network and no setup.

| Demo | What you see | What it shows about QAROS |
| --- | --- | --- |
| [Structure from motion](structure-from-motion.md) | A car's point cloud builds up photo by photo, with the current photo and the reprojection error as panels | Large accumulating point clouds next to images and plots |
| [Drone lidar](drone-lidar.md) | A drone's lidar sweeps through a scanned basement on the table | Millions of lidar points at tabletop scale |
| [Air traffic](air-traffic.md) | Ten minutes of aircraft over Germany and Switzerland in 3D, with a live map panel | Geospatial data, a region shrunk to a table, a map beside it |
| [Robot arm](robot-arm.md) | An SO-ARM100 arm from its URDF, moving through its joint ranges | Robots and digital twins from standard URDF |

## Run a demo

On the PC that runs the QAROS Hub, with the Hub running, from the repository root in PowerShell:

```powershell
uv run demos/structure_from_motion/run.py -- --sink qaros --local-app --panels 0.0.0.0:9877
```

You need [uv](https://docs.astral.sh/uv/) and a release build of the viewer (`cargo build --release -p qaros-rerun`, see [Installation](../INSTALLATION.md)). `uv run` reads each launcher's inline dependency list (Python 3.11 or newer, `rerun-sdk==0.40.0`) and keeps the environment in its own cache. With QAROS installed the viewer finds its runtime library by itself; otherwise set `QAROS_DLL` as in the [integration guide](../integration-guide.md#where-the-qaros-library-comes-from).

- On its first start the Hub asks a person at the PC to allow the viewer.
- Open the visualizer or start the player app on a headset to see the scene.
- Ctrl+C or closing the viewer window stops the demo; the viewer leaves QAROS in order.

Everything after `--` goes to the viewer unchanged, so every option in `qaros-rerun --help` works. The launcher adds the demo's placement (`--center`, `--origin`, `--scale`, `--yaw`); a later `--scale` or `--origin` after `--` overrides it. `--record` records the demo again from the dataset, and `--record-only` prepares the recording and exits.

## Panels in every demo

With `--panels`, every demo opens a **timeline panel** in front of the first headset near the scene: a slider and a Play button, 0.5 m in front of the head and 0.2 m below the eyes, tilted to face them.

- Drag the slider with a finger: the demo pauses and the time cursor moves along the whole recording. The desktop time bar follows.
- Tap Play to resume the loop.
- The thumb follows the viewer's time a few times a second, during playback and when someone moves the desktop time bar.

Demos with 2D views (structure from motion, air traffic) also open each view as a **panel** within reach of the same headset: 0.8 m away, to the right of the gaze, just below eye height. A Display Receiver places them only when no headset streams within 60 s.

The Hub keeps every panel where it is, or where you moved it, across runs. Close the panels in the Hub Display to have the next run open them in front of you again.

## Placement in the room

Each demo puts the data's center 0.5 m above and 1.7 m in front of the room origin, scaled to between 1 m and 1.7 m across: a tabletop model you can walk around, readable from 1 to 2 m away, and inside the app volume so QAROS clips nothing that matters. The robot arm sits 0.65 m above the origin, so its base clears the table; the flat drone floor plan 0.25 m above, so it is seen from above.

The demos take the room origin to be a QR origin marker lying on a table, with the floor 1.0 m below it. The data then floats at about standing eye level. With the room origin on the floor, raise everything by 1.0 m with `--origin 0,1.5,-1.7` after `--` (`0,1.65,-1.7` for the robot arm, `0,1.25,-1.7` for the drone).

See-through glasses add the stream's light to the room: black is transparent, and dark colours are faint. The demos therefore draw what matters in light colours and use dim colours only for context.

## What a start does

1. **First start only:** fetches the example's source from the Rerun repository at tag 0.40.0 into `demos/.cache/`, runs it to download its dataset and record a `.rrd`, and compacts the recording with `rerun rrd optimize` so it loads in seconds. This takes a few minutes per demo.
2. Stops a viewer left behind by a launcher that was killed or crashed. A viewer whose launcher still runs is another instance of the demo and keeps running.
3. Starts the viewer on port 9876, or on a free port when something else holds it, with its log in `demos/.cache/<demo>/logs/` (the newest 20 are kept; `demos/.cache/<demo>/viewer.log` links to the newest). With `--panels`, a taken panel port moves the panels to a free port, logged as "panels: ... is taken".
4. Sends the demo's blueprint, replays the recording, and sends the blueprint again so it wins over the one the example recorded. Some demos then log presentation data for the glasses (the drone's basement backdrop, the aircraft altitudes and colours). The recording plays and loops.
5. Runs until the viewer window is closed or Ctrl+C. The launcher waits up to 5 s for the viewer to leave QAROS before it terminates it; a second Ctrl+C ends both at once.

A blueprint error stops the launcher before anything starts (Rerun's strict mode), instead of falling back to Rerun's automatic layout.

## Without a headset

On any machine, including a Mac, a demo runs with the desktop window and the PNG image sink:

```sh
uv run demos/structure_from_motion/run.py
```

To record a video of a demo, write every frame with a camera-like view and encode the PNGs with any encoder: `--write-every 1 --motion orbit --eye-height 0.65 --radius 1.5 --period 45 --fov 60 --eye-resolution 1280x720` after `--` gives a third of a circle in 900 frames, 30 s at 30 frames per second.

## Notes for your own recordings

The demos are a template for bringing your own data to the glasses ([integration guide](../integration-guide.md#send-your-own-data)). Worth knowing:

- A replayed `.rrd` keeps the example's own application id, and the viewer applies a blueprint only to recordings of the same application, so each launcher sends its blueprint under the example's application id.
- Examples log many small chunks. Compacting with `rerun rrd optimize` makes a replay stream in seconds instead of minutes.
- Time windows over large point clouds are re-queried every frame while playing, so the window length sets the frame rate.
- Without a zoom in the blueprint, Rerun's map view keeps the zoom it saved before the replay arrived (street level); the air traffic blueprint sets zoom 7.
- 2D view panels are published only for recordings with a 3D view: the headset stream renders the 3D view, and a recording without one gives no stereo frame and no panels.
- Rerun's labels are egui overlays drawn after the 3D render, so they do not reach the headset.

The demos are Rerun Python examples that have a 3D view, download less than about 200 MB, record in a few minutes, and read in stereo at tabletop scale. Examples whose dataset licence restricts use, or whose dataset downloads several gigabytes, are not used.
