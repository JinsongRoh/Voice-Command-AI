//! The PTY's size, carried in the terminal output for web viewers.
//!
//! Programs lay out their screen for the PTY's size; Claude Code's Ink UI
//! and ConPTY redraw with relative cursor moves and rely on the terminal
//! wrapping at exactly that width. A viewer whose terminal has other
//! columns or rows draws later lines over earlier ones. Viewers therefore
//! render at the PTY's size (and scale the font to their pane); they never
//! resize the PTY themselves.
//!
//! The size travels in band, as a private OSC sequence in front of an
//! output chunk (`ESC ] 7377 ; <cols> ; <rows> BEL`), so it stays in order
//! with the output it applies to, also in replayed history. Terminals and
//! ANSI strippers ignore unknown OSC sequences. It is repeated now and then
//! so that a viewer that only replays the newest page still learns it.

use std::time::{Duration, Instant};

use vca_pty::PtySize;

/// OSC number of the size marker (the UI's `terminalFit.ts` matches it).
pub const SIZE_OSC: u16 = 7377;
/// Output chunks after which the size is repeated.
const REPEAT_CHUNKS: u32 = 32;
/// Time after which the size is repeated.
const REPEAT_AFTER: Duration = Duration::from_secs(5);

/// The marker for `size`.
pub fn size_marker(size: PtySize) -> Vec<u8> {
    format!("\x1b]{SIZE_OSC};{};{}\x07", size.cols, size.rows).into_bytes()
}

/// Decides which output chunks of one PTY carry the size marker: the
/// first, every one after a resize, and one now and then.
#[derive(Debug, Default)]
pub struct SizeMarker {
    last: Option<(PtySize, Instant)>,
    chunks: u32,
}

impl SizeMarker {
    /// `data` with the marker in front when it is due.
    pub fn prefix(&mut self, size: PtySize, now: Instant, data: &[u8]) -> Vec<u8> {
        let due = match self.last {
            None => true,
            Some((last, at)) => {
                last != size
                    || self.chunks >= REPEAT_CHUNKS
                    || now.saturating_duration_since(at) >= REPEAT_AFTER
            }
        };
        if !due {
            self.chunks += 1;
            return data.to_vec();
        }
        self.last = Some((size, now));
        self.chunks = 1;
        let mut out = size_marker(size);
        out.extend_from_slice(data);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIZE: PtySize = PtySize {
        cols: 120,
        rows: 40,
    };

    #[test]
    fn marker_format() {
        assert_eq!(size_marker(SIZE), b"\x1b]7377;120;40\x07");
    }

    #[test]
    fn first_chunk_carries_the_size() {
        let mut m = SizeMarker::default();
        let now = Instant::now();
        assert_eq!(m.prefix(SIZE, now, b"hi"), b"\x1b]7377;120;40\x07hi");
        assert_eq!(m.prefix(SIZE, now, b"there"), b"there");
    }

    #[test]
    fn a_resize_is_marked_on_the_next_chunk() {
        let mut m = SizeMarker::default();
        let now = Instant::now();
        m.prefix(SIZE, now, b"a");
        let narrow = PtySize { cols: 80, rows: 24 };
        assert_eq!(m.prefix(narrow, now, b"b"), b"\x1b]7377;80;24\x07b");
        assert_eq!(m.prefix(narrow, now, b"c"), b"c");
    }

    #[test]
    fn the_size_is_repeated_after_chunks_and_time() {
        let mut m = SizeMarker::default();
        let now = Instant::now();
        m.prefix(SIZE, now, b"x");
        for _ in 1..REPEAT_CHUNKS {
            assert_eq!(m.prefix(SIZE, now, b"x"), b"x");
        }
        assert!(m.prefix(SIZE, now, b"x").starts_with(b"\x1b]"));
        assert_eq!(m.prefix(SIZE, now, b"x"), b"x");
        let later = now + REPEAT_AFTER;
        assert!(m.prefix(SIZE, later, b"x").starts_with(b"\x1b]"));
    }
}
