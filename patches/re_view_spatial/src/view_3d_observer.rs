//! Patch for qaros-rerun: lets an embedding application see what each 3D view renders.
//!
//! The 3D view hands its visualizers' draw data to a `ViewBuilder` and the data is consumed
//! there. An observer registered here gets the same draw data, by reference, right before that
//! happens, inside the viewer frame and on the UI thread, together with what is needed to
//! render the scene again from other cameras.

use std::sync::OnceLock;

use re_renderer::QueueableDrawData;
use re_sdk_types::components::ViewCoordinates;
use re_viewer_context::{ViewQuery, ViewerContext};

pub struct View3DFrame<'frame, 'viewer> {
    pub viewer_ctx: &'frame ViewerContext<'viewer>,
    pub query: &'frame ViewQuery<'frame>,
    /// The view's up/forward convention, validated the same way the 3D view uses it.
    pub view_coordinates: ViewCoordinates,
    pub scene_bounding_box: re_math::BoundingBox,
    /// The desktop camera of this view.
    pub world_from_desktop_view: re_math::IsoTransform,
    pub draw_data: &'frame [QueueableDrawData],
}

pub type View3DObserver = Box<dyn Fn(&View3DFrame<'_, '_>) + Send + Sync>;

static OBSERVER: OnceLock<View3DObserver> = OnceLock::new();

/// Registers the observer; only the first registration takes effect.
pub fn set_view_3d_observer(observer: View3DObserver) -> Result<(), View3DObserver> {
    OBSERVER.set(observer)
}

/// True once an observer is registered: the view's draw data is then also drawn from other
/// cameras, so nothing may be left out for lying outside the desktop camera's view.
pub(crate) fn is_set() -> bool {
    OBSERVER.get().is_some()
}

pub(crate) fn notify(frame: &View3DFrame<'_, '_>) {
    if let Some(observer) = OBSERVER.get() {
        observer(frame);
    }
}
