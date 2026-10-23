# /// script
# requires-python = ">=3.11"
# dependencies = ["rerun-sdk==0.40.0", "numpy", "opencv-python>4.6", "requests>=2.31,<3", "tqdm"]
# ///
"""Structure from motion: a 3D model of a car grows from photos, camera by camera."""

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

import rerun as rr
import rerun.blueprint as rrb
from launcher import Demo, Placement, demo_blueprint, demo_time_panel, run


def blueprint() -> rrb.Blueprint:
    return demo_blueprint(
        rrb.Horizontal(
            rrb.Spatial3DView(
                name="Reconstruction",
                origin="/",
                contents=["+ /**", "- /plot/**", "- /description"],
                line_grid=rrb.LineGrid3D(visible=False),
                overrides={
                    # Each frame logs only the points its camera sees; keeping all earlier
                    # frames lets the whole car build up as the camera moves around it.
                    "/points": [
                        rrb.VisibleTimeRanges(
                            timeline="frame",
                            start=rrb.TimeRangeBoundary.infinite(),
                            end=rrb.TimeRangeBoundary.cursor_relative(),
                        ),
                        rr.Points3D.from_fields(radii=0.012),
                    ],
                },
            ),
            rrb.Vertical(
                rrb.Spatial2DView(name="Camera", origin="/camera/image"),
                rrb.TimeSeriesView(name="Reprojection error", origin="/plot"),
            ),
            column_shares=[3, 2],
        ),
        demo_time_panel("frame", frames_per_second=0.8),
    )


DEMO = Demo(
    name="structure_from_motion",
    title="Structure from motion",
    application_id="rerun_example_structure_from_motion",
    example_directory="examples/python/structure_from_motion",
    example_files=[
        "examples/python/structure_from_motion/structure_from_motion/__init__.py",
        "examples/python/structure_from_motion/structure_from_motion/__main__.py",
        "examples/python/structure_from_motion/structure_from_motion/read_write_model.py",
    ],
    example_arguments=["-m", "structure_from_motion", "--dataset", "colmap_fiat", "--resize", "800x600"],
    placement=Placement(center=(-2.1, 1.2, 1.0), scale=0.3),
    blueprint=blueprint,
)

if __name__ == "__main__":
    run(DEMO)
