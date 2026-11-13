//! The id QAROS gives a panel of a common name: a name-based (version 5) UUID in the QAR
//! namespace (`qar::CreateUUIDFromName`, `CommonImpl.cpp`, used by `GuiPanelIdFromCommonName`
//! in `GuiPanel.hpp` at qar-streaming `e487a311`). A panel the Hub carried into a new session
//! is listed with an empty display name, so the id is what finds it in a listing.

use qaros::GuiPanelId;
use sha1::{Digest, Sha1};

/// `a3b8f2b0-4b2f-4e9b-9a3e-6c2a5f9b0d10`, frozen in QAROS.
const QAR_NAME_NAMESPACE: [u8; 16] = [
    0xa3, 0xb8, 0xf2, 0xb0, 0x4b, 0x2f, 0x4e, 0x9b, 0x9a, 0x3e, 0x6c, 0x2a, 0x5f, 0x9b, 0x0d, 0x10,
];

/// RFC 4122 section 4.3: the first 16 bytes of SHA-1 over the namespace and the name, with the
/// version in the high nibble of byte 6 and the variant in the high bits of byte 8.
pub(crate) fn panel_id_of_common_name(common_name: &str) -> GuiPanelId {
    let mut hasher = Sha1::new();
    hasher.update(QAR_NAME_NAMESPACE);
    hasher.update(common_name.as_bytes());
    let digest = hasher.finalize();
    let mut id = [0_u8; 16];
    id.copy_from_slice(&digest[..16]);
    id[6] = (id[6] & 0x0f) | 0x50;
    id[8] = (id[8] & 0x3f) | 0x80;
    GuiPanelId(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The ids the Hub logged on 2026-11-02 for the structure-from-motion demo's panels.
    #[test]
    fn the_ids_match_the_ones_qaros_gave_the_demo_panels() {
        assert_eq!(
            panel_id_of_common_name("camera.stream.rerun-structure_from_motion.rerun").to_string(),
            "64:80:04:8E:3F:5F:5F:D9:B7:6F:1B:6F:12:13:0D:56"
        );
        assert_eq!(
            panel_id_of_common_name("reprojection_error.stream.rerun-structure_from_motion.rerun")
                .to_string(),
            "A9:E9:8E:39:6D:71:56:B0:AA:1C:AC:30:87:74:AF:76"
        );
        assert_eq!(
            panel_id_of_common_name("timeline.panel-stream.rerun-structure_from_motion.rerun")
                .to_string(),
            "5B:E0:DF:04:8C:D5:57:E8:BE:C8:1A:9B:50:B8:FE:83"
        );
    }
}
