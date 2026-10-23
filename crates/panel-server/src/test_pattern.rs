/// Produces frames shaped like a plot panel, so the server and page can be exercised without
/// Rerun: a dark background, a grid, a sine trace that scrolls with the frame index, and a
/// progress bar along the bottom edge.
#[derive(Clone, Copy, Debug)]
pub struct TestPattern {
    pub width: u32,
    pub height: u32,
}

const BACKGROUND: [u8; 4] = [18, 18, 22, 255];
const GRID: [u8; 4] = [48, 48, 56, 255];
const TRACE: [u8; 4] = [80, 200, 255, 255];
const PROGRESS: [u8; 4] = [255, 160, 40, 255];

impl TestPattern {
    pub fn frame_rgba8(self, frame_index: u64) -> Vec<u8> {
        let width = self.width as usize;
        let height = self.height as usize;
        let mut pixels = BACKGROUND.repeat(width * height);
        let mut paint = |column: usize, row: usize, color: [u8; 4]| {
            if column < width && row < height {
                let offset = (row * width + column) * 4;
                pixels[offset..offset + 4].copy_from_slice(&color);
            }
        };

        for row in (0..height).step_by(32) {
            for column in 0..width {
                paint(column, row, GRID);
            }
        }
        for column in (0..width).step_by(32) {
            for row in 0..height {
                paint(column, row, GRID);
            }
        }

        #[allow(clippy::cast_precision_loss)]
        let phase = frame_index as f32 * 0.1;
        #[allow(clippy::cast_precision_loss)]
        let amplitude = height as f32 * 0.35;
        #[allow(clippy::cast_precision_loss)]
        let middle = height as f32 * 0.5;
        for column in 0..width {
            #[allow(clippy::cast_precision_loss)]
            let value = (column as f32 * 0.05 + phase).sin();
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let row = (middle - value * amplitude).round().max(0.0) as usize;
            for thickness in 0..3 {
                paint(column, row + thickness, TRACE);
            }
        }

        let progress_columns = usize::try_from(frame_index % 120).unwrap_or(0) * width / 120;
        for column in 0..progress_columns {
            for row in height.saturating_sub(6)..height {
                paint(column, row, PROGRESS);
            }
        }
        pixels
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pixel(frame: &[u8], width: u32, column: u32, row: u32) -> [u8; 4] {
        let offset = ((row * width + column) * 4) as usize;
        frame[offset..offset + 4].try_into().unwrap()
    }

    #[test]
    fn frames_have_the_requested_size_and_animate() {
        let pattern = TestPattern {
            width: 320,
            height: 180,
        };
        let first = pattern.frame_rgba8(0);
        let later = pattern.frame_rgba8(30);
        assert_eq!(first.len(), 320 * 180 * 4);
        assert_ne!(first, later);
    }

    #[test]
    fn progress_bar_grows_with_the_frame_index() {
        let pattern = TestPattern {
            width: 240,
            height: 100,
        };
        let frame = pattern.frame_rgba8(60);
        assert_eq!(pixel(&frame, 240, 100, 98), PROGRESS);
        assert_ne!(pixel(&frame, 240, 200, 98), PROGRESS);
    }

    #[test]
    fn trace_starts_at_the_middle_on_frame_zero() {
        let pattern = TestPattern {
            width: 64,
            height: 101,
        };
        let frame = pattern.frame_rgba8(0);
        assert_eq!(pixel(&frame, 64, 1, 50), TRACE);
    }
}
