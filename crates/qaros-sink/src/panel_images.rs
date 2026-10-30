//! The latest image per stream panel, written by the UI thread (the capture, later the
//! timeline) and read by the QAROS thread, which copies it into the panel's video sender.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

/// The key of the timeline panel's image; it holds a dot, which no view name holds.
pub const TIMELINE_IMAGE_KEY: &str = "timeline.panel";

const BYTES_PER_PIXEL: usize = 4;

#[derive(Clone, Debug)]
pub struct PanelImage {
    /// Raised only when the content or the size changed; drives shows.
    pub sequence: u64,
    /// Set on every publish; drives settling, so a view that never changes still settles.
    pub published_at: Instant,
    pub width: u32,
    pub height: u32,
    /// BGRA, alpha 255, rows packed.
    pub bgra8: Arc<Vec<u8>>,
}

#[derive(Clone, Default)]
pub struct PanelImages {
    images: Arc<Mutex<BTreeMap<String, PanelImage>>>,
}

impl PanelImages {
    /// Swizzles to BGRA with alpha 255, sets `published_at`, and raises the sequence only when
    /// the pixels or the size changed. The change test compares the pixels with the stored
    /// image, which is exact and no slower than hashing them. Returns the image's sequence.
    pub fn publish_rgba8(
        &self,
        key: &str,
        width: u32,
        height: u32,
        mut rgba8: Vec<u8>,
    ) -> Result<u64, String> {
        let expected_length = width as usize * height as usize * BYTES_PER_PIXEL;
        if width == 0 || height == 0 || rgba8.len() != expected_length {
            return Err(format!(
                "{} bytes for a {width}x{height} image",
                rgba8.len()
            ));
        }
        for pixel in rgba8.chunks_exact_mut(BYTES_PER_PIXEL) {
            pixel.swap(0, 2);
            pixel[3] = u8::MAX;
        }
        let bgra8 = rgba8;
        let published_at = Instant::now();
        let previous = self.latest(key);
        let unchanged = previous.as_ref().is_some_and(|previous| {
            previous.width == width && previous.height == height && *previous.bgra8 == bgra8
        });
        let compared_sequence = previous.map(|previous| previous.sequence);
        let mut images = self.images.lock().unwrap_or_else(PoisonError::into_inner);
        // The pixels were compared outside the lock, so the QAROS thread never waits on a
        // comparison; a publish of the same key in between makes this one count as a change.
        if unchanged
            && let Some(stored) = images.get_mut(key)
            && Some(stored.sequence) == compared_sequence
        {
            stored.published_at = published_at;
            return Ok(stored.sequence);
        }
        let sequence = images.get(key).map_or(0, |stored| stored.sequence + 1);
        images.insert(
            key.to_owned(),
            PanelImage {
                sequence,
                published_at,
                width,
                height,
                bgra8: Arc::new(bgra8),
            },
        );
        Ok(sequence)
    }

    pub fn latest(&self, key: &str) -> Option<PanelImage> {
        self.images
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(key)
            .cloned()
    }

    /// The published views, without the timeline.
    pub fn view_names(&self) -> Vec<String> {
        self.images
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .keys()
            .filter(|key| key.as_str() != TIMELINE_IMAGE_KEY)
            .cloned()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn two_pixels(first_red: u8) -> Vec<u8> {
        vec![first_red, 20, 30, 0, 40, 50, 60, 128]
    }

    #[test]
    fn an_identical_publish_keeps_the_sequence_and_moves_the_publish_time() {
        let images = PanelImages::default();
        assert_eq!(images.publish_rgba8("camera", 2, 1, two_pixels(10)), Ok(0));
        let first = images.latest("camera").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(2));
        assert_eq!(images.publish_rgba8("camera", 2, 1, two_pixels(10)), Ok(0));
        let second = images.latest("camera").unwrap();
        assert_eq!(second.sequence, 0);
        assert!(second.published_at > first.published_at);
    }

    #[test]
    fn a_changed_pixel_or_size_raises_the_sequence() {
        let images = PanelImages::default();
        images
            .publish_rgba8("camera", 2, 1, two_pixels(10))
            .unwrap();
        assert_eq!(images.publish_rgba8("camera", 2, 1, two_pixels(11)), Ok(1));
        assert_eq!(images.publish_rgba8("camera", 1, 2, two_pixels(11)), Ok(2));
        assert_eq!(images.latest("camera").unwrap().height, 2);
    }

    #[test]
    fn stored_pixels_are_bgra_with_full_alpha() {
        let images = PanelImages::default();
        images
            .publish_rgba8("camera", 2, 1, two_pixels(10))
            .unwrap();
        assert_eq!(
            *images.latest("camera").unwrap().bgra8,
            vec![30, 20, 10, 255, 60, 50, 40, 255]
        );
    }

    #[test]
    fn a_wrong_length_is_refused_and_the_timeline_is_no_view() {
        let images = PanelImages::default();
        assert!(
            images
                .publish_rgba8("camera", 3, 1, two_pixels(10))
                .is_err()
        );
        assert!(images.publish_rgba8("camera", 0, 0, Vec::new()).is_err());
        images
            .publish_rgba8("camera", 2, 1, two_pixels(10))
            .unwrap();
        images
            .publish_rgba8(TIMELINE_IMAGE_KEY, 2, 1, two_pixels(10))
            .unwrap();
        assert_eq!(images.view_names(), ["camera"]);
    }
}
