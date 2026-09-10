//! Styled history (#146) and the history-spanning search (#147): a masked
//! password stays observably masked after its line scrolls off, and a
//! needle is located in whichever region holds it.

use std::time::Duration;

use termlens::{Key, Location, Terminal};

mod common;

/// A 30x6 terminal that prints a concealed secret, then enough lines to
/// scroll it into history, then parks.
fn scrolled(styles: bool, scrollback: usize) -> termlens::Result<Terminal> {
    let lines: String = (1..=12).map(|i| format!("line {i}\n")).collect();
    common::spawn_emit(
        Terminal::builder()
            .size(30, 6)
            .scrollback(scrollback)
            .scrollback_styles(styles)
            .timeout(Duration::from_secs(10)),
        &[
            "--raw",
            r"pw: \e[1;31m\e[8mSECRET\e[0m|\n",
            &lines,
            "READY",
            "--wait",
        ],
    )
}

#[test]
fn a_masked_field_stays_masked_after_it_scrolls_off() -> termlens::Result<()> {
    let mut t = scrolled(true, 1000)?;
    t.wait_until(|s| s.contains("READY"))?;
    let s = t.screen();
    assert!(s.styled_scrollback());
    assert!(!s.contains("SECRET"), "it scrolled off the grid: {s}");
    assert!(
        s.full_text().contains("SECRET"),
        "but it reached the terminal"
    );
    // Row 0 of history is the secret's row; columns 4..10 are the field.
    for col in 4..10 {
        let cell = s.scrollback_cell(0, col).expect("a retained cell");
        assert!(cell.style().conceal, "column {col} is concealed");
        assert!(cell.style().bold, "and bold, from the primary parser");
    }
    assert!(
        !s.scrollback_cell(0, 3).unwrap().style().conceal,
        "the label is not"
    );
    assert_eq!(
        s.scrollback_cell(0, 4).unwrap().contents(),
        "S",
        "the text is still there, as a terminal holds it"
    );
    assert!(
        s.scrollback_cell(0, 30).is_none(),
        "past the captured width"
    );
    assert!(
        s.scrollback_cell(999, 0).is_none(),
        "past the retained rows"
    );
    t.send(Key::Enter)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}

#[test]
fn without_the_knob_history_stays_text_only() -> termlens::Result<()> {
    let mut t = scrolled(false, 1000)?;
    t.wait_until(|s| s.contains("READY"))?;
    let s = t.screen();
    assert!(!s.styled_scrollback());
    assert!(
        s.scrollback_cell(0, 4).is_none(),
        "nothing retained, nothing to pay for"
    );
    assert!(
        s.scrollback_text().contains("SECRET"),
        "the text is retained regardless"
    );
    t.send(Key::Enter)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}

/// Bounded like the text: at the cap the oldest styled rows are gone, and
/// the two stay in lockstep.
#[test]
fn styled_history_is_bounded_with_the_text() -> termlens::Result<()> {
    let mut t = scrolled(true, 5)?;
    t.wait_until(|s| s.contains("READY"))?;
    let s = t.screen();
    assert_eq!(s.scrollback_rows(), 5);
    let first = s.scrollback_text().lines().next().unwrap().to_owned();
    let cells: String = (0..30)
        .filter_map(|c| s.scrollback_cell(0, c))
        .map(|c| {
            if c.contents().is_empty() {
                " ".to_owned()
            } else {
                c.contents().to_owned()
            }
        })
        .collect();
    assert_eq!(
        cells.trim_end(),
        first,
        "row 0 of the cells is row 0 of the text"
    );
    assert!(s.scrollback_cell(5, 0).is_none());
    t.send(Key::Enter)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}

#[test]
fn locate_says_which_region_holds_the_needle() -> termlens::Result<()> {
    let mut t = scrolled(false, 1000)?;
    t.wait_until(|s| s.contains("READY"))?;
    let s = t.screen();
    assert_eq!(
        s.locate("READY"),
        Some(Location::Screen { row: 5, col: 0 }),
        "{s}"
    );
    assert_eq!(
        s.locate("SECRET"),
        Some(Location::History { row: 0, col: 4 })
    );
    assert_eq!(
        s.locate("line 2"),
        Some(Location::History { row: 2, col: 0 })
    );
    assert_eq!(s.locate("nowhere"), None);
    // The grid wins when a needle is in both: `line 1` also starts `line 10`
    // on the grid.
    assert!(
        matches!(s.locate("line 1"), Some(Location::Screen { .. })),
        "{s}"
    );
    // #306: the one-fact questions without a match. The column is the
    // same number either region reports; the row deliberately is not
    // offered, since a grid row and a history row are different things.
    let ready = s.locate("READY").expect("on the grid");
    assert!(ready.is_on_screen() && !ready.is_in_history());
    assert_eq!(ready.col(), 0);
    let secret = s.locate("SECRET").expect("in history");
    assert!(secret.is_in_history() && !secret.is_on_screen());
    assert_eq!(secret.col(), 4);
    assert!(s.locate("line 1").is_some_and(Location::is_on_screen));
    t.send(Key::Enter)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}

/// Not a test of correctness: prints how long the two retention modes take
/// on a flood, for the numbers `scrollback_styles`'s rustdoc quotes.
#[test]
#[ignore = "timing probe: run with --ignored --nocapture to remeasure the rustdoc's numbers"]
fn how_much_styled_history_costs() -> termlens::Result<()> {
    for styles in [false, true] {
        let started = std::time::Instant::now();
        let mut t = common::spawn_emit(
            Terminal::builder()
                .size(80, 24)
                .scrollback_styles(styles)
                .timeout(Duration::from_secs(120)),
            &["--seq", "20000", "READY", "--wait"],
        )?;
        t.wait_until(|s| s.contains("READY"))?;
        println!("styles={styles}: {:?}", started.elapsed());
        t.send(Key::Enter)?;
        t.wait_exit()?;
    }
    Ok(())
}
