use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

/// Encodes a crossterm key event as the bytes an xterm-compatible terminal
/// would send. Used where the console delivers key events instead of a VT
/// byte stream (Windows). Returns `None` for releases and unsupported keys.
pub fn encode_key_event(ev: KeyEvent, application_cursor: bool) -> Option<Vec<u8>> {
    if ev.kind == KeyEventKind::Release {
        return None;
    }
    let ctrl = ev.modifiers.contains(KeyModifiers::CONTROL);
    let alt = ev.modifiers.contains(KeyModifiers::ALT);
    let csi_or_ss3 = |c: u8| -> Vec<u8> {
        if application_cursor {
            vec![0x1b, b'O', c]
        } else {
            vec![0x1b, b'[', c]
        }
    };
    let mut out = match ev.code {
        KeyCode::Char(c) if ctrl => {
            let lower = c.to_ascii_lowercase();
            match lower {
                'a'..='z' => vec![lower as u8 - b'a' + 1],
                '@' | ' ' | '2' => vec![0],
                '[' | '3' => vec![0x1b],
                '\\' | '4' => vec![0x1c],
                ']' | '5' => vec![0x1d],
                '^' | '6' => vec![0x1e],
                '_' | '-' | '7' => vec![0x1f],
                '?' | '8' => vec![0x7f],
                _ => c.to_string().into_bytes(),
            }
        }
        KeyCode::Char(c) => c.to_string().into_bytes(),
        KeyCode::Enter => b"\r".to_vec(),
        KeyCode::Backspace => b"\x7f".to_vec(),
        KeyCode::Tab => b"\t".to_vec(),
        KeyCode::BackTab => b"\x1b[Z".to_vec(),
        KeyCode::Esc => b"\x1b".to_vec(),
        KeyCode::Up => csi_or_ss3(b'A'),
        KeyCode::Down => csi_or_ss3(b'B'),
        KeyCode::Right => csi_or_ss3(b'C'),
        KeyCode::Left => csi_or_ss3(b'D'),
        KeyCode::Home => csi_or_ss3(b'H'),
        KeyCode::End => csi_or_ss3(b'F'),
        KeyCode::Insert => b"\x1b[2~".to_vec(),
        KeyCode::Delete => b"\x1b[3~".to_vec(),
        KeyCode::PageUp => b"\x1b[5~".to_vec(),
        KeyCode::PageDown => b"\x1b[6~".to_vec(),
        KeyCode::F(n) => match n {
            1 => b"\x1bOP".to_vec(),
            2 => b"\x1bOQ".to_vec(),
            3 => b"\x1bOR".to_vec(),
            4 => b"\x1bOS".to_vec(),
            5 => b"\x1b[15~".to_vec(),
            6 => b"\x1b[17~".to_vec(),
            7 => b"\x1b[18~".to_vec(),
            8 => b"\x1b[19~".to_vec(),
            9 => b"\x1b[20~".to_vec(),
            10 => b"\x1b[21~".to_vec(),
            11 => b"\x1b[23~".to_vec(),
            12 => b"\x1b[24~".to_vec(),
            _ => return None,
        },
        _ => return None,
    };
    if alt {
        out.insert(0, 0x1b);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    #[test]
    fn plain_and_control_characters() {
        let none = KeyModifiers::NONE;
        let ctrl = KeyModifiers::CONTROL;
        assert_eq!(
            encode_key_event(key(KeyCode::Char('a'), none), false).unwrap(),
            b"a"
        );
        assert_eq!(
            encode_key_event(key(KeyCode::Char('é'), none), false).unwrap(),
            "é".as_bytes()
        );
        assert_eq!(
            encode_key_event(key(KeyCode::Char('c'), ctrl), false).unwrap(),
            b"\x03"
        );
        assert_eq!(
            encode_key_event(key(KeyCode::Char(']'), ctrl), false).unwrap(),
            b"\x1d"
        );
        assert_eq!(
            encode_key_event(key(KeyCode::Char('x'), KeyModifiers::ALT), false).unwrap(),
            b"\x1bx"
        );
    }

    #[test]
    fn special_keys() {
        let none = KeyModifiers::NONE;
        assert_eq!(
            encode_key_event(key(KeyCode::Enter, none), false).unwrap(),
            b"\r"
        );
        assert_eq!(
            encode_key_event(key(KeyCode::Up, none), false).unwrap(),
            b"\x1b[A"
        );
        assert_eq!(
            encode_key_event(key(KeyCode::Up, none), true).unwrap(),
            b"\x1bOA"
        );
        assert_eq!(
            encode_key_event(key(KeyCode::Delete, none), false).unwrap(),
            b"\x1b[3~"
        );
        assert_eq!(
            encode_key_event(key(KeyCode::F(5), none), false).unwrap(),
            b"\x1b[15~"
        );
        assert!(encode_key_event(key(KeyCode::F(13), none), false).is_none());
    }

    #[test]
    fn releases_are_ignored() {
        let mut ev = key(KeyCode::Char('a'), KeyModifiers::NONE);
        ev.kind = KeyEventKind::Release;
        assert!(encode_key_event(ev, false).is_none());
    }
}
