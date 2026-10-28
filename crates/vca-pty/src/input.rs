//! Conversion of [`vca_core::Input`] into the bytes a terminal would send.

use vca_core::{ControlKey, Input};

use crate::error::PtyError;
use crate::modes::TerminalModes;

/// Byte sequence for Enter. Terminals send carriage return; the line
/// discipline (or the raw-mode application) treats it as "submit".
pub const ENTER: &[u8] = b"\r";
const PASTE_START: &[u8] = b"\x1b[200~";
const PASTE_END: &[u8] = b"\x1b[201~";

/// Input encoded for a PTY, split so that the caller can pause between the
/// text and the Enter key. Many TUIs treat text and `\r` arriving in one
/// read as a paste and would insert a newline instead of submitting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodedInput {
    /// Bytes to write first (may be empty).
    pub body: Vec<u8>,
    /// Whether Enter ([`ENTER`]) must be written after `body`.
    pub submit: bool,
}

/// Bytes for a single control key, honoring application cursor mode.
pub fn encode_key(key: ControlKey, modes: TerminalModes) -> &'static [u8] {
    let app = modes.application_cursor;
    match key {
        ControlKey::Enter => ENTER,
        ControlKey::Escape => b"\x1b",
        ControlKey::Interrupt => b"\x03",
        ControlKey::Tab => b"\t",
        ControlKey::Backspace => b"\x7f",
        ControlKey::Up if app => b"\x1bOA",
        ControlKey::Down if app => b"\x1bOB",
        ControlKey::Right if app => b"\x1bOC",
        ControlKey::Left if app => b"\x1bOD",
        ControlKey::Up => b"\x1b[A",
        ControlKey::Down => b"\x1b[B",
        ControlKey::Right => b"\x1b[C",
        ControlKey::Left => b"\x1b[D",
    }
}

/// Encodes text as typed or pasted input.
///
/// Multi-line text is wrapped in bracketed-paste markers when the
/// application enabled bracketed paste, so that embedded newlines do not
/// submit early. Newlines inside a paste are sent as carriage returns, as
/// real terminals do (Windows ConPTY drops bare line feeds). Any paste
/// markers inside the text are removed first, so the text cannot terminate
/// the paste and inject keystrokes.
pub fn encode_text(text: &str, modes: TerminalModes) -> Vec<u8> {
    let multi_line = text.contains(['\n', '\r']);
    if modes.bracketed_paste && multi_line {
        let cleaned = strip_paste_markers(&paste_newlines(text));
        let mut out = Vec::with_capacity(cleaned.len() + PASTE_START.len() + PASTE_END.len());
        out.extend_from_slice(PASTE_START);
        out.extend_from_slice(&cleaned);
        out.extend_from_slice(PASTE_END);
        out
    } else {
        text.as_bytes().to_vec()
    }
}

/// Encodes an [`Input`]. [`Input::Choose`] needs agent-specific knowledge
/// and is rejected with [`PtyError::Unsupported`]; adapters translate it into
/// text or keys first.
pub fn encode_input(input: &Input, modes: TerminalModes) -> Result<EncodedInput, PtyError> {
    match input {
        Input::Text { text, submit } => Ok(EncodedInput {
            body: encode_text(text, modes),
            submit: *submit,
        }),
        Input::Key { key } => Ok(EncodedInput {
            body: encode_key(*key, modes).to_vec(),
            submit: false,
        }),
        Input::Choose { .. } => Err(PtyError::Unsupported(
            "Input::Choose must be translated by the agent adapter".into(),
        )),
    }
}

/// Converts `\r\n` and `\n` to `\r`.
fn paste_newlines(text: &str) -> Vec<u8> {
    text.replace("\r\n", "\r").replace('\n', "\r").into_bytes()
}

/// Removes paste markers until none are left, so that removing one cannot
/// join its neighbors into a new marker.
fn strip_paste_markers(bytes: &[u8]) -> Vec<u8> {
    let mut current = strip_paste_markers_once(bytes);
    loop {
        let next = strip_paste_markers_once(&current);
        if next.len() == current.len() {
            return next;
        }
        current = next;
    }
}

fn strip_paste_markers_once(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let rest = &bytes[i..];
        if rest.starts_with(PASTE_START) || rest.starts_with(PASTE_END) {
            i += PASTE_START.len();
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLAIN: TerminalModes = TerminalModes {
        bracketed_paste: false,
        application_cursor: false,
        alternate_screen: false,
    };
    const PASTE: TerminalModes = TerminalModes {
        bracketed_paste: true,
        application_cursor: false,
        alternate_screen: false,
    };

    #[test]
    fn control_keys() {
        assert_eq!(encode_key(ControlKey::Enter, PLAIN), b"\r");
        assert_eq!(encode_key(ControlKey::Interrupt, PLAIN), b"\x03");
        assert_eq!(encode_key(ControlKey::Escape, PLAIN), b"\x1b");
        assert_eq!(encode_key(ControlKey::Tab, PLAIN), b"\t");
        assert_eq!(encode_key(ControlKey::Backspace, PLAIN), b"\x7f");
        assert_eq!(encode_key(ControlKey::Up, PLAIN), b"\x1b[A");
        assert_eq!(encode_key(ControlKey::Down, PLAIN), b"\x1b[B");
        assert_eq!(encode_key(ControlKey::Right, PLAIN), b"\x1b[C");
        assert_eq!(encode_key(ControlKey::Left, PLAIN), b"\x1b[D");
    }

    #[test]
    fn arrows_in_application_cursor_mode() {
        let modes = TerminalModes {
            application_cursor: true,
            ..PLAIN
        };
        assert_eq!(encode_key(ControlKey::Up, modes), b"\x1bOA");
        assert_eq!(encode_key(ControlKey::Left, modes), b"\x1bOD");
    }

    #[test]
    fn text_with_submit() {
        let enc = encode_input(
            &Input::Text {
                text: "fix the bug".into(),
                submit: true,
            },
            PASTE,
        )
        .unwrap();
        assert_eq!(enc.body, b"fix the bug");
        assert!(enc.submit);
    }

    #[test]
    fn multi_line_text_uses_bracketed_paste_only_when_enabled() {
        assert_eq!(encode_text("a\nb", PLAIN), b"a\nb");
        assert_eq!(encode_text("a\nb", PASTE), b"\x1b[200~a\rb\x1b[201~");
        assert_eq!(encode_text("a\r\nb\n", PASTE), b"\x1b[200~a\rb\r\x1b[201~");
        assert_eq!(encode_text("single", PASTE), b"single");
    }

    #[test]
    fn paste_markers_inside_text_are_removed() {
        let enc = encode_text("a\n\x1b[201~rm -rf /\r", PASTE);
        assert_eq!(enc, b"\x1b[200~a\rrm -rf /\r\x1b[201~");
    }

    #[test]
    fn removing_a_marker_cannot_form_a_new_one() {
        let enc = encode_text("a\n\x1b[20\x1b[200~1~rm -rf /\r", PASTE);
        assert_eq!(enc, b"\x1b[200~a\rrm -rf /\r\x1b[201~");
        let enc = encode_text("a\n\x1b[2\x1b[20\x1b[200~1~01~x", PASTE);
        assert_eq!(enc, b"\x1b[200~a\rx\x1b[201~");
    }

    #[test]
    fn choose_is_unsupported() {
        let err = encode_input(
            &Input::Choose {
                decision_id: "d".into(),
                option_id: "o".into(),
            },
            PLAIN,
        )
        .unwrap_err();
        assert!(matches!(err, PtyError::Unsupported(_)));
    }
}
