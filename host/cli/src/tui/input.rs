//! Translates key presses into the bytes a terminal would send.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

pub fn key_to_bytes(key: KeyEvent) -> Option<Vec<u8>> {
    let bytes: Vec<u8> = match key.code {
        KeyCode::Char(c) if key.modifiers.contains(KeyModifiers::CONTROL) => {
            let control = match c.to_ascii_lowercase() {
                c @ 'a'..='z' => c as u8 - b'a' + 1,
                '@' | ' ' => 0x00,
                '[' => 0x1B,
                '\\' => 0x1C,
                ']' => 0x1D,
                '^' => 0x1E,
                '_' => 0x1F,
                _ => return None,
            };
            vec![control]
        }
        KeyCode::Char(c) => {
            let mut buf = [0; 4];
            let mut bytes = Vec::new();
            if key.modifiers.contains(KeyModifiers::ALT) {
                bytes.push(0x1B);
            }
            bytes.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            bytes
        }
        KeyCode::Enter => b"\r".to_vec(),
        KeyCode::Backspace => vec![0x7F],
        KeyCode::Tab => b"\t".to_vec(),
        KeyCode::BackTab => b"\x1b[Z".to_vec(),
        KeyCode::Esc => vec![0x1B],
        KeyCode::Up => b"\x1b[A".to_vec(),
        KeyCode::Down => b"\x1b[B".to_vec(),
        KeyCode::Right => b"\x1b[C".to_vec(),
        KeyCode::Left => b"\x1b[D".to_vec(),
        KeyCode::Home => b"\x1b[H".to_vec(),
        KeyCode::End => b"\x1b[F".to_vec(),
        KeyCode::Insert => b"\x1b[2~".to_vec(),
        KeyCode::Delete => b"\x1b[3~".to_vec(),
        KeyCode::PageUp => b"\x1b[5~".to_vec(),
        KeyCode::PageDown => b"\x1b[6~".to_vec(),
        KeyCode::F(n @ 1..=4) => vec![0x1B, b'O', b'P' + n - 1],
        _ => return None,
    };
    Some(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    #[test]
    fn maps_keys_to_terminal_bytes() {
        assert_eq!(key_to_bytes(key(KeyCode::Char('x'), KeyModifiers::NONE)), Some(b"x".to_vec()));
        assert_eq!(key_to_bytes(key(KeyCode::Char('é'), KeyModifiers::NONE)), Some("é".as_bytes().to_vec()));
        assert_eq!(key_to_bytes(key(KeyCode::Char('c'), KeyModifiers::CONTROL)), Some(vec![0x03]));
        assert_eq!(key_to_bytes(key(KeyCode::Char('x'), KeyModifiers::ALT)), Some(b"\x1bx".to_vec()));
        assert_eq!(key_to_bytes(key(KeyCode::Enter, KeyModifiers::NONE)), Some(b"\r".to_vec()));
        assert_eq!(key_to_bytes(key(KeyCode::Up, KeyModifiers::NONE)), Some(b"\x1b[A".to_vec()));
        assert_eq!(key_to_bytes(key(KeyCode::F(2), KeyModifiers::NONE)), Some(b"\x1bOQ".to_vec()));
        assert_eq!(key_to_bytes(key(KeyCode::F(5), KeyModifiers::NONE)), None);
    }
}
