//! The app volume's used-by peers as QAROS reports them, kept current during the run so Stop
//! never has to ask (ADR-0093).
//!
//! Asking is slow: `qar_query_app_volumes_count` and `qar_query_app_volumes` each run QAROS's
//! reconciliation query, which waits out its full 2 s reply window before it returns
//! ([QS] `SharedObjectClient.hpp` `Query`, `kSharedObjectReconcileQueryTimeoutMs`), so one
//! listing blocks about 4 s. The volume subscription delivers each of the Hub's bakes of the
//! volume, used-by peers included, without a round trip; its callback copies the peers into
//! [`ReportedUsers`]. The one listing the viewer makes at connect, to see whether QAROS handed a
//! closed volume back, seeds the same record.
//!
//! [QS]: https://github.com/Quaternar/qar_streaming/tree/e487a31158caca9a9cd2fcf6073456b316d8b7c6/libs/qar-streaming "QAROS streaming library source (private)"

use std::ffi::c_void;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr::NonNull;
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::Instant;

use qaros::qaros_sys::{QarApi, QarAppVolume, QarAppVolumeLifetimeStatus, defaults};
use qaros::{AppVolumeId, CancelToken, PeerId, Session};

use crate::raw_result::check;

/// The used-by peers QAROS reported last for one volume, and how many reports came, written by
/// the subscription's callback on library threads and read on the QAROS thread.
#[derive(Clone, Default)]
pub(crate) struct ReportedUsers {
    shared: Arc<(Mutex<UsersReport>, Condvar)>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct UsersReport {
    /// `None` until a report arrived.
    pub(crate) peers: Option<Vec<PeerId>>,
    /// Counts every report, so a waiter tells a new one from the one it has seen.
    pub(crate) count: u64,
}

impl ReportedUsers {
    pub(crate) fn report(&self, peers: Vec<PeerId>) {
        let (report, changed) = &*self.shared;
        let mut report = report.lock().unwrap_or_else(PoisonError::into_inner);
        report.peers = Some(peers);
        report.count += 1;
        changed.notify_all();
    }

    pub(crate) fn latest(&self) -> UsersReport {
        let (report, _) = &*self.shared;
        report
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// The first report after the `seen_count`th, or `None` once `deadline` passes without one.
    pub(crate) fn wait_for_report_after(
        &self,
        seen_count: u64,
        deadline: Instant,
    ) -> Option<UsersReport> {
        let (report, changed) = &*self.shared;
        let mut report = report.lock().unwrap_or_else(PoisonError::into_inner);
        loop {
            if report.count > seen_count {
                return Some(report.clone());
            }
            let left = deadline.checked_duration_since(Instant::now())?;
            report = changed
                .wait_timeout(report, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }
}

/// The volume as the connect-time listing shows it.
#[derive(Debug)]
pub(crate) struct ListedVolume {
    pub(crate) closed: bool,
    pub(crate) used_by: Result<Vec<PeerId>, String>,
}

/// One listing (`qar_query_app_volumes`), about 4 s: whether `volume` is closed and which peers
/// use it. `Ok(None)` when the session does not list it.
pub(crate) fn list_volume(
    session: &Session,
    volume: AppVolumeId,
) -> Result<Option<ListedVolume>, String> {
    let library = session.library();
    let api = library.raw_api();
    let handles = query_volume_handles(api, session)?;
    let found = find_volume(api, &handles, volume);
    for &handle in &handles {
        // SAFETY: each handle the query wrote is destroyed once, after its last use above.
        unsafe { (api.qar_impl_app_volume_handle_destroy)(handle) };
    }
    found
}

/// Every handle `qar_query_app_volumes` wrote; the caller destroys them.
fn query_volume_handles(api: &QarApi, session: &Session) -> Result<Vec<*mut QarAppVolume>, String> {
    let raw_session = session.as_raw();
    let mut count = 0_usize;
    // SAFETY: the session is alive and used on this thread; the out-pointer is valid.
    let counted = unsafe { (api.qar_impl_query_app_volumes_count)(raw_session, &raw mut count) };
    check(api, "qar_query_app_volumes_count", counted)?;
    if count == 0 {
        return Ok(Vec::new());
    }
    let mut handles: Vec<*mut QarAppVolume> = vec![std::ptr::null_mut(); count];
    let mut written = 0_usize;
    // SAFETY: as above; the buffer holds `handles.len()` handle slots.
    let queried = unsafe {
        (api.qar_impl_query_app_volumes)(
            raw_session,
            handles.as_mut_ptr(),
            handles.len(),
            &raw mut written,
        )
    };
    handles.truncate(written.min(count));
    handles.retain(|handle| !handle.is_null());
    if let Err(error) = check(api, "qar_query_app_volumes", queried) {
        for &handle in &handles {
            // SAFETY: each written handle is destroyed once; none is used after a failed query.
            unsafe { (api.qar_impl_app_volume_handle_destroy)(handle) };
        }
        return Err(error);
    }
    Ok(handles)
}

fn find_volume(
    api: &QarApi,
    handles: &[*mut QarAppVolume],
    volume: AppVolumeId,
) -> Result<Option<ListedVolume>, String> {
    for &handle in handles {
        // SAFETY: `handle` is a live handle from the query.
        if unsafe { read_volume_id(api, handle) }? != volume {
            continue;
        }
        let mut lifetime = QarAppVolumeLifetimeStatus::QAR_APP_VOLUME_ACTIVE;
        // SAFETY: as above; the out-pointer is valid.
        let read_lifetime =
            unsafe { (api.qar_impl_app_volume_get_lifetime_status)(handle, &raw mut lifetime) };
        check(api, "qar_app_volume_get_lifetime_status", read_lifetime)?;
        return Ok(Some(ListedVolume {
            closed: lifetime == QarAppVolumeLifetimeStatus::QAR_APP_VOLUME_CLOSED,
            // SAFETY: as above.
            used_by: unsafe { read_used_by_peers(api, handle) },
        }));
    }
    Ok(None)
}

/// # Safety
///
/// `handle` is a live app volume handle.
unsafe fn read_volume_id(api: &QarApi, handle: *mut QarAppVolume) -> Result<AppVolumeId, String> {
    let mut listed_id = defaults::qar_app_volume_id_default();
    // SAFETY: forwarded to the caller; the out-pointer is valid.
    let read_id = unsafe { (api.qar_impl_app_volume_get_id)(handle, &raw mut listed_id) };
    check(api, "qar_app_volume_get_id", read_id)?;
    Ok(AppVolumeId(listed_id.data))
}

/// # Safety
///
/// `handle` is a live app volume handle.
unsafe fn read_used_by_peers(
    api: &QarApi,
    handle: *mut QarAppVolume,
) -> Result<Vec<PeerId>, String> {
    let mut count = 0_usize;
    // SAFETY: forwarded to the caller; the out-pointer is valid.
    let counted =
        unsafe { (api.qar_impl_app_volume_get_used_by_peers_count)(handle, &raw mut count) };
    check(api, "qar_app_volume_get_used_by_peers_count", counted)?;
    if count == 0 {
        return Ok(Vec::new());
    }
    let mut peers = vec![defaults::qar_peer_id_default(); count];
    let mut written = 0_usize;
    // SAFETY: as above; the buffer holds `peers.len()` peer ids.
    let read = unsafe {
        (api.qar_impl_app_volume_get_used_by_peers)(
            handle,
            peers.as_mut_ptr(),
            peers.len(),
            &raw mut written,
        )
    };
    check(api, "qar_app_volume_get_used_by_peers", read)?;
    peers.truncate(written.min(count));
    Ok(peers.into_iter().map(|peer| PeerId(peer.data)).collect())
}

/// What the callback reads updates with and where it records them. `api` points into the
/// library, which the subscription's cancel token keeps alive until after the cancel.
struct WatchState {
    api: *const QarApi,
    volume: AppVolumeId,
    reported: ReportedUsers,
}

/// The update subscription that keeps [`ReportedUsers`] current, cancelled when dropped. Like
/// the gesture subscription it is `!Send` and stays on the QAROS thread.
pub(crate) struct UsedByWatch {
    token: CancelToken,
    state: NonNull<WatchState>,
}

impl UsedByWatch {
    /// `qar_app_volumes_subscribe_updates`, which replays the current state first and then
    /// every bake ([QS] `SharedObjectClient.hpp` `ReceiveUpdates`).
    pub(crate) fn subscribe(
        session: &Session,
        volume: AppVolumeId,
        reported: ReportedUsers,
    ) -> Result<Self, String> {
        let library = session.library();
        let token = library
            .create_cancel_token(None)
            .map_err(|error| format!("qar_cancel_token_create: {error}"))?;
        let api = library.raw_api();
        let state = NonNull::from(Box::leak(Box::new(WatchState {
            api: std::ptr::from_ref(api),
            volume,
            reported,
        })));
        // SAFETY: the session is alive and used on this thread; `state` stays valid until the
        // cancel in Drop has returned, the token lives in `Self`, and the trampoline matches the
        // callback type.
        let subscribed = unsafe {
            (api.qar_impl_app_volumes_subscribe_updates)(
                session.as_raw(),
                Some(used_by_trampoline),
                state.as_ptr().cast::<c_void>(),
                token.as_raw(),
            )
        };
        if let Err(error) = check(api, "qar_app_volumes_subscribe_updates", subscribed) {
            // SAFETY: nothing was subscribed, so nothing else holds the state.
            drop(unsafe { Box::from_raw(state.as_ptr()) });
            return Err(error);
        }
        Ok(Self { token, state })
    }
}

impl Drop for UsedByWatch {
    /// The cancel waits for a running callback, so the state is freed only once no callback can
    /// reach it.
    fn drop(&mut self) {
        if let Err(error) = self.token.cancel() {
            log::warn!("qaros: cancelling the volume's used-by subscription: {error}");
        }
        // SAFETY: the cancel returned, so no callback runs or will run with the state, which is
        // freed once.
        drop(unsafe { Box::from_raw(self.state.as_ptr()) });
    }
}

/// Records the used-by peers of an update on the watched volume. A panic must not unwind into C.
unsafe extern "C" fn used_by_trampoline(handle: *mut QarAppVolume, user_state: *mut c_void) {
    if handle.is_null() || user_state.is_null() {
        return;
    }
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: `user_state` is the state registered with this trampoline, alive until the
        // subscription's cancel returns.
        let state = unsafe { &*user_state.cast::<WatchState>() };
        // SAFETY: the table lives in the library, which the subscription's token keeps alive.
        let api = unsafe { &*state.api };
        // SAFETY: the handle is borrowed for the duration of the callback.
        if unsafe { read_volume_id(api, handle) }.ok() != Some(state.volume) {
            return;
        }
        // SAFETY: as above. A failed read keeps the last report.
        if let Ok(peers) = unsafe { read_used_by_peers(api, handle) } {
            state.reported.report(peers);
        }
    }));
    if outcome.is_err() {
        log::error!("qaros: an app volume update callback panicked; the update is dropped");
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn peer(first_byte: u8) -> PeerId {
        let mut bytes = [0; 16];
        bytes[0] = first_byte;
        PeerId(bytes)
    }

    #[test]
    fn nothing_is_known_before_the_first_report() {
        let reported = ReportedUsers::default();

        assert_eq!(
            reported.latest(),
            UsersReport {
                peers: None,
                count: 0
            }
        );
    }

    #[test]
    fn each_report_replaces_the_peers_and_is_counted() {
        let reported = ReportedUsers::default();

        reported.report(vec![peer(1), peer(2)]);
        reported.report(vec![peer(2)]);

        assert_eq!(
            reported.latest(),
            UsersReport {
                peers: Some(vec![peer(2)]),
                count: 2
            }
        );
    }

    #[test]
    fn a_waiter_gets_the_report_made_from_another_thread() {
        let reported = ReportedUsers::default();
        reported.report(vec![peer(1)]);
        let callback_side = reported.clone();

        let reporting = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            callback_side.report(Vec::new());
        });
        let report = reported.wait_for_report_after(1, Instant::now() + Duration::from_secs(5));
        reporting.join().unwrap();

        assert_eq!(
            report,
            Some(UsersReport {
                peers: Some(Vec::new()),
                count: 2
            })
        );
    }

    #[test]
    fn a_waiter_gives_up_at_its_deadline_without_a_new_report() {
        let reported = ReportedUsers::default();
        reported.report(vec![peer(1)]);
        let started = Instant::now();

        let report = reported.wait_for_report_after(1, started + Duration::from_millis(30));

        assert_eq!(report, None);
        let waited = started.elapsed();
        assert!(
            (Duration::from_millis(30)..Duration::from_millis(500)).contains(&waited),
            "{waited:?}"
        );
    }
}
