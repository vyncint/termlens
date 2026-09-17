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
        ("dim", Some("opacity=\"0.6\""), "opacity:0.6", "\x1b[0;2m"),
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
    // `opacity:0.6` and the background stay untouched. Without either, a
    // dim blink would flash back to full brightness.
    let both = Screen::parse("size: 2x1  cursor: hidden\nab\n\nstyles:\n0: 0-1 blink dim")?;
    let svg = both.to_svg();
    assert!(
        svg.contains("opacity=\"0.6\"") && svg.contains("values=\"0.6;0\""),
        "a dim blink keeps dim:\n{svg}"
    );
    let html = both.to_html();
    assert!(
        html.contains("opacity:0.6;animation:termlens-blink"),
        "a dim blink keeps dim, and its background paints:\n{html}"
    );
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
