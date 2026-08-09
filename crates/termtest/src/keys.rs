//! Typed key input and its byte encodings.
//!
//! Every [`Key`] maps to the byte sequence an `xterm`-compatible terminal
//! sends in its default modes (normal keypad, no application cursor keys).
//! Applications that enable DECCKM application-cursor mode still accept the
//! CSI forms in every mainstream input parser (crossterm, termion, ncurses),
//! so v0.1 keeps the mapping static; see `docs/DESIGN.md` for the roadmap
//! entry on mode-aware encoding.

/// A key press to send to the terminal.
///
/// Encodings follow xterm defaults; see [`Key::encode`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Key {
    /// A literal character, sent as its UTF-8 bytes.
    Char(char),
    /// Enter / Return. Sends CR (`\r`), which the PTY line discipline
    /// delivers as end-of-line in both raw and canonical modes.
    Enter,
    /// The Escape key alone (byte `0x1B`).
    Esc,
    /// Tab (`0x09`).
    Tab,
    /// Shift-Tab (`ESC [ Z`).
    BackTab,
    /// Backspace. Sends DEL (`0x7F`), xterm's default erase byte.
    Backspace,
    /// Forward delete (`ESC [ 3 ~`).
    Delete,
    /// Up arrow (`ESC [ A`).
    Up,
    /// Down arrow (`ESC [ B`).
    Down,
    /// Left arrow (`ESC [ D`).
    Left,
    /// Right arrow (`ESC [ C`).
    Right,
    /// Home (`ESC [ H`).
    Home,
    /// End (`ESC [ F`).
    End,
    /// Page Up (`ESC [ 5 ~`).
    PageUp,
    /// Page Down (`ESC [ 6 ~`).
    PageDown,
    /// Function key F1–F12. [`Key::encode`] panics outside that range.
    F(u8),
    /// A Control chord: `Ctrl('c')` sends `0x03`. Accepts letters (case
    /// insensitive), `@ [ \ ] ^ _`, space, and `?` (DEL). [`Key::encode`]
    /// panics for characters with no control mapping.
    Ctrl(char),
    /// An Alt (Meta) chord: ESC followed by the character's UTF-8 bytes.
    Alt(char),
}

impl Key {
    /// The exact bytes this key sends, per xterm defaults.
    ///
    /// # Panics
    ///
    /// Panics for `Key::F(0)` / `Key::F(n > 12)` and for `Key::Ctrl(c)`
    /// where `c` has no control-code mapping. These are programming errors
    /// in the test itself, so failing loudly beats sending garbage to the
    /// application under test.
    #[must_use]
    #[track_caller]
    pub fn encode(self) -> Vec<u8> {
        match self {
            Key::Char(c) => {
                let mut buf = [0u8; 4];
                c.encode_utf8(&mut buf).as_bytes().to_vec()
            }
            Key::Enter => vec![b'\r'],
            Key::Esc => vec![0x1b],
            Key::Tab => vec![b'\t'],
            Key::BackTab => b"\x1b[Z".to_vec(),
            Key::Backspace => vec![0x7f],
            Key::Delete => b"\x1b[3~".to_vec(),
            Key::Up => b"\x1b[A".to_vec(),
            Key::Down => b"\x1b[B".to_vec(),
            Key::Right => b"\x1b[C".to_vec(),
            Key::Left => b"\x1b[D".to_vec(),
            Key::Home => b"\x1b[H".to_vec(),
            Key::End => b"\x1b[F".to_vec(),
            Key::PageUp => b"\x1b[5~".to_vec(),
            Key::PageDown => b"\x1b[6~".to_vec(),
            Key::F(n) => match n {
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
                _ => panic!("Key::F({n}): only F1-F12 exist"),
            },
            Key::Ctrl(c) => vec![ctrl_byte(c)],
            Key::Alt(c) => {
                let mut out = vec![0x1b];
                let mut buf = [0u8; 4];
                out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
                out
            }
        }
    }
}

#[track_caller]
fn ctrl_byte(c: char) -> u8 {
    match c {
        'a'..='z' => (c as u8) & 0x1f,
        'A'..='Z' | '@' | '[' | '\\' | ']' | '^' | '_' => (c as u8) & 0x1f,
        ' ' => 0x00,
        '?' => 0x7f,
        _ => panic!("Key::Ctrl({c:?}): no control-code mapping for this character"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xterm_encodings() {
        let table: &[(Key, &[u8])] = &[
            (Key::Char('j'), b"j"),
            (Key::Char('é'), "é".as_bytes()),
            (Key::Enter, b"\r"),
            (Key::Esc, b"\x1b"),
            (Key::Tab, b"\t"),
            (Key::BackTab, b"\x1b[Z"),
            (Key::Backspace, b"\x7f"),
            (Key::Delete, b"\x1b[3~"),
            (Key::Up, b"\x1b[A"),
            (Key::Down, b"\x1b[B"),
            (Key::Right, b"\x1b[C"),
            (Key::Left, b"\x1b[D"),
            (Key::Home, b"\x1b[H"),
            (Key::End, b"\x1b[F"),
            (Key::PageUp, b"\x1b[5~"),
            (Key::PageDown, b"\x1b[6~"),
            (Key::F(1), b"\x1bOP"),
            (Key::F(4), b"\x1bOS"),
            (Key::F(5), b"\x1b[15~"),
            (Key::F(10), b"\x1b[21~"),
            (Key::F(12), b"\x1b[24~"),
            (Key::Ctrl('c'), b"\x03"),
            (Key::Ctrl('C'), b"\x03"),
            (Key::Ctrl('['), b"\x1b"),
            (Key::Ctrl(' '), b"\x00"),
            (Key::Ctrl('?'), b"\x7f"),
            (Key::Alt('x'), b"\x1bx"),
        ];
        for (key, bytes) in table {
            assert_eq!(key.encode(), *bytes, "wrong encoding for {key:?}");
        }
    }

    #[test]
    #[should_panic(expected = "only F1-F12")]
    fn f13_panics() {
        let _ = Key::F(13).encode();
    }

    #[test]
    #[should_panic(expected = "no control-code mapping")]
    fn ctrl_digit_panics() {
        let _ = Key::Ctrl('1').encode();
    }
}
