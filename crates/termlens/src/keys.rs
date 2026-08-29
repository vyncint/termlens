//! Typed key input and its byte encodings.
//!
//! Every [`Key`] maps to the byte sequence an `xterm`-compatible terminal
//! sends. [`Key::encode`] gives the default-mode form; when sent through
//! [`Terminal::send`](crate::Terminal::send), cursor keys (arrows,
//! Home/End) automatically switch to their `ESC O _` application forms
//! while the application has DECCKM set — the emulator knows the mode, so
//! the bytes always match what the application configured its terminal to
//! send.

/// A key press to send to the terminal.
///
/// Encodings follow xterm defaults; see [`Key::encode`].
///
/// Marked `#[non_exhaustive]`: the keyboard is open-ended — F13+, the
/// keypad and the media keys are all absent today — and a `Key` is
/// something tests *construct* (`t.send(Key::Enter)`) far more often than
/// they match on, so the attribute costs downstream code a `_ =>` arm in
/// the rare match and buys every future key a place to land without a
/// major release. The cost lands on `match`, which needs a wildcard arm even
/// when it already names every variant that exists today:
///
/// ```compile_fail
/// # use termlens::Key;
/// fn arity(k: Key) -> u8 {
///     match k {
///         Key::Char(_) | Key::Ctrl(_) | Key::Alt(_) | Key::F(_) => 0,
///         Key::Enter | Key::Esc | Key::Tab | Key::BackTab | Key::Backspace => 1,
///         Key::Insert | Key::Delete | Key::Up | Key::Down | Key::Left => 2,
///         Key::Right | Key::Home | Key::End | Key::PageUp | Key::PageDown => 3,
///     }
/// }
/// ```
///
/// One `_` arm is the whole cost, and constructing is unaffected:
///
/// ```
/// # use termlens::Key;
/// fn arity(k: Key) -> u8 {
///     match k {
///         Key::Enter => 1,
///         _ => 0,
///     }
/// }
/// assert_eq!(arity(Key::Enter), 1);
/// assert_eq!(arity(Key::Insert), 0);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Key {
    /// A literal character, sent as its UTF-8 bytes.
    Char(char),
    /// Enter / Return. Sends CR (`\r`), which the PTY line discipline
    /// delivers as end-of-line in both raw and canonical modes.
    Enter,
    /// The Escape key alone (byte `0x1B`).
    ///
    /// **Wire ambiguity**: `Esc` immediately followed by another key is
    /// byte-identical to an [`Alt`](Key::Alt) chord, and every input
    /// parser resolves it as one — real keyboards are saved only by human
    /// inter-key delay, which [`send`](crate::Terminal::send) does not
    /// add. Where the `Esc` has an observable effect, wait for it and the
    /// next key is unambiguous:
    ///
    /// ```text
    /// t.send(Key::Esc)?;
    /// t.wait_until(|s| s.contains("NORMAL"))?; // Esc took effect
    /// t.send(Key::Char('?'))?;                 // now unambiguous
    /// ```
    ///
    /// Where it has none — leaving a text field's insert mode often changes
    /// nothing visible — there is nothing to wait *for*, and
    /// [`send_after`](crate::Terminal::send_after) puts a named delay
    /// between the two writes instead.
    Esc,
    /// Tab (`0x09`).
    Tab,
    /// Shift-Tab (`ESC [ Z`).
    BackTab,
    /// Backspace. Sends DEL (`0x7F`), xterm's default erase byte.
    Backspace,
    /// Insert (`ESC [ 2 ~`).
    Insert,
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
    /// The exact bytes this key sends, per xterm defaults (normal cursor
    /// mode). [`Terminal::send`](crate::Terminal::send) prefers the
    /// mode-aware encoding, switching cursor keys to `ESC O _` while the
    /// application has DECCKM set.
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
            Key::Insert => b"\x1b[2~".to_vec(),
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

mod sealed {
    pub trait Sealed {}
    impl Sealed for super::Key {}
    impl Sealed for super::Chord {}
}

/// Anything [`Terminal::send`](crate::Terminal::send) can send: a [`Key`]
/// or a modifier [`Chord`]. Sealed — the set is fixed by the crate.
pub trait Input: sealed::Sealed {
    /// Encode for the wire. `application_cursor` selects the DECCKM form
    /// where the key has one.
    #[doc(hidden)]
    fn encode_modal(&self, application_cursor: bool) -> Vec<u8>;
}

impl Input for Key {
    fn encode_modal(&self, application_cursor: bool) -> Vec<u8> {
        if application_cursor {
            // DECCKM: cursor keys send SS3 forms.
            let ss3 = match self {
                Key::Up => Some(b'A'),
                Key::Down => Some(b'B'),
                Key::Right => Some(b'C'),
                Key::Left => Some(b'D'),
                Key::Home => Some(b'H'),
                Key::End => Some(b'F'),
                _ => None,
            };
            if let Some(final_byte) = ss3 {
                return vec![0x1b, b'O', final_byte];
            }
        }
        self.encode()
    }
}

impl Input for Chord {
    fn encode_modal(&self, _application_cursor: bool) -> Vec<u8> {
        self.encode()
    }
}

/// A modifier chord over a special key — `Ctrl-Right`, `Shift-Up`,
/// `Alt-PageDown`, `Ctrl-Shift-F5`. Build it from a [`Key`]:
///
/// ```
/// use termlens::Key;
/// assert_eq!(Key::Right.ctrl().encode(), b"\x1b[1;5C");
/// assert_eq!(Key::Up.shift().encode(), b"\x1b[1;2A");
/// assert_eq!(Key::F(5).ctrl().shift().encode(), b"\x1b[15;6~");
/// ```
///
/// For plain character chords keep using [`Key::Ctrl`] / [`Key::Alt`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Chord {
    key: Key,
    ctrl: bool,
    alt: bool,
    shift: bool,
}

impl Key {
    /// A `Ctrl` chord over this special key (arrows, Home/End,
    /// PageUp/Down, Insert, Delete, F1–F12).
    ///
    /// # Panics
    ///
    /// Panics for keys without a CSI-modifier form — for `Key::Char` use
    /// [`Key::Ctrl`] / [`Key::Alt`] instead.
    #[must_use]
    #[track_caller]
    pub fn ctrl(self) -> Chord {
        Chord::over(self).ctrl()
    }

    /// An `Alt` chord over this special key. See [`Key::ctrl`].
    ///
    /// # Panics
    ///
    /// Panics for keys without a CSI-modifier form.
    #[must_use]
    #[track_caller]
    pub fn alt(self) -> Chord {
        Chord::over(self).alt()
    }

    /// A `Shift` chord over this special key. See [`Key::ctrl`].
    ///
    /// # Panics
    ///
    /// Panics for keys without a CSI-modifier form.
    #[must_use]
    #[track_caller]
    pub fn shift(self) -> Chord {
        Chord::over(self).shift()
    }
}

impl Chord {
    #[track_caller]
    fn over(key: Key) -> Self {
        assert!(
            chord_base(key).is_some(),
            "{key:?} has no CSI-modifier chord form; for characters use \
             Key::Ctrl(c) / Key::Alt(c)"
        );
        Self {
            key,
            ctrl: false,
            alt: false,
            shift: false,
        }
    }

    /// Add `Ctrl` to the chord.
    #[must_use]
    pub fn ctrl(mut self) -> Self {
        self.ctrl = true;
        self
    }

    /// Add `Alt` to the chord.
    #[must_use]
    pub fn alt(mut self) -> Self {
        self.alt = true;
        self
    }

    /// Add `Shift` to the chord.
    #[must_use]
    pub fn shift(mut self) -> Self {
        self.shift = true;
        self
    }

    /// The exact bytes this chord sends: the xterm CSI-modifier form,
    /// `ESC [ 1 ; m <letter>` or `ESC [ <n> ; m ~`, where `m` is
    /// `1 + shift + 2·alt + 4·ctrl`.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let base = chord_base(self.key).expect("validated at construction");
        let modifier = 1 + u8::from(self.shift) + 2 * u8::from(self.alt) + 4 * u8::from(self.ctrl);
        match base {
            ChordBase::Letter(letter) => format!("\x1b[1;{modifier}{letter}").into_bytes(),
            ChordBase::Tilde(number) => format!("\x1b[{number};{modifier}~").into_bytes(),
        }
    }
}

enum ChordBase {
    /// `ESC [ 1 ; m <letter>` keys.
    Letter(char),
    /// `ESC [ n ; m ~` keys.
    Tilde(u8),
}

fn chord_base(key: Key) -> Option<ChordBase> {
    Some(match key {
        Key::Up => ChordBase::Letter('A'),
        Key::Down => ChordBase::Letter('B'),
        Key::Right => ChordBase::Letter('C'),
        Key::Left => ChordBase::Letter('D'),
        Key::Home => ChordBase::Letter('H'),
        Key::End => ChordBase::Letter('F'),
        Key::F(n @ 1..=4) => ChordBase::Letter(['P', 'Q', 'R', 'S'][usize::from(n) - 1]),
        Key::Insert => ChordBase::Tilde(2),
        Key::Delete => ChordBase::Tilde(3),
        Key::PageUp => ChordBase::Tilde(5),
        Key::PageDown => ChordBase::Tilde(6),
        Key::F(5) => ChordBase::Tilde(15),
        Key::F(n @ 6..=10) => ChordBase::Tilde(11 + n), // 17,18,19,20,21
        Key::F(11) => ChordBase::Tilde(23),
        Key::F(12) => ChordBase::Tilde(24),
        _ => return None,
    })
}

/// SGR (1006) mouse report. `press = false` is the release form.
///
/// Coordinates are 1-based on the wire. The `+ 1` is done in `u32` so a
/// caller that somehow skips the grid check cannot wrap `u16::MAX` to
/// column/row 0 in release builds.
pub(crate) fn mouse_sgr(button: u8, col: u16, row: u16, press: bool) -> Vec<u8> {
    let suffix = if press { 'M' } else { 'm' };
    format!(
        "\x1b[<{button};{};{}{suffix}",
        u32::from(col) + 1,
        u32::from(row) + 1
    )
    .into_bytes()
}

/// Legacy (X10/normal) mouse report: `ESC [ M Cb Cx Cy`, byte-valued.
/// Coordinates beyond 222 are unrepresentable; the caller validates.
pub(crate) fn mouse_legacy(button: u8, col: u16, row: u16) -> Vec<u8> {
    let mut out = b"\x1b[M".to_vec();
    out.push(32 + button);
    out.push(32 + 1 + u8::try_from(col).expect("caller validated"));
    out.push(32 + 1 + u8::try_from(row).expect("caller validated"));
    out
}

/// UTF-8 (mode 1005) mouse report: the legacy layout, but each
/// coordinate is written as a UTF-8 *character* rather than a raw byte.
/// Identical to [`mouse_legacy`] up to coordinate 127 (columns and rows
/// 0..=94); past that the legacy form emits a bare byte no UTF-8 reader
/// can accept, which is exactly the bug this encoding exists to avoid.
pub(crate) fn mouse_utf8(button: u8, col: u16, row: u16) -> Vec<u8> {
    let mut out = b"\x1b[M".to_vec();
    out.push(32 + button); // the button byte stays a byte
    let mut buf = [0u8; 4];
    for coordinate in [col, row] {
        let value = 32 + 1 + u32::from(coordinate);
        let ch = char::from_u32(value).expect("32 + 1 + u16 is always a valid scalar value");
        out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
    }
    out
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
            (Key::Insert, b"\x1b[2~"),
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
    fn cursor_keys_switch_to_ss3_under_application_cursor_mode() {
        assert_eq!(Key::Up.encode_modal(true), b"\x1bOA");
        assert_eq!(Key::End.encode_modal(true), b"\x1bOF");
        assert_eq!(Key::Up.encode_modal(false), b"\x1b[A");
        // Only cursor keys switch; everything else is mode-independent.
        assert_eq!(Key::F(5).encode_modal(true), Key::F(5).encode());
        assert_eq!(Key::Delete.encode_modal(true), Key::Delete.encode());
        // Chords stay CSI-modified regardless of mode.
        assert_eq!(Key::Up.ctrl().encode_modal(true), b"\x1b[1;5A");
    }

    #[test]
    fn chord_encodings() {
        let table: &[(Chord, &[u8])] = &[
            (Key::Right.ctrl(), b"\x1b[1;5C"),
            (Key::Up.shift(), b"\x1b[1;2A"),
            (Key::Left.alt(), b"\x1b[1;3D"),
            (Key::Home.ctrl(), b"\x1b[1;5H"),
            (Key::End.ctrl().shift(), b"\x1b[1;6F"),
            (Key::PageDown.alt(), b"\x1b[6;3~"),
            (Key::Insert.shift(), b"\x1b[2;2~"),
            (Key::Delete.ctrl(), b"\x1b[3;5~"),
            (Key::F(1).ctrl(), b"\x1b[1;5P"),
            (Key::F(5).ctrl().shift(), b"\x1b[15;6~"),
            (Key::F(10).alt(), b"\x1b[21;3~"),
            (Key::F(12).ctrl().alt().shift(), b"\x1b[24;8~"),
        ];
        for (chord, bytes) in table {
            assert_eq!(chord.encode(), *bytes, "wrong encoding for {chord:?}");
        }
    }

    /// The hazard `send_after` exists for, stated as the identity it is.
    /// Nothing in an encoding can fix this — the two are the same bytes — so
    /// the only remedy is to separate the writes in time.
    #[test]
    fn esc_then_a_key_is_byte_identical_to_an_alt_chord() {
        let mut separate = Key::Esc.encode();
        separate.extend(Key::Char('j').encode());
        assert_eq!(separate, Key::Alt('j').encode());
        assert_eq!(separate, b"\x1bj");
    }

    #[test]
    #[should_panic(expected = "no CSI-modifier chord form")]
    fn char_chords_panic_toward_key_ctrl() {
        let _ = Key::Char('a').ctrl();
    }

    #[test]
    fn mouse_encodings() {
        assert_eq!(mouse_sgr(0, 9, 6, true), b"\x1b[<0;10;7M");
        assert_eq!(mouse_sgr(0, 9, 6, false), b"\x1b[<0;10;7m");
        assert_eq!(mouse_sgr(64, 0, 0, true), b"\x1b[<64;1;1M");
        assert_eq!(mouse_legacy(0, 0, 0), b"\x1b[M\x20\x21\x21");
        assert_eq!(mouse_legacy(3, 9, 6,), b"\x1b[M\x23\x2a\x27");
        // Modifiers ride on the wheel's code exactly as on a button: +4
        // shift, +8 alt, +16 ctrl. Ctrl-wheel-up is 64 + 16, in both forms.
        assert_eq!(mouse_sgr(80, 0, 0, true), b"\x1b[<80;1;1M");
        assert_eq!(mouse_sgr(69, 2, 3, true), b"\x1b[<69;3;4M"); // shift + wheel down
        assert_eq!(mouse_legacy(80, 0, 0), b"\x1b[M\x70\x21\x21"); // 32 + 80
    }

    /// `u16::MAX + 1` used to wrap to 0 in release and panic in debug.
    /// The encoder must stay in range even if a caller skips the grid check.
    #[test]
    fn mouse_sgr_does_not_wrap_at_u16_max() {
        let expected = format!(
            "\x1b[<0;{};{}M",
            u32::from(u16::MAX) + 1,
            u32::from(u16::MAX) + 1
        );
        assert_eq!(mouse_sgr(0, u16::MAX, u16::MAX, true), expected.as_bytes());
        let release = format!(
            "\x1b[<0;{};{}m",
            u32::from(u16::MAX) + 1,
            u32::from(u16::MAX) + 1
        );
        assert_eq!(mouse_sgr(0, u16::MAX, u16::MAX, false), release.as_bytes());
    }

    #[test]
    fn utf8_mouse_matches_legacy_until_the_encodings_diverge() {
        // Coordinate byte is 32 + 1 + n, so n = 94 is the last value that
        // fits in one UTF-8 byte (127) and n = 95 is the first that does
        // not (128 -> c2 80).
        for n in [0u16, 10, 94] {
            assert_eq!(
                mouse_utf8(0, n, n),
                mouse_legacy(0, n, n),
                "encodings must agree at coordinate {n}"
            );
        }
        assert_eq!(mouse_utf8(0, 95, 0), b"\x1b[M\x20\xc2\x80\x21");
        // The case that motivated this: column 100 is a bare 0x85 in the
        // legacy form, which a UTF-8 reader cannot accept.
        assert_eq!(mouse_legacy(0, 100, 3), b"\x1b[M\x20\x85\x24");
        assert_eq!(mouse_utf8(0, 100, 3), b"\x1b[M\x20\xc2\x85\x24");
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
