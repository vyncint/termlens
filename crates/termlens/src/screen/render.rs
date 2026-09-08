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

fn escape_xml(text: &str, out: &mut String) {
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
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

    /// The screen as a self-contained SVG: a `<rect>` per background run, a
    /// `<text>` per foreground run, fixed cell metrics (9×18 px), a
    /// monospace fallback font stack and no font embedding — enough for a
    /// bug report or a README, not a typeset transcript. Concealed text is
    /// drawn as blanks, as a terminal shows it; dim is opacity.
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
             Menlo, Consolas, 'DejaVu Sans Mono', monospace\" font-size=\"14\">"
        );
        let _ = writeln!(
            out,
            "<rect width=\"{width}\" height=\"{height}\" fill=\"{DEFAULT_BG}\"/>"
        );
        for row in 0..self.rows() {
            let y = u32::from(row) * CELL_H;
            for (col, cols, style, text) in runs(self, row) {
                let (fg, bg) = painted(&style);
                let x = u32::from(col) * CELL_W;
                if bg != DEFAULT_BG {
                    let _ = writeln!(
                        out,
                        "<rect x=\"{x}\" y=\"{y}\" width=\"{}\" height=\"{CELL_H}\" fill=\"{bg}\"/>",
                        u32::from(cols) * CELL_W
                    );
                }
                // Trailing blanks draw nothing as text — the background run
                // above is what paints them — so the element carries only
                // the glyphs. Concealed text is blanks, as a terminal shows it.
                let shown = if style.conceal { "" } else { text.trim_end() };
                if shown.is_empty() {
                    continue;
                }
                let _ = write!(
                    out,
                    "<text x=\"{x}\" y=\"{}\" fill=\"{fg}\" xml:space=\"preserve\"",
                    y + 14
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
                escape_xml(shown, &mut out);
                out.push_str("</text>\n");
            }
        }
        out.push_str("</svg>\n");
        out
    }

    /// The screen as HTML: a `<pre>` with one `<span style>` per run, so it
    /// pastes into a GitHub step summary or a pull-request comment. The
    /// same run model as [`to_svg`](Self::to_svg); concealed text is shown
    /// as blanks.
    #[must_use]
    pub fn to_html(&self) -> String {
        let mut out = format!(
            "<pre style=\"background:{DEFAULT_BG};color:{DEFAULT_FG};font-family:monospace;\
             line-height:1.2;padding:8px\">"
        );
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
                let mut css = format!("color:{fg};background:{bg}");
                if style.bold {
                    css.push_str(";font-weight:bold");
                }
                if style.italic {
                    css.push_str(";font-style:italic");
                }
                if style.dim {
                    css.push_str(";opacity:0.6");
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
