//! `Serialize`/`Deserialize` for [`Screen`] (feature `serde`).
//!
//! The shape is **rows of cells**, not a flat vector, so a JSON snapshot
//! diffs by row; the header fields and the out-of-band state ride beside
//! the grid. Reading a screen back re-establishes the one invariant
//! `from_parts` only debug-asserts — every row holds exactly `cols` cells
//! and there are exactly `rows` of them — through a validating constructor
//! rather than a bare derive, so a hand-edited or truncated file is an
//! error and never a screen that panics on its first `cell()`.

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

/// What a [`Screen`] is on the wire.
#[derive(Serialize)]
struct Wire<'a> {
    cols: u16,
    rows: u16,
    cursor: Cursor,
    cells: Vec<&'a [Cell]>,
    state: &'a TermState,
}

#[derive(Deserialize)]
struct OwnedWire {
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
