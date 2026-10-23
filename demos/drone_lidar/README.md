# Drone lidar

Rerun's `drone_lidar` example: a drone flight through a basement with its indoor lidar, 18.7M points over 401 s.

## Start and stop

```sh
uv run demos/drone_lidar/run.py
```

On the QAROS PC, add the viewer's QAROS options after `--`, for example `-- --sink qaros --pairing-code <PAIRING-CODE>`. Ctrl+C or closing the viewer window stops it.

The first start downloads the dataset (157 MB) and records it once, about 3 minutes on an Apple M5 Pro; later starts replay the cached 210 MB recording in seconds without network. The flight plays at 6x, so one loop is a little over a minute.

## What it shows

- The blue points are the lidar scans of the last 5 s of flight, so the measured patch moves through the rooms with the drone.
- The red trail is the drone's path over the last minute.

## In the glasses

The whole basement fits on the table, about 1.5 m long. The demo has no 2D panels. Lean in to follow the trail through a doorway, or step back to see the floor plan.

## If something looks wrong

- The model looks frozen: the first seconds after start are the recording loading; playback starts by itself.
- Low frame rate: the lidar window is the cost. Set `DRONE_LIDAR_WINDOW_SECONDS` (for example 3, or 10 for a wider sweep) in the environment. The flight path is a 60 s trail (`DRONE_LIDAR_PATH_SECONDS`); the whole path (`inf`) costs more the further the flight goes.

## Data

Indoor lidar and trajectory that [Flyability](https://www.flyability.com) provided for Rerun's `drone_lidar` example, hosted by Rerun ([livemap.las.zip](https://storage.googleapis.com/rerun-example-datasets/flyability/basement/livemap.las.zip), [livetraj.csv](https://storage.googleapis.com/rerun-example-datasets/flyability/basement/livetraj.csv)). The example does not state a licence for it.
