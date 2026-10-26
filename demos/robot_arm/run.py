# /// script
# requires-python = ">=3.11"
# dependencies = ["rerun-sdk==0.40.0"]
# ///
"""Robot arm: an SO-100 arm loaded from its URDF, moving through its joint ranges."""

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

import rerun.blueprint as rrb
from launcher import Demo, Placement, demo_blueprint, demo_time_panel, run

URDF_DATA = "examples/rust/animated_urdf/data"
URDF_PARTS = [
    "Base",
    "Base_Motor",
    "Fixed_Jaw",
    "Fixed_Jaw_Motor",
    "Lower_Arm",
    "Lower_Arm_Motor",
    "Moving_Jaw",
    "Rotation_Pitch",
    "Rotation_Pitch_Motor",
    "Upper_Arm",
    "Upper_Arm_Motor",
    "Wrist_Pitch_Roll",
    "Wrist_Pitch_Roll_Motor",
]


def blueprint() -> rrb.Blueprint:
    return demo_blueprint(
        rrb.Spatial3DView(
            name="Robot arm",
            origin="/",
            line_grid=rrb.LineGrid3D(visible=False),
            overrides={"so_arm100/collision_geometries": rrb.EntityBehavior(visible=False)},
        ),
        demo_time_panel("example_time", playback_speed=1.0),
    )


DEMO = Demo(
    name="robot_arm",
    title="Robot arm",
    application_id="rerun_example_animated_urdf",
    example_directory="examples/python/animated_urdf",
    example_files=[
        "examples/python/animated_urdf/animated_urdf.py",
        f"{URDF_DATA}/so100.urdf",
        f"{URDF_DATA}/LICENSE",
        *(f"{URDF_DATA}/assets/{part}.stl" for part in URDF_PARTS),
    ],
    example_arguments=["animated_urdf.py"],
    # The middle of the space the arm sweeps; at twice its size the sweep is about 1.4 m wide.
    # 0.65 m above the origin, so the base (0.6 m below the sweep's middle) stays above the table.
    placement=Placement(center=(0.0, -0.03, 0.23), origin=(0.0, 0.65, -1.7), scale=2.0),
    blueprint=blueprint,
)

if __name__ == "__main__":
    run(DEMO)
