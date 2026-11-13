//! Copies of the 3D view's draw data, one per eye.
//!
//! `QueueableDrawData` boxes its contents and is not `Clone`, and a `ViewBuilder` consumes what
//! it is given. Every concrete draw data type in `re_renderer` 0.40.0 is `Clone` (the GPU
//! resources inside are reference counted), so a copy is a downcast through `Any` plus a clone.
//! Types not listed here are skipped and reported once; a Rerun upgrade that adds a renderer
//! shows up as a warning rather than missing geometry going unnoticed.

use std::any::Any;

use re_renderer::QueueableDrawData;
use re_renderer::renderer::{
    DepthCloudDrawData, GaussianSplatDrawData, GenericSkyboxDrawData, LineDrawData, MeshDrawData,
    PointCloudDrawData, RectangleDrawData, VolumeDrawData, VoxelGridDrawData, WorldGridDrawData,
};

pub fn copy_draw_data(draw_data: &QueueableDrawData) -> Option<QueueableDrawData> {
    let erased: &dyn Any = &**draw_data;
    copy_as::<PointCloudDrawData>(erased)
        .or_else(|| copy_as::<LineDrawData>(erased))
        .or_else(|| copy_as::<MeshDrawData>(erased))
        .or_else(|| copy_as::<RectangleDrawData>(erased))
        .or_else(|| copy_as::<DepthCloudDrawData>(erased))
        .or_else(|| copy_as::<GaussianSplatDrawData>(erased))
        .or_else(|| copy_as::<VoxelGridDrawData>(erased))
        .or_else(|| copy_as::<VolumeDrawData>(erased))
        .or_else(|| copy_as::<WorldGridDrawData>(erased))
        .or_else(|| copy_as::<GenericSkyboxDrawData>(erased))
        .or_else(|| {
            re_log::warn_once!(
                "headset: cannot copy draw data for renderer {}; it is missing from the headset view",
                draw_data.renderer_name()
            );
            None
        })
}

pub fn copy_all(draw_data: &[QueueableDrawData]) -> Vec<QueueableDrawData> {
    draw_data.iter().filter_map(copy_draw_data).collect()
}

fn copy_as<DrawDataType>(erased: &dyn Any) -> Option<QueueableDrawData>
where
    DrawDataType: re_renderer::renderer::DrawData + Clone + Send + Sync + 'static,
{
    erased
        .downcast_ref::<DrawDataType>()
        .map(|draw_data| QueueableDrawData::from(draw_data.clone()))
}
