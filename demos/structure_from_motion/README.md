# Structure from motion

Rerun's `structure_from_motion` example on the `colmap_fiat` dataset: a COLMAP reconstruction of a car, replayed photo by photo.

## Start and stop

```sh
uv run demos/structure_from_motion/run.py
```

On the QAROS PC, add the viewer's QAROS options after `--`, for example `-- --sink qaros --pairing-code <PAIRING-CODE> --panels 0.0.0.0:9877`. Ctrl+C or closing the viewer window stops it.

The first start downloads the dataset (80 MB) and records it once, about 4 minutes on an Apple M5 Pro; later starts replay the cached 7 MB recording in seconds without network. The 51 photos play at 0.8 per second, so one loop is about a minute.

## What it shows

- Each photo adds a camera frustum to the 3D view, and the features it shares with earlier photos become 3D points. The point cloud accumulates over all past photos, so the car builds up as the camera moves around it.
- The camera panel shows the current photo with its detected keypoints; the reprojection error panel plots the average error per photo.

## In the glasses

The point cloud is centered on the median of the reconstructed points, at 0.3 of its reconstructed size (the car is about 1 m long), with the camera and reprojection error panels to the right of the volume. Walk around it: the model stays in place and each eye sees it from its own position.

## If something looks wrong

- Nothing in the volume: the recording is still loading on the first start after a `--record`; wait for "playing in a loop" in the console.
- The layout on the PC looks like Rerun's default: close the viewer and run the command again; the launcher stops leftover viewers itself.

## Data

The `colmap_fiat` reconstruction that Rerun hosts for its `structure_from_motion` example ([colmap_fiat.zip](https://storage.googleapis.com/rerun-example-datasets/colmap/colmap_fiat.zip)). The example does not state a licence for it.
