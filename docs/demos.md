# Demos

Real Rerun examples shown in the glasses through QAROS. Each demo starts with one command, records itself once from a Rerun 0.40.0 example on the first start, and from then on replays the cached recording in a loop, with no network and no setup.

| Demo | What it shows | Data | Details |
| --- | --- | --- | --- |
| [Structure from motion](../demos/structure_from_motion/run.py) | A 3D point cloud of a car builds up from photos, camera by camera, beside the current photo and its reprojection error | `colmap_fiat` reconstruction hosted by Rerun for its `structure_from_motion` example, 80 MB [[COLMAP]] | [README](../demos/structure_from_motion/README.md) |
| [Drone lidar](../demos/drone_lidar/run.py) | A drone flies through a basement; its last lidar scans and its recent flight path move through the rooms, over a dim scan of the whole basement | Indoor lidar and trajectory provided by Flyability for Rerun's `drone_lidar` example, 157 MB [[FLY]] | [README](../demos/drone_lidar/README.md) |
| [Air traffic](../demos/air_traffic/run.py) | Ten minutes of ADS-B aircraft tracks over Germany and Switzerland in 3D above the region outlines, beside the same tracks on a map | INVOLI sample provided for Rerun's `air_traffic_data` example, 2.7 MB [[INVOLI]]; Eurostat GISCO NUTS 2021 outlines, 30 MB [[NUTS]] | [README](../demos/air_traffic/README.md) |
| [Robot arm](../demos/robot_arm/run.py) | An SO-ARM100 arm loaded from its URDF moves through its joint ranges | URDF and meshes from TheRobotStudio's SO-ARM100, Apache-2.0, shipped with Rerun's `animated_urdf` example, 2.9 MB [[SOARM]] | [README](../demos/robot_arm/README.md) |

Download sizes are the `Content-Length` of each dataset URL in the example's source.

## Running a demo

You need [uv](https://docs.astral.sh/uv/) and a release build of the viewer (`cargo build --release -p qaros-rerun`, see [INSTALLATION.md](INSTALLATION.md)). `uv run` reads each launcher's inline dependency list (Python 3.11 or newer, `rerun-sdk==0.40.0`) and keeps the environment in its own cache.

Mac, with a desktop window and the PNG image sink:

```sh
uv run demos/structure_from_motion/run.py
```

Windows PowerShell, streaming into QAROS (see the [integration guide](integration-guide.md) for the QAROS runtime and joining):

```powershell
uv run demos\structure_from_motion\run.py -- --sink qaros --pairing-code <PAIRING-CODE> --panels 0.0.0.0:9877
```

Everything after `--` goes to the viewer unchanged, so every option in `qaros-rerun --help` works. The launcher adds the demo's placement (`--center`, `--origin`, `--scale`, `--yaw`); a later `--scale` or `--origin` after `--` overrides it. `--record` records the demo again from the dataset, and `--record-only` prepares the recording and exits.

What a start does:

1. On the first start only: fetches the example's source from the Rerun repository at tag 0.40.0 into `demos/.cache/`, runs it to download its dataset and record a `.rrd`, and compacts the recording with `rerun rrd optimize` so it loads in seconds. On an Apple M5 Pro with a fast connection this took between 2.5 and 4 minutes per demo, including the Python environment, with every demo recording at the same time.
2. Stops a viewer whose launcher was killed or crashed (`viewer-<launcher pid>.pid` in the demo's cache directory). A viewer whose launcher still runs is another instance of the demo and keeps running.
3. Starts the viewer on port 9876, or on a free port when something else holds it, with its log in `demos/.cache/<demo>/logs/viewer-<UTC start>-<launcher pid>.log` instead of the console (the newest 20 are kept). `demos/.cache/<demo>/viewer.log` is a hard link to the newest start's log; on Windows it stays on the earlier log while that instance still runs. With `--panels`, a taken panel port (a second instance) moves the panels to a free port, logged as "panels: ... is taken".
4. Sends the demo's blueprint, replays the recording, and sends the blueprint again so it wins over the one the example recorded. Demos that need it then log static presentation data into the same recording (the drone's basement backdrop, the aircraft altitudes and colours, see below). The time panel plays and loops over the whole recording.
5. Runs until the viewer window is closed or Ctrl+C; both stop the viewer. The console hands Ctrl+C to the viewer too, which leaves QAROS and ends itself as on a QAROS Stop; the launcher waits up to 5 s for that before it terminates the viewer, and a second Ctrl+C ends both at once.

A blueprint error stops the launcher before anything starts (Rerun's strict mode), instead of falling back to Rerun's automatic layout.

## Layout in the glasses

Every blueprint hides the blueprint and selection panels, collapses the time panel to its one-line bar, and switches the 3D grid off. The 3D view goes to the headset as stereo; each 2D view becomes a QAROS GUI panel, created within reach of the first viewer near the volume (0.8 m away, to the right of the gaze, just below eye height); after that the Hub keeps it where it is, or where a user moved it, across runs (close the panels in the Hub Display for fresh placement). Placement puts the data's center 0.5 m above and 1.7 m in front of the room origin (the robot arm 0.65 m above, so its base clears the table; the flat drone floor plan 0.25 m above, so it is seen from above), scaled to between 1 m and 1.7 m across: a tabletop model you can walk around, readable from 1 to 2 m away, and inside the app volume so the mixer clips nothing that matters.

The demos take the room origin to be a QR origin marker lying on a table, with the floor 1.0 m below it: the data then floats at about standing eye level, 1.5 m above the floor, and nothing reaches below the floor. With the room origin on the floor, raise everything by 1.0 m with `--origin 0,1.5,-1.7` after `--` (`0,1.65,-1.7` for the robot arm, `0,1.25,-1.7` for the drone); a later `--origin` overrides the launcher's.

See-through glasses add the stream's light to the room: black is transparent, and dark colours are faint. The demos therefore draw what matters in light colours (white lidar scans, light aircraft) and use dim colours only for context (the drone's backdrop).

The centers come from the data: the scene bounds the viewer logs per headset frame with `RUST_LOG=qaros_rerun=debug`, over a whole loop, or percentiles of the dataset's points where a few stray points stretch the bounds.

| Demo | Placement (`--center` in Rerun units, `--scale`) | 2D panels | Blueprint settings and presentation data |
| --- | --- | --- | --- |
| Structure from motion | `-0.7,1.3,1.5` (median of the points), 0.18 | Camera image with keypoints, reprojection error | Point cloud accumulates over all past frames; point radius 0.012; 0.8 photos per second |
| Drone lidar | `-33.5,-4.1,1.4` (middle of the scanned basement), 0.024 | none | Lidar shows the last 5 s of scans in white (`DRONE_LIDAR_WINDOW_SECONDS`); flight path is an orange trail of the last 60 s (`DRONE_LIDAR_PATH_SECONDS`, `inf` for the whole path, which slows the stream as the flight goes on); playback 6x. Presentation: the whole scan, one point in 60, as a dim static backdrop coloured by height |
| Air traffic | `900000,5520000,120000` (middle of the traffic), 1.8e-6, app volume 2.4 m | Map (OpenStreetMap tiles) at zoom 7 | Region outlines in 3D only; 1-minute trails (`AIR_TRAFFIC_TRAIL_SECONDS`) of dots 2.5 UI points wide; playback 10x; no time series. Presentation: altitudes 20x instead of the example's 10x, a light colour per aircraft |
| Robot arm | `0,-0.03,0.23` (middle of the arm's sweep), 2.0 | none | Collision meshes hidden |

## How the examples were chosen

The demos are Rerun Python examples that have a 3D view (the headset stream renders the 3D view; a recording without one gives no stereo frame and no panels), download less than about 200 MB, record in a few minutes, and read in stereo at tabletop scale. Examples whose dataset licence restricts use, or whose dataset downloads several gigabytes, are not used.

## Evidence

Under [evidence/demos](evidence/demos), per demo a stereo frame (`stereo.png`: left and right eye side by side at 960x1000 per eye, the headset-like field of view, composited on a dark background where the stream is transparent) and the 2D panels the viewer served (`panel_<view>.png`). Each was captured through the demo's launcher with Rerun 0.40.0 on an Apple M5 Pro, headless, with the image sink, a head walking the default 1.6 m circle while looking at the data with its eyes 0.65 m above the origin, as a standing person's eyes above a table (`--motion orbit --eye-height 0.65`), and panel capture:

```sh
uv run demos/air_traffic/run.py -- --headless 1920x1080 --sink image --out out/air_traffic \
    --frames 2500 --write-every 100 --motion orbit --eye-height 0.65 --quit-when-done --panels 127.0.0.1:9877
curl -s -o panel_map.png http://127.0.0.1:9877/views/map/frame.png    # while it runs
```

Shown frames per second of the whole viewer in those runs, render and readback only (no QAROS encoding, no network), from the statistics the viewer logs at exit:

| Demo | Shown per second |
| --- | --- |
| Robot arm | 142 |
| Structure from motion | 92 |
| Air traffic | 42 |
| Drone lidar | 33 |

For a video of a demo, write every frame with a camera-like view and encode the PNGs with any encoder: `--write-every 1 --motion orbit --eye-height 0.65 --radius 1.5 --period 45 --fov 60 --eye-resolution 1280x720` gives a third of a circle in 900 frames, 30 s at 30 frames per second.

## Notes

- A replayed `.rrd` keeps the example's own application and recording id, and the viewer applies a blueprint only to recordings of the same application, so each launcher sends its blueprint under the example's application id.
- Examples log many small chunks. Compacting with `rerun rrd optimize` makes a replay stream in seconds instead of minutes.
- Time windows over large point clouds are re-queried every frame while playing, so the window length sets the frame rate. Accumulating the whole drone flight is not an option at this size.
- Without a zoom in the blueprint, Rerun's map view keeps the zoom it saved before the replay arrived (street level), so the air traffic blueprint sets zoom 7.
- Rerun's map view needs the `map_view` cargo feature, which `qaros-rerun` enables.
- Panel capture learns the list of views from the 3D view observer, so a recording without a 3D view publishes no panels, and panels of views that a new blueprint removed stay listed.
- Rerun's labels are egui overlays drawn after the 3D render, so they do not reach the headset stream.

[COLMAP]: https://storage.googleapis.com/rerun-example-datasets/colmap/colmap_fiat.zip "colmap_fiat dataset used by the structure_from_motion example"
[FLY]: https://storage.googleapis.com/rerun-example-datasets/flyability/basement/livemap.las.zip "Flyability basement lidar used by the drone_lidar example"
[INVOLI]: https://storage.googleapis.com/rerun-example-datasets/involi/involi_demo_set_1_10min.zip "INVOLI 10-minute set used by the air_traffic_data example"
[NUTS]: https://gisco-services.ec.europa.eu/distribution/v2/nuts/download/ref-nuts-2021-01m.json.zip "Eurostat GISCO NUTS 2021 boundaries"
[SOARM]: https://github.com/TheRobotStudio/SO-ARM100 "SO-ARM100 by TheRobotStudio"
