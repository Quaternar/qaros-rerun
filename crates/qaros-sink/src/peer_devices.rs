//! What kind of device a peer is, from `qar_peer_spec_get_category` and
//! `qar_peer_spec_get_device_name`: the `qaros` crate leaves both to its raw table (its README's
//! coverage table, "Peers: presence, role, category, device identity").

use std::ffi::{CStr, c_char};

use qaros::PeerId;
use qaros::qaros_sys::{QarApi, QarDeviceCategory, QarPeerSpec, defaults};

use crate::connection::QarosConnection;
use crate::raw_result::check;

/// A peer's device as QAROS reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PeerDevice {
    pub category: QarDeviceCategory,
    /// For logs, for example "Meta Quest 3"; empty when QAROS knows nothing better than the
    /// device id.
    pub name: String,
}

impl PeerDevice {
    /// `QAR_DEVICE_CATEGORY_HEADSET`: "a headset or glasses of any model", as opposed to a
    /// Visualizer receiver showing streams on the PC (`QAR_DEVICE_CATEGORY_VISUALIZER`).
    pub fn is_headset(&self) -> bool {
        self.category == QarDeviceCategory::QAR_DEVICE_CATEGORY_HEADSET
    }

    pub fn category_name(&self) -> &'static str {
        match self.category {
            QarDeviceCategory::QAR_DEVICE_CATEGORY_DESKTOP_PC => "desktop PC",
            QarDeviceCategory::QAR_DEVICE_CATEGORY_SERVER => "server",
            QarDeviceCategory::QAR_DEVICE_CATEGORY_HEADSET => "headset",
            QarDeviceCategory::QAR_DEVICE_CATEGORY_VISUALIZER => "Visualizer receiver",
            QarDeviceCategory::QAR_DEVICE_CATEGORY_CAMERA => "camera",
            QarDeviceCategory::QAR_DEVICE_CATEGORY_OBSERVER => "camera observer",
            _ => "unknown device",
        }
    }
}

const DEVICE_NAME_BUFFER_SIZE: usize = 128;

/// The device of `peer` in the session's peer list; `Ok(None)` when the list does not hold it.
pub(crate) fn read_peer_device(
    connection: &QarosConnection,
    peer: PeerId,
) -> Result<Option<PeerDevice>, String> {
    let session = connection.session();
    let library = session.library();
    let api = library.raw_api();
    let raw_session = session.as_raw();
    let mut count = 0_usize;
    // SAFETY: the session is alive and used on this thread; the out-pointer is valid.
    let counted = unsafe { (api.qar_impl_query_peer_specs_count)(raw_session, &raw mut count) };
    check(api, "qar_query_peer_specs_count", counted)?;
    let mut specs: Vec<*mut QarPeerSpec> = vec![std::ptr::null_mut(); count];
    let mut written = 0_usize;
    // SAFETY: as above; the buffer holds `specs.len()` handle slots.
    let queried = unsafe {
        (api.qar_impl_query_peer_specs)(
            raw_session,
            specs.as_mut_ptr(),
            specs.len(),
            &raw mut written,
        )
    };
    // Every spec the query wrote is ours to destroy, whatever it returned.
    let written_specs = &specs[..written.min(specs.len())];
    let found = check(api, "qar_query_peer_specs", queried).and_then(|()| {
        written_specs
            .iter()
            .filter(|spec| !spec.is_null())
            .find_map(|&spec| device_of_spec_if_peer(api, spec, peer).transpose())
            .transpose()
    });
    for &spec in written_specs.iter().filter(|spec| !spec.is_null()) {
        // SAFETY: each written handle is destroyed once, after its last use above.
        unsafe { (api.qar_impl_peer_spec_handle_destroy)(spec) };
    }
    found
}

/// The device `spec` describes, if `spec` is `peer`'s.
fn device_of_spec_if_peer(
    api: &QarApi,
    spec: *mut QarPeerSpec,
    peer: PeerId,
) -> Result<Option<PeerDevice>, String> {
    let mut spec_peer = defaults::qar_peer_id_default();
    // SAFETY: `spec` is a live handle from the query; the out-pointer is valid.
    let read_id = unsafe { (api.qar_impl_peer_spec_get_id)(spec, &raw mut spec_peer) };
    check(api, "qar_peer_spec_get_id", read_id)?;
    if spec_peer.data != peer.0 {
        return Ok(None);
    }
    let mut category = QarDeviceCategory::QAR_DEVICE_CATEGORY_UNKNOWN;
    // SAFETY: as above.
    let read_category = unsafe { (api.qar_impl_peer_spec_get_category)(spec, &raw mut category) };
    check(api, "qar_peer_spec_get_category", read_category)?;
    let mut name = [0 as c_char; DEVICE_NAME_BUFFER_SIZE];
    // SAFETY: as above; the buffer is writable for its full length, which is passed.
    let read_name =
        unsafe { (api.qar_impl_peer_spec_get_device_name)(spec, name.as_mut_ptr(), name.len()) };
    // The name is only for logs; the category alone decides.
    let name = match check(api, "qar_peer_spec_get_device_name", read_name) {
        // SAFETY: on success the buffer holds a NUL-terminated string.
        Ok(()) => unsafe { CStr::from_ptr(name.as_ptr()) }
            .to_string_lossy()
            .into_owned(),
        Err(error) => {
            log::debug!("qaros: peer {peer} has a device category but no name: {error}");
            String::new()
        }
    };
    Ok(Some(PeerDevice { category, name }))
}
