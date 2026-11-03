//! Whether a held panel was closed by someone else while this run holds it
//! (`qar_gui_panels_subscribe_panel_updates` and `qar_gui_panel_get_state`, reached through the
//! raw table: the `qaros` crate's panel subscription reads every update's content URI, which
//! fails for a stream panel).
//!
//! A close in the Hub Display is a `QAR_GUI_PANEL_STATE_CLOSED` proposal of the Display's peer.
//! Without a watch the run learned of it only from a panel listing, and its Stop then hid the
//! closed panel, which brought it back as `Hidden` for the next run to reattach at its old place.

use std::ffi::c_void;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicU8, Ordering};

use qaros::qaros_sys::{QarApi, QarGuiPanel, QarGuiPanelId, QarGuiPanelState};
use qaros::{CancelToken, GuiPanelId};

use crate::connection::QarosConnection;
use crate::raw_result::{check, is_success};

/// No update seen yet. An update delivered first can describe the panel as it was before this
/// run's claim, closed included, so a close counts only once the panel was seen open.
const NOTHING_SEEN: u8 = 0;
const SEEN_OPEN: u8 = 1;
const CLOSED_AFTER_OPEN: u8 = 2;

/// What the callback reads updates with and where it records them. `api` points into the
/// library, which the subscription's cancel token keeps alive until after the cancel.
struct WatchState {
    api: *const QarApi,
    seen: AtomicU8,
}

/// A state subscription on one panel, cancelled when dropped. Like the gesture subscription it
/// is `!Send` and stays on the QAROS thread.
pub(crate) struct PanelStateWatch {
    token: CancelToken,
    state: NonNull<WatchState>,
}

impl PanelStateWatch {
    pub(crate) fn subscribe(
        connection: &QarosConnection,
        panel: GuiPanelId,
    ) -> Result<Self, String> {
        let session = connection.session();
        let library = session.library();
        let token = library
            .create_cancel_token(None)
            .map_err(|error| format!("qar_cancel_token_create: {error}"))?;
        let api = library.raw_api();
        let state = NonNull::from(Box::leak(Box::new(WatchState {
            api: std::ptr::from_ref(api),
            seen: AtomicU8::new(NOTHING_SEEN),
        })));
        let raw_panel = QarGuiPanelId { data: panel.0 };
        // SAFETY: the session is alive and used on this thread; the panel id lives for the
        // call; `state` stays valid until the cancel in Drop has returned, the token lives in
        // `Self`, and the trampoline matches the callback type.
        let subscribed = unsafe {
            (api.qar_impl_gui_panels_subscribe_panel_updates)(
                session.as_raw(),
                &raw const raw_panel,
                Some(state_trampoline),
                state.as_ptr().cast::<c_void>(),
                token.as_raw(),
            )
        };
        if let Err(error) = check(api, "qar_gui_panels_subscribe_panel_updates", subscribed) {
            // SAFETY: nothing was subscribed, so nothing else holds the state.
            drop(unsafe { Box::from_raw(state.as_ptr()) });
            return Err(error);
        }
        Ok(Self { token, state })
    }

    pub(crate) fn closed_after_open(&self) -> bool {
        // SAFETY: the state lives until Drop, after the cancel.
        let state = unsafe { self.state.as_ref() };
        state.seen.load(Ordering::Acquire) == CLOSED_AFTER_OPEN
    }
}

impl Drop for PanelStateWatch {
    /// The cancel waits for a running callback, so the state is freed only once no callback can
    /// reach it.
    fn drop(&mut self) {
        if let Err(error) = self.token.cancel() {
            log::warn!("qaros: cancelling a panel state subscription: {error}");
        }
        // SAFETY: the cancel returned, so no callback runs or will run with the state, which is
        // freed once.
        drop(unsafe { Box::from_raw(self.state.as_ptr()) });
    }
}

fn next_seen(seen: u8, is_closed: bool) -> u8 {
    match (seen, is_closed) {
        (CLOSED_AFTER_OPEN, _) | (SEEN_OPEN, true) => CLOSED_AFTER_OPEN,
        (_, false) => SEEN_OPEN,
        (_, true) => seen,
    }
}

/// Reads the update's state and records it. A panic must not unwind into C.
unsafe extern "C" fn state_trampoline(handle: *mut QarGuiPanel, user_state: *mut c_void) {
    if handle.is_null() || user_state.is_null() {
        return;
    }
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: `user_state` is the state registered with this trampoline, alive until the
        // subscription's cancel returns.
        let state = unsafe { &*user_state.cast::<WatchState>() };
        // SAFETY: the table lives in the library, which the subscription's token keeps alive.
        let api = unsafe { &*state.api };
        let mut raw_state = QarGuiPanelState::QAR_GUI_PANEL_STATE_VISIBLE;
        // SAFETY: the handle is borrowed for the duration of the callback; the out-pointer is
        // valid.
        let read = unsafe { (api.qar_impl_gui_panel_get_state)(handle, &raw mut raw_state) };
        if !is_success(read) {
            return;
        }
        let is_closed = raw_state == QarGuiPanelState::QAR_GUI_PANEL_STATE_CLOSED;
        let _ = state
            .seen
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |seen| {
                Some(next_seen(seen, is_closed))
            });
    }));
    if outcome.is_err() {
        log::error!("qaros: a panel state callback panicked; the update is dropped");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seen_after(updates: &[bool]) -> u8 {
        updates
            .iter()
            .fold(NOTHING_SEEN, |seen, &is_closed| next_seen(seen, is_closed))
    }

    #[test]
    fn a_close_counts_only_after_the_panel_was_seen_open_and_then_stays() {
        assert_eq!(
            seen_after(&[true]),
            NOTHING_SEEN,
            "a first update may be the state before the claim reopened it"
        );
        assert_eq!(seen_after(&[true, false]), SEEN_OPEN);
        assert_eq!(seen_after(&[false, true]), CLOSED_AFTER_OPEN);
        assert_eq!(seen_after(&[false, true, false]), CLOSED_AFTER_OPEN);
    }
}
