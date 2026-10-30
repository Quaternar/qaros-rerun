//! The two panel calls of the stream route the `qaros` crate has no safe form of: its
//! `GuiPanelConfig` carries no content, and it has no `set_stream` (`qaros/src/panels.rs` at
//! `ebfe3bf`; "stream panels and panel gestures. Reach them through `library.raw_api()`",
//! `gui-panels.mdx` L471-474).

use std::ffi::CString;

use qaros::qaros_sys::{
    QarGuiPanelId, QarGuiPanelSize, QarGuiPanelStreamIdExt, QarPeerId, QarPose, QarQuaternion,
    QarStreamId, QarStructureType, QarVector3, defaults,
};
use qaros::{GuiPanelConfig, GuiPanelId, StreamId};

use crate::connection::QarosConnection;
use crate::raw_result::check;

/// `qar_gui_panels_get_or_create` with `QarGuiPanelStreamIdExt { stream_id }` chained on the
/// init built from `config` as `GuiPanels::get_or_create` builds it: common name, display name,
/// pose, size and visible-to peers. The stream must belong to a video sender of this session
/// (header L2122-2134, L4194-4206). An existing panel is returned unchanged.
pub fn get_or_create_stream(
    connection: &QarosConnection,
    config: &GuiPanelConfig,
    stream_id: StreamId,
) -> Result<GuiPanelId, String> {
    let common_name = CString::new(config.common_name.as_str())
        .map_err(|_| "the panel's common name holds a NUL byte".to_owned())?;
    let display_name = config
        .display_name
        .as_deref()
        .map(CString::new)
        .transpose()
        .map_err(|_| "the panel's display name holds a NUL byte".to_owned())?;
    let visible_to: Vec<QarPeerId> = config
        .visible_to
        .iter()
        .map(|peer| QarPeerId { data: peer.0 })
        .collect();
    let mut stream_extension = QarGuiPanelStreamIdExt {
        header: defaults::structure_header(
            QarStructureType::QAR_STRUCTURE_TYPE_GUI_PANEL_STREAM_ID_EXT,
        ),
        stream_id: QarStreamId { data: stream_id.0 },
    };
    let mut init = defaults::qar_gui_panel_init_default();
    init.header.next = (&raw mut stream_extension).cast();
    init.common_name = common_name.as_ptr();
    init.display_name = display_name
        .as_ref()
        .map_or(std::ptr::null(), |name| name.as_ptr());
    init.pose = raw_pose(config.pose);
    init.size = QarGuiPanelSize {
        width_meters: config.size.width,
        height_meters: config.size.height,
        content_scale: config.size.content_scale,
    };
    if !visible_to.is_empty() {
        init.visible_to_peers = visible_to.as_ptr();
        init.visible_to_peer_count = visible_to.len();
    }
    let session = connection.session();
    let library = session.library();
    let api = library.raw_api();
    let mut panel_id = defaults::qar_gui_panel_id_default();
    // SAFETY: the session is alive and used on this thread; init, its extension and everything
    // they point to live for the call; the out-pointer is valid.
    let created = unsafe {
        (api.qar_impl_gui_panels_get_or_create)(
            session.as_raw(),
            &raw const init,
            &raw mut panel_id,
        )
    };
    check(api, "qar_gui_panels_get_or_create (stream)", created)?;
    Ok(GuiPanelId(panel_id.data))
}

/// `qar_gui_panels_set_stream`: points a stream panel at `stream_id`. A website panel refuses
/// with `QAR_STATUS_GUI_PANEL_CHANGE_NOT_PERMITTED` (header L4236-4243).
pub fn set_stream(
    connection: &QarosConnection,
    panel: GuiPanelId,
    stream_id: StreamId,
) -> Result<(), String> {
    let session = connection.session();
    let library = session.library();
    let api = library.raw_api();
    let raw_panel = QarGuiPanelId { data: panel.0 };
    let raw_stream = QarStreamId { data: stream_id.0 };
    // SAFETY: the session is alive and used on this thread; the ids live for the call.
    let set = unsafe {
        (api.qar_impl_gui_panels_set_stream)(
            session.as_raw(),
            &raw const raw_panel,
            &raw const raw_stream,
        )
    };
    check(api, "qar_gui_panels_set_stream", set)
}

fn raw_pose(pose: qaros::Pose) -> QarPose {
    QarPose {
        orientation: QarQuaternion {
            x: pose.orientation.x,
            y: pose.orientation.y,
            z: pose.orientation.z,
            w: pose.orientation.w,
        },
        position: QarVector3 {
            x: pose.position.x,
            y: pose.position.y,
            z: pose.position.z,
        },
    }
}
