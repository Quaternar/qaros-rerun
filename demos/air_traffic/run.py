# /// script
# requires-python = ">=3.11"
# dependencies = [
#     "rerun-sdk==0.40.0",
#     "geopandas",
#     "numpy",
#     "polars",
#     "pyproj",
#     "requests",
#     "shapely",
#     "tqdm",
# ]
# ///
"""Air traffic: two hours of ADS-B tracks over Switzerland as a 3D air picture on the table."""

import os
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

import rerun.blueprint as rrb
from launcher import Demo, Placement, demo_blueprint, demo_time_panel, run

# Every frame re-queries each aircraft's trail, so the trail length sets the frame rate: 60 s
# streams 1.3 to 3 times faster than 120 s and still shows each aircraft's heading.
# AIR_TRAFFIC_TRAIL_SECONDS=120 brings back the longer trails.
TRAIL_SECONDS = float(os.environ.get("AIR_TRAFFIC_TRAIL_SECONDS", "60"))
MAP_ZOOM_LEVEL = 7.0


def blueprint() -> rrb.Blueprint:
    trail = rrb.VisibleTimeRange(
        "unix_time",
        start=rrb.TimeRangeBoundary.cursor_relative(seconds=-TRAIL_SECONDS),
        end=rrb.TimeRangeBoundary.cursor_relative(),
    )
    # One series per aircraft makes a time series view too slow to redraw at headset rates, so
    # the 2D side is the map alone.
    return demo_blueprint(
        rrb.Horizontal(
            rrb.Spatial3DView(
                name="Air picture",
                origin="/",
                contents=["+ /aircraft/**", "+ /region_boundaries/**", "- /region_boundaries/**/2D"],
                line_grid=rrb.LineGrid3D(visible=False),
                time_ranges=trail,
            ),
            # Without a zoom in the blueprint, the map view saves the zoom it used before the
            # replay arrived (16, street level) into the blueprint and stays there; zoom 7 shows
            # the whole region.
            rrb.MapView(name="Map", origin="/", zoom=MAP_ZOOM_LEVEL),
            column_shares=[3, 2],
        ),
        demo_time_panel("unix_time", playback_speed=10.0),
    )


DEMO = Demo(
    name="air_traffic",
    title="Air traffic",
    application_id="rerun_example_air_traffic_data",
    example_directory="examples/python/air_traffic_data",
    example_files=["examples/python/air_traffic_data/air_traffic_data.py"],
    example_arguments=["air_traffic_data.py", "--dataset", "10min"],
    placement=Placement(center=(886_000.0, 5_490_000.0, 0.0), scale=1.5e-6),
    blueprint=blueprint,
)

if __name__ == "__main__":
    run(DEMO)
