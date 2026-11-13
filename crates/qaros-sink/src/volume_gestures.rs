//! Hand and controller gestures on the app volume (`qar_app_volumes_subscribe_all_gesture_events`
//! and the `qar_app_volume_gesture_get_*` getters), reached through the raw table: the `qaros`
//! crate does not wrap app volume gestures.
//!
//! QAROS's default rules fold a one-hand grab into the volume's app pose and a two-hand spread
//! into its app scale, and the runtime applies them itself (`was_mapped_to_app_transform`). The
//! viewer applies no delta; it logs each event and reads the volume's transform back while a
//! gesture runs, so the content follows the hands frame by frame.
//!
//! The library calls back on its own threads. The callback reads each event through the getters
//! inside the call, never keeps the handle, copies the values into [`VolumeGestures`] and wakes
//! the viewer; the QAROS thread drains the queue.

use std::collections::{BTreeSet, VecDeque};
use std::ffi::c_void;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr::NonNull;
use std::sync::{Arc, Mutex, PoisonError};

use qaros::qaros_sys::{
    QarApi, QarAppVolumeGesture, QarAppVolumeId, QarGestureKind, QarGesturePhase, QarHandedness,
    QarQuaternion, QarVector3, defaults,
};
use qaros::{AppVolumeId, CancelToken, PeerId};

use crate::connection::{QarosConnection, WakeViewer};
use crate::panel_gestures::GesturePhase;
use crate::raw_result::{check, is_success};

/// Oldest events are dropped beyond this many undrained ones.
const GESTURE_QUEUE_LIMIT: usize = 512;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VolumeGestureKind {
    Click,
    Hover,
    /// One-hand grab; the default rules move the content with it.
    Grab,
    /// Two-hand spread; the default rules scale the content with it.
    Spread,
    /// Two-hand rotate; off in the default rules.
    Rotate,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hand {
    Unspecified,
    Left,
    Right,
}

/// One gesture event. Deltas are accumulated since the gesture started, in app volume space
/// (header L4864-4902).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VolumeGesture {
    pub source_peer: PeerId,
    pub gesture_id: u64,
    pub kind: VolumeGestureKind,
    pub phase: GesturePhase,
    pub hand: Hand,
    pub translation: [f32; 3],
    /// `x, y, z, w`.
    pub rotation: [f32; 4],
    /// Whether the runtime already folded the gesture into the app pose or app scale.
    pub mapped_to_app_transform: bool,
    pub action_point: Option<[f32; 3]>,
}

impl VolumeGesture {
    /// Whether the gesture holds the volume's transform in motion after this event.
    fn is_running(&self) -> bool {
        matches!(self.phase, GesturePhase::Started | GesturePhase::Updated)
    }
}

const GESTURE_KINDS: [(QarGestureKind, VolumeGestureKind); 5] = [
    (QarGestureKind::QAR_GESTURE_CLICK, VolumeGestureKind::Click),
    (QarGestureKind::QAR_GESTURE_HOVER, VolumeGestureKind::Hover),
    (
        QarGestureKind::QAR_GESTURE_SINGLE_POINTER_6DOF,
        VolumeGestureKind::Grab,
    ),
    (
        QarGestureKind::QAR_GESTURE_DUAL_POINTER_TRANSLATE_DISTANCE,
        VolumeGestureKind::Spread,
    ),
    (
        QarGestureKind::QAR_GESTURE_DUAL_POINTER_ROTATE,
        VolumeGestureKind::Rotate,
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

/// A C enum's value as `i64`, which holds both MSVC's `int` and the `unsigned int` elsewhere.
fn enum_value(raw: impl Into<i64>) -> i64 {
    raw.into()
}

fn decode_kind(kind: i64) -> Option<VolumeGestureKind> {
    GESTURE_KINDS
        .iter()
        .find(|(raw, _)| enum_value(raw.0) == kind)
        .map(|(_, kind)| *kind)
}

fn decode_phase(phase: i64) -> Option<GesturePhase> {
    GESTURE_PHASES
        .iter()
        .find(|(raw, _)| enum_value(raw.0) == phase)
        .map(|(_, phase)| *phase)
}

fn decode_hand(handedness: i64) -> Hand {
    if handedness == enum_value(QarHandedness::QAR_HANDEDNESS_LEFT.0) {
        Hand::Left
    } else if handedness == enum_value(QarHandedness::QAR_HANDEDNESS_RIGHT.0) {
        Hand::Right
    } else {
        Hand::Unspecified
    }
}

/// A bounded queue the library thread fills and the QAROS thread drains.
#[derive(Clone, Default)]
pub struct VolumeGestures {
    queue: Arc<Mutex<VecDeque<VolumeGesture>>>,
}

impl VolumeGestures {
    pub fn push(&self, gesture: VolumeGesture) {
        let mut queue = self.queue.lock().unwrap_or_else(PoisonError::into_inner);
        if queue.len() >= GESTURE_QUEUE_LIMIT {
            queue.pop_front();
        }
        queue.push_back(gesture);
    }

    pub fn take(&self) -> Vec<VolumeGesture> {
        self.queue
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .drain(..)
            .collect()
    }
}

/// The gestures holding the volume's transform in motion, by source peer and gesture id. While
/// one runs the viewer reads the transform every frame rather than on the update poll.
#[derive(Debug, Default)]
pub(crate) struct RunningGestures {
    running: BTreeSet<(PeerId, u64)>,
}

impl RunningGestures {
    /// Logs the drained events and tracks which gestures still run. Returns whether any event
    /// may have changed the volume's transform, so the caller reads it now.
    pub(crate) fn absorb(&mut self, gestures: &[VolumeGesture]) -> bool {
        let mut transform_may_have_changed = false;
        for gesture in gestures {
            log_gesture(gesture);
            let key = (gesture.source_peer, gesture.gesture_id);
            if gesture.is_running() {
                self.running.insert(key);
            } else {
                self.running.remove(&key);
            }
            transform_may_have_changed |= gesture.mapped_to_app_transform;
        }
        transform_may_have_changed
    }

    pub(crate) fn any_running(&self) -> bool {
        !self.running.is_empty()
    }
}

/// Starts, ends and instant events at info, so a run's log shows every grab and spread; the
/// updates in between at debug.
fn log_gesture(gesture: &VolumeGesture) {
    let level = if gesture.phase == GesturePhase::Updated {
        log::Level::Debug
    } else {
        log::Level::Info
    };
    let [x, y, z] = gesture.translation;
    let [rx, ry, rz, rw] = gesture.rotation;
    log::log!(
        level,
        "qaros: volume gesture {:?} {:?} ({:?} hand, id {}): translation ({x:.3}, {y:.3}, {z:.3}), rotation ({rx:.3}, {ry:.3}, {rz:.3}, {rw:.3}), action point {:?}, {}",
        gesture.kind,
        gesture.phase,
        gesture.hand,
        gesture.gesture_id,
        gesture.action_point,
        if gesture.mapped_to_app_transform {
            "applied to the app transform by the runtime"
        } else {
            "not mapped"
        }
    );
}

/// What the callback reads its events with and hands them to. `api` points into the library,
/// which the subscription's cancel token keeps alive until after the cancel.
struct GestureCallbackState {
    api: *const QarApi,
    inbox: VolumeGestures,
    wake: WakeViewer,
}

/// The gesture subscription on the viewer's app volume, cancelled when dropped. The cancel
/// token holds the library, so the callback state's table outlives every callback; the token
/// and the raw state make the subscription `!Send`, which keeps it on the QAROS thread.
pub struct VolumeGestureSubscription {
    token: CancelToken,
    state: NonNull<GestureCallbackState>,
}

impl VolumeGestureSubscription {
    /// `qar_app_volumes_subscribe_all_gesture_events` on `volume`, every kind on one callback
    /// so the phases of one gesture keep their order (header L4737-4800).
    pub fn subscribe(
        connection: &QarosConnection,
        volume: AppVolumeId,
        inbox: VolumeGestures,
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
        let raw_volume = QarAppVolumeId { data: volume.0 };
        // SAFETY: the session is alive and used on this thread; the volume id lives for the
        // call; `state` stays valid until the cancel in Drop has returned, the token lives in
        // `Self`, and the trampoline matches the callback type.
        let subscribed = unsafe {
            (api.qar_impl_app_volumes_subscribe_all_gesture_events)(
                session.as_raw(),
                &raw const raw_volume,
                Some(gesture_trampoline),
                state.as_ptr().cast::<c_void>(),
                token.as_raw(),
            )
        };
        if let Err(error) = check(
            api,
            "qar_app_volumes_subscribe_all_gesture_events",
            subscribed,
        ) {
            // SAFETY: nothing was subscribed, so nothing else holds the state.
            drop(unsafe { Box::from_raw(state.as_ptr()) });
            return Err(error);
        }
        Ok(Self { token, state })
    }
}

impl Drop for VolumeGestureSubscription {
    /// The cancel waits for a running callback, so the state is freed only once no callback can
    /// reach it. The token itself is destroyed after this body.
    fn drop(&mut self) {
        if let Err(error) = self.token.cancel() {
            log::warn!("qaros: cancelling the volume gesture subscription: {error}");
        }
        // SAFETY: the cancel returned, so no callback runs or will run with the state, which is
        // freed once.
        drop(unsafe { Box::from_raw(self.state.as_ptr()) });
    }
}

/// Reads one event through the getters and queues it. A panic must not unwind into C.
unsafe extern "C" fn gesture_trampoline(
    gesture: *mut QarAppVolumeGesture,
    user_state: *mut c_void,
) {
    if gesture.is_null() || user_state.is_null() {
        return;
    }
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: `user_state` is the state registered with this trampoline, alive until the
        // subscription's cancel returns.
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
        log::error!("qaros: a volume gesture callback panicked; the event is dropped");
    }
}

/// # Safety
///
/// `gesture` is the live handle of the running callback.
unsafe fn read_gesture(api: &QarApi, gesture: *mut QarAppVolumeGesture) -> Option<VolumeGesture> {
    let mut source_peer = defaults::qar_peer_id_default();
    let mut target_volume = QarAppVolumeId { data: [0; 16] };
    let mut gesture_id = 0_u64;
    let mut kind = QarGestureKind::QAR_GESTURE_CLICK;
    let mut phase = QarGesturePhase::QAR_GESTURE_PHASE_INSTANT;
    let mut handedness = QarHandedness::QAR_HANDEDNESS_UNSPECIFIED;
    let mut translation = QarVector3 {
        x: 0.0,
        y: 0.0,
        z: 0.0,
    };
    let mut rotation = QarQuaternion {
        x: 0.0,
        y: 0.0,
        z: 0.0,
        w: 1.0,
    };
    let mut mapped = false;
    let mut action_point = translation;
    let mut has_action_point = false;
    let mut start_point = translation;
    let mut has_start_point = false;
    // SAFETY (all getters): the handle is live for the callback; the out-pointers are valid.
    let lifecycle = unsafe {
        (api.qar_impl_app_volume_gesture_get_lifecycle)(
            gesture,
            &raw mut source_peer,
            &raw mut target_volume,
        )
    };
    let classification = unsafe {
        (api.qar_impl_app_volume_gesture_get_classification)(gesture, &raw mut kind, &raw mut phase)
    };
    if !(is_success(lifecycle) && is_success(classification)) {
        log::warn!("qaros: a volume gesture whose lifecycle or kind cannot be read");
        return None;
    }
    // The remaining getters do not apply to every kind; a failed read leaves the default.
    let _ =
        unsafe { (api.qar_impl_app_volume_gesture_get_gesture_id)(gesture, &raw mut gesture_id) };
    let _ =
        unsafe { (api.qar_impl_app_volume_gesture_get_handedness)(gesture, &raw mut handedness) };
    let deltas = unsafe {
        (api.qar_impl_app_volume_gesture_get_deltas)(
            gesture,
            &raw mut translation,
            &raw mut rotation,
            &raw mut mapped,
        )
    };
    if !is_success(deltas) {
        mapped = false;
    }
    let points = unsafe {
        (api.qar_impl_app_volume_gesture_get_points)(
            gesture,
            &raw mut action_point,
            &raw mut has_action_point,
            &raw mut start_point,
            &raw mut has_start_point,
        )
    };
    decode_gesture(RawGesture {
        source_peer: PeerId(source_peer.data),
        gesture_id,
        kind: enum_value(kind.0),
        phase: enum_value(phase.0),
        handedness: enum_value(handedness.0),
        translation: [translation.x, translation.y, translation.z],
        rotation: [rotation.x, rotation.y, rotation.z, rotation.w],
        mapped_to_app_transform: mapped,
        action_point: (is_success(points) && has_action_point).then_some([
            action_point.x,
            action_point.y,
            action_point.z,
        ]),
    })
}

/// The values of one event as the getters return them.
#[derive(Clone, Copy)]
pub(crate) struct RawGesture {
    pub source_peer: PeerId,
    pub gesture_id: u64,
    pub kind: i64,
    pub phase: i64,
    pub handedness: i64,
    pub translation: [f32; 3],
    pub rotation: [f32; 4],
    pub mapped_to_app_transform: bool,
    pub action_point: Option<[f32; 3]>,
}

/// Unknown kinds and phases (the header reserves room for more) give `None`.
pub(crate) fn decode_gesture(raw: RawGesture) -> Option<VolumeGesture> {
    Some(VolumeGesture {
        source_peer: raw.source_peer,
        gesture_id: raw.gesture_id,
        kind: decode_kind(raw.kind)?,
        phase: decode_phase(raw.phase)?,
        hand: decode_hand(raw.handedness),
        translation: raw.translation,
        rotation: raw.rotation,
        mapped_to_app_transform: raw.mapped_to_app_transform,
        action_point: raw.action_point,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const PEER: PeerId = PeerId([0xb2; 16]);

    fn raw(
        kind: QarGestureKind,
        phase: QarGesturePhase,
        gesture_id: u64,
        mapped: bool,
    ) -> RawGesture {
        RawGesture {
            source_peer: PEER,
            gesture_id,
            kind: enum_value(kind.0),
            phase: enum_value(phase.0),
            handedness: enum_value(QarHandedness::QAR_HANDEDNESS_RIGHT.0),
            translation: [0.1, 0.0, -0.2],
            rotation: [0.0, 0.0, 0.0, 1.0],
            mapped_to_app_transform: mapped,
            action_point: Some([0.0, 1.0, -0.5]),
        }
    }

    #[test]
    fn a_grab_decodes_with_its_values() {
        let grab = decode_gesture(raw(
            QarGestureKind::QAR_GESTURE_SINGLE_POINTER_6DOF,
            QarGesturePhase::QAR_GESTURE_PHASE_UPDATED,
            7,
            true,
        ))
        .unwrap();
        assert_eq!(grab.kind, VolumeGestureKind::Grab);
        assert_eq!(grab.phase, GesturePhase::Updated);
        assert_eq!(grab.hand, Hand::Right);
        assert_eq!(grab.gesture_id, 7);
        // The values are copied through unchanged, so they compare exactly.
        assert_eq!(
            grab.translation.map(f32::to_bits),
            [0.1_f32, 0.0, -0.2].map(f32::to_bits)
        );
        assert_eq!(
            grab.action_point.map(|point| point.map(f32::to_bits)),
            Some([0.0_f32, 1.0, -0.5].map(f32::to_bits))
        );
        assert!(grab.mapped_to_app_transform);
    }

    #[test]
    fn unknown_kinds_and_phases_are_skipped() {
        let mut unknown_kind = raw(
            QarGestureKind::QAR_GESTURE_HOVER,
            QarGesturePhase::QAR_GESTURE_PHASE_STARTED,
            1,
            false,
        );
        unknown_kind.kind = 9;
        assert!(decode_gesture(unknown_kind).is_none());
        let mut unknown_phase = raw(
            QarGestureKind::QAR_GESTURE_HOVER,
            QarGesturePhase::QAR_GESTURE_PHASE_STARTED,
            1,
            false,
        );
        unknown_phase.phase = 9;
        assert!(decode_gesture(unknown_phase).is_none());
    }

    #[test]
    fn a_running_spread_keeps_the_transform_read_each_frame_until_it_ends() {
        let event = |phase, mapped| {
            decode_gesture(raw(
                QarGestureKind::QAR_GESTURE_DUAL_POINTER_TRANSLATE_DISTANCE,
                phase,
                3,
                mapped,
            ))
            .unwrap()
        };
        let mut running = RunningGestures::default();
        assert!(running.absorb(&[event(QarGesturePhase::QAR_GESTURE_PHASE_STARTED, true)]));
        assert!(running.any_running());
        assert!(running.absorb(&[event(QarGesturePhase::QAR_GESTURE_PHASE_UPDATED, true)]));
        assert!(running.any_running());
        assert!(running.absorb(&[event(QarGesturePhase::QAR_GESTURE_PHASE_ENDED, true)]));
        assert!(!running.any_running());
    }

    #[test]
    fn an_unmapped_click_reads_nothing_and_runs_nothing() {
        let click = decode_gesture(raw(
            QarGestureKind::QAR_GESTURE_CLICK,
            QarGesturePhase::QAR_GESTURE_PHASE_INSTANT,
            0,
            false,
        ))
        .unwrap();
        let mut running = RunningGestures::default();
        assert!(!running.absorb(&[click]));
        assert!(!running.any_running());
    }

    #[test]
    fn the_queue_keeps_the_newest_events_in_order() {
        let gestures = VolumeGestures::default();
        for gesture_id in 0..600 {
            gestures.push(
                decode_gesture(raw(
                    QarGestureKind::QAR_GESTURE_SINGLE_POINTER_6DOF,
                    QarGesturePhase::QAR_GESTURE_PHASE_UPDATED,
                    gesture_id,
                    true,
                ))
                .unwrap(),
            );
        }
        let taken = gestures.take();
        assert_eq!(taken.len(), GESTURE_QUEUE_LIMIT);
        assert_eq!(taken.first().map(|gesture| gesture.gesture_id), Some(88));
        assert_eq!(taken.last().map(|gesture| gesture.gesture_id), Some(599));
        assert!(gestures.take().is_empty());
    }
}
