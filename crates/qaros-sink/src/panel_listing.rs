//! The session's GUI panels, read through the raw table with the content type first
//! (`qar_gui_panel_get_content_type`, header L4557-4567). The `qaros` crate's
//! `GuiPanels::list` reads every panel's content URI, which fails for a panel that shows a video
//! stream, and with it the whole listing (`docs/known-issues.md`).

use std::ffi::c_char;

use qaros::qaros_sys::{
    QarApi, QarGuiPanel, QarGuiPanelContentType, QarGuiPanelState, QarStatusCode, defaults,
};
use qaros::{GuiPanelId, GuiPanelSize, GuiPanelState, Pose, Quat, StreamId, Vec3};

use crate::connection::QarosConnection;
use crate::raw_result::check;

/// The C API's string getters report neither the size they need nor truncation; the `qaros`
/// crate reads them into a buffer of this size.
const GETTER_BUFFER_SIZE: usize = 4096;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PanelContent {
    Uri(String),
    Stream(StreamId),
    /// A stream panel whose stream id QAROS cannot read, with the reason. 0.9.0-dev.3184 reports
    /// this for a panel that holds no stream id yet ("0 payload bytes"); the panel is still
    /// listed, so one such panel does not hide the others.
    UnreadableStream(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct ListedPanel {
    pub id: GuiPanelId,
    pub display_name: String,
    pub pose: Pose,
    pub size: GuiPanelSize,
    pub state: GuiPanelState,
    pub content: PanelContent,
}

impl ListedPanel {
    /// The URI a website panel shows; empty for a stream panel.
    pub fn content_uri(&self) -> &str {
        match &self.content {
            PanelContent::Uri(uri) => uri,
            PanelContent::Stream(_) | PanelContent::UnreadableStream(_) => "",
        }
    }
}

/// `qar_query_gui_panels_count` and `qar_query_gui_panels`, then per handle the getters, with
/// `qar_gui_panel_get_content_type` before the URI or the stream id. Every written handle is
/// destroyed, whatever the reads returned.
pub fn list_panels(connection: &QarosConnection) -> Result<Vec<ListedPanel>, String> {
    let session = connection.session();
    let library = session.library();
    let api = library.raw_api();
    let raw_session = session.as_raw();
    let mut count = 0_usize;
    // SAFETY: the session is alive and used on this thread; the out-pointer is valid.
    let counted = unsafe { (api.qar_impl_query_gui_panels_count)(raw_session, &raw mut count) };
    check(api, "qar_query_gui_panels_count", counted)?;
    if count == 0 {
        return Ok(Vec::new());
    }
    let mut handles: Vec<*mut QarGuiPanel> = vec![std::ptr::null_mut(); count];
    let mut written = 0_usize;
    // SAFETY: as above; the buffer holds `handles.len()` handle slots.
    let queried = unsafe {
        (api.qar_impl_query_gui_panels)(
            raw_session,
            handles.as_mut_ptr(),
            handles.len(),
            &raw mut written,
        )
    };
    let written_handles = &handles[..written.min(handles.len())];
    let listed = check(api, "qar_query_gui_panels", queried).and_then(|()| {
        written_handles
            .iter()
            .filter(|handle| !handle.is_null())
            .map(|&handle| read_listed_panel(api, handle))
            .collect()
    });
    for &handle in written_handles.iter().filter(|handle| !handle.is_null()) {
        // SAFETY: each written handle is destroyed once, after its last use above.
        unsafe { (api.qar_impl_gui_panel_handle_destroy)(handle) };
    }
    listed
}

fn read_listed_panel(api: &QarApi, handle: *mut QarGuiPanel) -> Result<ListedPanel, String> {
    let mut raw_id = defaults::qar_gui_panel_id_default();
    let mut raw_pose = defaults::qar_pose_default();
    let mut raw_size = defaults::qar_gui_panel_size_default();
    let mut raw_state = QarGuiPanelState::QAR_GUI_PANEL_STATE_VISIBLE;
    // SAFETY (the four getters): `handle` is a live handle from the query; the out-pointers
    // are valid.
    let read_id = unsafe { (api.qar_impl_gui_panel_get_id)(handle, &raw mut raw_id) };
    check(api, "qar_gui_panel_get_id", read_id)?;
    let read_pose = unsafe { (api.qar_impl_gui_panel_get_pose)(handle, &raw mut raw_pose) };
    check(api, "qar_gui_panel_get_pose", read_pose)?;
    let read_size = unsafe { (api.qar_impl_gui_panel_get_size)(handle, &raw mut raw_size) };
    check(api, "qar_gui_panel_get_size", read_size)?;
    let read_state = unsafe { (api.qar_impl_gui_panel_get_state)(handle, &raw mut raw_state) };
    check(api, "qar_gui_panel_get_state", read_state)?;
    let display_name = read_string(|buffer, size| {
        // SAFETY: as above; the buffer is writable for `size` bytes.
        let result = unsafe { (api.qar_impl_gui_panel_get_display_name)(handle, buffer, size) };
        check(api, "qar_gui_panel_get_display_name", result)
    })?;
    let content = read_content(api, handle)?;
    Ok(ListedPanel {
        id: GuiPanelId(raw_id.data),
        display_name,
        pose: Pose {
            orientation: Quat {
                x: raw_pose.orientation.x,
                y: raw_pose.orientation.y,
                z: raw_pose.orientation.z,
                w: raw_pose.orientation.w,
            },
            position: Vec3::new(
                raw_pose.position.x,
                raw_pose.position.y,
                raw_pose.position.z,
            ),
        },
        size: GuiPanelSize {
            width: raw_size.width_meters,
            height: raw_size.height_meters,
            content_scale: raw_size.content_scale,
        },
        state: panel_state(raw_state),
        content,
    })
}

/// The content type decides which getter may be called. `QAR_STATUS_NOT_IMPLEMENTED` is read
/// as a website: only `qaros-stub` lacks the getter, and it has no stream panels.
fn read_content(api: &QarApi, handle: *mut QarGuiPanel) -> Result<PanelContent, String> {
    let mut content_type = QarGuiPanelContentType::QAR_GUI_PANEL_CONTENT_TYPE_WEBSITE_URI;
    // SAFETY: `handle` is live; the out-pointer is valid.
    let read_type =
        unsafe { (api.qar_impl_gui_panel_get_content_type)(handle, &raw mut content_type) };
    if read_type.code == QarStatusCode::QAR_STATUS_NOT_IMPLEMENTED {
        content_type = QarGuiPanelContentType::QAR_GUI_PANEL_CONTENT_TYPE_WEBSITE_URI;
    } else {
        check(api, "qar_gui_panel_get_content_type", read_type)?;
    }
    if content_type == QarGuiPanelContentType::QAR_GUI_PANEL_CONTENT_TYPE_STREAM_ID {
        let mut stream_id = defaults::qar_stream_id_default();
        // SAFETY: as above.
        let read_stream =
            unsafe { (api.qar_impl_gui_panel_get_content_stream_id)(handle, &raw mut stream_id) };
        return Ok(
            match check(api, "qar_gui_panel_get_content_stream_id", read_stream) {
                Ok(()) => PanelContent::Stream(StreamId(stream_id.data)),
                Err(reason) => PanelContent::UnreadableStream(reason),
            },
        );
    }
    if content_type != QarGuiPanelContentType::QAR_GUI_PANEL_CONTENT_TYPE_WEBSITE_URI {
        return Err(format!(
            "qar_gui_panel_get_content_type: unknown content type {content_type:?}"
        ));
    }
    let uri = read_string(|buffer, size| {
        // SAFETY: as above; the buffer is writable for `size` bytes.
        let result = unsafe { (api.qar_impl_gui_panel_get_content_uri)(handle, buffer, size) };
        check(api, "qar_gui_panel_get_content_uri", result)
    })?;
    Ok(PanelContent::Uri(uri))
}

fn panel_state(raw_state: QarGuiPanelState) -> GuiPanelState {
    match raw_state {
        QarGuiPanelState::QAR_GUI_PANEL_STATE_VISIBLE => GuiPanelState::Visible,
        QarGuiPanelState::QAR_GUI_PANEL_STATE_MINIMIZED => GuiPanelState::Minimized,
        QarGuiPanelState::QAR_GUI_PANEL_STATE_HIDDEN => GuiPanelState::Hidden,
        QarGuiPanelState::QAR_GUI_PANEL_STATE_CLOSED => GuiPanelState::Closed,
        other => GuiPanelState::Other(other),
    }
}

/// Text up to the first NUL, invalid UTF-8 replaced.
fn read_string(
    getter: impl FnOnce(*mut c_char, usize) -> Result<(), String>,
) -> Result<String, String> {
    let mut buffer = vec![0 as c_char; GETTER_BUFFER_SIZE];
    getter(buffer.as_mut_ptr(), buffer.len())?;
    let bytes: Vec<u8> = buffer
        .iter()
        .take_while(|&&character| character != 0)
        .map(|&character| character.to_ne_bytes()[0])
        .collect();
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}
