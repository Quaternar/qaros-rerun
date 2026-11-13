//! `--frame-stamp`: draws the user, frame id and eye into each eye image, so the pairing of
//! image and pose can be checked in the QAROS visualizer, in the image sink's PNGs, and by
//! `qaros-stub`, which decodes the strip of every shown frame.
//!
//! Layout at the eye's top-left corner, with `cell` = eye width / 64 (at least 1 pixel):
//! - rows `0..2 cell`: a 32-cell strip, most significant bit first, white for 1 and black for
//!   0, holding `0xA` in the top four bits, the eye in bit 27 (1 = right) and the low 27 bits
//!   of the frame id; `qaros-stub` reads the same format (`qar-streaming-rust/crates/qaros-stub/src/frame_stamp.rs`
//!   in the QAROS repository);
//! - from row `3 cell`: `U<user> F<frame> L|R` in a 5x7 pixel font scaled by `cell / 2`,
//!   white on a black box.
//!
//! The stamp is opaque and its depth is set to a fixed distance, so the mixer keeps it.

use crate::{EyeImage, FrameId, UserId};

const MAGIC: u32 = 0xA;
const FRAME_ID_BITS: u32 = 27;
const WORD_BITS: u32 = 32;
const GLYPH_WIDTH: u32 = 5;
const GLYPH_HEIGHT: u32 = 7;
const GLYPH_ADVANCE: u32 = GLYPH_WIDTH + 1;
const WHITE: [u8; 4] = [255, 255, 255, 255];
const BLACK: [u8; 4] = [0, 0, 0, 255];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameStamp {
    pub user: UserId,
    pub frame_id: FrameId,
    pub eye_index: usize,
}

impl FrameStamp {
    pub fn text(&self) -> String {
        let eye = if self.eye_index == 0 { 'L' } else { 'R' };
        format!("U{} F{} {eye}", self.user.0, self.frame_id)
    }

    fn word(&self) -> u32 {
        let frame_bits = u32::try_from(self.frame_id & ((1 << FRAME_ID_BITS) - 1)).unwrap_or(0);
        (MAGIC << 28) | (u32::from(self.eye_index != 0) << FRAME_ID_BITS) | frame_bits
    }
}

/// What the strip says: the low 27 bits of the frame id and the eye.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StripReading {
    pub frame_id_bits: u32,
    pub right_eye: bool,
}

impl StripReading {
    pub fn matches(&self, frame_id: FrameId, eye_index: usize) -> bool {
        u64::from(self.frame_id_bits) == frame_id & ((1 << FRAME_ID_BITS) - 1)
            && self.right_eye == (eye_index != 0)
    }
}

fn cell_size(width: u32) -> u32 {
    (width / 64).max(1)
}

fn glyph_scale(width: u32) -> u32 {
    (cell_size(width) / 2).max(1)
}

/// The rows from the top that the stamp of an eye `width` pixels wide can touch: the strip and
/// the text below it. A sink that draws the stamp on its own (the GPU path) stamps an image of
/// this many rows and lays it over the top of the eye.
pub fn stamp_rows(width: u32) -> u32 {
    3 * cell_size(width) + (GLYPH_HEIGHT + 1) * glyph_scale(width)
}

/// Rows of 5 bits, most significant bit leftmost.
fn glyph(character: char) -> [u8; 7] {
    match character {
        '0' => [0x0e, 0x11, 0x13, 0x15, 0x19, 0x11, 0x0e],
        '1' => [0x04, 0x0c, 0x04, 0x04, 0x04, 0x04, 0x0e],
        '2' => [0x0e, 0x11, 0x01, 0x02, 0x04, 0x08, 0x1f],
        '3' => [0x1e, 0x01, 0x01, 0x0e, 0x01, 0x01, 0x1e],
        '4' => [0x02, 0x06, 0x0a, 0x12, 0x1f, 0x02, 0x02],
        '5' => [0x1f, 0x10, 0x1e, 0x01, 0x01, 0x11, 0x0e],
        '6' => [0x06, 0x08, 0x10, 0x1e, 0x11, 0x11, 0x0e],
        '7' => [0x1f, 0x01, 0x02, 0x04, 0x08, 0x08, 0x08],
        '8' => [0x0e, 0x11, 0x11, 0x0e, 0x11, 0x11, 0x0e],
        '9' => [0x0e, 0x11, 0x11, 0x0f, 0x01, 0x02, 0x0c],
        'U' => [0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0e],
        'F' => [0x1f, 0x10, 0x10, 0x1e, 0x10, 0x10, 0x10],
        'L' => [0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x1f],
        'R' => [0x1e, 0x11, 0x11, 0x1e, 0x14, 0x12, 0x11],
        _ => [0; 7],
    }
}

struct Canvas<'image> {
    image: &'image mut EyeImage,
    depth: f32,
}

impl Canvas<'_> {
    fn fill(
        &mut self,
        x_range: std::ops::Range<u32>,
        y_range: std::ops::Range<u32>,
        color: [u8; 4],
    ) {
        let [width, height] = self.image.resolution_in_pixel;
        for y in y_range.start..y_range.end.min(height) {
            for x in x_range.start..x_range.end.min(width) {
                let pixel = (y * width + x) as usize;
                self.image.color_rgba8[pixel * 4..pixel * 4 + 4].copy_from_slice(&color);
                self.image.depth[pixel] = self.depth;
            }
        }
    }
}

/// Draws the stamp into the eye image. `stamp_depth` is written for every stamped pixel, in
/// the sink's depth encoding.
pub fn stamp_eye(image: &mut EyeImage, stamp: FrameStamp, stamp_depth: f32) {
    if image.validate().is_err() {
        return;
    }
    let width = image.resolution_in_pixel[0];
    let cell = cell_size(width);
    let scale = glyph_scale(width);
    let mut canvas = Canvas {
        image,
        depth: stamp_depth,
    };

    let word = stamp.word();
    for bit in 0..WORD_BITS {
        let color = if (word >> (WORD_BITS - 1 - bit)) & 1 == 1 {
            WHITE
        } else {
            BLACK
        };
        canvas.fill(bit * cell..(bit + 1) * cell, 0..2 * cell, color);
    }

    let text = stamp.text();
    let text_top = 3 * cell;
    let glyph_count = u32::try_from(text.chars().count()).unwrap_or(0);
    canvas.fill(
        0..(glyph_count * GLYPH_ADVANCE + 1) * scale,
        2 * cell..text_top + (GLYPH_HEIGHT + 1) * scale,
        BLACK,
    );
    for (slot, character) in (0_u32..).zip(text.chars()) {
        let left = (slot * GLYPH_ADVANCE + 1) * scale;
        for (row, bits) in (0_u32..).zip(glyph(character)) {
            for column in 0..GLYPH_WIDTH {
                if (bits >> (GLYPH_WIDTH - 1 - column)) & 1 == 1 {
                    let x = left + column * scale;
                    let y = text_top + row * scale;
                    canvas.fill(x..x + scale, y..y + scale, WHITE);
                }
            }
        }
    }
}

fn is_white(rgba8: &[u8], width: u32, x: u32, y: u32) -> Option<bool> {
    let pixel = (y * width + x) as usize;
    rgba8.get(pixel * 4).map(|&red| red >= 128)
}

/// Reads the strip back from tightly packed RGBA rows; `None` without a stamp.
pub fn read_strip(rgba8: &[u8], resolution_in_pixel: [u32; 2]) -> Option<StripReading> {
    let [width, height] = resolution_in_pixel;
    let cell = cell_size(width);
    if width < WORD_BITS * cell || height < 2 * cell {
        return None;
    }
    let mut word = 0_u32;
    for bit in 0..WORD_BITS {
        let white = is_white(rgba8, width, bit * cell + cell / 2, cell)?;
        word = (word << 1) | u32::from(white);
    }
    (word >> 28 == MAGIC).then_some(StripReading {
        frame_id_bits: word & ((1 << FRAME_ID_BITS) - 1),
        right_eye: (word >> FRAME_ID_BITS) & 1 == 1,
    })
}

/// Reads the human-readable line back by matching each glyph cell against the font; used to
/// check that what a person sees in the visualizer says the same as the strip.
pub fn read_text(rgba8: &[u8], resolution_in_pixel: [u32; 2]) -> String {
    const ALPHABET: &str = "0123456789UFLR";
    let [width, height] = resolution_in_pixel;
    let cell = cell_size(width);
    let scale = glyph_scale(width);
    let text_top = 3 * cell;
    let mut text = String::new();
    for slot in 0_u32.. {
        let left = (slot * GLYPH_ADVANCE + 1) * scale;
        if left + GLYPH_WIDTH * scale > width || text_top + GLYPH_HEIGHT * scale > height {
            break;
        }
        let mut rows = [0_u8; 7];
        for (row, bits) in (0_u32..).zip(rows.iter_mut()) {
            for column in 0..GLYPH_WIDTH {
                let x = left + column * scale + scale / 2;
                let y = text_top + row * scale + scale / 2;
                if is_white(rgba8, width, x, y) == Some(true) {
                    *bits |= 1 << (GLYPH_WIDTH - 1 - column);
                }
            }
        }
        match ALPHABET.chars().find(|&character| glyph(character) == rows) {
            Some(character) => text.push(character),
            None if rows == [0; 7] => text.push(' '),
            None => break,
        }
    }
    text.trim_end().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gray_image(resolution_in_pixel: [u32; 2]) -> EyeImage {
        let pixel_count = (resolution_in_pixel[0] * resolution_in_pixel[1]) as usize;
        EyeImage {
            resolution_in_pixel,
            color_rgba8: [90, 90, 90, 255].repeat(pixel_count),
            depth: vec![0.5; pixel_count],
        }
    }

    #[test]
    fn strip_and_text_encode_the_frame_and_eye() {
        for (resolution, frame_id, eye_index) in [
            ([64, 64], 0, 0),
            ([64, 64], 77, 1),
            ([960, 1000], 123_456, 1),
            ([1832, 1920], (1 << 27) + 42, 0),
        ] {
            let mut image = gray_image(resolution);
            let stamp = FrameStamp {
                user: UserId(2),
                frame_id,
                eye_index,
            };
            stamp_eye(&mut image, stamp, 0.25);
            let strip = read_strip(&image.color_rgba8, resolution).unwrap();
            assert!(
                strip.matches(frame_id, eye_index),
                "{resolution:?} {strip:?}"
            );
            assert!(!strip.matches(frame_id + 1, eye_index));
            if resolution[0] >= 128 {
                assert_eq!(
                    read_text(&image.color_rgba8, resolution),
                    stamp.text(),
                    "{resolution:?}"
                );
            }
        }
    }

    #[test]
    fn the_stamp_stays_within_its_rows() {
        for resolution in [[64, 64], [960, 1000], [1832, 1920]] {
            let mut image = gray_image(resolution);
            let untouched = image.clone();
            stamp_eye(
                &mut image,
                FrameStamp {
                    user: UserId(12),
                    frame_id: 88_888_888,
                    eye_index: 1,
                },
                0.25,
            );
            let row_bytes = resolution[0] as usize * 4;
            let rows = stamp_rows(resolution[0]) as usize;
            assert_eq!(
                image.color_rgba8[rows * row_bytes..],
                untouched.color_rgba8[rows * row_bytes..],
                "{resolution:?}: nothing below row {rows}"
            );
            assert_ne!(
                image.color_rgba8[(rows - 1) * row_bytes..rows * row_bytes],
                untouched.color_rgba8[(rows - 1) * row_bytes..rows * row_bytes],
                "{resolution:?}: the last stamp row is used"
            );
        }
    }

    #[test]
    fn small_eyes_keep_the_strip_and_clip_the_text() {
        let mut image = gray_image([64, 64]);
        stamp_eye(
            &mut image,
            FrameStamp {
                user: UserId(1),
                frame_id: 7,
                eye_index: 0,
            },
            0.25,
        );
        assert_eq!(read_text(&image.color_rgba8, [64, 64]), "U1 F7 L");
    }

    #[test]
    #[allow(clippy::float_cmp)]
    fn only_the_stamp_corner_changes_and_it_gets_the_stamp_depth() {
        let resolution = [960, 1000];
        let mut image = gray_image(resolution);
        stamp_eye(
            &mut image,
            FrameStamp {
                user: UserId(1),
                frame_id: 5,
                eye_index: 0,
            },
            0.25,
        );
        let changed: Vec<usize> = image
            .depth
            .iter()
            .enumerate()
            .filter(|(_, depth)| **depth != 0.5)
            .map(|(pixel, _)| pixel)
            .collect();
        let (max_x, max_y) = changed.iter().fold((0, 0), |(max_x, max_y), pixel| {
            (max_x.max(pixel % 960), max_y.max(pixel / 960))
        });
        assert!(max_x < 480 && max_y < 120, "{max_x} {max_y}");
        assert!(changed.iter().all(|&pixel| image.depth[pixel] == 0.25));
        let far_corner = (999 * 960 + 959) * 4;
        assert_eq!(
            image.color_rgba8[far_corner..far_corner + 4],
            [90, 90, 90, 255]
        );
    }

    #[test]
    fn unstamped_images_read_as_none() {
        let image = gray_image([960, 1000]);
        assert_eq!(read_strip(&image.color_rgba8, [960, 1000]), None);
        assert_eq!(read_text(&image.color_rgba8, [960, 1000]), "");
    }
}
