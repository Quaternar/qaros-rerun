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

import colorsys
import json
import os
import sys
import zlib
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

import rerun as rr
import rerun.blueprint as rrb
from launcher import Demo, Placement, demo_blueprint, demo_time_panel, run

# Every frame re-queries each aircraft's trail, so the trail length sets the frame rate: 60 s
# streams 1.3 to 3 times faster than 120 s and still shows each aircraft's heading.
# AIR_TRAFFIC_TRAIL_SECONDS=120 brings back the longer trails.
TRAIL_SECONDS = float(os.environ.get("AIR_TRAFFIC_TRAIL_SECONDS", "60"))
MAP_ZOOM_LEVEL = 7.0
# The example draws altitudes 10 times higher; at tabletop scale that still leaves cruising
# aircraft 2 cm above the map, so the demo raises them 20 times instead (about 0.4 m).
ALTITUDE_EXAGGERATION = 20.0
# Aircraft as dots a few UI points wide, so each one-minute trail reads as a line in the glasses.
AIRCRAFT_RADIUS_UI_POINTS = 2.5


DATASET_DIRECTORY = "dataset/10min/involi_demo_set_1_10min"


def present_in_the_glasses(recording: rr.RecordingStream, example_directory: Path) -> None:
    """Higher altitudes, and a light colour per aircraft: see-through glasses add light to the
    room, so the example's darker colours would all but vanish."""
    recording.log("aircraft", rr.Transform3D(scale=[1.0, 1.0, ALTITUDE_EXAGGERATION]), static=True)
    for icao_id in aircraft_ids(example_directory / DATASET_DIRECTORY):
        recording.log(f"aircraft/{icao_id}", rr.Points3D.from_fields(colors=light_color(icao_id)), static=True)


def aircraft_ids(dataset_directory: Path) -> set[str]:
    identifiers = set()
    for path in dataset_directory.glob("*.json"):
        for batch in json.loads(path.read_text(encoding="utf-8")):
            for record in batch["records"]:
                if icao_id := record["aircraft"].get("ids", {}).get("icao"):
                    identifiers.add(icao_id)
    return identifiers


def light_color(icao_id: str) -> tuple[int, int, int]:
    hue = (zlib.crc32(icao_id.encode()) % 360) / 360.0
    red, green, blue = colorsys.hsv_to_rgb(hue, 0.5, 1.0)
    return round(red * 255), round(green * 255), round(blue * 255)


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
                # A default rather than an override: it reaches every aircraft entity, and the
                # example logs no radii of its own.
                defaults=[rr.Points3D.from_fields(radii=-AIRCRAFT_RADIUS_UI_POINTS)],
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
    # Centered on the traffic rather than on the region outlines; the outlines reach about 1.1 m
    # north of it, inside the 2.4 m volume.
    placement=Placement(center=(900_000.0, 5_520_000.0, 120_000.0), scale=1.8e-6, volume_size=(2.4, 2.4, 2.4)),
    blueprint=blueprint,
    presentation=present_in_the_glasses,
)

if __name__ == "__main__":
    run(DEMO)
