//! How many columns the grid gives a run of text.
//!
//! Neither `unicode-width` measure is the grid's. The emulator (vt100 0.18)
//! lays text out a character at a time: each takes its own width, at most
//! two columns; a zero-width character joins the cell before it; and an
//! emoji-presentation selector (U+FE0F) makes that cell two columns wide
//! when the text it completes is. So a ZWJ family is one cluster to
//! `UnicodeWidthStr` but two wide cells here, and `⚠️` is one column to a
//! sum of character widths but two here. Every place that turns text into
//! columns asks this instead (#580).

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// The emoji-presentation selector, the one zero-width character that can
/// change the width of the cell it joins.
pub(crate) const VS16: char = '\u{FE0F}';

/// How many columns `text` takes when drawn on one row from the cursor,
/// laid out exactly as the emulator lays it out.
///
/// A zero-width character at the very start joins a cell before the run,
/// which is not this text's to count.
pub(crate) fn columns(text: &str) -> usize {
    let mut total = 0;
    // The last cell this text started, and its width, so a selector after
    // it can widen it.
    let mut cell = String::new();
    let mut cell_width = 0;
    for c in text.chars() {
        let width = match c.width() {
            Some(width) => width.min(2),
            // vt100 draws no control character, and treats any other
            // character without a width as one column.
            None if u32::from(c) < 256 => continue,
            None => 1,
        };
        if width > 0 {
            total += width;
            cell.clear();
            cell.push(c);
            cell_width = width;
        } else if !cell.is_empty() {
            cell.push(c);
            if c == VS16 {
                let widened = if cell.width() > 1 { 2 } else { 1 };
                total = total - cell_width + widened;
                cell_width = widened;
            }
        }
    }
    total
}

/// Whether a cell holding `contents` is two columns wide: its first
/// character is, or an emoji-presentation selector makes the cluster so.
pub(crate) fn cell_is_wide(contents: &str) -> bool {
    columns(contents) == 2
}

#[cfg(test)]
mod tests {
    use super::{cell_is_wide, columns};

    #[test]
    fn narrow_and_wide_characters_count_their_own_width() {
        assert_eq!(columns("abc"), 3);
        assert_eq!(columns("\u{6f22}\u{5b57}"), 4);
        assert_eq!(columns(""), 0);
    }

    #[test]
    fn a_zero_width_character_joins_the_cell_before_it() {
        // e + combining acute.
        assert_eq!(columns("e\u{301}x"), 2);
        // Nothing before it in the run: it joins a cell this text did not
        // start, and costs nothing here.
        assert_eq!(columns("\u{301}x"), 1);
    }

    #[test]
    fn a_ligature_is_still_one_cell_per_letter() {
        // Lam-alef is one column to the string measure, two letters here.
        assert_eq!(unicode_width::UnicodeWidthStr::width("\u{644}\u{627}"), 1);
        assert_eq!(columns("\u{644}\u{627}"), 2);
    }

    #[test]
    fn a_zwj_family_is_two_wide_cells_not_one_cluster() {
        // 👨 ZWJ 👩: the joiner joins the man's cell, the woman starts one.
        let family = "\u{1f468}\u{200d}\u{1f469}";
        assert_eq!(unicode_width::UnicodeWidthStr::width(family), 2);
        assert_eq!(columns(family), 4);
    }

    #[test]
    fn the_presentation_selector_widens_the_emoji_it_follows() {
        // ⚠ (narrow) + VS16 is two columns; a skin tone modifier is a wide
        // character of its own.
        assert_eq!(columns("\u{26a0}\u{fe0f}X"), 3);
        assert_eq!(columns("\u{1f44d}\u{1f3fd}"), 4);
        // The selector on a letter changes nothing.
        assert_eq!(columns("a\u{fe0f}"), 1);
        assert!(cell_is_wide("\u{26a0}\u{fe0f}"));
        assert!(!cell_is_wide("\u{26a0}"));
        assert!(cell_is_wide("\u{6f22}"));
    }

    #[test]
    fn control_characters_take_no_column() {
        assert_eq!(columns("a\u{7}b\u{85}c"), 3);
    }
}
