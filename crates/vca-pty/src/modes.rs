//! Lightweight tracking of terminal input modes switched by the application.
//!
//! Only the DEC private modes that change how *input* must be encoded are
//! tracked: bracketed paste (`?2004`), application cursor keys (`?1`) and
//! the alternate screen (`?47`, `?1047`, `?1049`). The scanner keeps its
//! state across calls, so sequences split over chunk boundaries are handled.

/// Input-relevant terminal modes currently enabled by the application.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TerminalModes {
    /// `ESC[?2004h`: pasted text must be wrapped in `ESC[200~ ... ESC[201~`.
    pub bracketed_paste: bool,
    /// `ESC[?1h` (DECCKM): arrow keys are sent as `ESC O x` instead of `ESC [ x`.
    pub application_cursor: bool,
    /// The application switched to the alternate screen (a full-screen TUI).
    pub alternate_screen: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Ground,
    Escape,
    CsiEntry,
    PrivateParams,
}

/// Incremental scanner that updates [`TerminalModes`] from output bytes.
#[derive(Debug, Clone)]
pub struct ModeTracker {
    modes: TerminalModes,
    state: State,
    params: Vec<u8>,
}

const MAX_PARAM_LEN: usize = 64;

impl Default for ModeTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl ModeTracker {
    pub fn new() -> Self {
        Self {
            modes: TerminalModes::default(),
            state: State::Ground,
            params: Vec::new(),
        }
    }

    pub fn modes(&self) -> TerminalModes {
        self.modes
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.step(b);
        }
    }

    fn step(&mut self, b: u8) {
        self.state = match (self.state, b) {
            (_, 0x1b) => State::Escape,
            (State::Ground, _) => State::Ground,
            (State::Escape, b'[') => State::CsiEntry,
            (State::Escape, b'c') => {
                // RIS: full reset.
                self.modes = TerminalModes::default();
                State::Ground
            }
            (State::Escape, _) => State::Ground,
            (State::CsiEntry, b'?') => {
                self.params.clear();
                State::PrivateParams
            }
            (State::CsiEntry, _) => State::Ground,
            (State::PrivateParams, b'0'..=b'9' | b';') => {
                if self.params.len() < MAX_PARAM_LEN {
                    self.params.push(b);
                    State::PrivateParams
                } else {
                    State::Ground
                }
            }
            (State::PrivateParams, b'h') => {
                self.apply(true);
                State::Ground
            }
            (State::PrivateParams, b'l') => {
                self.apply(false);
                State::Ground
            }
            (State::PrivateParams, _) => State::Ground,
        };
    }

    fn apply(&mut self, enable: bool) {
        for param in self.params.split(|&b| b == b';') {
            match param {
                b"2004" => self.modes.bracketed_paste = enable,
                b"1" => self.modes.application_cursor = enable,
                b"47" | b"1047" | b"1049" => self.modes.alternate_screen = enable,
                _ => {}
            }
        }
        self.params.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tracks_bracketed_paste() {
        let mut t = ModeTracker::new();
        t.feed(b"hello \x1b[?2004h world");
        assert!(t.modes().bracketed_paste);
        t.feed(b"\x1b[?2004l");
        assert!(!t.modes().bracketed_paste);
    }

    #[test]
    fn handles_split_sequences() {
        let mut t = ModeTracker::new();
        for b in b"\x1b[?20" {
            t.feed(&[*b]);
        }
        t.feed(b"04h");
        assert!(t.modes().bracketed_paste);
    }

    #[test]
    fn handles_combined_params() {
        let mut t = ModeTracker::new();
        t.feed(b"\x1b[?1;1049;2004h");
        assert_eq!(
            t.modes(),
            TerminalModes {
                bracketed_paste: true,
                application_cursor: true,
                alternate_screen: true
            }
        );
        t.feed(b"\x1bc");
        assert_eq!(t.modes(), TerminalModes::default());
    }

    #[test]
    fn ignores_non_private_and_unknown_sequences() {
        let mut t = ModeTracker::new();
        t.feed(b"\x1b[2004h\x1b[?25h\x1b[?12004h\x1b[31m");
        assert_eq!(t.modes(), TerminalModes::default());
    }

    #[test]
    fn escape_restarts_sequence() {
        let mut t = ModeTracker::new();
        t.feed(b"\x1b[?20\x1b[?2004h");
        assert!(t.modes().bracketed_paste);
    }
}
