//! Pure math between QAROS stream metadata and Rerun's renderer.
//!
//! QAROS hands out per-eye poses (OpenXR conventions: right-handed, Y up, minus Z forward) in
//! the bound app volume's app content space, in app units, already taken through the volume's
//! pose, app pose and app scale (`qar_render_frame_info_get_view_pose`, `qar_streaming.h` at
//! `b1338df`), and asymmetric fields of view as four angles. Rerun's
//! `re_renderer` only accepts a symmetric perspective with an infinite reverse-Z depth range,
//! plus a "viewport transformation" applied on top. This crate converts between the two and
//! has no dependency on Rerun or QAROS.

mod coordinates;
mod depth;
mod eye;
mod placement;
mod pose;
mod projection;

pub use coordinates::{AxisDirection, CoordinatesError, ViewCoordinates};
pub use depth::{DepthEncoding, DepthRange};
pub use eye::{EyeCamera, EyeCameraError};
pub use placement::{AppPlacement, VolumeTransform};
pub use pose::{Fov, NearFar, Pose};
pub use projection::{
    RectF32, SymmetricDecomposition, off_axis_infinite_reverse_z, principal_point_in_pixels,
    rerun_ndc_scale_and_translation, rerun_perspective_infinite_reverse_z,
};
