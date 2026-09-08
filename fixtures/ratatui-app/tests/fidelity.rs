//! The one check only a PTY harness can make for a ratatui application: the
//! screen termlens renders from the real binary equals the buffer
//! `TestBackend` renders in-process from the same `draw` — cells *and*
//! styles, at two sizes, with a resize between (#252). A disagreement here
//! is a bug in the terminal layer: crossterm's encoding, the PTY, or
//! termlens's emulation.

use ratatui::buffer::Buffer;
use ratatui::style::{Color, Modifier};
use ratatui_app::{render, State};
use serde_json::{json, Value};
use termlens::{Key, Screen};

/// A ratatui colour in termlens's JSON: `"default"`, `{"indexed": n}` or
/// `{"rgb": [r, g, b]}`.
fn color(color: Color) -> Value {
    let indexed = |n: u8| json!({ "indexed": n });
    match color {
        Color::Reset => json!("default"),
        Color::Black => indexed(0),
        Color::Red => indexed(1),
        Color::Green => indexed(2),
        Color::Yellow => indexed(3),
        Color::Blue => indexed(4),
        Color::Magenta => indexed(5),
        Color::Cyan => indexed(6),
        Color::Gray => indexed(7),
        Color::DarkGray => indexed(8),
        Color::LightRed => indexed(9),
        Color::LightGreen => indexed(10),
        Color::LightYellow => indexed(11),
        Color::LightBlue => indexed(12),
        Color::LightMagenta => indexed(13),
        Color::LightCyan => indexed(14),
        Color::White => indexed(15),
        Color::Indexed(n) => indexed(n),
        Color::Rgb(r, g, b) => json!({ "rgb": [r, g, b] }),
    }
}

/// The buffer as termlens cells, rows of them, the way a `Screen` is
/// serialized — so the expected screen is built from the PTY screen's own
/// JSON with only the cells swapped, and the diff compares nothing else.
fn cells(buffer: &Buffer) -> Value {
    let area = buffer.area;
    let mut rows = Vec::new();
    for y in 0..area.height {
        let mut row = Vec::new();
        let mut skip = false;
        for x in 0..area.width {
            let cell = buffer.cell((x, y)).expect("in the buffer");
            let symbol = cell.symbol();
            let width = unicode_width(symbol);
            let modifier = cell.modifier;
            let style = json!({
                "fg": color(cell.fg),
                "bg": color(cell.bg),
                "bold": modifier.contains(Modifier::BOLD),
                "dim": modifier.contains(Modifier::DIM),
                "italic": modifier.contains(Modifier::ITALIC),
                "underline": modifier.contains(Modifier::UNDERLINED),
                "reverse": modifier.contains(Modifier::REVERSED),
                "blink": modifier.contains(Modifier::SLOW_BLINK) || modifier.contains(Modifier::RAPID_BLINK),
                "conceal": modifier.contains(Modifier::HIDDEN),
                "strikethrough": modifier.contains(Modifier::CROSSED_OUT),
            });
            if skip {
                // The column after a wide character: termlens's continuation
                // cell, in the leading cell's style.
                row.push(json!({ "contents": "", "style": style, "wide": false, "wide_continuation": true }));
                skip = false;
                continue;
            }
            skip = width == 2;
            row.push(json!({ "contents": symbol, "style": style, "wide": width == 2, "wide_continuation": false }));
        }
        rows.push(Value::Array(row));
    }
    Value::Array(rows)
}

/// Display width of one grapheme, the way both renderers count it.
fn unicode_width(symbol: &str) -> usize {
    ratatui::text::Span::raw(symbol).width()
}

/// The PTY screen with its cells replaced by what `TestBackend` drew for
/// the same state and size: equal to the PTY screen exactly when the two
/// renderings agree.
fn expected(from: &Screen, state: &State) -> Screen {
    let (cols, rows) = from.size();
    let mut value = serde_json::to_value(from).expect("a Screen serializes");
    value["cells"] = cells(&render(cols, rows, state));
    serde_json::from_value(value).expect("the cells hold together")
}

#[test]
#[cfg_attr(
    windows,
    ignore = "ConPTY closes a DEC 2026 bracket before the content it wrapped, so no frame holds what was drawn (#149)"
)]
fn the_pty_rendering_equals_test_backend_at_two_sizes() -> termlens::Result<()> {
    let mut t = termlens::bin!("ratatui-app")?;
    let mut state = State::default();

    let frame = t.wait_frame(|s| s.contains("Counter: 0"))?;
    let want = expected(&frame, &state);
    assert!(
        frame.diff(&want).is_empty(),
        "80x24, initial:\n{}",
        frame.diff(&want)
    );
    // The highlight is real reverse video, and the CJK item is two cells.
    assert!(
        frame.find_by(|c| c.style().reverse).is_some(),
        "{}",
        frame.with_styles()
    );
    let (row, col) = frame.find("東京").expect("the CJK item");
    assert!(frame.cell(row, col).unwrap().is_wide());

    t.send(Key::Char('j'))?;
    state.counter = 1;
    state.selected = 1;
    state.last = "down".to_owned();
    let frame = t.wait_frame(|s| s.contains("Counter: 1"))?;
    let want = expected(&frame, &state);
    assert!(
        frame.diff(&want).is_empty(),
        "80x24, after j:\n{}",
        frame.diff(&want)
    );

    t.resize(60, 14)?;
    state.last = "resize:60x14".to_owned();
    let frame = t.wait_frame(|s| s.contains("last: resize:60x14"))?;
    assert_eq!(frame.size(), (60, 14), "{frame}");
    let want = expected(&frame, &state);
    assert!(
        frame.diff(&want).is_empty(),
        "60x14, after the resize:\n{}",
        frame.diff(&want)
    );

    t.send(Key::Char('q'))?;
    assert!(t.wait_exit()?.success());
    assert!(!t.screen().alternate_screen(), "the terminal was restored");
    Ok(())
}
