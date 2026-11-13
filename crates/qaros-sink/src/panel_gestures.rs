//! Touch input on a GUI panel (`qar_gui_panels_subscribe_all_gesture_events` and the
//! `qar_gui_panel_gesture_get_*` getters, header L4301-4482), reached through the raw table.
//!
//! The library calls back on its own threads. The callback reads each event through the
//! getters inside the call, never keeps the handle, copies the values into [`PanelGestures`]
//! and wakes the viewer; the QAROS thread drains the queue.

use std::collections::VecDeque;
use std::ffi::c_void;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr::NonNull;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use qaros::qaros_sys::{
    QarApi, QarGesturePhase, QarGuiPanelGesture, QarGuiPanelGestureKind, QarGuiPanelId,
    QarGuiPanelPoint, defaults,
};
use qaros::{CancelToken, GuiPanelId, PeerId};

use crate::connection::{QarosConnection, WakeViewer};
use crate::raw_result::{check, is_success};

/// Oldest events are dropped beyond this many undrained ones.
const GESTURE_QUEUE_LIMIT: usize = 256;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PanelGestureKind {
    Hover,
    Tap,
    LongPress,
    Drag,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GesturePhase {
    Started,
    Updated,
    Ended,
    Instant,
    Canceled,
}

/// One gesture event, positions in the sender's pixels, top-left origin, clamped to the image
/// (`QarGuiPanelPoint`, header L1180-1197).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PanelGesture {
    pub source_peer: PeerId,
    /// Unique per source peer only; 0 for the instant kinds (header L4338-4350, L4419-4426).
    pub gesture_id: u64,
    pub kind: PanelGestureKind,
    pub phase: GesturePhase,
    pub position: [i32; 2],
    pub start_position: [i32; 2],
    pub tap_count: u8,
    pub received: Instant,
}

const GESTURE_KINDS: [(QarGuiPanelGestureKind, PanelGestureKind); 4] = [
    (
        QarGuiPanelGestureKind::QAR_GUI_PANEL_GESTURE_KIND_HOVER,
        PanelGestureKind::Hover,
    ),
    (
        QarGuiPanelGestureKind::QAR_GUI_PANEL_GESTURE_KIND_TAP,
        PanelGestureKind::Tap,
    ),
    (
        QarGuiPanelGestureKind::QAR_GUI_PANEL_GESTURE_KIND_LONG_PRESS,
        PanelGestureKind::LongPress,
    ),
    (
        QarGuiPanelGestureKind::QAR_GUI_PANEL_GESTURE_KIND_DRAG,
        PanelGestureKind::Drag,
    ),
];

const GESTURE_PHASES: [(QarGesturePhase, GesturePhase); 5] = [
    (
        QarGesturePhase::QAR_GESTURE_PHASE_STARTED,
        GesturePhase::Started,
    ),
    (
        QarGesturePhase::QAR_GESTURE_PHASE_UPDATED,
        GesturePhase::Updated,
    ),
    (
        QarGesturePhase::QAR_GESTURE_PHASE_ENDED,
        GesturePhase::Ended,
    ),
    (
        QarGesturePhase::QAR_GESTURE_PHASE_INSTANT,
        GesturePhase::Instant,
    ),
    (
        QarGesturePhase::QAR_GESTURE_PHASE_CANCELED,
        GesturePhase::Canceled,
    ),
];

/// A C enum's value as `i32`. C enums are `int` under MSVC and `unsigned int` elsewhere
/// (`qaros_sys::defaults` notes the same), so the value goes through `i64`, which holds both;
/// a value beyond `i32` is no kind or phase the header knows.
fn enum_value(raw: impl Into<i64>) -> Option<i32> {
    i32::try_from(raw.into()).ok()
}

/// The raw values of one event as the getters return them, `received` now. Unknown kinds and
/// phases (the header reserves room for more, L4441-4446) give `None`.
pub fn decode_gesture(
    kind: i32,
    phase: i32,
    source_peer: PeerId,
    gesture_id: u64,
    position: [i32; 2],
    start_position: [i32; 2],
    tap_count: u8,
) -> Option<PanelGesture> {
    let (_, kind) = GESTURE_KINDS
        .iter()
        .find(|(raw, _)| enum_value(raw.0) == Some(kind))?;
    let (_, phase) = GESTURE_PHASES
        .iter()
        .find(|(raw, _)| enum_value(raw.0) == Some(phase))?;
    Some(PanelGesture {
        source_peer,
        gesture_id,
        kind: *kind,
        phase: *phase,
        position,
        start_position,
        tap_count,
        received: Instant::now(),
    })
}

/// A bounded queue the library thread fills and the QAROS or UI thread drains.
#[derive(Clone, Default)]
pub struct PanelGestures {
    queue: Arc<Mutex<VecDeque<PanelGesture>>>,
}

impl PanelGestures {
    pub fn push(&self, gesture: PanelGesture) {
        let mut queue = self.queue.lock().unwrap_or_else(PoisonError::into_inner);
        if queue.len() >= GESTURE_QUEUE_LIMIT {
            queue.pop_front();
        }
        queue.push_back(gesture);
    }

    pub fn take(&self) -> Vec<PanelGesture> {
        self.queue
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .drain(..)
            .collect()
    }
}

/// What the callback reads its events with and hands them to. `api` points into the library,
/// which the subscription's cancel token keeps alive until after the cancel.
struct GestureCallbackState {
    api: *const QarApi,
    inbox: PanelGestures,
    wake: WakeViewer,
}

/// A gesture subscription on one panel, cancelled when dropped. The cancel token holds the
/// library, so the callback state's table outlives every callback; the token and the raw state
/// make the subscription `!Send`, which keeps it on the QAROS thread.
pub struct PanelGestureSubscription {
    token: CancelToken,
    state: NonNull<GestureCallbackState>,
}

impl PanelGestureSubscription {
    /// `qar_gui_panels_subscribe_all_gesture_events` on `panel`, every kind on one callback so a
    /// hover and the drag that grows out of the same contact keep their order (header
    /// L4385-4417), with a cancel token of its own from `library.create_cancel_token(None)`.
    pub fn subscribe(
        connection: &QarosConnection,
        panel: GuiPanelId,
        inbox: PanelGestures,
        wake: WakeViewer,
    ) -> Result<Self, String> {
        let session = connection.session();
        let library = session.library();
        let token = library
            .create_cancel_token(None)
            .map_err(|error| format!("qar_cancel_token_create: {error}"))?;
        let api = library.raw_api();
        let state = NonNull::from(Box::leak(Box::new(GestureCallbackState {
            api: std::ptr::from_ref(api),
            inbox,
            wake,
        })));
        let raw_panel = QarGuiPanelId { data: panel.0 };
        // SAFETY: the session is alive and used on this thread; the panel id lives for the
        // call; `state` stays valid until the cancel in Drop has returned, the token lives in
        // `Self`, and the trampoline matches the callback type.
        let subscribed = unsafe {
            (api.qar_impl_gui_panels_subscribe_all_gesture_events)(
                session.as_raw(),
                &raw const raw_panel,
                Some(gesture_trampoline),
                state.as_ptr().cast::<c_void>(),
                token.as_raw(),
            )
        };
        if let Err(error) = check(
            api,
            "qar_gui_panels_subscribe_all_gesture_events",
            subscribed,
        ) {
            // SAFETY: nothing was subscribed, so nothing else holds the state.
            drop(unsafe { Box::from_raw(state.as_ptr()) });
            return Err(error);
        }
        Ok(Self { token, state })
    }
}

impl Drop for PanelGestureSubscription {
    /// The cancel waits for a running callback (header L2703-2733), so the state is freed only
    /// once no callback can reach it. The token itself is destroyed after this body.
    fn drop(&mut self) {
        if let Err(error) = self.token.cancel() {
            log::warn!("qaros: cancelling a gesture subscription: {error}");
        }
        // SAFETY: the cancel returned, so no callback runs or will run with the state, which is
        // freed once.
        drop(unsafe { Box::from_raw(self.state.as_ptr()) });
    }
}

/// Reads one event through the getters and queues it. A panic must not unwind into C.
unsafe extern "C" fn gesture_trampoline(gesture: *mut QarGuiPanelGesture, user_state: *mut c_void) {
    if gesture.is_null() || user_state.is_null() {
        return;
    }
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: `user_state` is the state registered with this trampoline, alive until the
        // subscription's cancel returns; its table is alive as long as the library.
        let state = unsafe { &*user_state.cast::<GestureCallbackState>() };
        // SAFETY: the table lives in the library, which the subscription's token keeps alive.
        let api = unsafe { &*state.api };
        // SAFETY: the handle is borrowed for the duration of the callback.
        if let Some(event) = unsafe { read_gesture(api, gesture) } {
            state.inbox.push(event);
            (state.wake)();
        }
    }));
    if outcome.is_err() {
        log::error!("qaros: a panel gesture callback panicked; the event is dropped");
    }
}

/// # Safety
///
/// `gesture` is the live handle of the running callback.
unsafe fn read_gesture(api: &QarApi, gesture: *mut QarGuiPanelGesture) -> Option<PanelGesture> {
    let mut source_peer = defaults::qar_peer_id_default();
    let mut gesture_id = 0_u64;
    let mut kind = QarGuiPanelGestureKind::QAR_GUI_PANEL_GESTURE_KIND_HOVER;
    let mut phase = QarGesturePhase::QAR_GESTURE_PHASE_INSTANT;
    let mut position = QarGuiPanelPoint {
        x_pixels: 0,
        y_pixels: 0,
    };
    let mut start_position = position;
    let mut tap_count = 0_u8;
    // SAFETY (all getters): the handle is live for the callback; the out-pointers are valid.
    let lifecycle = unsafe {
        (api.qar_impl_gui_panel_gesture_get_lifecycle)(
            gesture,
            &raw mut source_peer,
            &raw mut gesture_id,
        )
    };
    let classification = unsafe {
        (api.qar_impl_gui_panel_gesture_get_classification)(gesture, &raw mut kind, &raw mut phase)
    };
    let positions = unsafe {
        (api.qar_impl_gui_panel_gesture_get_positions)(
            gesture,
            &raw mut position,
            &raw mut start_position,
        )
    };
    if !(is_success(lifecycle) && is_success(classification) && is_success(positions)) {
        log::warn!("qaros: a panel gesture whose lifecycle, kind or positions cannot be read");
        return None;
    }
    // A tap count that cannot be read counts as none: only taps carry one.
    let tap_read =
        unsafe { (api.qar_impl_gui_panel_gesture_get_tap_count)(gesture, &raw mut tap_count) };
    if !is_success(tap_read) {
        tap_count = 0;
    }
    let (Some(kind), Some(phase)) = (enum_value(kind.0), enum_value(phase.0)) else {
        return None;
    };
    decode_gesture(
        kind,
        phase,
        PeerId(source_peer.data),
        gesture_id,
        [position.x_pixels, position.y_pixels],
        [start_position.x_pixels, start_position.y_pixels],
        tap_count,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const PEER: PeerId = PeerId([0xa1; 16]);

    fn decode(
        kind: QarGuiPanelGestureKind,
        phase: QarGesturePhase,
        gesture_id: u64,
    ) -> Option<PanelGesture> {
        decode_gesture(
            enum_value(kind.0).unwrap(),
            enum_value(phase.0).unwrap(),
            PEER,
            gesture_id,
            [10, 20],
            [5, 6],
            1,
        )
    }

    #[test]
    fn known_kinds_and_phases_decode_with_their_values() {
        let drag = decode(
            QarGuiPanelGestureKind::QAR_GUI_PANEL_GESTURE_KIND_DRAG,
            QarGesturePhase::QAR_GESTURE_PHASE_UPDATED,
            42,
        )
        .unwrap();
        assert_eq!(drag.kind, PanelGestureKind::Drag);
        assert_eq!(drag.phase, GesturePhase::Updated);
        assert_eq!(drag.gesture_id, 42);
        assert_eq!(drag.source_peer, PEER);
        assert_eq!(drag.position, [10, 20]);
        assert_eq!(drag.start_position, [5, 6]);
        assert_eq!(
            decode(
                QarGuiPanelGestureKind::QAR_GUI_PANEL_GESTURE_KIND_LONG_PRESS,
                QarGesturePhase::QAR_GESTURE_PHASE_STARTED,
                3
            )
            .map(|gesture| (gesture.kind, gesture.phase)),
            Some((PanelGestureKind::LongPress, GesturePhase::Started))
        );
    }

    #[test]
    fn instant_kinds_keep_gesture_id_zero() {
        let tap = decode(
            QarGuiPanelGestureKind::QAR_GUI_PANEL_GESTURE_KIND_TAP,
            QarGesturePhase::QAR_GESTURE_PHASE_INSTANT,
            0,
        )
        .unwrap();
        assert_eq!(
            (tap.kind, tap.phase, tap.gesture_id),
            (PanelGestureKind::Tap, GesturePhase::Instant, 0)
        );
        assert_eq!(tap.tap_count, 1);
    }

    #[test]
    fn unknown_kinds_and_phases_are_skipped() {
        assert!(
            decode(
                QarGuiPanelGestureKind(9),
                QarGesturePhase::QAR_GESTURE_PHASE_STARTED,
                1
            )
            .is_none()
        );
        assert!(
            decode(
                QarGuiPanelGestureKind::QAR_GUI_PANEL_GESTURE_KIND_TAP,
                QarGesturePhase(9),
                0
            )
            .is_none()
        );
    }

    #[test]
    fn values_beyond_i32_are_no_enum_value() {
        assert_eq!(enum_value(3_u32), Some(3));
        assert_eq!(enum_value(-1_i32), Some(-1));
        assert_eq!(enum_value(u32::MAX), None);
    }

    #[test]
    fn the_queue_keeps_the_newest_events_in_order() {
        let gestures = PanelGestures::default();
        for gesture_id in 0..300 {
            gestures.push(
                decode(
                    QarGuiPanelGestureKind::QAR_GUI_PANEL_GESTURE_KIND_DRAG,
                    QarGesturePhase::QAR_GESTURE_PHASE_UPDATED,
                    gesture_id,
                )
                .unwrap(),
            );
        }
        let taken = gestures.take();
        assert_eq!(taken.len(), GESTURE_QUEUE_LIMIT);
        assert_eq!(taken.first().map(|gesture| gesture.gesture_id), Some(44));
        assert_eq!(taken.last().map(|gesture| gesture.gesture_id), Some(299));
        assert!(gestures.take().is_empty());
    }
}
