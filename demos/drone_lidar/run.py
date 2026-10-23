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

import rerun.blueprint as rrb
from launcher import Demo, Placement, demo_blueprint, demo_time_panel, run

# The window is re-queried every frame, so its length sets the frame rate: 5 s streams about
# 1.4 to 2.3 times faster than 10 s. DRONE_LIDAR_WINDOW_SECONDS=10 brings back the wider sweep.
LIDAR_WINDOW_SECONDS = float(os.environ.get("DRONE_LIDAR_WINDOW_SECONDS", "5"))
# The flight path is 4013 single-point rows (10 per second over 401 s), all re-queried every
# frame, so a path from the start slows the stream down as the flight goes on.
# DRONE_LIDAR_PATH_SECONDS=inf brings back the whole path.
PATH_SECONDS = float(os.environ.get("DRONE_LIDAR_PATH_SECONDS", "60"))


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
                "/lidar": rrb.VisibleTimeRanges(
                    timeline="time",
                    start=rrb.TimeRangeBoundary.cursor_relative(seconds=-LIDAR_WINDOW_SECONDS),
                    end=rrb.TimeRangeBoundary.cursor_relative(),
                ),
                "/drone": rrb.VisibleTimeRanges(
                    timeline="time",
                    start=path_start(),
                    end=rrb.TimeRangeBoundary.cursor_relative(),
                ),
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
    placement=Placement(center=(-19.5, -4.7, 2.2), scale=0.016),
    blueprint=blueprint,
)

if __name__ == "__main__":
    run(DEMO)
