//! A CPU video sender that a GUI panel shows (`QarVideoSender`, header L4109-4183): the
//! `qaros` crate leaves video senders to its raw table ("video senders ... stay raw",
//! `language-bindings.mdx` L75-77 at `ebfe3bf`).
//!
//! One colour view in one `B8G8R8A8` texture at the panel image's size. The 3137 mixer receives
//! `B8G8R8A8` for panel streams, so the library converts nothing on the way.

use std::ptr::NonNull;
use std::rc::Rc;
use std::time::Instant;

use qaros::StreamId;
use qaros::qaros_sys::{
    QarGraphicsAPI, QarPixelFormat, QarStatusCode, QarStructureType, QarTextureSize,
    QarVideoFrameCpu, QarVideoFrameView, QarVideoFrameViewEye, QarVideoFrameViewType,
    QarVideoSender, QarVideoSenderInit, defaults,
};

use crate::connection::QarosConnection;
use crate::raw_result::{check, failure, is_success};

/// `qar_video_sender_init_default()`'s reconnect window (header L6374-6384).
const RECONNECT_TIMEOUT_MILLISECONDS: u64 = 300_000;
const BYTES_PER_PIXEL: usize = 4;

/// A producer-owned stream that exists before any receiver connects. Holding the connection
/// keeps the session and the library alive until the sender is destroyed, which gives the C
/// teardown order, and makes the sender `!Send`: every call stays on the QAROS thread.
pub struct VideoSender {
    handle: NonNull<QarVideoSender>,
    stream_id: StreamId,
    width: u32,
    height: u32,
    connection: Rc<QarosConnection>,
}

/// What a `qar_video_sender_try_begin_frame` call came to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BeginOutcome {
    Ready,
    /// No receiver is ready, the stream is paused or reconnecting: try again later, quietly.
    NotNow,
    Failed,
}

/// The statuses 602 (paused), 603 (reconnecting) and 604 (no active transfers) mean "not now"
/// (header L376-383; the C# binding's `PrimarylessVideoSender.cs` L43-56 treats the same three
/// as recoverable). `show_frame` also returns 604 when every reserved receiver was lost before
/// delivery (`PrimarylessVideoSender.ShowFrame`).
pub(crate) fn is_not_ready(code: QarStatusCode) -> bool {
    code == QarStatusCode::QAR_STATUS_MEDIA_STREAMING_STREAM_IS_PAUSED
        || code == QarStatusCode::QAR_STATUS_MEDIA_STREAMING_STREAM_IS_RECONNECTING
        || code == QarStatusCode::QAR_STATUS_MEDIA_STREAMING_NO_ACTIVE_TRANSFERS
}

/// A successful begin with `out_ready = false` is "not now" too.
pub(crate) fn begin_outcome(code: QarStatusCode, ready: bool) -> BeginOutcome {
    if code == QarStatusCode::QAR_STATUS_SUCCESS {
        return if ready {
            BeginOutcome::Ready
        } else {
            BeginOutcome::NotNow
        };
    }
    if is_not_ready(code) {
        BeginOutcome::NotNow
    } else {
        BeginOutcome::Failed
    }
}

/// `qar_video_sender_init_default()` (header L6374-6384) with one colour view of the whole
/// texture and one `B8G8R8A8` texture of `width` by `height`, CPU ingress.
pub(crate) fn cpu_sender_init(width: u32, height: u32) -> QarVideoSenderInit {
    let mut layout = defaults::qar_video_frame_layout_default();
    layout.views[0] = QarVideoFrameView {
        start_x: 0,
        start_y: 0,
        end_x: width,
        end_y: height,
        texture_index: 0,
        array_layer_index: 0,
        texture_format: QarPixelFormat::QAR_PIXEL_FORMAT_B8G8R8A8,
        data_type: QarVideoFrameViewType::QAR_VIDEO_FRAME_VIEW_TYPE_COLOR,
        eye: QarVideoFrameViewEye::QAR_VIDEO_FRAME_VIEW_EYE_NONE,
    };
    layout.views_count = 1;
    layout.textures[0] = QarTextureSize {
        format: QarPixelFormat::QAR_PIXEL_FORMAT_B8G8R8A8,
        width,
        height,
        array_layers: 1,
    };
    layout.textures_count = 1;
    QarVideoSenderInit {
        header: defaults::structure_header(QarStructureType::QAR_STRUCTURE_TYPE_VIDEO_SENDER_INIT),
        layout,
        enable_reconnects: true,
        reconnect_timeout_ms: RECONNECT_TIMEOUT_MILLISECONDS,
        graphics_api: QarGraphicsAPI::QAR_GRAPHICS_API_CPU,
    }
}

impl VideoSender {
    /// `qar_video_sender_create` with [`cpu_sender_init`], then `qar_video_sender_stream_id`.
    pub fn create_cpu(
        connection: &Rc<QarosConnection>,
        width: u32,
        height: u32,
    ) -> Result<Self, String> {
        if width == 0 || height == 0 {
            return Err(format!("a video sender of {width}x{height} pixels"));
        }
        let session = connection.session();
        let library = session.library();
        let api = library.raw_api();
        let init = cpu_sender_init(width, height);
        let mut raw_sender: *mut QarVideoSender = std::ptr::null_mut();
        // SAFETY: the session is alive and used on this thread; init lives for the call and
        // its header chains nothing; the out-pointer is valid.
        let created = unsafe {
            (api.qar_impl_video_sender_create)(
                session.as_raw(),
                &raw const init,
                &raw mut raw_sender,
            )
        };
        check(api, "qar_video_sender_create", created)?;
        let handle =
            NonNull::new(raw_sender).ok_or("qar_video_sender_create: success without a sender")?;
        let mut raw_stream_id = defaults::qar_stream_id_default();
        // SAFETY: the sender is live; the out-pointer is valid.
        let read_id = unsafe {
            (api.qar_impl_video_sender_stream_id)(handle.as_ptr(), &raw mut raw_stream_id)
        };
        if let Err(error) = check(api, "qar_video_sender_stream_id", read_id) {
            // SAFETY: created above, destroyed once, never used again.
            unsafe { (api.qar_impl_video_sender_handle_destroy)(handle.as_ptr()) };
            return Err(error);
        }
        Ok(Self {
            handle,
            stream_id: StreamId(raw_stream_id.data),
            width,
            height,
            connection: Rc::clone(connection),
        })
    }

    pub fn stream_id(&self) -> StreamId {
        self.stream_id
    }

    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// `qar_video_sender_try_begin_frame`, then on ready `qar_video_sender_frame_cpu`.
    /// `Ok(None)` for "not now" ([`begin_outcome`]); `Err` for any other status.
    pub fn try_begin(&mut self) -> Result<Option<CpuFrameCycle<'_>>, String> {
        let session = self.connection.session();
        let library = session.library();
        let api = library.raw_api();
        let mut ready = false;
        let began = {
            let _span = frame_sink::perf::Span::start("panel_try_begin");
            // SAFETY: the sender is live and used on this thread; the out-pointer is valid.
            unsafe {
                (api.qar_impl_video_sender_try_begin_frame)(self.handle.as_ptr(), &raw mut ready)
            }
        };
        match begin_outcome(began.code, ready) {
            BeginOutcome::NotNow => return Ok(None),
            BeginOutcome::Failed => {
                return Err(failure(api, "qar_video_sender_try_begin_frame", began));
            }
            BeginOutcome::Ready => {}
        }
        let mut frame = defaults::qar_video_frame_cpu_default();
        // SAFETY: a ready begin opened the cycle; the out-pointer is valid.
        let got_frame =
            unsafe { (api.qar_impl_video_sender_frame_cpu)(self.handle.as_ptr(), &raw mut frame) };
        if !is_success(got_frame) {
            let error = failure(api, "qar_video_sender_frame_cpu", got_frame);
            // The API cannot abandon a begun cycle; showing closes it.
            if let Err(show_error) = self.show_frame() {
                log::warn!("qaros: closing a frame cycle without its frame: {show_error}");
            }
            return Err(error);
        }
        Ok(Some(CpuFrameCycle {
            sender: self,
            frame,
            shown: false,
        }))
    }

    fn show_frame(&mut self) -> Result<(), String> {
        let session = self.connection.session();
        let library = session.library();
        let api = library.raw_api();
        let _span = frame_sink::perf::Span::start("panel_show");
        // SAFETY: the sender is live, with a begun cycle.
        let shown = unsafe { (api.qar_impl_video_sender_show_frame)(self.handle.as_ptr()) };
        // A receiver lost or paused between the begin and the show drops this frame; the next
        // ready begin reaches whoever is connected then, so the sender has not failed.
        if is_not_ready(shown.code) {
            log::debug!(
                "qaros: video sender {}: frame dropped, status {}",
                self.stream_id,
                shown.code.0
            );
            return Ok(());
        }
        check(api, "qar_video_sender_show_frame", shown)
    }
}

impl Drop for VideoSender {
    fn drop(&mut self) {
        let session = self.connection.session();
        let library = session.library();
        let api = library.raw_api();
        let started = Instant::now();
        // SAFETY: the handle came from qar_video_sender_create and is destroyed once, while
        // the session it belongs to is still alive (the connection drops after this body).
        unsafe { (api.qar_impl_video_sender_handle_destroy)(self.handle.as_ptr()) };
        log::info!(
            "qaros: video sender {} destroyed in {} us",
            self.stream_id,
            started.elapsed().as_micros()
        );
    }
}

/// One begun frame of a [`VideoSender`]. The CPU frame's buffer belongs to the sender and is
/// valid only until the show, which the borrow encodes. Dropped unshown, it shows whatever the
/// buffer holds: the C API has no way to abandon a begun cycle.
pub struct CpuFrameCycle<'sender> {
    sender: &'sender mut VideoSender,
    frame: QarVideoFrameCpu,
    shown: bool,
}

impl CpuFrameCycle<'_> {
    /// Copies a BGRA8 image of exactly the sender's size into `textures[0]`, row by row with
    /// the frame's pitch. No swizzle: the panel images already hold BGRA with alpha 255.
    pub fn write_bgra8(&mut self, bgra8: &[u8], width: u32, height: u32) -> Result<(), String> {
        if (width, height) != self.sender.size() {
            let (sender_width, sender_height) = self.sender.size();
            return Err(format!(
                "a {width}x{height} image for a {sender_width}x{sender_height} sender"
            ));
        }
        if self.frame.textures_count < 1 {
            return Err("the CPU frame has no texture".to_owned());
        }
        let texture = &self.frame.textures[0];
        if texture.size.format != QarPixelFormat::QAR_PIXEL_FORMAT_B8G8R8A8 {
            return Err(format!(
                "the CPU frame's texture has format {:?}, not B8G8R8A8",
                texture.size.format
            ));
        }
        if texture.size.width != width || texture.size.height != height {
            return Err(format!(
                "the CPU frame's texture is {}x{}, the image {width}x{height}",
                texture.size.width, texture.size.height
            ));
        }
        if texture.texture_data.is_null() {
            return Err("the CPU frame's texture has no data".to_owned());
        }
        let _span = frame_sink::perf::Span::start("panel_copy");
        // SAFETY: between a ready begin and the show, `texture_data` points to
        // `texture_data_size` writable bytes owned by the sender (header L4140-4141), and the
        // `&mut` borrow of the sender keeps anything else from using them.
        let destination = unsafe {
            std::slice::from_raw_parts_mut(texture.texture_data, texture.texture_data_size)
        };
        copy_rows_bgra(bgra8, width, height, destination, texture.pitch)
    }

    /// `qar_video_sender_show_frame`. A frame dropped because its receivers paused or went
    /// away is `Ok`: only other statuses mean the sender failed.
    pub fn show(mut self) -> Result<(), String> {
        self.shown = true;
        self.sender.show_frame()
    }
}

impl Drop for CpuFrameCycle<'_> {
    fn drop(&mut self) {
        if self.shown {
            return;
        }
        log::warn!(
            "qaros: video sender {}: a frame cycle ended without a show; showing it as it is",
            self.sender.stream_id
        );
        if let Err(error) = self.sender.show_frame() {
            log::warn!("qaros: {error}");
        }
    }
}

/// Copies `height` rows of `width` BGRA pixels from the tightly packed `source` into
/// `destination`, whose rows start every `pitch` bytes. Bytes between rows stay as they are.
pub(crate) fn copy_rows_bgra(
    source: &[u8],
    width: u32,
    height: u32,
    destination: &mut [u8],
    pitch: u32,
) -> Result<(), String> {
    let row_bytes = width as usize * BYTES_PER_PIXEL;
    let pitch = pitch as usize;
    let rows = height as usize;
    if source.len() != row_bytes * rows {
        return Err(format!(
            "{} bytes for a {width}x{height} BGRA image",
            source.len()
        ));
    }
    if pitch < row_bytes {
        return Err(format!("a pitch of {pitch} bytes for rows of {row_bytes}"));
    }
    if rows == 0 {
        return Ok(());
    }
    let needed = pitch * (rows - 1) + row_bytes;
    if destination.len() < needed {
        return Err(format!(
            "a texture of {} bytes for {rows} rows at a pitch of {pitch}",
            destination.len()
        ));
    }
    for (row, source_row) in source.chunks_exact(row_bytes).enumerate() {
        let start = row * pitch;
        destination[start..start + row_bytes].copy_from_slice(source_row);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_paused_reconnecting_and_idle_streams_are_not_ready() {
        assert!(is_not_ready(QarStatusCode(602)));
        assert!(is_not_ready(QarStatusCode(603)));
        assert!(is_not_ready(QarStatusCode(604)));
        assert!(!is_not_ready(QarStatusCode::QAR_STATUS_SUCCESS));
        assert!(!is_not_ready(QarStatusCode(7)));
        assert!(!is_not_ready(QarStatusCode(4)));
    }

    #[test]
    fn paused_reconnecting_and_idle_streams_are_not_now_and_other_failures_fail() {
        let success = QarStatusCode::QAR_STATUS_SUCCESS;
        assert_eq!(begin_outcome(success, true), BeginOutcome::Ready);
        assert_eq!(begin_outcome(success, false), BeginOutcome::NotNow);
        for not_now in [
            QarStatusCode::QAR_STATUS_MEDIA_STREAMING_STREAM_IS_PAUSED,
            QarStatusCode::QAR_STATUS_MEDIA_STREAMING_STREAM_IS_RECONNECTING,
            QarStatusCode::QAR_STATUS_MEDIA_STREAMING_NO_ACTIVE_TRANSFERS,
        ] {
            assert_eq!(begin_outcome(not_now, false), BeginOutcome::NotNow);
        }
        assert_eq!(
            QarStatusCode::QAR_STATUS_MEDIA_STREAMING_STREAM_IS_PAUSED,
            QarStatusCode(602)
        );
        assert_eq!(
            QarStatusCode::QAR_STATUS_MEDIA_STREAMING_STREAM_IS_RECONNECTING,
            QarStatusCode(603)
        );
        assert_eq!(
            QarStatusCode::QAR_STATUS_MEDIA_STREAMING_NO_ACTIVE_TRANSFERS,
            QarStatusCode(604)
        );
        // QAR_STATUS_LOGIC_ERROR (a second begin) and QAR_STATUS_NOT_IMPLEMENTED.
        assert_eq!(begin_outcome(QarStatusCode(7), false), BeginOutcome::Failed);
        assert_eq!(
            begin_outcome(QarStatusCode::QAR_STATUS_NOT_IMPLEMENTED, false),
            BeginOutcome::Failed
        );
        assert_eq!(QarStatusCode::QAR_STATUS_NOT_IMPLEMENTED, QarStatusCode(4));
    }

    #[test]
    fn the_init_asks_for_one_cpu_colour_view_in_one_bgra_texture_of_the_image_size() {
        let init = cpu_sender_init(640, 360);
        assert_eq!(
            init.header.type_,
            QarStructureType::QAR_STRUCTURE_TYPE_VIDEO_SENDER_INIT
        );
        assert_eq!(init.header.type_, QarStructureType(0x3005));
        assert!(init.header.next.is_null());
        assert_eq!(init.graphics_api, QarGraphicsAPI::QAR_GRAPHICS_API_CPU);
        assert!(init.enable_reconnects);
        assert_eq!(init.reconnect_timeout_ms, 300_000);
        assert_eq!(init.layout.views_count, 1);
        assert_eq!(init.layout.textures_count, 1);
        let view = init.layout.views[0];
        assert_eq!(
            (view.start_x, view.start_y, view.end_x, view.end_y),
            (0, 0, 640, 360)
        );
        assert_eq!(view.texture_index, 0);
        assert_eq!(
            view.texture_format,
            QarPixelFormat::QAR_PIXEL_FORMAT_B8G8R8A8
        );
        assert_eq!(
            view.data_type,
            QarVideoFrameViewType::QAR_VIDEO_FRAME_VIEW_TYPE_COLOR
        );
        assert_eq!(
            view.eye,
            QarVideoFrameViewEye::QAR_VIDEO_FRAME_VIEW_EYE_NONE
        );
        let texture = init.layout.textures[0];
        assert_eq!(texture.format, QarPixelFormat::QAR_PIXEL_FORMAT_B8G8R8A8);
        assert_eq!(
            (texture.width, texture.height, texture.array_layers),
            (640, 360, 1)
        );
    }

    #[test]
    fn rows_land_at_the_pitch_and_the_padding_between_them_is_untouched() {
        let width = 2;
        let height = 3;
        let pitch = 12;
        let source: Vec<u8> = (0..24).collect();
        let padding = 0xEE;
        let mut destination = vec![padding; 36];
        copy_rows_bgra(&source, width, height, &mut destination, pitch).unwrap();
        for row in 0..3 {
            let start = row * 12;
            assert_eq!(
                &destination[start..start + 8],
                &source[row * 8..row * 8 + 8],
                "row {row}"
            );
            assert!(
                destination[start + 8..start + 12]
                    .iter()
                    .all(|&byte| byte == padding),
                "row {row}'s padding"
            );
        }
    }

    #[test]
    fn a_short_texture_a_narrow_pitch_or_a_wrong_image_length_is_refused() {
        let source = vec![0; 2 * 2 * 4];
        assert!(copy_rows_bgra(&source, 2, 2, &mut [0; 15], 8).is_err());
        assert!(copy_rows_bgra(&source, 2, 2, &mut [0; 64], 4).is_err());
        assert!(copy_rows_bgra(&source[1..], 2, 2, &mut [0; 64], 8).is_err());
        // The last row needs no padding after it.
        assert!(copy_rows_bgra(&source, 2, 2, &mut [0; 20], 12).is_ok());
    }
}
