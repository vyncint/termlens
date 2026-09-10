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
fn styled() -> termlens::Result<Terminal> {
    emit(&[
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
