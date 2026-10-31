# /// script
# requires-python = ">=3.11"
# dependencies = ["rerun-sdk==0.40.0", "laspy", "numpy", "requests", "tqdm"]
# ///
"""Indoor drone lidar: a drone flies through a basement and its lidar map builds up around it."""

import math
import os
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

import numpy as np
import rerun as rr
import rerun.blueprint as rrb
from launcher import CACHE_DIRECTORY, Demo, Placement, demo_blueprint, demo_time_panel, run

# The window is re-queried every frame, so its length sets the frame rate: 5 s streams about
# 1.4 to 2.3 times faster than 10 s. DRONE_LIDAR_WINDOW_SECONDS=10 brings back the wider sweep.
LIDAR_WINDOW_SECONDS = float(os.environ.get("DRONE_LIDAR_WINDOW_SECONDS", "5"))
# The flight path is 4013 single-point rows (10 per second over 401 s), all re-queried every
# frame, so a path from the start slows the stream down as the flight goes on.
# DRONE_LIDAR_PATH_SECONDS=inf brings back the whole path.
PATH_SECONDS = float(os.environ.get("DRONE_LIDAR_PATH_SECONDS", "60"))
# The whole basement as a static backdrop: one lidar point in MAP_THINNING, coloured by height and
# dimmer than the scans (see-through glasses show dim colours as faint).
# A static entity is not re-queried per time step, so it costs far less than a long window.
MAP_THINNING = 60
MAP_CACHE = CACHE_DIRECTORY / "drone_lidar" / f"map-1-in-{MAP_THINNING}.npz"
LOW_COLOR = np.array([30.0, 70.0, 170.0])
HIGH_COLOR = np.array([70.0, 170.0, 150.0])
# The latest scans, bright against the backdrop.
SCAN_COLOR = (255, 255, 255)


def present_in_the_glasses(recording: rr.RecordingStream, example_directory: Path) -> None:
    positions, colors = basement_map(example_directory / "dataset" / "livemap.las")
    recording.log("map", rr.Points3D(positions, colors=colors, radii=-1.0), static=True)


def basement_map(las_path: Path) -> tuple[np.ndarray, np.ndarray]:
    if not MAP_CACHE.exists():
        import laspy

        points = laspy.read(las_path).points
        # The example's own conversion to meters.
        positions = np.column_stack((points.X / 1000.0, points.Y / 1000.0, points.Z / 1000.0))
        positions = positions[::MAP_THINNING].astype(np.float32)
        heights = positions[:, 2]
        low, high = np.percentile(heights, [2, 98])
        fraction = np.clip((heights - low) / max(high - low, 1e-6), 0.0, 1.0)[:, None]
        colors = (LOW_COLOR * (1.0 - fraction) + HIGH_COLOR * fraction).astype(np.uint8)
        np.savez(MAP_CACHE, positions=positions, colors=colors)
    cached = np.load(MAP_CACHE)
    return cached["positions"], cached["colors"]


def path_start() -> rrb.TimeRangeBoundary:
    if math.isinf(PATH_SECONDS):
        return rrb.TimeRangeBoundary.infinite()
    return rrb.TimeRangeBoundary.cursor_relative(seconds=-PATH_SECONDS)


def blueprint() -> rrb.Blueprint:
    # The lidar shows only the last scans: keeping every scan re-queries 18.7M points each frame
    # and drops the stream below 1 fps. The flight path is a trail of the last minute.
    return demo_blueprint(
        rrb.Spatial3DView(
            name="Drone lidar",
            origin="/",
            line_grid=rrb.LineGrid3D(visible=False),
            overrides={
                "/lidar": [
                    rrb.VisibleTimeRanges(
                        timeline="time",
                        start=rrb.TimeRangeBoundary.cursor_relative(seconds=-LIDAR_WINDOW_SECONDS),
                        end=rrb.TimeRangeBoundary.cursor_relative(),
                    ),
                    rr.Points3D.from_fields(colors=SCAN_COLOR, radii=-1.5),
                ],
                "/drone": [
                    rrb.VisibleTimeRanges(
                        timeline="time",
                        start=path_start(),
                        end=rrb.TimeRangeBoundary.cursor_relative(),
                    ),
                    rr.Points3D.from_fields(colors=(255, 120, 60), radii=0.4),
                ],
            },
        ),
        demo_time_panel("time", playback_speed=6.0),
    )


DEMO = Demo(
    name="drone_lidar",
    title="Drone lidar",
    application_id="rerun_example_drone_lidar",
    example_directory="examples/python/drone_lidar",
    example_files=["examples/python/drone_lidar/drone_lidar.py"],
    example_arguments=["drone_lidar.py"],
    # The middle of the scanned basement (1st to 99th percentile of the lidar points): 71 m by 47 m,
    # 1.7 m by 1.1 m here. Stray returns farther out fall outside the app volume.
    # Lower than the other demos (0.25 m above the origin): the basement is a flat floor plan,
    # 12 cm high here, and reads from above, not edge-on at eye level.
    placement=Placement(center=(-33.5, -4.1, 1.4), origin=(0.0, 0.25, -1.7), scale=0.024),
    blueprint=blueprint,
    presentation=present_in_the_glasses,
)

if __name__ == "__main__":
    run(DEMO)
