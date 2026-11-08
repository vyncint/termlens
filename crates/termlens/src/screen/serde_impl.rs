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

use std::fmt;

use serde::de::{DeserializeSeed, Error as _, IgnoredAny, MapAccess, SeqAccess, Visitor};
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

/// What is read back: the [`Wire`] fields, owned, less `format`, which
/// [`WireVisitor`] checks as it reads it.
struct OwnedWire {
    cols: u16,
    rows: u16,
    cursor: Cursor,
    cells: Vec<Vec<Cell>>,
    state: TermState,
}

/// The fields, in the order [`Wire`] writes them, which is the order a
/// format without field names reads them in.
const FIELDS: &[&str] = &["format", "cols", "rows", "cursor", "cells", "state"];

/// A field name; one this build does not know is skipped, as the derive
/// that read [`OwnedWire`] until #713 skipped it.
#[derive(Deserialize)]
#[serde(field_identifier, rename_all = "snake_case")]
enum Field {
    Format,
    Cols,
    Rows,
    Cursor,
    Cells,
    State,
    #[serde(other)]
    Other,
}

/// The refusal of a format this build does not read.
fn unknown_format<E: serde::de::Error>(format: u32) -> E {
    E::custom(format!(
        "saved-screen JSON format {format} is not one this termlens reads (it reads format {JSON_FORMAT}); \
         a newer termlens wrote this file"
    ))
}

/// [`OwnedWire`] by hand, so the grid is read against the header (#713).
///
/// A derive read `cells` into vectors before anything looked at their
/// count, so the file's size bounded the memory and not the screen's: a
/// 1x1 header over 67 million empty rows, 201 MB on disk, took 1.77 GB to
/// read before it was refused. The grid is read through [`Grid`] instead,
/// which stops at the first row or cell past what `rows` and `cols`
/// declare. Otherwise this is the derive's document: the same fields,
/// `format` defaulting to 1 (0.10 wrote none, and its shape is format 1),
/// an unknown field skipped, a missing or repeated one an error.
struct WireVisitor;

impl<'de> Visitor<'de> for WireVisitor {
    type Value = OwnedWire;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("struct OwnedWire")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<OwnedWire, A::Error> {
        let mut format: Option<u32> = None;
        let mut cols: Option<u16> = None;
        let mut rows: Option<u16> = None;
        let mut cursor: Option<Cursor> = None;
        let mut cells: Option<Vec<Vec<Cell>>> = None;
        let mut state: Option<TermState> = None;
        fn once<T, E: serde::de::Error>(slot: &Option<T>, name: &'static str) -> Result<(), E> {
            match slot {
                Some(_) => Err(E::duplicate_field(name)),
                None => Ok(()),
            }
        }
        while let Some(key) = map.next_key::<Field>()? {
            match key {
                Field::Format => {
                    once(&format, "format")?;
                    let read: u32 = map.next_value()?;
                    // Refused at once: a newer format's grid is not this
                    // one's, and its shape is not worth reading to say so.
                    if read != JSON_FORMAT {
                        return Err(unknown_format(read));
                    }
                    format = Some(read);
                }
                Field::Cols => {
                    once(&cols, "cols")?;
                    cols = Some(map.next_value()?);
                }
                Field::Rows => {
                    once(&rows, "rows")?;
                    rows = Some(map.next_value()?);
                }
                Field::Cursor => {
                    once(&cursor, "cursor")?;
                    cursor = Some(map.next_value()?);
                }
                Field::Cells => {
                    once(&cells, "cells")?;
                    cells = Some(map.next_value_seed(Grid { cols, rows })?);
                }
                Field::State => {
                    once(&state, "state")?;
                    state = Some(map.next_value()?);
                }
                Field::Other => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        Ok(OwnedWire {
            cols: cols.ok_or_else(|| A::Error::missing_field("cols"))?,
            rows: rows.ok_or_else(|| A::Error::missing_field("rows"))?,
            cursor: cursor.ok_or_else(|| A::Error::missing_field("cursor"))?,
            cells: cells.ok_or_else(|| A::Error::missing_field("cells"))?,
            state: state.ok_or_else(|| A::Error::missing_field("state"))?,
        })
    }

    /// A format without field names writes the fields in order, so the
    /// header is read before the grid.
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<OwnedWire, A::Error> {
        let format = seq.next_element()?.unwrap_or(JSON_FORMAT);
        if format != JSON_FORMAT {
            return Err(unknown_format(format));
        }
        let missing =
            |index: usize| A::Error::invalid_length(index, &"struct OwnedWire with 6 elements");
        let cols: u16 = seq.next_element()?.ok_or_else(|| missing(1))?;
        let rows: u16 = seq.next_element()?.ok_or_else(|| missing(2))?;
        let cursor = seq.next_element()?.ok_or_else(|| missing(3))?;
        let cells = seq
            .next_element_seed(Grid {
                cols: Some(cols),
                rows: Some(rows),
            })?
            .ok_or_else(|| missing(4))?;
        let state = seq.next_element()?.ok_or_else(|| missing(5))?;
        Ok(OwnedWire {
            cols,
            rows,
            cursor,
            cells,
            state,
        })
    }
}

/// How many rows or cells to read for a declared count: the count, or the
/// per-axis limit when there is none yet (the grid came before the header)
/// or it is one no screen has, which the checks after the read refuse.
fn bound(declared: Option<u16>) -> usize {
    match declared {
        Some(n) if (1..=MAX_DIMENSION).contains(&n) => usize::from(n),
        _ => usize::from(MAX_DIMENSION),
    }
}

/// `cells`, read against the declared size (#713). Each row is read
/// through [`Row`], and the first one past the count is refused, so the
/// grid never holds more than `rows` rows of at most `cols` cells.
struct Grid {
    cols: Option<u16>,
    rows: Option<u16>,
}

impl<'de> DeserializeSeed<'de> for Grid {
    type Value = Vec<Vec<Cell>>;

    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<Self::Value, D::Error> {
        deserializer.deserialize_seq(self)
    }
}

impl<'de> Visitor<'de> for Grid {
    type Value = Vec<Vec<Cell>>;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a sequence")
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
        let most = bound(self.rows);
        let mut grid = Vec::with_capacity(seq.size_hint().unwrap_or(0).min(most));
        while let Some(row) = seq.next_element_seed(Row {
            index: grid.len(),
            cols: self.cols,
        })? {
            if grid.len() == most {
                return Err(A::Error::custom(match (self.cols, self.rows) {
                    (Some(cols), Some(rows)) if usize::from(rows) == most => {
                        format!("a {cols}x{rows} screen needs {rows} rows of cells, not more")
                    }
                    _ => format!(
                        "more than {MAX_DIMENSION} rows of cells, past the \
                         {MAX_DIMENSION}-per-axis limit of a terminal"
                    ),
                }));
            }
            grid.push(row);
        }
        Ok(grid)
    }
}

/// One row of `cells`, held to the declared width as [`Grid`] holds the
/// rows to the declared count.
struct Row {
    index: usize,
    cols: Option<u16>,
}

impl<'de> DeserializeSeed<'de> for Row {
    type Value = Vec<Cell>;

    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<Self::Value, D::Error> {
        deserializer.deserialize_seq(self)
    }
}

impl<'de> Visitor<'de> for Row {
    type Value = Vec<Cell>;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a sequence")
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
        let most = bound(self.cols);
        let mut row = Vec::with_capacity(seq.size_hint().unwrap_or(0).min(most));
        while let Some(cell) = seq.next_element::<Cell>()? {
            if row.len() == most {
                let index = self.index;
                return Err(A::Error::custom(match self.cols {
                    Some(cols) if usize::from(cols) == most => {
                        format!(
                            "row {index} of a {cols}-column screen holds more than {cols} cells"
                        )
                    }
                    _ => format!(
                        "row {index} holds more than {MAX_DIMENSION} cells, past the \
                         {MAX_DIMENSION}-per-axis limit of a terminal"
                    ),
                }));
            }
            row.push(cell);
        }
        Ok(row)
    }
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
        let wire = deserializer.deserialize_struct("OwnedWire", FIELDS, WireVisitor)?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::screen::Style;

    fn cell() -> String {
        serde_json::to_string(&Cell::new("x".into(), Style::default(), false, false)).unwrap()
    }

    const CURSOR: &str = r#""cursor":{"row":0,"col":0,"visible":true}"#;

    /// The grid is checked against the header while it is read (#713): the
    /// row past the declared count is refused before anything after it is
    /// read, so the text after it here — not JSON at all — is never seen.
    /// A file of empty rows used to cost a vector per row first.
    #[test]
    fn a_row_past_the_declared_count_is_refused_before_the_rest_is_read() {
        let cell = cell();
        let json =
            format!(r#"{{"format":1,"cols":1,"rows":1,{CURSOR},"cells":[[{cell}],[], not json"#);
        let err = serde_json::from_str::<Screen>(&json)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("a 1x1 screen needs 1 rows of cells, not more"),
            "{err}"
        );
    }

    /// A row is held to the declared width the same way (#713).
    #[test]
    fn a_cell_past_the_declared_width_is_refused_before_the_rest_is_read() {
        let cell = cell();
        let json = format!(
            r#"{{"format":1,"cols":1,"rows":2,{CURSOR},"cells":[[{cell}],[{cell},{cell}, not json"#
        );
        let err = serde_json::from_str::<Screen>(&json)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("row 1 of a 1-column screen holds more than 1 cells"),
            "{err}"
        );
    }

    /// The header need not come first — `jq -S` sorts `cells` ahead of
    /// `cols` — so a grid read before it is held to the largest a terminal
    /// has (#713), and a document in any key order still reads.
    #[test]
    fn a_grid_read_before_the_header_is_held_to_the_terminal_limit() {
        let rows = "[],".repeat(1000);
        let json = format!(r#"{{"cells":[{rows}[], not json"#);
        let err = serde_json::from_str::<Screen>(&json)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("more than 1000 rows of cells, past the 1000-per-axis limit"),
            "{err}"
        );
        let cell = cell();
        let wide = format!(
            r#"{{"cells":[[{}{cell}, not json"#,
            format!("{cell},").repeat(1000)
        );
        let err = serde_json::from_str::<Screen>(&wide)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("row 0 holds more than 1000 cells, past the 1000-per-axis limit"),
            "{err}"
        );

        let screen = Screen::parse("size: 3x2  cursor: 1,2\nab\ncd").unwrap();
        let value = serde_json::to_value(&screen).unwrap();
        let reversed = ["state", "cells", "cursor", "rows", "cols", "format"]
            .iter()
            .map(|key| format!("{key:?}:{}", value[key]))
            .collect::<Vec<_>>()
            .join(",");
        let read: Screen = serde_json::from_str(&format!("{{{reversed}}}")).unwrap();
        assert_eq!(read, screen);
    }

    /// What the derive refused and allowed, the hand-written reader does
    /// too: a missing or repeated field is an error, an unknown one is
    /// skipped, and a 0.10 file without `format` reads as format 1.
    #[test]
    fn the_reader_keeps_the_derived_field_rules() {
        let screen = Screen::parse("size: 2x1  cursor: 0,1\nab").unwrap();
        let mut value = serde_json::to_value(&screen).unwrap();
        let object = value.as_object_mut().unwrap();
        object.insert("added_by_a_later_release".into(), serde_json::json!([1, 2]));
        object.remove("format");
        let read: Screen = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(read, screen);

        value.as_object_mut().unwrap().remove("cells");
        let err = serde_json::from_value::<Screen>(value)
            .unwrap_err()
            .to_string();
        assert!(err.contains("missing field `cells`"), "{err}");

        let json = serde_json::to_string(&screen).unwrap();
        let twice = json.replacen(r#""cols":2,"#, r#""cols":2,"cols":2,"#, 1);
        let err = serde_json::from_str::<Screen>(&twice)
            .unwrap_err()
            .to_string();
        assert!(err.contains("duplicate field `cols`"), "{err}");

        // A format without field names: the fields in order, which
        // serde_json reads from an array; the grid is still bounded.
        let value = serde_json::to_value(&screen).unwrap();
        let array: Vec<_> = FIELDS.iter().map(|key| value[key].clone()).collect();
        let read: Screen = serde_json::from_value(serde_json::json!(array)).unwrap();
        assert_eq!(read, screen);
        let mut longer = array.clone();
        longer[4]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!([]));
        let err = serde_json::from_value::<Screen>(serde_json::json!(longer))
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("a 2x1 screen needs 1 rows of cells, not more"),
            "{err}"
        );

        let newer = json.replacen(r#""format":1"#, r#""format":2"#, 1);
        let err = serde_json::from_str::<Screen>(&newer)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("format 2 is not one this termlens reads"),
            "{err}"
        );
    }
}
