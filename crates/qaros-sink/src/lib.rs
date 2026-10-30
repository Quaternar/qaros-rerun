//! The QAROS side of the headset stream, over the `qaros` crate from the QAROS repository.
//!
//! - [`connect`] loads `qar-streaming-c`, rejoins with the persisted onboarding id or onboards
//!   with a pairing code, gets or creates the app volume Rerun's world lives in, and subscribes
//!   to peer updates and render stream requests.
//! - [`QarosSink`] implements [`frame_sink::FrameSink`] over one CPU render sender per user:
//!   stereo color and depth, written into the frame textures the negotiated layout describes.
//!   Users come from a [`UserEventSource`] ([`TargetPeers`]: the running target apps in the
//!   peer list) and a [`frame_sink::RenderPolicy`] decides whom to render for.
//! - [`QarosPanels`] opens or reattaches a GUI panel per published 2D view and a timeline
//!   panel, within reach of a headset user's head, and hides them at Stop for the next run.
//!   On [`PanelRoute::Web`] a panel shows the panel server's page; on [`PanelRoute::Stream`]
//!   it shows a CPU [`VideoSender`] this viewer owns.
//! - With the `vulkan` feature, `vulkan::VulkanSink` implements `frame_sink::GpuFrameSink`
//!   over one Vulkan render sender per user: the renderer draws into the senders' own images.
//!
//! QAROS handles are not `Send`. Everything here runs on the viewer's UI thread; the only
//! cross-thread surfaces are the library callbacks (render requests, peer updates, sender
//! creations), which forward into channels and wake the viewer.

mod begin_drain;
mod connection;
// Only the Windows handler reads most of it; elsewhere its tests do.
#[cfg_attr(not(windows), allow(dead_code))]
mod console_stop;
mod host_timing;
// Panel gestures are reachable only through the raw table: the gesture subscription and the
// gesture getters.
#[allow(unsafe_code)]
mod panel_gestures;
mod panel_images;
// The listing that reads each panel's content type first is raw: the content getters.
#[allow(unsafe_code)]
mod panel_listing;
mod panels;
// The peer category and device name are reachable only through the raw table.
#[allow(unsafe_code)]
mod peer_devices;
mod pending;
mod pixels;
mod process_exit;
// The status message of a raw call.
#[allow(unsafe_code)]
mod raw_result;
mod sink;
mod stop_watchdog;
// The stream panel's get-or-create with its stream extension, and set_stream.
#[allow(unsafe_code)]
mod stream_panel_calls;
mod stream_panels;
#[cfg(feature = "stub-control")]
pub mod stub_control;
mod unanswered;
mod users;
// Video senders stay raw in the binding: create, the CPU frame cycle and destroy.
#[allow(unsafe_code)]
mod video_sender;
#[cfg(feature = "vulkan")]
pub mod vulkan;

pub use begin_drain::BEGIN_WAIT_DRAIN_LIMIT;
pub use connection::{
    ConnectOptions, QarosConnection, WakeViewer, connect, library_path_from_environment,
    runtime_directory_from_environment, storage_directory_from_environment,
};
pub use console_stop::stop_on_console_events;
pub use host_timing::keep_process_waits_precise;
pub use panel_gestures::{
    GesturePhase, PanelGesture, PanelGestureKind, PanelGestureSubscription, PanelGestures,
    decode_gesture,
};
pub use panel_images::{PanelImage, PanelImages, TIMELINE_IMAGE_KEY};
pub use panel_listing::{ListedPanel, PanelContent, list_panels};
pub use panels::{
    DeadPages, HEADSET_WAIT, PAGE_SILENCE_LIMIT, PanelActivity, PanelRoute, PanelView,
    PanelsAtStop, QarosPanels,
};
pub use peer_devices::PeerDevice;
pub use process_exit::{
    BOUNDED_FLUSH_WAIT, end_process_after_bounded_flush, end_process_after_teardown,
};
pub use qaros;
pub use sink::{PoseSpace, QarosSink, QarosSinkOptions};
pub use stop_watchdog::{
    STOP_DEADLINE, STOP_DEADLINE_CODE, TeardownStep, current_teardown_step, enter_teardown_step,
    start_stop_watchdog,
};
pub use stream_panel_calls::{get_or_create_stream, set_stream};
pub use stream_panels::{SENDER_RETIRE_LIMIT, STREAM_REFRESH};
pub use unanswered::{UNANSWERED_MEMORY, UnansweredPeers};
pub use users::{
    FRAME_FAILURE_MINIMUM, FRAME_FAILURE_PERIOD, HEDGED_CREATION_LIMIT, PeerEvent, ReleaseCause,
    TargetPeers, UserEventSource, is_render_target,
};
pub use video_sender::{CpuFrameCycle, VideoSender};
