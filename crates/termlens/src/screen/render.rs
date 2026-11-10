//! Renderings of a [`Screen`] a person can *see*: ANSI for a terminal, SVG
//! for a bug report or a README, HTML for a pull request or a step summary
//! (#248). All three are pure functions of the screen, derived straight
//! from its cells and styles, treat a wide character as one glyph over two
//! columns, and need no dependency.

use std::fmt::Write as _;

use super::{Cell, Color, Screen, Style};

/// The colour a default foreground renders as where a real colour is
/// needed (SVG, HTML); the ANSI rendering leaves the terminal's own.
const DEFAULT_FG: &str = "#d4d4d4";
/// Same for the background.
const DEFAULT_BG: &str = "#1e1e1e";

/// The xterm 256-colour palette as `#rrggbb`.
fn palette(index: u8) -> String {
    const ANSI: [&str; 16] = [
        "#000000", "#cd3131", "#0dbc79", "#e5e510", "#2472c8", "#bc3fbc", "#11a8cd", "#e5e5e5",
        "#666666", "#f14c4c", "#23d18b", "#f5f543", "#3b8eea", "#d670d6", "#29b8db", "#ffffff",
    ];
    match index {
        0..=15 => ANSI[usize::from(index)].to_owned(),
        16..=231 => {
            let i = index - 16;
            let level = |v: u8| if v == 0 { 0 } else { 55 + 40 * u32::from(v) };
            format!(
                "#{:02x}{:02x}{:02x}",
                level(i / 36),
                level((i / 6) % 6),
                level(i % 6)
            )
        }
        232..=255 => {
            let grey = 8 + 10 * u32::from(index - 232);
            format!("#{grey:02x}{grey:02x}{grey:02x}")
        }
    }
}

fn css_color(color: Color, default: &str) -> String {
    match color {
        Color::Default => default.to_owned(),
        Color::Indexed(i) => palette(i),
        Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
    }
}

/// The foreground and background a cell is *painted* with, reverse video
/// applied, as CSS colours.
fn painted(style: &Style) -> (String, String) {
    let fg = css_color(style.fg, DEFAULT_FG);
    let bg = css_color(style.bg, DEFAULT_BG);
    if style.reverse {
        (bg, fg)
    } else {
        (fg, bg)
    }
}

/// `fg` faded toward `bg` the way SGR 2 fades a glyph: 60% of the
/// foreground over the background, the colour the SVG's `opacity="0.6"`
/// text composites to over its `<rect>`. Both are `#rrggbb`, as
/// [`css_color`] writes them.
fn dimmed(fg: &str, bg: &str) -> String {
    let channel = |hex: &str, at: usize| u32::from_str_radix(&hex[1 + 2 * at..3 + 2 * at], 16);
    let mut out = String::from("#");
    for at in 0..3 {
        let (Ok(f), Ok(b)) = (channel(fg, at), channel(bg, at)) else {
            unreachable!("css_color writes #rrggbb, got {fg} and {bg}");
        };
        let _ = write!(out, "{:02x}", (f * 6 + b * 4 + 5) / 10);
    }
    out
}

/// The SGR parameters that set `style` from a reset terminal.
fn sgr(style: &Style) -> String {
    let mut params = vec!["0".to_owned()];
    for (on, code) in [
        (style.bold, "1"),
        (style.dim, "2"),
        (style.italic, "3"),
        (style.underline, "4"),
        (style.blink, "5"),
        (style.reverse, "7"),
        (style.conceal, "8"),
        (style.strikethrough, "9"),
    ] {
        if on {
            params.push(code.to_owned());
        }
    }
    match style.fg {
        Color::Default => {}
        Color::Indexed(i @ 0..=7) => params.push((30 + u16::from(i)).to_string()),
        Color::Indexed(i @ 8..=15) => params.push((82 + u16::from(i)).to_string()),
        Color::Indexed(i) => params.push(format!("38;5;{i}")),
        Color::Rgb(r, g, b) => params.push(format!("38;2;{r};{g};{b}")),
    }
    match style.bg {
        Color::Default => {}
        Color::Indexed(i @ 0..=7) => params.push((40 + u16::from(i)).to_string()),
        Color::Indexed(i @ 8..=15) => params.push((92 + u16::from(i)).to_string()),
        Color::Indexed(i) => params.push(format!("48;5;{i}")),
        Color::Rgb(r, g, b) => params.push(format!("48;2;{r};{g};{b}")),
    }
    format!("\x1b[{}m", params.join(";"))
}

/// What a cell shows: its text, or a space for a blank.
fn glyph(cell: &Cell) -> &str {
    if cell.contents().is_empty() {
        " "
    } else {
        cell.contents()
    }
}

/// Escape text for an XML (SVG) or HTML text node or attribute value.
///
/// Characters outside the XML 1.0 `Char` production — C0 controls other
/// than tab, LF and CR, and the noncharacters U+FFFE and U+FFFF — cannot be
/// written at all, escaped or not: one of them makes the whole SVG
/// ill-formed, and no parser opens it (#586). They reach here from a cell
/// or a window title, so each becomes U+FFFD, one character for one, which
/// keeps a cell one column wide.
fn escape_xml(text: &str, out: &mut String) {
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\t' | '\n' | '\r' => out.push(c),
            '\u{0}'..='\u{1f}' | '\u{fffe}' | '\u{ffff}' => out.push(char::REPLACEMENT_CHARACTER),
            other => out.push(other),
        }
    }
}

/// One row as runs of consecutive cells sharing a style, wide
/// continuations folded into the cell before them: `(start col, width
/// in columns, style, text)`.
fn runs(screen: &Screen, row: u16) -> Vec<(u16, u16, Style, String)> {
    let mut runs: Vec<(u16, u16, Style, String)> = Vec::new();
    for col in 0..screen.cols() {
        let Some(cell) = screen.cell(row, col) else {
            break;
        };
        if cell.is_wide_continuation() {
            if let Some(last) = runs.last_mut() {
                last.1 += 1;
            }
            continue;
        }
        let style = *cell.style();
        match runs.last_mut() {
            Some(last) if last.2 == style => {
                last.1 += 1;
                last.3.push_str(glyph(cell));
            }
            _ => runs.push((col, 1, style, glyph(cell).to_owned())),
        }
    }
    runs
}

/// The text of one style run as the SVG draws it: `(start col, columns,
/// text)` for each stretch of the run's cells that are all one width.
///
/// A viewer lays a `<text>` out at its font's own advance: at 14 px that
/// is about 8.4 px or less for the fonts in the stack and 14 px for a CJK
/// fallback, and a cell is 9 px. So every piece is written with its length
/// in cells and the viewer fits the glyphs to it (#725). One fit spreads a
/// piece evenly, which puts each glyph on its cell only when they are all
/// as wide as each other: hence a piece per width, and a piece of its own
/// for a wide character a resize left without its second column, which is
/// a wide glyph in one cell.
///
/// Blanks at the end of the run draw nothing as text, since the background
/// `<rect>` is what paints them, and neither does a piece that is blank
/// throughout; both are left out. `keep_blanks` is for a run whose blanks
/// do draw: an underline or a strikethrough runs under them, and an
/// underlined empty form field is all blanks (#586).
fn text_pieces(
    screen: &Screen,
    row: u16,
    start: u16,
    cols: u16,
    keep_blanks: bool,
) -> Vec<(u16, u16, String)> {
    // Each cell that holds a glyph: its column, the columns it takes, and
    // whether the glyph is a wide one.
    let mut cells: Vec<(u16, u16, bool, &str)> = Vec::new();
    for col in start..start.saturating_add(cols) {
        let Some(cell) = screen.cell(row, col) else {
            break;
        };
        if cell.is_wide_continuation() {
            if let Some(last) = cells.last_mut() {
                last.1 += 1;
            }
            continue;
        }
        cells.push((col, 1, cell.is_wide(), glyph(cell)));
    }
    let blank = |text: &str| text.chars().all(char::is_whitespace);
    if !keep_blanks {
        while cells.last().is_some_and(|cell| blank(cell.3)) {
            cells.pop();
        }
    }
    let mut pieces: Vec<(u16, u16, String)> = Vec::new();
    let mut width = None;
    for (col, columns, wide, text) in cells {
        match pieces.last_mut() {
            Some(last) if width == Some((wide, columns)) => {
                last.1 += columns;
                last.2.push_str(text);
            }
            _ => pieces.push((col, columns, text.to_owned())),
        }
        width = Some((wide, columns));
    }
    if !keep_blanks {
        pieces.retain(|piece| !blank(&piece.2));
    }
    pieces
}

impl Screen {
    /// The screen as ANSI: one line per row, every cell painted through SGR
    /// sequences derived from its [`Style`], a reset at each row's end, a
    /// newline after every row. Paste it into a terminal and the failure is
    /// on screen, in colour, in the shape the user saw. The cursor is not
    /// encoded; it is in the header of the text rendering.
    ///
    /// Concealed text is emitted as `SGR 8` and left to the terminal, as a
    /// real one would; default colours are left to the terminal too.
    #[must_use]
    pub fn to_ansi(&self) -> String {
        let mut out = String::new();
        for row in 0..self.rows() {
            for (_, _, style, text) in runs(self, row) {
                if !style.is_default() {
                    out.push_str(&sgr(&style));
                }
                out.push_str(&text);
                if !style.is_default() {
                    out.push_str("\x1b[0m");
                }
            }
            out.push('\n');
        }
        out
    }

    /// The screen as a self-contained SVG: a `<rect>` per background run,
    /// the text of each run in `<text>` elements fitted to the cells they
    /// cover, fixed cell metrics (9×18 px), a monospace fallback font stack
    /// and no font embedding — enough for a bug report or a README, not a
    /// typeset transcript. Concealed text is drawn as blanks, as a terminal
    /// shows it; dim is opacity, and blink is an `<animate>` on the glyph's
    /// own opacity — the background `<rect>` is a sibling, so it keeps
    /// painting, as it does in a terminal.
    ///
    /// No font in the stack advances exactly one cell, so each `<text>`
    /// says how long it is — `textLength`, its cells at 9 px each — and
    /// asks for the glyphs to be fitted to that length
    /// (`lengthAdjust="spacingAndGlyphs"`). Text then stays on the grid the
    /// `<rect>`s are on: a wide character fills its two columns, and the
    /// border of a box ends at its corner. The fit stretches the glyphs as
    /// well as spacing them: a narrow one by the few percent its font's
    /// advance is short of 9 px, and a CJK one, 14 px in its font, to 18,
    /// so it is drawn some 30% wider than a terminal draws it. A run
    /// is split where narrow cells meet wide ones, since one fit suits
    /// glyphs of one width. Two things the fit cannot do: a glyph the
    /// viewer takes from a fallback font of another width shifts its
    /// neighbours within that one `<text>`, and a viewer that ignores
    /// `textLength` lays the text out at its font's own advance, off the
    /// grid.
    ///
    /// Unlike the HTML rendering, the SVG one cannot honour
    /// `prefers-reduced-motion`: SMIL is not reachable from CSS, and an
    /// `<animate>` has no media-query form. A reader who needs the motion
    /// stopped should take the HTML rendering, which does honour it.
    ///
    /// The root carries `role="img"` and a `<title>` naming the picture —
    /// `termlens screen, 80x24`, and the application's own
    /// [`title`](Self::title) after it when one was set. These files are made
    /// to be attached to a pull request or a bug report, where an image with
    /// no accessible name is announced as nothing at all.
    #[must_use]
    pub fn to_svg(&self) -> String {
        const CELL_W: u32 = 9;
        const CELL_H: u32 = 18;
        let width = u32::from(self.cols()) * CELL_W;
        let height = u32::from(self.rows()) * CELL_H;
        let mut out = String::new();
        let _ = writeln!(
            out,
            "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{width}\" height=\"{height}\" \
             viewBox=\"0 0 {width} {height}\" font-family=\"'JetBrains Mono', 'Fira Code', \
             Menlo, Consolas, 'DejaVu Sans Mono', monospace\" font-size=\"14\" \
             role=\"img\">"
        );
        // The accessible name, and the first child because that is where a
        // reader looks for it (#316). The size alone is a poor name but an
        // honest one; the application's own title is the better one when it
        // set one, and it is arbitrary text, so it is escaped like any cell.
        out.push_str("<title>");
        let mut name = format!("termlens screen, {}x{}", self.cols(), self.rows());
        if !self.title().is_empty() {
            name.push_str(": ");
            name.push_str(self.title());
        }
        escape_xml(&name, &mut out);
        out.push_str("</title>\n");
        let _ = writeln!(
            out,
            "<rect width=\"{width}\" height=\"{height}\" fill=\"{DEFAULT_BG}\"/>"
        );
        for row in 0..self.rows() {
            let y = u32::from(row) * CELL_H;
            for (col, cols, style, _) in runs(self, row) {
                let (fg, bg) = painted(&style);
                let x = u32::from(col) * CELL_W;
                if bg != DEFAULT_BG {
                    let _ = writeln!(
                        out,
                        "<rect x=\"{x}\" y=\"{y}\" width=\"{}\" height=\"{CELL_H}\" fill=\"{bg}\"/>",
                        u32::from(cols) * CELL_W
                    );
                }
                // Concealed text is blanks, as a terminal shows it.
                if style.conceal {
                    continue;
                }
                let decorated = style.underline || style.strikethrough;
                for (col, cols, text) in text_pieces(self, row, col, cols, decorated) {
                    // The length is the piece's cells, and the glyphs are
                    // fitted to it, not only spaced out over it: stretched
                    // by the few percent their advance is short of the
                    // cell, box-drawing characters go on meeting each other.
                    let _ = write!(
                        out,
                        "<text x=\"{}\" y=\"{}\" textLength=\"{}\" \
                         lengthAdjust=\"spacingAndGlyphs\" fill=\"{fg}\" xml:space=\"preserve\"",
                        u32::from(col) * CELL_W,
                        y + 14,
                        u32::from(cols) * CELL_W
                    );
                    if style.bold {
                        out.push_str(" font-weight=\"bold\"");
                    }
                    if style.italic {
                        out.push_str(" font-style=\"italic\"");
                    }
                    if style.dim {
                        out.push_str(" opacity=\"0.6\"");
                    }
                    let mut decoration = Vec::new();
                    if style.underline {
                        decoration.push("underline");
                    }
                    if style.strikethrough {
                        decoration.push("line-through");
                    }
                    if !decoration.is_empty() {
                        let _ = write!(out, " text-decoration=\"{}\"", decoration.join(" "));
                    }
                    out.push('>');
                    escape_xml(&text, &mut out);
                    if style.blink {
                        // A hard toggle, not a fade, and the glyph alone:
                        // the background `<rect>` above is a sibling. An
                        // SMIL animation outranks the attribute it
                        // animates, so a dim blink carries dim's 0.6 in its
                        // values or it would animate back to full opacity.
                        let opacity = if style.dim { "0.6" } else { "1" };
                        let _ = write!(
                            out,
                            "<animate attributeName=\"opacity\" values=\"{opacity};0\" \
                             calcMode=\"discrete\" dur=\"1s\" repeatCount=\"indefinite\"/>"
                        );
                    }
                    out.push_str("</text>\n");
                }
            }
        }
        out.push_str("</svg>\n");
        out
    }

    /// The screen as HTML: a `<pre>` with one `<span style>` per run, so it
    /// pastes into a GitHub step summary or a pull-request comment. The
    /// same run model as [`to_svg`](Self::to_svg); concealed text is shown
    /// as blanks, and blink is a `termlens-blink` animation on the run's
    /// colour — not its opacity — so a dim blinking run keeps its dim and
    /// the run's background keeps painting, as it does in a terminal. Dim
    /// is the same picture as the SVG's: the foreground at 60% over the
    /// run's background, written as that blended colour, so the background
    /// stays at full strength. The
    /// `@keyframes` live in a `<style>` element inside the `<pre>` (a
    /// `style` attribute cannot hold one) and travel only when a run
    /// blinks, so a screen without one renders as it always has. A
    /// `prefers-reduced-motion` rule beside them stops the animation for
    /// a reader who asked for that; the selector matches the inline style,
    /// since that is where the `animation` lands.
    #[must_use]
    pub fn to_html(&self) -> String {
        let mut out = format!(
            "<pre style=\"background:{DEFAULT_BG};color:{DEFAULT_FG};font-family:monospace;\
             line-height:1.2;padding:8px\">"
        );
        if (0..self.rows()).any(|row| runs(self, row).iter().any(|(_, _, style, _)| style.blink)) {
            out.push_str(
                "<style>@keyframes termlens-blink{50%{color:transparent}}\
                 @media(prefers-reduced-motion:reduce){\
                 [style*=termlens-blink]{animation:none}}</style>",
            );
        }
        for row in 0..self.rows() {
            for (_, cols, style, text) in runs(self, row) {
                let shown: String = if style.conceal {
                    " ".repeat(usize::from(cols))
                } else {
                    text
                };
                if style.is_default() {
                    escape_xml(&shown, &mut out);
                    continue;
                }
                let (fg, bg) = painted(&style);
                // Dim fades the glyph and not the cell, as the SVG's text
                // opacity does over its `<rect>`: an `opacity` on the span
                // would fade the run's background with it (#645).
                let fg = if style.dim { dimmed(&fg, &bg) } else { fg };
                let mut css = format!("color:{fg};background:{bg}");
                if style.bold {
                    css.push_str(";font-weight:bold");
                }
                if style.italic {
                    css.push_str(";font-style:italic");
                }
                if style.blink {
                    css.push_str(";animation:termlens-blink 1s step-end infinite");
                }
                let mut decoration = Vec::new();
                if style.underline {
                    decoration.push("underline");
                }
                if style.strikethrough {
                    decoration.push("line-through");
                }
                if !decoration.is_empty() {
                    let _ = write!(css, ";text-decoration:{}", decoration.join(" "));
                }
                let _ = write!(out, "<span style=\"{css}\">");
                escape_xml(&shown, &mut out);
                out.push_str("</span>");
            }
            out.push('\n');
        }
        out.push_str("</pre>\n");
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_palette_is_xterms() {
        assert_eq!(palette(1), "#cd3131");
        assert_eq!(palette(16), "#000000");
        assert_eq!(palette(21), "#0000ff");
        assert_eq!(palette(196), "#ff0000");
        assert_eq!(palette(232), "#080808");
        assert_eq!(palette(255), "#eeeeee");
    }

    #[test]
    fn sgr_sets_the_style_from_a_reset() {
        let mut style = Style {
            bold: true,
            fg: Color::Indexed(1),
            bg: Color::Rgb(30, 30, 46),
            ..Style::default()
        };
        assert_eq!(sgr(&style), "\x1b[0;1;31;48;2;30;30;46m");
        style.fg = Color::Indexed(9);
        assert_eq!(sgr(&style), "\x1b[0;1;91;48;2;30;30;46m");
        style.fg = Color::Indexed(200);
        assert_eq!(sgr(&style), "\x1b[0;1;38;5;200;48;2;30;30;46m");
    }
}
