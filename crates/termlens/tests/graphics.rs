//! Inline graphics, end to end: a real program transmits a real image into a
//! real PTY, and the test asserts what came off the wire.
//!
//! The `image-echo` fixture draws a 4×2 image whose every pixel is known, at
//! a known cursor position, with labels above and below it. That makes three
//! classes of claim assertable that no content predicate can reach, because
//! none of them changes a cell:
//!
//! - **that** an image went out, and how many — counted as images rather
//!   than as escapes, so the protocol's 4096-byte chunking cannot inflate it
//!   and a delete cannot pose as a transmission;
//! - **where** it landed and how big it declared itself, which is what keeps
//!   a picture in step with the text laid out around it;
//! - **what it depicted**, with the `decode` feature.

use std::time::Duration;

use termlens::{GraphicsAction, GraphicsFormat, GraphicsProtocol, Screen, Terminal};

mod common;
use common as util;

fn spawn(mode: &str) -> termlens::Result<Terminal> {
    Terminal::builder()
        .size(40, 10)
        .timeout(Duration::from_secs(30))
        .env_clear()
        .arg(mode)
        .spawn(util::fixture_bin("image-echo"))
}

/// The fixture draws its lower label last, so a screen carrying it carries
/// the payload above it too.
fn drawn(screen: &Screen) -> bool {
    screen.contains("done")
}

/// Drive one mode to completion and hand back the settled screen.
fn run(mode: &str) -> termlens::Result<(Terminal, Screen)> {
    let mut terminal = spawn(mode)?;
    terminal.wait_until(drawn)?;
    let screen = terminal.screen();
    Ok((terminal, screen))
}

#[test]
fn a_transmitted_image_is_counted_and_described() -> termlens::Result<()> {
    let (_terminal, screen) = run("kitty")?;
    let seen = screen.graphics();
    assert_eq!(seen.kitty(), 1, "one image: {seen:?}");
    assert_eq!(seen.sixel(), 0);
    assert_eq!(seen.deletes(), 0);
    assert!(!seen.is_empty());

    let image = seen.last().expect("a payload");
    assert_eq!(image.protocol(), GraphicsProtocol::Kitty);
    assert_eq!(image.action(), GraphicsAction::TransmitAndPlace);
    assert_eq!(image.format(), GraphicsFormat::Rgba);
    assert!(image.compressed(), "the fixture zlib's it");
    assert_eq!(image.id(), Some(7));
    assert_eq!(image.size(), Some((4, 2)), "the size it declared");
    assert_eq!(image.cells(), Some((2, 1)), "pinned to the cells reserved");
    assert!(image.bytes() > 0);
    Ok(())
}

#[test]
fn an_image_is_stamped_with_the_cell_it_was_placed_on() -> termlens::Result<()> {
    // The fixture moves to row 2, column 5 (1-based) and transmits there.
    // This is the one fact about an image that lives in the grid rather than
    // in the payload — and the one an application gets wrong when a picture
    // drifts out from under its own labels.
    for mode in ["kitty", "sixel"] {
        let (_terminal, screen) = run(mode)?;
        let seen = screen.graphics();
        let image = seen.last().expect("a payload");
        assert_eq!(image.at(), (1, 4), "{mode} landed at {:?}", image.at());
    }
    Ok(())
}

#[test]
fn a_chunked_transmission_is_one_image_not_three() -> termlens::Result<()> {
    // The protocol caps a payload at 4096 bytes and continues with `m=1`, so
    // every image of consequence arrives in several escapes. Counting each
    // one reported a single chart as several pictures — and the
    // continuations carry no control block, so nothing could be said about
    // those "pictures" either.
    let (_terminal, chunked) = run("kitty-chunks")?;
    let (_terminal, whole) = run("kitty-plain")?;

    let chunked_seen = chunked.graphics();
    let whole_seen = whole.graphics();
    assert_eq!(chunked_seen.kitty(), 1, "{chunked_seen:?}");
    assert_eq!(whole_seen.kitty(), 1, "{whole_seen:?}");

    let split = chunked_seen.last().expect("a payload");
    let single = whole_seen.last().expect("a payload");
    assert!(split.chunks() > 1, "the fixture split it: {split:?}");
    assert_eq!(single.chunks(), 1);
    // Same image, so the same data — however it was cut up on the way.
    assert_eq!(split.data(), single.data());
    assert_eq!(split.size(), single.size());
    Ok(())
}

#[test]
fn a_delete_is_not_an_image_transmitted() -> termlens::Result<()> {
    // An application that tears down what it drew and one that draws twice
    // as much are opposite behaviours; folding a delete into the image count
    // made them the same number.
    let (_terminal, screen) = run("delete")?;
    let seen = screen.graphics();
    assert_eq!(seen.kitty(), 1, "one image: {seen:?}");
    assert_eq!(seen.deletes(), 1, "and one teardown: {seen:?}");
    assert_eq!(seen.total(), 1);

    let payloads = seen.payloads();
    assert_eq!(
        payloads.len(),
        2,
        "both are still on the wire: {payloads:?}"
    );
    assert_eq!(payloads[1].action(), GraphicsAction::Delete);
    assert_eq!(payloads[1].id(), Some(7));
    assert!(!payloads[1].action().carries_image());
    Ok(())
}

#[test]
fn a_program_that_draws_no_image_reports_none() -> termlens::Result<()> {
    // The negative assertion, which is the one this exists for as often as
    // not: "this must render as text in every terminal, and never go out as
    // an image".
    let (_terminal, screen) = run("none")?;
    let seen = screen.graphics();
    assert!(seen.is_empty(), "{seen:?}");
    assert_eq!(seen.bytes(), 0);
    assert!(seen.payloads().is_empty());
    assert!(seen.last().is_none());
    Ok(())
}

#[test]
fn the_image_leaves_the_text_around_it_alone() -> termlens::Result<()> {
    // Nothing about a payload touches the grid, which is exactly why the
    // counters exist — and it is worth one assertion that the emulator did
    // not quietly start rendering one.
    let (_terminal, screen) = run("kitty")?;
    assert!(screen.contains("label above"), "{screen}");
    assert!(screen.contains("label below"), "{screen}");
    // The payload was transmitted at row 1, and the row is untouched.
    assert_eq!(screen.row_text(1).trim(), "", "{screen}");
    Ok(())
}

#[test]
fn a_bounded_capture_keeps_the_counts_and_drops_the_bytes() -> termlens::Result<()> {
    // The budget is a memory bound, not an observation bound: every count
    // and every declared fact survives it, and only the data goes.
    let mut terminal = Terminal::builder()
        .size(40, 10)
        .timeout(Duration::from_secs(30))
        .env_clear()
        .capture_graphics(0)
        .arg("kitty")
        .spawn(util::fixture_bin("image-echo"))?;
    terminal.wait_until(drawn)?;
    let screen = terminal.screen();
    let seen = screen.graphics();

    assert_eq!(seen.kitty(), 1, "still counted: {seen:?}");
    let image = seen.last().expect("a payload");
    assert_eq!(image.size(), Some((4, 2)), "still described");
    assert!(image.bytes() > 0, "still measured");
    assert_eq!(image.data(), None, "and not kept");
    Ok(())
}

#[cfg(feature = "decode")]
mod decoded {
    use super::*;
    use termlens::DecodeError;

    /// GitHub Primer's brightest contribution green — the fixture's colour,
    /// and the reason this crate can now say "the image was green" at all.
    const GREEN: [u8; 4] = [0x39, 0xd3, 0x53, 0xff];
    const BLUE: [u8; 4] = [0x00, 0x00, 0xff, 0xff];

    #[test]
    fn a_kitty_image_decodes_into_the_pixels_that_were_drawn() -> termlens::Result<()> {
        // The claim the whole feature exists for: not "an image of about the
        // right size went out", but "*this* image went out".
        for mode in ["kitty", "kitty-plain", "kitty-chunks"] {
            let (_terminal, screen) = run(mode)?;
            let seen = screen.graphics();
            let bitmap = seen
                .last()
                .expect("a payload")
                .decode()
                .unwrap_or_else(|error| panic!("{mode}: {error}"));

            assert_eq!((bitmap.width(), bitmap.height()), (4, 2), "{mode}");
            assert_eq!(bitmap.pixel(0, 0), Some(GREEN), "{mode}");
            assert_eq!(bitmap.pixel(1, 1), Some(GREEN), "{mode}");
            assert_eq!(bitmap.pixel(2, 0), Some(BLUE), "{mode}");
            // The fourth column is transparent, and kitty has the alpha
            // channel to say so.
            assert_eq!(bitmap.pixel(3, 0), Some([0, 0, 0, 0]), "{mode}");
            assert_eq!(bitmap.pixel(4, 0), None, "out of bounds is None");

            let colours = bitmap.colours();
            assert_eq!(colours[0], (GREEN, 4), "{mode}: {colours:?}");
        }
        Ok(())
    }

    #[test]
    fn a_sixel_decodes_into_the_pixels_that_were_painted() -> termlens::Result<()> {
        let (_terminal, screen) = run("sixel")?;
        let seen = screen.graphics();
        let image = seen.last().expect("a payload");
        assert_eq!(image.protocol(), GraphicsProtocol::Sixel);
        assert_eq!(image.format(), GraphicsFormat::Sixel);
        assert_eq!(image.size(), Some((4, 2)), "from its raster attributes");

        let bitmap = image.decode().expect("decodes");
        assert_eq!((bitmap.width(), bitmap.height()), (4, 2));
        assert_eq!(bitmap.pixel(0, 0), Some([0, 255, 0, 255]));
        assert_eq!(bitmap.pixel(1, 1), Some([0, 255, 0, 255]));
        assert_eq!(bitmap.pixel(2, 0), Some([0, 0, 255, 255]));
        // Sixel has no alpha, so a pixel nothing painted reads as
        // transparent rather than as a colour we would have to invent.
        assert_eq!(bitmap.pixel(3, 0), Some([0, 0, 0, 0]));
        Ok(())
    }

    #[test]
    fn a_delete_refuses_to_decode_and_says_why() -> termlens::Result<()> {
        let (_terminal, screen) = run("delete")?;
        let seen = screen.graphics();
        let delete = &seen.payloads()[1];
        assert_eq!(
            delete.decode().unwrap_err(),
            DecodeError::NoImage(GraphicsAction::Delete)
        );
        Ok(())
    }

    #[test]
    fn an_uncaptured_payload_names_the_bound_rather_than_guessing() -> termlens::Result<()> {
        let mut terminal = Terminal::builder()
            .size(40, 10)
            .timeout(Duration::from_secs(30))
            .env_clear()
            .capture_graphics(0)
            .arg("kitty")
            .spawn(util::fixture_bin("image-echo"))?;
        terminal.wait_until(drawn)?;
        let screen = terminal.screen();
        let seen = screen.graphics();
        assert_eq!(
            seen.last().expect("a payload").decode().unwrap_err(),
            DecodeError::NotCaptured
        );
        Ok(())
    }
}
