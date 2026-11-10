//! Leaving a test: `Screen::diff` (#246), the ANSI/SVG/HTML renderings
//! (#248) and, with the `serde` feature, a `Screen` as JSON and back (#247).

use std::time::Duration;

use termlens::{Key, Screen, Terminal};

mod common;

fn emit(steps: &[&str]) -> termlens::Result<Terminal> {
    common::spawn_emit(
        Terminal::builder()
            .size(30, 4)
            .timeout(Duration::from_secs(10)),
        steps,
    )
}

/// A small styled screen every rendering below is made from: a bold cyan
/// title, a reverse-video highlight, a dim note, a concealed field, a wide
/// character and an RGB background.
///
/// It sets its window title too, and to something holding `<` and `&`: the
/// SVG names itself with it (#316), so the rendering has to escape it. The
/// title is set explicitly rather than left unset because ConPTY sets one of
/// its own when the application does not, and the snapshots below are shared
/// with the Windows leg — an application that sets its own title reads back
/// exactly as set there (`observe.rs` asserts that on every platform).
fn styled() -> termlens::Result<Terminal> {
    emit(&[
        "--raw",
        r"\e]0;myapp <2> & co\a",
        "--raw",
        r"\e[1;36mmyapp\e[0m  \e[7m> Alpha\e[0m  汉字\n",
        "--raw",
        r"\e[2mnote\e[0m pw: \e[8msecret\e[28m \e[48;2;30;30;46m  \e[0m\nDONE",
        "--wait",
    ])
}

#[test]
fn a_diff_shows_only_what_changed_and_where() -> termlens::Result<()> {
    let mut t = emit(&[
        "--raw",
        r"Counter: 1\n\e[7m> Quit\e[0m\nrow three\nrow four",
        "--wait",
    ])?;
    t.wait_until(|s| s.contains("row four"))?;
    let a = t.screen();
    t.send(Key::Enter)?;
    t.wait_exit()?;
    let mut t = emit(&[
        "--raw",
        r"Counter: 2\n  Quit\nrow three\nrow four",
        "--wait",
    ])?;
    t.wait_until(|s| s.contains("row four"))?;
    let b = t.screen();

    let diff = a.diff(&b);
    assert!(!diff.is_empty());
    let changed: Vec<(u16, u16)> = diff.cells().map(|(r, c, _, _)| (r, c)).collect();
    // The digit, and every cell of the highlighted word plus the marker.
    assert_eq!(changed[0], (0, 9), "{diff}");
    assert!(
        changed.iter().all(|&(r, _)| r <= 1),
        "rows 2 and 3 are unchanged: {diff}"
    );
    assert!(a.diff(&a).is_empty());
    assert_eq!(a.diff(&a).to_string(), "no difference");
    // #308: which rows changed, and which style runs, as an API rather
    // than a substring of the rendering. Row 0 lost a digit under the
    // same style; row 1 lost its reverse-video highlight.
    assert_eq!(diff.changed_rows().collect::<Vec<_>>(), [0, 1], "{diff}");
    assert_eq!(
        diff.style_changes().collect::<Vec<_>>(),
        [(1, "0-5 reverse", "(none)")],
        "{diff}"
    );
    assert!(a.diff(&a).changed_rows().next().is_none());
    assert!(a.diff(&a).style_changes().next().is_none());
    insta::assert_snapshot!("diff_rendering", diff.to_string());
    t.send(Key::Enter)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}

#[test]
fn sizes_that_differ_diff_the_overlap_and_say_so() -> termlens::Result<()> {
    let mut a = emit(&["same\nDONE", "--wait"])?;
    a.wait_until(|s| s.contains("DONE"))?;
    let mut b = common::spawn_emit(
        Terminal::builder()
            .size(20, 6)
            .timeout(Duration::from_secs(10)),
        &["same\nDONE", "--wait"],
    )?;
    b.wait_until(|s| s.contains("DONE"))?;
    let diff = a.screen().diff(&b.screen());
    assert!(!diff.is_empty(), "a size change is a change");
    assert_eq!(diff.cells().count(), 0, "the overlap is identical: {diff}");
    let text = diff.to_string();
    assert!(text.contains("size: 30x4 → 20x6"), "{text}");
    assert!(text.contains("compared over the 20x4 overlap"), "{text}");
    a.send(Key::Enter)?;
    b.send(Key::Enter)?;
    Ok(())
}

#[test]
fn the_three_renderings_are_pure_functions_of_the_screen() -> termlens::Result<()> {
    let mut t = styled()?;
    t.wait_until(|s| s.contains("DONE"))?;
    let s = t.screen();
    let ansi = s.to_ansi();
    assert_eq!(ansi.lines().count(), 4, "one line per row");
    assert!(ansi.contains("\x1b[0;1;36mmyapp\x1b[0m"), "{ansi:?}");
    assert!(ansi.contains("\x1b[0;7m> Alpha\x1b[0m"), "{ansi:?}");
    assert!(
        ansi.contains("\x1b[0;8msecret\x1b[0m"),
        "concealed is the terminal's to hide: {ansi:?}"
    );
    let svg = s.to_svg();
    assert!(
        svg.starts_with("<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"270\" height=\"72\""),
        "{svg}"
    );
    // The accessible name (#316): the root is a single image, and its first
    // child says what the image is — the size, and the application's own
    // title after it, escaped like any other arbitrary text.
    assert!(svg.contains(" role=\"img\">"), "{svg}");
    let title_at = svg.find("<title>").expect("a <title>");
    assert!(
        svg[..title_at].ends_with(">\n"),
        "the title is the first child, not buried: {svg}"
    );
    assert!(
        svg.contains("<title>termlens screen, 30x4: myapp &lt;2&gt; &amp; co</title>"),
        "the title is escaped, not pasted: {svg}"
    );
    assert!(
        svg.contains("汉字</text>"),
        "a wide character is one glyph: {svg}"
    );
    assert!(
        !svg.contains("secret"),
        "concealed text is drawn as blanks: {svg}"
    );
    assert!(
        svg.contains("fill=\"#1e1e2e\""),
        "the RGB background run: {svg}"
    );
    let html = s.to_html();
    assert!(html.starts_with("<pre style="), "{html}");
    assert!(html.contains("font-weight:bold\">myapp</span>"), "{html}");
    assert!(!html.contains("secret"), "{html}");
    insta::assert_snapshot!("styled_ansi", ansi);
    insta::assert_snapshot!("styled_svg", svg);
    insta::assert_snapshot!("styled_html", html);
    t.send(Key::Enter)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}

/// Every attribute `Style` carries has to reach all three renderings
/// (#378). Blink was the one that did not: `to_ansi` wrote SGR 5 while the
/// SVG and HTML dropped it, so a blinking cell rendered exactly like a
/// steady one in the two artefacts a reviewer looks at. The table is the
/// eight attributes, each applied to the same run in turn; every rendering
/// must differ from the unstyled screen's, and each carries a marker its
/// own machinery writes.
///
/// Conceal is the one that renders by omission — blanks, no text element —
/// so its SVG check is the absence of the baseline's glyphs, not a marker.
#[test]
fn every_style_attribute_reaches_every_rendering() -> termlens::Result<()> {
    let plain = Screen::parse("size: 2x1  cursor: hidden\nab")?;
    let (plain_svg, plain_html, plain_ansi) = (plain.to_svg(), plain.to_html(), plain.to_ansi());
    let cases: [(&str, Option<&str>, &str, &str); 8] = [
        (
            "bold",
            Some("font-weight=\"bold\""),
            "font-weight:bold",
            "\x1b[0;1m",
        ),
        // HTML dim is the foreground blended 60% over the background
        // (#645): #d4d4d4 over #1e1e1e.
        ("dim", Some("opacity=\"0.6\""), "color:#8b8b8b", "\x1b[0;2m"),
        (
            "italic",
            Some("font-style=\"italic\""),
            "font-style:italic",
            "\x1b[0;3m",
        ),
        (
            "underline",
            Some("text-decoration=\"underline\""),
            "text-decoration:underline",
            "\x1b[0;4m",
        ),
        (
            "blink",
            Some("<animate attributeName=\"opacity\""),
            "animation:termlens-blink",
            "\x1b[0;5m",
        ),
        (
            "reverse",
            Some("fill=\"#d4d4d4\"/>"),
            "color:#1e1e1e;background:#d4d4d4",
            "\x1b[0;7m",
        ),
        ("conceal", None, ">  </span>", "\x1b[0;8m"),
        (
            "strikethrough",
            Some("text-decoration=\"line-through\""),
            "text-decoration:line-through",
            "\x1b[0;9m",
        ),
    ];
    for (attribute, svg_marker, html_marker, ansi_marker) in cases {
        let styled = Screen::parse(&format!(
            "size: 2x1  cursor: hidden\nab\n\nstyles:\n0: 0-1 {attribute}"
        ))?;
        let (svg, html, ansi) = (styled.to_svg(), styled.to_html(), styled.to_ansi());
        assert_ne!(svg, plain_svg, "SVG drops {attribute}:\n{svg}");
        assert_ne!(html, plain_html, "HTML drops {attribute}:\n{html}");
        assert_ne!(ansi, plain_ansi, "ANSI drops {attribute}:\n{ansi}");
        match svg_marker {
            Some(marker) => assert!(
                svg.contains(marker),
                "SVG: no {marker} for {attribute}:\n{svg}"
            ),
            None => assert!(!svg.contains("ab"), "conceal must draw no text:\n{svg}"),
        }
        assert!(
            html.contains(html_marker),
            "HTML: no {html_marker} for {attribute}:\n{html}"
        );
        assert!(
            ansi.contains(ansi_marker),
            "ANSI: no {ansi_marker:?} for {attribute}:\n{ansi}"
        );
    }

    // Dim and blink together, the pair the two animate differently: an SMIL
    // animation outranks the opacity attribute it animates, so the SVG's
    // values have to carry dim's 0.6; the HTML animates `color`, so its
    // dimmed colour and the background stay untouched. Without either, a
    // dim blink would flash back to full brightness.
    let both = Screen::parse("size: 2x1  cursor: hidden\nab\n\nstyles:\n0: 0-1 blink dim")?;
    let svg = both.to_svg();
    assert!(
        svg.contains("opacity=\"0.6\"") && svg.contains("values=\"0.6;0\""),
        "a dim blink keeps dim:\n{svg}"
    );
    let html = both.to_html();
    assert!(
        html.contains("color:#8b8b8b;background:#1e1e1e;animation:termlens-blink"),
        "a dim blink keeps dim, and its background paints:\n{html}"
    );
    Ok(())
}

/// SGR 2 fades the glyph, not the cell (#645). Over a coloured background
/// the SVG draws the `<rect>` at full strength and gives only the `<text>`
/// `opacity="0.6"`; the HTML must paint the same picture, so its span keeps
/// the background whole and carries the foreground already blended — with
/// no `opacity` that would fade the background too.
#[test]
fn a_dim_run_fades_its_text_and_not_its_background_in_svg_and_html() -> termlens::Result<()> {
    let s = Screen::parse("size: 10x1  cursor: hidden\nstatus\n\nstyles:\n0: 0-9 bg=4 dim")?;
    let svg = s.to_svg();
    assert!(
        svg.contains("fill=\"#2472c8\"/>"),
        "the rect is painted:\n{svg}"
    );
    let rect = svg
        .lines()
        .find(|l| l.contains("#2472c8"))
        .expect("the background rect");
    assert!(!rect.contains("opacity"), "the rect is not faded: {rect}");
    let text = svg
        .lines()
        .find(|l| l.contains(">status"))
        .expect("the text");
    assert!(
        text.contains("fill=\"#d4d4d4\"") && text.contains("opacity=\"0.6\""),
        "the text is: {text}"
    );

    let html = s.to_html();
    assert!(
        !html.contains("opacity"),
        "nothing in the HTML fades the background:\n{html}"
    );
    // 0.6 * fg + 0.4 * bg per channel: d4/24 -> 8e, d4/72 -> ad, d4/c8 -> cf.
    assert!(
        html.contains("<span style=\"color:#8eadcf;background:#2472c8\">status    </span>"),
        "the HTML paints what the SVG composites to:\n{html}"
    );
    // Reverse swaps first and dim fades what is then the foreground.
    let reversed =
        Screen::parse("size: 2x1  cursor: hidden\nab\n\nstyles:\n0: 0-1 fg=4 reverse dim")?;
    let html = reversed.to_html();
    assert!(
        html.contains(&format!(
            "color:#{};background:#2472c8",
            blend("1e1e1e", "2472c8")
        )),
        "{html}"
    );
    Ok(())
}

/// The channel-wise 60/40 blend the HTML writes for dim, for the test above.
fn blend(fg: &str, bg: &str) -> String {
    (0..3)
        .map(|at| {
            let f = u32::from_str_radix(&fg[2 * at..2 * at + 2], 16).unwrap();
            let b = u32::from_str_radix(&bg[2 * at..2 * at + 2], 16).unwrap();
            format!("{:02x}", (f * 6 + b * 4 + 5) / 10)
        })
        .collect()
}

/// Every character of `doc` is one XML 1.0 allows (the `Char` production)
/// and its tags nest. Those are the two ways #586 broke the SVG; it is not
/// a full parse (entities and attribute syntax are not checked), which
/// keeps the tests free of an XML dependency.
fn assert_well_formed_xml(doc: &str) {
    for (i, c) in doc.char_indices() {
        let allowed = matches!(c, '\t' | '\n' | '\r')
            || ('\u{20}'..='\u{d7ff}').contains(&c)
            || ('\u{e000}'..='\u{fffd}').contains(&c)
            || c >= '\u{10000}';
        assert!(allowed, "{c:?} at byte {i} is not an XML Char:\n{doc}");
    }
    let mut open = Vec::new();
    let mut rest = doc;
    while let Some(start) = rest.find('<') {
        let end = rest[start..].find('>').expect("an unclosed tag") + start;
        let tag = &rest[start + 1..end];
        let name = |t: &str| t.split([' ', '\n', '/']).next().unwrap_or("").to_owned();
        if let Some(closing) = tag.strip_prefix('/') {
            assert_eq!(
                open.pop().as_deref(),
                Some(closing),
                "mismatched </{closing}>:\n{doc}"
            );
        } else if !tag.ends_with('/') && !tag.starts_with('!') && !tag.starts_with('?') {
            open.push(name(tag));
        }
        rest = &rest[end + 1..];
    }
    assert!(open.is_empty(), "unclosed {open:?}:\n{doc}");
}

/// An underlined or struck-through run of blanks is a form field: a
/// terminal draws the line under it, and the HTML did, but the SVG trimmed
/// the run to nothing and dropped it with its decoration (#586).
#[test]
fn a_decorated_run_of_blanks_is_drawn_in_the_svg() -> termlens::Result<()> {
    let screen = Screen::parse(
        "size: 12x1  cursor: hidden\nName:   |  |\n\nstyles:\n0: 5-7 underline\n0: 9-10 strikethrough",
    )?;
    let svg = screen.to_svg();
    assert!(
        svg.contains("text-decoration=\"underline\">   </text>"),
        "the underlined field is drawn:\n{svg}"
    );
    assert!(
        svg.contains("text-decoration=\"line-through\">  </text>"),
        "the struck-through blanks are drawn:\n{svg}"
    );
    assert_well_formed_xml(&svg);
    Ok(())
}

/// Every `<text>` of an SVG rendering of a `cols`-column screen, in
/// document order, as `(x, y, textLength, text)` — after holding each to
/// the grid: it says how its length is fitted, it starts on a cell boundary
/// and is a whole number of cells long, and it ends inside the picture and
/// no later than the next one on its row starts.
fn svg_texts(svg: &str, cols: u32) -> Vec<(u32, u32, u32, String)> {
    let number = |line: &str, name: &str| -> u32 {
        let key = format!(" {name}=\"");
        let from = line
            .find(&key)
            .unwrap_or_else(|| panic!("no {name} on {line}"))
            + key.len();
        let digits = line[from..].split('"').next().unwrap_or_default();
        digits
            .parse()
            .unwrap_or_else(|_| panic!("{name}={digits:?} on {line}"))
    };
    let mut texts: Vec<(u32, u32, u32, String)> = Vec::new();
    for line in svg.lines().filter(|line| line.starts_with("<text ")) {
        assert!(
            line.contains(" lengthAdjust=\"spacingAndGlyphs\""),
            "the glyphs are not fitted to the length: {line}"
        );
        let (x, y, length) = (
            number(line, "x"),
            number(line, "y"),
            number(line, "textLength"),
        );
        assert_eq!((x % 9, length % 9), (0, 0), "off the 9 px grid: {line}");
        assert!(
            length > 0 && x + length <= cols * 9,
            "past the edge: {line}"
        );
        if let Some((last_x, last_y, last_length, _)) = texts.last() {
            assert!(
                *last_y != y || last_x + last_length <= x,
                "overlaps the text before it: {line}"
            );
        }
        let open = line.find('>').expect("the tag closes") + 1;
        let close = line.rfind("</text>").expect("the element closes");
        texts.push((x, y, length, line[open..close].to_owned()));
    }
    texts
}

/// A viewer lays a `<text>` out at its font's own advance, and no font in
/// the stack advances 9 px at 14 px: they advance 8.4 or less, and a CJK
/// fallback 14 where two cells are 18. With nothing in the element saying
/// how long it is, text drifted left of the `<rect>`s along a run, ten `東`
/// took fifteen columns, and the right border of an 80-column box was drawn
/// near column 74 on the rows it shared a run with and at column 79 on the
/// row where a highlight started a new one (#725). Every `<text>` now
/// carries its length in cells, with the glyphs fitted to it.
#[test]
fn the_svg_fits_every_run_of_text_to_the_cell_grid() -> termlens::Result<()> {
    let owned = |texts: &[(u32, u32, u32, &str)]| -> Vec<(u32, u32, u32, String)> {
        texts
            .iter()
            .map(|&(x, y, length, text)| (x, y, length, text.to_owned()))
            .collect()
    };

    // The box of the issue: the border rows are one run each, 80 cells
    // long, and the middle row's right border is a run of its own after the
    // highlighted cell. Both now end at the right edge.
    let border = "\u{2500}".repeat(78);
    let (top, bottom) = (
        format!("\u{256d}{border}\u{256e}"),
        format!("\u{2570}{border}\u{256f}"),
    );
    let boxed = Screen::parse(&format!(
        "size: 80x3  cursor: hidden\n{top}\n\u{2502}{}x\u{2502}\n{bottom}\n\nstyles:\n1: 78 reverse",
        " ".repeat(77)
    ))?;
    let svg = boxed.to_svg();
    assert_eq!(
        svg_texts(&svg, 80),
        owned(&[
            (0, 14, 720, &top),
            (0, 32, 9, "\u{2502}"),
            (702, 32, 9, "x"),
            (711, 32, 9, "\u{2502}"),
            (0, 50, 720, &bottom),
        ]),
        "{svg}"
    );
    assert_well_formed_xml(&svg);

    // Its other two: sixty dots up to a highlight at column 60, ten wide
    // characters up to a marker at column 20, and an 80-column ruler.
    let (dots, wide, ruler) = (
        ".".repeat(60),
        "\u{6771}".repeat(10),
        "0123456789".repeat(8),
    );
    let drift = Screen::parse(&format!(
        "size: 80x3  cursor: hidden\n{dots}HERE\n{wide}|col20\n{ruler}\n\nstyles:\n\
         0: 60-63 reverse\n1: 20-24 bg=1"
    ))?;
    let svg = drift.to_svg();
    assert_eq!(
        svg_texts(&svg, 80),
        owned(&[
            (0, 14, 540, &dots),
            (540, 14, 36, "HERE"),
            (0, 32, 180, &wide),
            (180, 32, 45, "|col2"),
            (225, 32, 9, "0"),
            (0, 50, 720, &ruler),
        ]),
        "{svg}"
    );

    // One stretch fits glyphs of one width, so a run is split where narrow
    // cells meet wide ones, and each piece is as long as its own cells. A
    // piece of blanks draws nothing and is left out; blanks inside a piece
    // are cells like any other.
    for (size, row, expected) in [
        (
            "10x1",
            "ab\u{6771}\u{4eac}cd",
            vec![
                (0, 14, 18, "ab"),
                (18, 14, 36, "\u{6771}\u{4eac}"),
                (54, 14, 18, "cd"),
            ],
        ),
        (
            "10x1",
            "\u{6771} \u{4eac}",
            vec![(0, 14, 18, "\u{6771}"), (27, 14, 18, "\u{4eac}")],
        ),
        ("10x1", "  ab  cd", vec![(0, 14, 72, "  ab  cd")]),
        // A mark is in its character's cell, and a selector makes its
        // emoji two cells.
        ("10x1", "e\u{301}x", vec![(0, 14, 18, "e\u{301}x")]),
        (
            "10x1",
            "\u{26a0}\u{fe0f}X",
            vec![(0, 14, 18, "\u{26a0}\u{fe0f}"), (18, 14, 9, "X")],
        ),
        // A wide character a resize left in the last column has one cell,
        // and is fitted to it rather than drawn past the edge.
        (
            "3x1",
            "ab\u{6771}",
            vec![(0, 14, 18, "ab"), (18, 14, 9, "\u{6771}")],
        ),
    ] {
        let screen = Screen::parse(&format!("size: {size}  cursor: hidden\n{row}"))?;
        let cols = u32::from(screen.cols());
        let svg = screen.to_svg();
        assert_eq!(svg_texts(&svg, cols), owned(&expected), "{row:?}:\n{svg}");
    }

    // A decorated run keeps its blanks (#586), and they are cells too.
    let form = Screen::parse(
        "size: 12x1  cursor: hidden\nName:   |  |\n\nstyles:\n0: 5-7 underline\n0: 9-10 strikethrough",
    )?;
    let svg = form.to_svg();
    assert_eq!(
        svg_texts(&svg, 12),
        owned(&[
            (0, 14, 45, "Name:"),
            (45, 14, 27, "   "),
            (72, 14, 9, "|"),
            (81, 14, 18, "  "),
            (99, 14, 9, "|"),
        ]),
        "{svg}"
    );
    Ok(())
}

/// U+FFFF in a cell went into the SVG as itself, and XML forbids it: no
/// parser opened the file (#586). It is drawn as U+FFFD instead.
#[test]
fn a_character_xml_forbids_in_a_cell_is_replaced() -> termlens::Result<()> {
    let screen = Screen::parse("size: 3x1  cursor: hidden\na\u{ffff}b\n\nstyles:\n0: 0-2 bold")?;
    assert_eq!(screen.row_text(0), "a\u{ffff}b", "the cell holds it");
    let svg = screen.to_svg();
    assert!(svg.contains(">a\u{fffd}b</text>"), "{svg}");
    assert_well_formed_xml(&svg);
    let html = screen.to_html();
    assert!(!html.contains('\u{ffff}'), "{html}");
    Ok(())
}

/// A window title is the application's, and a C0 control in it went into
/// the SVG's `<title>` as itself (#586).
#[test]
#[cfg_attr(
    windows,
    ignore = "ConPTY rewrites the stream before termlens sees it, so the title under test is its rendering (#149)"
)]
fn a_control_character_in_the_title_is_replaced() -> termlens::Result<()> {
    let mut t = emit(&["--raw", r"\e]0;a\x01b\aREADY", "--wait"])?;
    t.wait_until(|s| s.contains("READY"))?;
    let screen = t.screen();
    assert_eq!(screen.title(), "a\u{1}b", "the title holds it");
    let svg = screen.to_svg();
    assert!(svg.contains(": a\u{fffd}b</title>"), "{svg}");
    assert_well_formed_xml(&svg);
    t.send(Key::Enter)?;
    t.wait_exit()?;
    Ok(())
}

#[cfg(feature = "serde")]
mod json {
    use super::*;

    #[test]
    #[cfg_attr(
        windows,
        ignore = "the JSON snapshot carries the out-of-band state, and ConPTY sets some of it itself — focus reporting on, the console's title — so it differs from the Unix recording (#149)"
    )]
    fn a_screen_round_trips_through_json_as_an_equal_screen() -> termlens::Result<()> {
        let mut t = styled()?;
        t.wait_until(|s| s.contains("DONE"))?;
        let s = t.screen();
        let json = serde_json::to_string(&s).expect("serializes");
        let back: termlens::Screen = serde_json::from_str(&json).expect("deserializes");
        assert_eq!(back, s, "an equal Screen, out-of-band state included");
        assert!(back.diff(&s).is_empty());
        // Rows of cells, and a tagged colour with no parser needed.
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["rows"], 4);
        assert_eq!(value["cells"].as_array().unwrap().len(), 4);
        assert_eq!(value["cells"][0].as_array().unwrap().len(), 30);
        assert_eq!(
            value["cells"][0][0]["style"]["fg"],
            serde_json::json!({"indexed": 6})
        );
        assert_eq!(
            value["cells"][1][17]["style"]["bg"],
            serde_json::json!({"rgb": [30, 30, 46]})
        );
        insta::assert_json_snapshot!("styled_json", s);
        t.send(Key::Enter)?;
        assert!(t.wait_exit()?.success());
        Ok(())
    }

    /// The JSON is a persisted artifact, so it says which shape it is
    /// (#329): `"format": 1`, first. A 0.10 file has no such field and is
    /// format 1 by definition; a number this build does not know is
    /// refused with a message that names it.
    #[test]
    fn the_json_carries_its_format_and_reads_a_file_without_one() -> termlens::Result<()> {
        let mut t = emit(&["ab\nDONE", "--wait"])?;
        t.wait_until(|s| s.contains("DONE"))?;
        let screen = t.screen();
        let json = serde_json::to_string(&screen).expect("serializes");
        assert!(
            json.starts_with("{\"format\":1,"),
            "the format number leads the document: {}",
            &json[..40]
        );

        // What 0.10 wrote: the same document without the field.
        let mut value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(value.as_object_mut().unwrap().remove("format").is_some());
        let old: termlens::Screen =
            serde_json::from_value(value.clone()).expect("a 0.10 file reads");
        assert_eq!(old, screen, "and is the same screen");

        // What a later termlens might write.
        value["format"] = serde_json::json!(2);
        let err = serde_json::from_value::<termlens::Screen>(value)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("format 2") && err.contains("reads format 1"),
            "refused with the numbers: {err}"
        );
        t.send(Key::Enter)?;
        assert!(t.wait_exit()?.success());
        Ok(())
    }

    #[test]
    fn a_screen_that_does_not_hold_together_is_refused() -> termlens::Result<()> {
        let mut t = emit(&["ab\nDONE", "--wait"])?;
        t.wait_until(|s| s.contains("DONE"))?;
        let mut value: serde_json::Value = serde_json::to_value(t.screen()).unwrap();
        // Drop one cell from the first row: the file no longer holds together.
        value["cells"][0].as_array_mut().unwrap().pop();
        let err = serde_json::from_value::<termlens::Screen>(value).unwrap_err();
        assert!(
            err.to_string()
                .contains("row 0 of a 30-column screen holds 29 cells"),
            "refused with a reason: {err}"
        );
        t.send(Key::Enter)?;
        assert!(t.wait_exit()?.success());
        Ok(())
    }

    /// The column bound was `>` where the row bound was `>=`, so
    /// `col == cols` — a column that does not exist — passed validation
    /// and `render --text` wrote a header the text parser then refused
    /// (#375). Refusing it outright would make JSON that 0.10 and 0.11
    /// wrote unreadable, which the format promise forbids, so that one
    /// column — the pending-wrap position — is clamped onto the last cell
    /// instead, and every other cursor off the grid is refused.
    #[test]
    fn a_pending_wrap_cursor_is_clamped_and_a_cursor_off_the_grid_is_refused() {
        let screen = Screen::parse("size: 4x2  cursor: 1,3\nab\ncd").expect("a 4x2 screen");
        assert_eq!(screen.cursor(), (1, 3, true));

        // `col == cols`: read, clamped onto the last real cell, and the
        // text it renders to parses again — the round trip #375 was about.
        let mut value = serde_json::to_value(&screen).unwrap();
        value["cursor"]["row"] = 0.into();
        value["cursor"]["col"] = 4.into();
        let clamped: termlens::Screen =
            serde_json::from_value(value).expect("the pending-wrap column reads");
        assert_eq!(clamped.cursor(), (0, 3, true), "clamped onto the last cell");
        assert!(
            Screen::parse(&clamped.with_styles().to_string()).is_ok(),
            "…and the text it renders to parses"
        );

        for (row, col) in [(0_u16, 5_u16), (2, 0)] {
            let mut value = serde_json::to_value(&screen).unwrap();
            value["cursor"]["row"] = row.into();
            value["cursor"]["col"] = col.into();
            let err = serde_json::from_value::<termlens::Screen>(value)
                .expect_err("a cursor off the grid is refused")
                .to_string();
            assert!(
                err.contains(&format!("cursor {row},{col} is outside a 4x2 screen")),
                "refused with the existing message: {err}"
            );
        }

        let mut value = serde_json::to_value(&screen).unwrap();
        value["cursor"]["row"] = 1.into();
        value["cursor"]["col"] = 3.into();
        assert!(
            serde_json::from_value::<termlens::Screen>(value).is_ok(),
            "the last column and row are real"
        );
    }

    /// DESIGN §3 fixes the eight style booleans to SGR order (#381), and
    /// serde emits a struct's fields in declaration order — so the two
    /// agree only as long as `Style` declares them that way. Blink is
    /// SGR 5, reverse SGR 7; they were one declaration apart the wrong way,
    /// and this reads the key order out of the emitted string rather than
    /// a `Value`, which no longer remembers it.
    #[test]
    fn the_style_booleans_serialise_in_sgr_order() {
        let screen = Screen::parse(
            "size: 2x1  cursor: hidden\nab\n\nstyles:\n\
             0: 0-1 bold dim italic underline blink reverse conceal strikethrough",
        )
        .expect("all eight attributes parse");
        let json = serde_json::to_string(&screen).expect("serializes");
        let at = json.find("\"style\":").expect("a cell carries a style");
        let style = &json[at..][..json[at..].find('}').expect("the style object ends")];
        let mut last = 0;
        for key in [
            "bold",
            "dim",
            "italic",
            "underline",
            "blink",
            "reverse",
            "conceal",
            "strikethrough",
        ] {
            let needle = format!("\"{key}\":");
            let at = style
                .find(&needle)
                .unwrap_or_else(|| panic!("{key} is not in {style}"));
            assert!(at > last, "{key} is out of SGR order in {style}");
            last = at;
        }
    }

    /// A one-row screen to edit as JSON: `ab`, the wide `東` in columns 2
    /// and 3, then two blanks.
    fn document() -> serde_json::Value {
        let screen = Screen::parse("size: 6x1  cursor: 0,0\nab\u{6771}").expect("a 6x1 screen");
        serde_json::to_value(&screen).expect("serializes")
    }

    /// [`document`] with `edit` applied, read back.
    fn read_edited(edit: impl FnOnce(&mut serde_json::Value)) -> Result<Screen, String> {
        let mut value = document();
        edit(&mut value);
        serde_json::from_value(value).map_err(|e| e.to_string())
    }

    /// A cell carrying a control character drove the terminal of whoever
    /// rendered the file: `render --text` and `diff` printed it, so an OSC
    /// set the window title and `ESC [2J` cleared the screen (#582). vt100
    /// drops every control character before the grid, so no termlens wrote
    /// one, and the reader refuses it in the text reader's words (#376).
    #[test]
    fn a_control_character_in_a_cell_is_refused() {
        for contents in [
            "\u{1b}]0;PWNED\u{7}\u{1b}[2Jh",
            "\n",
            "\0",
            "a\u{7}",
            "\u{7f}",
            "\u{9b}2J",
        ] {
            let control = contents.chars().find(|ch| ch.is_control()).unwrap();
            let err = read_edited(|v| v["cells"][0][1]["contents"] = contents.into())
                .expect_err(contents);
            assert!(
                err.contains(&format!("cell 0,1: {control:?} is a control character")),
                "{contents:?}: {err}"
            );
        }
        // The styled history holds cells too, in rows of their own width.
        let err = read_edited(|v| {
            let mut cell = v["cells"][0][0].clone();
            cell["contents"] = "\u{1b}[2J".into();
            v["state"]["scrollback"] = serde_json::json!(["x"]);
            v["state"]["scrollback_cells"] = serde_json::json!([[cell]]);
        })
        .expect_err("a control character in the history");
        assert!(
            err.contains("scrollback cell 0,0: '\\u{1b}' is a control character"),
            "{err}"
        );
    }

    /// A cell is one character and the zero-width marks after it. vt100
    /// appends to a cell only what its width table calls zero-width, and
    /// printable ASCII is one column in every table, so a cell holding a
    /// second ASCII character is one no termlens wrote (#582). It rendered
    /// wider than its column and pushed the rest of the row out of line.
    #[test]
    fn a_cell_of_more_than_one_character_is_refused() {
        for contents in ["abc", "a ", "\u{e9}1", "\u{6771}x"] {
            let err = read_edited(|v| v["cells"][0][0]["contents"] = contents.into())
                .expect_err(contents);
            assert!(
                err.contains(&format!(
                    "cell 0,0: {contents:?} is more than one character"
                )),
                "{err}"
            );
        }
    }

    /// The wide flags no grid holds (#582). vt100 marks a cell wide only
    /// for a character its table makes two columns, which printable ASCII
    /// never is, and never an empty one; it clears a cell before making it
    /// a second column; and it clears or moves the two halves of a wide
    /// character together, while a resize cuts a row from the right — so a
    /// first column can be stranded, never a second.
    #[test]
    fn a_wide_flag_no_grid_holds_is_refused() {
        let refused = |expected: &str, edit: fn(&mut serde_json::Value)| {
            let err = read_edited(edit).expect_err(expected);
            assert!(err.contains(expected), "{expected}: {err}");
        };
        refused("cell 0,0: 'a' is one column wide but marked wide", |v| {
            v["cells"][0][0]["wide"] = true.into();
            v["cells"][0][1]["contents"] = "".into();
            v["cells"][0][1]["wide_continuation"] = true.into();
        });
        refused("cell 0,4: marked wide but holds no character", |v| {
            v["cells"][0][4]["wide"] = true.into();
        });
        refused("cell 0,4: marked wide but holds no character", |v| {
            v["cells"][0][4]["wide"] = true.into();
            v["cells"][0][4]["wide_continuation"] = true.into();
        });
        refused(
            "cell 0,3: a wide character's second column holding \"x\"",
            |v| v["cells"][0][3]["contents"] = "x".into(),
        );
        let orphan = "a wide character's second column with no wide character before it";
        refused(&format!("cell 0,0: {orphan}"), |v| {
            v["cells"][0][0]["contents"] = "".into();
            v["cells"][0][0]["wide_continuation"] = true.into();
        });
        refused(&format!("cell 0,5: {orphan}"), |v| {
            v["cells"][0][5]["wide_continuation"] = true.into();
        });
    }

    /// A terminal is at least 2x2 and the text reader refuses a screen
    /// under 1x1, so no termlens wrote an empty one. Read anyway, a screen
    /// of no columns re-serialised as a document of no rows, which the
    /// reader then refused (#582).
    #[test]
    fn a_screen_of_no_columns_or_no_rows_is_refused() {
        for (cols, rows, cells) in [
            (0, 1, serde_json::json!([[]])),
            (6, 0, serde_json::json!([])),
        ] {
            let err = read_edited(|v| {
                v["cols"] = cols.into();
                v["rows"] = rows.into();
                v["cells"] = cells;
            })
            .expect_err("an empty screen");
            assert!(
                err.contains(&format!(
                    "at least one column and one row, got {cols}x{rows}"
                )),
                "{err}"
            );
        }
    }

    /// Four tracking modes have a bit each, and the derive read any byte:
    /// `255` made `len()` say 8 while `iter()` yielded 4 (#582). The tracker
    /// never set another bit, so the reader drops them rather than refuse
    /// the file.
    #[test]
    fn a_mouse_mode_bit_no_mode_occupies_is_dropped() {
        let screen =
            read_edited(|v| v["state"]["mouse_modes"] = 255.into()).expect("the file reads");
        let modes = screen.mouse_modes();
        assert_eq!((modes.len(), modes.iter().count()), (4, 4), "{modes:?}");
        assert_eq!(
            serde_json::to_value(&screen).unwrap()["state"]["mouse_modes"],
            15
        );
        let alone: termlens::MouseModes =
            serde_json::from_str("240").expect("a set on its own reads");
        assert!(alone.is_empty(), "{alone:?}");
    }

    /// What the reader must go on reading (#582): cells a termlens wrote
    /// that this build would not have. A width is a table entry, and the
    /// tables move between unicode-width releases — U+11A3A was a mark in
    /// 0.2.1 and is one column in 0.2.2, U+1ACF went the other way, U+16FF2
    /// from one column to two — so each is written here both ways. So is an
    /// emoji-presentation sequence: `⚠️` one column, as vt100 0.16 wrote it,
    /// and two, as 0.18 does, with a keycap whose first character is an
    /// ASCII digit. U+FFFD, which termlens writes for a byte it could not
    /// decode; the space vt100 puts under a mark that lands on an empty
    /// cell; and a wide character a resize cut, which vt100 0.16 left in
    /// the last column, and before a blank once the grid widened again.
    #[test]
    fn a_cell_an_earlier_release_wrote_still_reads() {
        let reads = |edit: fn(&mut serde_json::Value)| {
            let mut value = document();
            edit(&mut value);
            let screen: Screen = serde_json::from_value(value.clone())
                .unwrap_or_else(|e| panic!("{e}: {}", value["cells"]));
            assert_eq!(
                serde_json::to_value(&screen).unwrap(),
                value,
                "and it writes back unchanged"
            );
        };
        reads(|v| v["cells"][0][0]["contents"] = "a\u{11a3a}".into());
        reads(|v| v["cells"][0][4]["contents"] = "\u{11a3a}".into());
        reads(|v| v["cells"][0][0]["contents"] = "a\u{1acf}".into());
        reads(|v| v["cells"][0][4]["contents"] = "\u{1acf}".into());
        reads(|v| v["cells"][0][4]["contents"] = "\u{16ff2}".into());
        reads(|v| {
            v["cells"][0][4]["contents"] = "\u{16ff2}".into();
            v["cells"][0][4]["wide"] = true.into();
            v["cells"][0][5]["wide_continuation"] = true.into();
        });
        reads(|v| {
            v["cells"][0][4]["contents"] = "1\u{fe0f}\u{20e3}".into();
            v["cells"][0][4]["wide"] = true.into();
            v["cells"][0][5]["wide_continuation"] = true.into();
        });
        reads(|v| v["cells"][0][4]["contents"] = "\u{26a0}\u{fe0f}".into());
        reads(|v| {
            v["cells"][0][4]["contents"] = "\u{26a0}\u{fe0f}".into();
            v["cells"][0][4]["wide"] = true.into();
            v["cells"][0][5]["wide_continuation"] = true.into();
        });
        reads(|v| v["cells"][0][4]["contents"] = "\u{fffd}".into());
        reads(|v| v["cells"][0][4]["contents"] = " \u{301}".into());
        reads(|v| {
            v["cells"][0][5]["contents"] = "\u{6771}".into();
            v["cells"][0][5]["wide"] = true.into();
        });
        reads(|v| v["cells"][0][3]["wide_continuation"] = false.into());
    }

    /// The cells a live terminal writes read back as the same screen
    /// (#582): combining marks, wide characters, a joiner, emoji that U+FE0F
    /// makes two columns — among them a keycap, whose first character is an
    /// ASCII digit — the space vt100 puts under a mark that lands on an
    /// empty cell, and whatever a resize through a wide character leaves.
    #[test]
    #[cfg_attr(
        windows,
        ignore = "ConPTY re-renders wide characters into columns of its own choosing (#149)"
    )]
    fn every_cell_a_live_terminal_writes_reads_back() -> termlens::Result<()> {
        // Eighteen columns: `e\u{301}`, `漢`, `☺️`, `👨\u{200d}` and `👩`, then
        // the keycap `1️⃣` in 13 and 14 and `#️` in 16 and 17; then a mark on
        // the empty cell (1,3).
        let text = "e\u{301} \u{6f22} \u{263a}\u{fe0f} \u{1f468}\u{200d}\u{1f469} \
                    1\u{fe0f}\u{20e3} #\u{fe0f}\\e[2;5H\u{301}\\r\\nREADY";
        let mut t = emit(&["--raw", text, "--wait"])?;
        t.wait_until(|s| s.contains("READY"))?;
        let read_back = |screen: &Screen| -> Screen {
            let json = serde_json::to_string(screen).expect("serializes");
            serde_json::from_str(&json).unwrap_or_else(|e| panic!("{e}:\n{screen}"))
        };

        let screen = t.screen();
        assert_eq!(screen.cell(0, 0).unwrap().contents(), "e\u{301}");
        let keycap = screen.cell(0, 13).unwrap();
        assert_eq!(keycap.contents(), "1\u{fe0f}\u{20e3}");
        assert!(keycap.is_wide(), "{screen:?}");
        assert_eq!(screen.cell(1, 3).unwrap().contents(), " \u{301}");
        assert_eq!(read_back(&screen), screen);

        // Through `#️`, and back.
        t.resize(17, 4)?;
        let cut = t.screen();
        assert_eq!(read_back(&cut), cut);
        t.resize(30, 4)?;
        let widened = t.screen();
        assert_eq!(read_back(&widened), widened);

        t.send(Key::Enter)?;
        t.wait_exit()?;
        Ok(())
    }
}

/// A grid can hold the words a styles block is made of. Reading them as
/// metadata deleted them, and a snapshot that silently loses a visible row
/// is worse than one that fails to parse (#296).
#[test]
#[cfg_attr(
    windows,
    ignore = "ConPTY rewrites the stream before termlens sees it, so the grid under test is its rendering (#149)"
)]
fn a_grid_holding_the_words_of_a_styles_block_round_trips() -> termlens::Result<()> {
    let mut t = emit(&["--raw", r"\r\nstyles:\r\n(none)\r\nREADY", "--wait"])?;
    t.wait_until(|s| s.contains("READY"))?;
    let screen = t.screen();
    assert_eq!(screen.row_text(1).trim_end(), "styles:");
    assert_eq!(screen.row_text(2).trim_end(), "(none)");

    let plain = screen.to_string();
    assert_eq!(
        Screen::parse(&plain)?.to_string(),
        plain,
        "content, not metadata"
    );
    let styled = screen.with_styles().to_string();
    let parsed = Screen::parse(&styled)?;
    assert_eq!(parsed.with_styles().to_string(), styled);
    assert!(screen.diff(&parsed).is_empty(), "{}", screen.diff(&parsed));

    t.send(Key::Enter)?;
    t.wait_exit()?;
    Ok(())
}

/// The emulator stores a wide character and its combining mark in one cell;
/// the parser walked back onto the continuation half, which holds no text,
/// and refused its own format (#297).
#[test]
#[cfg_attr(
    windows,
    ignore = "ConPTY re-renders wide characters into columns of its own choosing (#149)"
)]
fn a_wide_character_with_a_combining_mark_round_trips() -> termlens::Result<()> {
    // A wide+mark cell mid-row, and another ending exactly at the right
    // margin of the 30-column grid.
    // The characters go in as themselves; only \r\n is for the fixture to
    // interpret. 28 narrow cells put the second wide glyph in the last two
    // columns of the 30-column grid.
    let margin = "a".repeat(28);
    let text = format!("\u{6771}\u{301}X\\r\\n{margin}\u{6771}\u{302}\\r\\nREADY");
    let mut t = emit(&["--raw", &text, "--wait"])?;
    t.wait_until(|s| s.contains("READY"))?;
    let screen = t.screen();
    assert_eq!(screen.cell(0, 0).unwrap().contents(), "\u{6771}\u{301}");
    assert!(screen.cell(1, 28).unwrap().is_wide(), "{screen}");
    assert!(screen.cell(1, 29).unwrap().is_wide_continuation());

    let saved = screen.with_styles().to_string();
    let parsed = Screen::parse(&saved)?;
    assert!(screen.diff(&parsed).is_empty(), "{}", screen.diff(&parsed));
    assert_eq!(parsed.cell(0, 0).unwrap().contents(), "\u{6771}\u{301}");
    assert_eq!(parsed.with_styles().to_string(), saved);

    t.send(Key::Enter)?;
    t.wait_exit()?;
    Ok(())
}

/// A hidden cursor draws nothing, and the text format does not record where
/// it sat — so a screen parsed back from its own snapshot reported a
/// difference no reader could see (#298).
#[test]
#[cfg_attr(
    windows,
    ignore = "ConPTY drives the cursor itself, so its position and visibility are not the child's (#149)"
)]
fn a_hidden_cursor_round_trips_as_the_same_picture() -> termlens::Result<()> {
    let mut t = emit(&["--raw", r"hello\e[2;3H\e[?25l", "--wait"])?;
    t.wait_until(|s| s.contains("hello") && !s.cursor_visible())?;
    let screen = t.screen();
    assert_eq!(screen.cursor(), (1, 2, false));
    assert!(!screen.cursor_visible(), "the tuple and the accessor agree");

    let parsed = Screen::parse(&screen.with_styles().to_string())?;
    assert_eq!(
        parsed.cursor(),
        (0, 0, false),
        "the position is not in the text"
    );
    assert!(screen.diff(&parsed).is_empty(), "{}", screen.diff(&parsed));

    // Visibility itself, and a visible cursor's position, are still picture.
    let shown = Screen::parse("size: 30x4  cursor: 1,2\nhello")?;
    assert!(
        !parsed.diff(&shown).is_empty(),
        "hidden vs visible is a change"
    );
    let moved = Screen::parse("size: 30x4  cursor: 2,5\nhello")?;
    assert!(
        !shown.diff(&moved).is_empty(),
        "a visible cursor moving is a change"
    );

    t.send(Key::Enter)?;
    t.wait_exit()?;
    Ok(())
}

/// A no-break space or an ideographic space an application draws at the end
/// of a row is trimmed from the text form with the padding, so it parses
/// back as a blank cell. A terminal shows a blank for either, and `diff`
/// compares pictures: the parsed screen, the screen it was saved from and
/// that screen's JSON all diff empty, where the diff used to mark a blank
/// against a blank (#724).
#[test]
#[cfg_attr(
    windows,
    ignore = "ConPTY re-renders wide characters into columns of its own choosing (#149)"
)]
fn a_row_ending_in_drawn_whitespace_round_trips_as_the_same_picture() -> termlens::Result<()> {
    let mut t = emit(&[
        "--raw",
        "a\u{a0}\\r\\n\\e[41m\u{3000}\\e[0m\\r\\nREADY",
        "--wait",
    ])?;
    t.wait_until(|s| s.contains("READY"))?;
    let screen = t.screen();
    assert_eq!(screen.cell(0, 1).unwrap().contents(), "\u{a0}");
    let ideographic = screen.cell(1, 0).unwrap();
    assert_eq!(ideographic.contents(), "\u{3000}");
    assert!(ideographic.is_wide(), "{screen:?}");
    assert!(screen.cell(1, 1).unwrap().is_wide_continuation());

    let saved = screen.with_styles().to_string();
    assert!(
        !saved.contains(['\u{a0}', '\u{3000}']),
        "both are trimmed from the text: {saved:?}"
    );
    let parsed = Screen::parse(&saved)?;
    assert_eq!(parsed.cell(0, 1).unwrap().contents(), "");
    assert!(!parsed.cell(1, 0).unwrap().is_wide());
    assert_eq!(parsed.with_styles().to_string(), saved);
    assert!(screen.diff(&parsed).is_empty(), "{}", screen.diff(&parsed));
    assert!(parsed.diff(&screen).is_empty(), "{}", parsed.diff(&screen));

    // The JSON carries both characters, and is the same picture as the text.
    #[cfg(feature = "serde")]
    {
        let json = serde_json::to_string(&screen).expect("serializes");
        let from_json: Screen = serde_json::from_str(&json).expect("deserializes");
        assert_eq!(from_json, screen);
        assert_eq!(from_json.cell(1, 0).unwrap().contents(), "\u{3000}");
        let diff = from_json.diff(&parsed);
        assert!(diff.is_empty(), "{diff}");
    }

    t.send(Key::Enter)?;
    t.wait_exit()?;
    Ok(())
}
