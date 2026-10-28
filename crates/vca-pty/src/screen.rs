//! Optional in-memory terminal emulator used for previews and attach redraws.

use std::num::NonZeroU16;

use crate::session::PtySize;

/// Options for the terminal state tracker.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ScreenOptions {
    /// Number of scrollback rows kept by the emulator (0 = none).
    pub scrollback: usize,
}

/// Plain-text view of the current terminal screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreenSnapshot {
    pub size: PtySize,
    /// Visible rows, with trailing spaces removed.
    pub lines: Vec<String>,
    /// Cursor position as (row, col), zero based.
    pub cursor: (u16, u16),
    pub cursor_visible: bool,
    pub alternate_screen: bool,
    /// Last window title set by the application (OSC 0/2), if any.
    pub title: String,
}

impl ScreenSnapshot {
    /// All visible rows joined with `\n`, without trailing blank rows.
    pub fn text(&self) -> String {
        let end = self
            .lines
            .iter()
            .rposition(|l| !l.is_empty())
            .map_or(0, |i| i + 1);
        self.lines[..end].join("\n")
    }

    /// The last `n` non-empty rows, useful for compact session cards.
    pub fn tail(&self, n: usize) -> Vec<String> {
        let mut rows: Vec<String> = self
            .lines
            .iter()
            .rev()
            .filter(|l| !l.trim().is_empty())
            .take(n)
            .cloned()
            .collect();
        rows.reverse();
        rows
    }
}

#[derive(Debug, Default)]
struct TitleTracker {
    title: String,
}

impl vt100::Callbacks for TitleTracker {
    fn set_window_title(&mut self, _: &mut vt100::Screen, title: &[u8]) {
        self.title = String::from_utf8_lossy(title).into_owned();
    }
}

/// vt100 emulator fed with the session's output.
pub(crate) struct Screen {
    parser: vt100::Parser<TitleTracker>,
}

fn nz(v: u16) -> NonZeroU16 {
    NonZeroU16::new(v).unwrap_or(NonZeroU16::MIN)
}

impl Screen {
    pub(crate) fn new(size: PtySize, opts: ScreenOptions) -> Self {
        Self {
            parser: vt100::Parser::new_with_callbacks(
                nz(size.rows),
                nz(size.cols),
                opts.scrollback,
                TitleTracker::default(),
            ),
        }
    }

    pub(crate) fn process(&mut self, bytes: &[u8]) {
        self.parser.process(bytes);
    }

    pub(crate) fn resize(&mut self, size: PtySize) {
        self.parser.set_size(nz(size.rows), nz(size.cols));
    }

    pub(crate) fn snapshot(&self) -> ScreenSnapshot {
        let screen = self.parser.screen();
        let (rows, cols) = screen.size();
        ScreenSnapshot {
            size: PtySize {
                rows: rows.get(),
                cols: cols.get(),
            },
            lines: screen
                .rows(0, cols.get())
                .map(|r| r.trim_end().to_owned())
                .collect(),
            cursor: screen.cursor_position(),
            cursor_visible: !screen.hide_cursor(),
            alternate_screen: screen.alternate_screen(),
            title: self.parser.callbacks().title.clone(),
        }
    }

    /// Escape sequences that redraw the current screen on a fresh terminal,
    /// including input modes and the alternate screen.
    pub(crate) fn redraw(&self) -> Vec<u8> {
        let screen = self.parser.screen();
        let mut out = Vec::new();
        if screen.alternate_screen() {
            out.extend_from_slice(b"\x1b[?1049h");
        }
        out.extend_from_slice(screen.state_formatted().as_bytes());
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_text_cursor_and_title() {
        let mut s = Screen::new(PtySize { cols: 20, rows: 5 }, ScreenOptions::default());
        s.process(b"\x1b]2;my title\x07hello\r\nworld  ");
        let snap = s.snapshot();
        assert_eq!(snap.lines.len(), 5);
        assert_eq!(snap.text(), "hello\nworld");
        assert_eq!(snap.cursor, (1, 7));
        assert_eq!(snap.title, "my title");
        assert_eq!(snap.tail(1), vec!["world".to_owned()]);
        assert!(snap.cursor_visible);
    }

    #[test]
    fn redraw_reproduces_screen() {
        let mut s = Screen::new(PtySize { cols: 20, rows: 5 }, ScreenOptions::default());
        s.process(b"\x1b[?1049h\x1b[31mred\x1b[0m text");
        let redraw = s.redraw();
        let mut copy = Screen::new(PtySize { cols: 20, rows: 5 }, ScreenOptions::default());
        copy.process(&redraw);
        assert_eq!(copy.snapshot().text(), "red text");
        assert!(copy.snapshot().alternate_screen);
    }

    #[test]
    fn resize_changes_size() {
        let mut s = Screen::new(PtySize { cols: 20, rows: 5 }, ScreenOptions::default());
        s.resize(PtySize { cols: 40, rows: 10 });
        assert_eq!(s.snapshot().size, PtySize { cols: 40, rows: 10 });
    }
}
