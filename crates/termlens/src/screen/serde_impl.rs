//! `Serialize`/`Deserialize` for [`Screen`] (feature `serde`).
//!
//! The shape is **rows of cells**, not a flat vector, so a JSON snapshot
//! diffs by row; the header fields and the out-of-band state ride beside
//! the grid. Reading a screen back re-establishes the one invariant
//! `from_parts` only debug-asserts — every row holds exactly `cols` cells
//! and there are exactly `rows` of them — through a validating constructor
//! rather than a bare derive, so a hand-edited or truncated file is an
//! error and never a screen that panics on its first `cell()`.
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

use super::{Cell, Screen, TermState};

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
        if wire.cursor.row >= wire.rows.max(1) || wire.cursor.col > wire.cols {
            return Err(D::Error::custom(format!(
                "cursor {},{} is outside a {}x{} screen",
                wire.cursor.row, wire.cursor.col, wire.cols, wire.rows
            )));
        }
        Ok(Screen::from_parts(
            wire.cols,
            wire.rows,
            wire.cursor.row,
            wire.cursor.col,
            wire.cursor.visible,
            wire.cells.into_iter().flatten().collect(),
            wire.state,
        ))
    }
}
