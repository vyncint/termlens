//! The `vt100`-crate backend. Public types never leak from here: every
//! snapshot converts vt100's grid into termlens's own [`Screen`].

use super::seq::{SeqTracker, SyncEvent};
use super::{Emulator, Processed};
use crate::screen::{Cell, Color, Screen, Style};

pub(crate) struct Vt100Emulator {
    parser: ::vt100::Parser,
    tracker: SeqTracker,
}

impl Vt100Emulator {
    pub(crate) fn new(rows: u16, cols: u16) -> Self {
        Self {
            // No scrollback: v0.1 asserts on the visible screen only.
            parser: ::vt100::Parser::new(rows, cols, 0),
            tracker: SeqTracker::new(),
        }
    }
}

impl Emulator for Vt100Emulator {
    fn process(&mut self, bytes: &[u8]) -> Processed {
        for (i, &byte) in bytes.iter().enumerate() {
            if self.tracker.step(byte) == SyncEvent::End {
                self.parser.process(&bytes[..=i]);
                return Processed {
                    consumed: i + 1,
                    frame_complete: true,
                };
            }
        }
        self.parser.process(bytes);
        Processed {
            consumed: bytes.len(),
            frame_complete: false,
        }
    }

    fn snapshot(&self) -> Screen {
        let screen = self.parser.screen();
        let (rows, cols) = screen.size();
        let mut cells = Vec::with_capacity(usize::from(rows) * usize::from(cols));
        for row in 0..rows {
            for col in 0..cols {
                // In-range lookups on vt100 are always Some; blank fallback
                // keeps this total rather than panicking inside a snapshot.
                cells.push(screen.cell(row, col).map_or_else(
                    || Cell::new(String::new(), Style::default(), false, false),
                    convert_cell,
                ));
            }
        }
        let (cursor_row, cursor_col) = screen.cursor_position();
        Screen::from_parts(
            cols,
            rows,
            cursor_row,
            cursor_col,
            !screen.hide_cursor(),
            cells,
        )
    }

    fn mid_sequence(&self) -> bool {
        self.tracker.mid_sequence()
    }

    fn in_sync_update(&self) -> bool {
        self.tracker.in_sync_update()
    }

    fn set_size(&mut self, rows: u16, cols: u16) {
        self.parser.screen_mut().set_size(rows, cols);
    }
}

fn convert_cell(cell: &::vt100::Cell) -> Cell {
    let style = Style {
        fg: convert_color(cell.fgcolor()),
        bg: convert_color(cell.bgcolor()),
        bold: cell.bold(),
        dim: cell.dim(),
        italic: cell.italic(),
        underline: cell.underline(),
        reverse: cell.inverse(),
    };
    Cell::new(
        cell.contents().to_owned(),
        style,
        cell.is_wide(),
        cell.is_wide_continuation(),
    )
}

fn convert_color(color: ::vt100::Color) -> Color {
    match color {
        ::vt100::Color::Default => Color::Default,
        ::vt100::Color::Idx(i) => Color::Indexed(i),
        ::vt100::Color::Rgb(r, g, b) => Color::Rgb(r, g, b),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Feed a whole stream, looping across frame-boundary stops.
    fn feed_all(emu: &mut Vt100Emulator, bytes: &[u8]) {
        let mut off = 0;
        while off < bytes.len() {
            off += emu.process(&bytes[off..]).consumed;
        }
    }

    fn emu_with(bytes: &[u8]) -> Vt100Emulator {
        let mut emu = Vt100Emulator::new(4, 10);
        feed_all(&mut emu, bytes);
        emu
    }

    #[test]
    fn renders_plain_text_into_the_grid() {
        let emu = emu_with(b"hi\r\nthere");
        let screen = emu.snapshot();
        assert_eq!(screen.size(), (10, 4));
        assert_eq!(screen.text(), "hi\nthere\n\n");
        assert_eq!(screen.cursor(), (1, 5, true));
    }

    #[test]
    fn positions_wide_characters_with_continuations() {
        let emu = emu_with("汉x".as_bytes());
        let screen = emu.snapshot();
        let wide = screen.cell(0, 0).unwrap();
        assert!(wide.is_wide());
        assert_eq!(wide.contents(), "汉");
        assert!(screen.cell(0, 1).unwrap().is_wide_continuation());
        assert_eq!(screen.find("x"), Some((0, 2)));
    }

    #[test]
    fn captures_sgr_styles() {
        let emu = emu_with(b"\x1b[1;3;4;7;31mX\x1b[0m");
        let screen = emu.snapshot();
        let style = *screen.cell(0, 0).unwrap().style();
        assert!(style.bold && style.italic && style.underline && style.reverse);
        assert_eq!(style.fg, Color::Indexed(1));
        assert_eq!(screen.cell(0, 1).unwrap().style(), &Style::default());
    }

    #[test]
    fn hidden_cursor_is_reported() {
        let emu = emu_with(b"\x1b[?25l");
        assert_eq!(emu.snapshot().cursor(), (0, 0, false));
    }

    #[test]
    fn mid_sequence_tracks_partial_escape() {
        let mut emu = Vt100Emulator::new(4, 10);
        feed_all(&mut emu, b"text\x1b[3");
        assert!(emu.mid_sequence());
        feed_all(&mut emu, b"1m");
        assert!(!emu.mid_sequence());
    }

    #[test]
    fn process_stops_at_the_end_of_a_synchronized_update() {
        let mut emu = Vt100Emulator::new(4, 10);
        let stream = b"\x1b[?2026hframe1\x1b[?2026lnext";

        let first = emu.process(stream);
        assert!(first.frame_complete);
        // Everything through the ESU is consumed; "next" is not.
        assert_eq!(
            &stream[..first.consumed],
            b"\x1b[?2026hframe1\x1b[?2026l" as &[u8]
        );
        // The screen at the stop is the complete frame, untouched by "next".
        assert_eq!(emu.snapshot().text(), "frame1\n\n\n");
        assert!(!emu.in_sync_update());

        let rest = emu.process(&stream[first.consumed..]);
        assert!(!rest.frame_complete);
        assert!(emu.snapshot().contains("frame1next"));
    }

    #[test]
    fn in_sync_update_is_true_between_bsu_and_esu() {
        let mut emu = Vt100Emulator::new(4, 10);
        feed_all(&mut emu, b"\x1b[?2026hpartial");
        assert!(emu.in_sync_update());
        assert!(!emu.mid_sequence()); // the escape itself is finished
        feed_all(&mut emu, b"\x1b[?2026l");
        assert!(!emu.in_sync_update());
    }

    #[test]
    fn set_size_resizes_the_grid() {
        let mut emu = emu_with(b"hello");
        emu.set_size(2, 5);
        let screen = emu.snapshot();
        assert_eq!(screen.size(), (5, 2));
        assert_eq!(screen.text(), "hello\n");
    }
}
