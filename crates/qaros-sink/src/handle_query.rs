//! One QAROS listing in one reconciliation query.
//!
//! `qar_query_gui_panels_count`, `qar_query_gui_panels`, `qar_query_app_volumes_count` and
//! `qar_query_app_volumes` each run the session's reconciliation query, which always waits out
//! its 2 s reply window ([QS] `SharedObjectClient.hpp` `Query`, L404-L425;
//! `SharedObjectTypes.hpp` L27). A count before the query doubles a listing to 4 s. The query
//! alone writes `min(objects, buffer)` handles ([QS] `gui_panels.cpp` L1493-L1498,
//! `app_volumes.cpp` `qar_impl_query_app_volumes`), so a buffer it leaves a slot free in holds
//! every object; only a full buffer asks again with twice the room.
//!
//! [QS]: https://github.com/Quaternar/qar_streaming/tree/e487a31158caca9a9cd2fcf6073456b316d8b7c6/libs "QAROS streaming library source (private)"

/// Room for the first query; a session holds a handful of panels and volumes.
pub(crate) const FIRST_QUERY_CAPACITY: usize = 64;

/// Every non-null handle one `query` wrote into a buffer it did not fill, querying again with
/// twice the room after a full buffer. `query(buffer, capacity, written)` makes the raw call;
/// `destroy` releases a handle that is not returned: those of a full buffer, and those written
/// before a failed call.
pub(crate) fn query_all_handles<Handle>(
    mut query: impl FnMut(*mut *mut Handle, usize, &mut usize) -> Result<(), String>,
    destroy: impl Fn(*mut Handle),
) -> Result<Vec<*mut Handle>, String> {
    let mut capacity = FIRST_QUERY_CAPACITY;
    loop {
        let mut handles: Vec<*mut Handle> = vec![std::ptr::null_mut(); capacity];
        let mut written = 0_usize;
        let queried = query(handles.as_mut_ptr(), capacity, &mut written);
        handles.truncate(written.min(capacity));
        handles.retain(|handle| !handle.is_null());
        let buffer_was_full = written >= capacity;
        if queried.is_err() || buffer_was_full {
            for &handle in &handles {
                destroy(handle);
            }
        }
        queried?;
        if !buffer_was_full {
            return Ok(handles);
        }
        capacity *= 2;
    }
}

// The fake query writes handles through the raw buffer, as the C API does.
#[cfg(test)]
#[allow(unsafe_code)]
mod tests {
    use std::cell::RefCell;

    use super::*;

    /// Fake handles: distinct non-null addresses that are never dereferenced.
    fn fake_handle(index: usize) -> *mut u8 {
        std::ptr::without_provenance_mut(0x1000 + index * 8)
    }

    struct FakeSession {
        objects: usize,
        capacities_asked: RefCell<Vec<usize>>,
        destroyed: RefCell<Vec<*mut u8>>,
    }

    impl FakeSession {
        fn with_objects(objects: usize) -> Self {
            Self {
                objects,
                capacities_asked: RefCell::new(Vec::new()),
                destroyed: RefCell::new(Vec::new()),
            }
        }

        fn query(&self, buffer: *mut *mut u8, capacity: usize, written: &mut usize) {
            self.capacities_asked.borrow_mut().push(capacity);
            let copied = self.objects.min(capacity);
            for index in 0..copied {
                // SAFETY: the buffer holds `capacity` slots and `index < capacity`.
                unsafe { buffer.add(index).write(fake_handle(index)) };
            }
            *written = copied;
        }
    }

    #[test]
    fn a_few_objects_take_one_query_and_nothing_is_destroyed() {
        let session = FakeSession::with_objects(3);
        let handles = query_all_handles(
            |buffer, capacity, written| {
                session.query(buffer, capacity, written);
                Ok(())
            },
            |handle| session.destroyed.borrow_mut().push(handle),
        )
        .expect("listed");

        assert_eq!(
            handles,
            vec![fake_handle(0), fake_handle(1), fake_handle(2)]
        );
        assert_eq!(*session.capacities_asked.borrow(), vec![64]);
        assert!(session.destroyed.borrow().is_empty());
    }

    #[test]
    fn a_full_buffer_is_released_and_asked_again_with_twice_the_room() {
        let session = FakeSession::with_objects(FIRST_QUERY_CAPACITY);
        let handles = query_all_handles(
            |buffer, capacity, written| {
                session.query(buffer, capacity, written);
                Ok(())
            },
            |handle| session.destroyed.borrow_mut().push(handle),
        )
        .expect("listed");

        assert_eq!(handles.len(), 64);
        assert_eq!(handles[63], fake_handle(63));
        assert_eq!(*session.capacities_asked.borrow(), vec![64, 128]);
        let destroyed = session.destroyed.borrow();
        assert_eq!(destroyed.len(), 64);
        assert_eq!(destroyed[0], fake_handle(0));
        assert_eq!(destroyed[63], fake_handle(63));
    }

    #[test]
    fn a_failed_query_destroys_what_it_wrote_and_returns_its_error() {
        let session = FakeSession::with_objects(2);
        let listed = query_all_handles(
            |buffer, capacity, written| {
                session.query(buffer, capacity, written);
                Err("qar_query_gui_panels: failed".to_owned())
            },
            |handle| session.destroyed.borrow_mut().push(handle),
        );

        assert_eq!(listed, Err("qar_query_gui_panels: failed".to_owned()));
        assert_eq!(
            *session.destroyed.borrow(),
            vec![fake_handle(0), fake_handle(1)]
        );
    }

    #[test]
    fn null_handles_are_left_out() {
        let handles = query_all_handles(
            |buffer: *mut *mut u8, _capacity, written| {
                // SAFETY: the buffer holds at least three slots.
                unsafe {
                    buffer.write(fake_handle(0));
                    buffer.add(1).write(std::ptr::null_mut());
                    buffer.add(2).write(fake_handle(2));
                }
                *written = 3;
                Ok(())
            },
            |_| {},
        )
        .expect("listed");

        assert_eq!(handles, vec![fake_handle(0), fake_handle(2)]);
    }
}
