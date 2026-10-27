//! `Serialize`/`Deserialize` for [`Screen`] (feature `serde`).
//!
//! The shape is **rows of cells**, not a flat vector, so a JSON snapshot
//! diffs by row; the header fields and the out-of-band state ride beside
//! the grid. Reading a screen back re-establishes the one invariant
//! `from_parts` only debug-asserts — every row holds exactly `cols` cells
//! and there are exactly `rows` of them — through a validating constructor
//! rather than a bare derive, so a hand-edited or truncated file is an
//! error and never a screen that panics on its first `cell()`. Each cell is
//! held to what a termlens could have written into one (#582): the CLI
//! prints what it reads, and a cell carrying `ESC` drove the terminal of
//! whoever ran `termlens render --text` on the file.
//!
//! The JSON is a persisted artifact, not a wire between two copies of one
//! version: `termlens diff` and `termlens render` read it back as a saved
//! screen, and consumers commit it. So it carries a **format number**
//! (#329). `"format": 1` is the shape specified in `docs/DESIGN.md` §3;
//! a file written before the field existed (0.10) reads as format 1, since
//! that is what it is; a number this build does not know is refused with
//! a message naming it rather than read as far as the fields happen to
//! line up.

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::{parse, Cell, MouseModes, Screen, TermState};
use crate::terminal::MAX_DIMENSION;

/// The cursor as three named fields rather than a tuple, so the JSON reads.
#[derive(Serialize, Deserialize)]
struct Cursor {
    row: u16,
    col: u16,
    visible: bool,
}

/// The one shape this build writes and reads. A new number is a new
/// stability candidate or a major version, never a silent change
/// (`docs/STABILITY.md`).
const JSON_FORMAT: u32 = 1;

/// What a [`Screen`] is on the wire. `format` first, so a reader sees it
/// before the grid.
#[derive(Serialize)]
struct Wire<'a> {
    format: u32,
    cols: u16,
    rows: u16,
    cursor: Cursor,
    cells: Vec<&'a [Cell]>,
    state: &'a TermState,
}

/// 0.10 wrote no `format` field; its shape is format 1.
fn format_when_absent() -> u32 {
    JSON_FORMAT
}

#[derive(Deserialize)]
struct OwnedWire {
    #[serde(default = "format_when_absent")]
    format: u32,
    cols: u16,
    rows: u16,
    cursor: Cursor,
    cells: Vec<Vec<Cell>>,
    state: TermState,
}

impl Serialize for Screen {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let width = usize::from(self.cols).max(1);
        Wire {
            format: JSON_FORMAT,
            cols: self.cols,
            rows: self.rows,
            cursor: Cursor {
                row: self.cursor_row,
                col: self.cursor_col,
                visible: self.cursor_visible,
            },
            cells: self.cells.chunks(width).collect(),
            state: &self.state,
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Screen {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let wire = OwnedWire::deserialize(deserializer)?;
        if wire.format != JSON_FORMAT {
            return Err(D::Error::custom(format!(
                "saved-screen JSON format {} is not one this termlens reads (it reads format {JSON_FORMAT}); \
                 a newer termlens wrote this file",
                wire.format
            )));
        }
        if wire.cols > MAX_DIMENSION || wire.rows > MAX_DIMENSION {
            return Err(D::Error::custom(format!(
                "{}x{} is past the {MAX_DIMENSION}-per-axis limit of a terminal, \
                 so no termlens wrote this screen",
                wire.cols, wire.rows
            )));
        }
        // `spawn` and `resize` refuse a terminal under 2x2 and the text
        // reader a screen under 1x1, so no termlens made an empty one. Read
        // anyway, one of no columns re-serialised as a document of no rows,
        // which this reader then refused.
        if wire.cols == 0 || wire.rows == 0 {
            return Err(D::Error::custom(format!(
                "a screen has at least one column and one row, got {}x{}",
                wire.cols, wire.rows
            )));
        }
        if wire.cells.len() != usize::from(wire.rows) {
            return Err(D::Error::custom(format!(
                "a {}x{} screen needs {} rows of cells, not {}",
                wire.cols,
                wire.rows,
                wire.rows,
                wire.cells.len()
            )));
        }
        if let Some((row, cells)) = wire
            .cells
            .iter()
            .enumerate()
            .find(|(_, row)| row.len() != usize::from(wire.cols))
        {
            return Err(D::Error::custom(format!(
                "row {row} of a {}-column screen holds {} cells",
                wire.cols,
                cells.len()
            )));
        }
        for (row, cells) in wire.cells.iter().enumerate() {
            check_row(cells)
                .map_err(|(col, why)| D::Error::custom(format!("cell {row},{col}: {why}")))?;
        }
        // History rows keep the width they were captured at, so they are
        // checked as rows of their own.
        for (row, cells) in wire
            .state
            .scrollback_cells
            .iter()
            .flat_map(|h| h.iter())
            .enumerate()
        {
            check_row(cells).map_err(|(col, why)| {
                D::Error::custom(format!("scrollback cell {row},{col}: {why}"))
            })?;
        }
        // `col == cols` is the pending-wrap position a terminal parks at
        // after a write fills the last row. Every release up to 0.11.1 wrote
        // it (#401 stops the emulator producing it), so refusing it here
        // would make JSON those releases saved unreadable -- which the format
        // promise in docs/STABILITY.md forbids. Clamp it onto the last cell
        // instead, which is where #401 now reports it, so such a file also
        // round-trips through `render --text` (#375). Any other position off
        // the grid is a file nothing wrote, and is refused.
        let cursor_col = if wire.cursor.col == wire.cols {
            wire.cols - 1
        } else {
            wire.cursor.col
        };
        if wire.cursor.row >= wire.rows || cursor_col >= wire.cols {
            return Err(D::Error::custom(format!(
                "cursor {},{} is outside a {}x{} screen",
                wire.cursor.row, wire.cursor.col, wire.cols, wire.rows
            )));
        }
        Ok(Screen::from_parts(
            wire.cols,
            wire.rows,
            wire.cursor.row,
            cursor_col,
            wire.cursor.visible,
            wire.cells.into_iter().flatten().collect(),
            wire.state,
        ))
    }
}

/// Only the four tracking modes, whatever the file says (#582). The derive
/// took any byte, and a stray bit made `len()` count a mode `iter()` never
/// yields: 8 against 4 for `255`. No termlens wrote one — the tracker sets
/// exactly those four bits — so masking drops nothing a writer recorded,
/// and unlike a refusal it still reads a file whose writer knew a fifth.
impl<'de> Deserialize<'de> for MouseModes {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // The derive's own shape, a newtype named `MouseModes` over the
        // bits, so no serde format reads a different document than before.
        #[derive(Deserialize)]
        #[serde(rename = "MouseModes")]
        struct Bits(u8);
        let Bits(bits) = Bits::deserialize(deserializer)?;
        Ok(MouseModes::from_bits(bits))
    }
}

/// Whether `cells`, a row of the grid or of the history, could have come
/// from a termlens; `Err` names the first column that could not, and why.
///
/// These are the rules `Screen::parse` builds its cells by, less every one
/// a width table decides (#582). Whether a character is a mark, one column
/// or two is an entry in `unicode_width`'s tables, and the tables move
/// between releases — 0.2.1 to 0.2.2, both of which vt100 0.16 accepted,
/// moved 197 code points — so a cell another release wrote correctly can
/// look malformed under this one's. Two facts are no table's: a control
/// character has no width, and printable ASCII is one column. Checks built
/// from those, and from the wide flags, refuse only what no termlens wrote.
///
/// One rule of the text reader's is left out on purpose: a wide character
/// need not be followed by its second column. vt100 0.16, behind every
/// release up to 0.11.6, narrowed a row by truncating it, so a resize that
/// cut a wide character left it in the last column, and widening again
/// left it before a blank.
fn check_row(cells: &[Cell]) -> Result<(), (usize, String)> {
    for (col, cell) in cells.iter().enumerate() {
        check_cell(cell).map_err(|why| (col, why))?;
        // vt100 clears or moves both halves of a wide character together,
        // and a resize cuts a row from the right, so it can strand a first
        // column but never a second.
        let after_wide = col.checked_sub(1).is_some_and(|before| cells[before].wide);
        if cell.wide_continuation && !after_wide {
            return Err((
                col,
                "a wide character's second column with no wide character before it".into(),
            ));
        }
    }
    Ok(())
}

/// One cell's contents and flags, by the rules [`check_row`] explains.
fn check_cell(cell: &Cell) -> Result<(), String> {
    for ch in cell.contents.chars() {
        // A control character, asked the way the text reader asks. No
        // termlens wrote one into a cell, since vt100 drops them before the
        // grid, and `render --text` and `diff` would print it to a terminal.
        parse::cell_width(ch)?;
    }
    let mut chars = cell.contents.chars();
    let Some(first) = chars.next() else {
        return if cell.wide {
            Err("marked wide but holds no character".into())
        } else {
            Ok(())
        };
    };
    if cell.wide_continuation {
        return Err(format!(
            "a wide character's second column holding {:?}",
            cell.contents
        ));
    }
    // vt100 marks a cell wide for its first character's own width, which
    // printable ASCII never has, or, from 0.18, for the whole cell's once a
    // U+FE0F asks for emoji presentation: a table makes the keycaps `1️⃣`
    // and `#️⃣` two columns.
    if cell.wide && one_column(first) && !cell.contents.contains('\u{fe0f}') {
        return Err(format!("{first:?} is one column wide but marked wide"));
    }
    // vt100 appends to a cell only what its table calls zero-width, which
    // printable ASCII never is.
    if chars.any(one_column) {
        return Err(format!(
            "{:?} is more than one character; a cell holds one and the zero-width marks after it",
            cell.contents
        ));
    }
    Ok(())
}

/// Printable ASCII: the characters every `unicode_width` release makes one
/// column wide in code, ahead of its tables.
fn one_column(ch: char) -> bool {
    matches!(ch, ' '..='~')
}
