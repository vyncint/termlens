//! termlens fixture: transmits one known inline image, so a test can assert
//! what came off the wire against what was drawn.
//!
//! Fixture rules: deterministic by construction — no clocks, no animation,
//! no randomness. The same bytes go out on every run.
//!
//! ```text
//! image-echo kitty        one 4x2 RGBA image, zlib'd, placed on 2x1 cells
//! image-echo kitty-plain  the same image uncompressed
//! image-echo kitty-chunks the same image, split across the protocol's chunks
//! image-echo sixel        the same image as a sixel
//! image-echo delete       an image, then the delete that takes it away
//! ```
//!
//! Every mode ends by printing `done` and blocking on stdin, which is the
//! instant-exit guard the suite requires of a program that writes and stops.

use std::io::{self, Read, Write};

/// Four pixels across, two down. Two flat colours and one transparent
/// column, so an assertion can name an exact pixel rather than a tolerance.
const WIDTH: u32 = 4;
const HEIGHT: u32 = 2;
/// GitHub Primer's brightest contribution green, which is where this
/// fixture's colours came from.
const GREEN: [u8; 4] = [0x39, 0xd3, 0x53, 0xff];
const BLUE: [u8; 4] = [0x00, 0x00, 0xff, 0xff];
const CLEAR: [u8; 4] = [0x00, 0x00, 0x00, 0x00];

/// Column 0–1 green, column 2 blue, column 3 transparent.
fn pixels() -> Vec<[u8; 4]> {
    let mut out = Vec::new();
    for _ in 0..HEIGHT {
        out.extend_from_slice(&[GREEN, GREEN, BLUE, CLEAR]);
    }
    out
}

fn rgba() -> Vec<u8> {
    pixels().iter().flatten().copied().collect()
}

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn base64(data: &[u8]) -> String {
    let mut out = String::new();
    for group in data.chunks(3) {
        let mut bits = 0u32;
        for (index, byte) in group.iter().enumerate() {
            bits |= u32::from(*byte) << (16 - 8 * index);
        }
        for index in 0..=group.len() {
            out.push(char::from(
                ALPHABET[(bits >> (18 - 6 * index) & 0x3f) as usize],
            ));
        }
        for _ in group.len()..3 {
            out.push('=');
        }
    }
    out
}

/// `a=T` with the placement pinned in cells, the way an application that
/// lays out in characters and draws in pixels has to.
fn kitty(compress: bool, chunk: usize) -> String {
    let raw = rgba();
    let payload = base64(&if compress {
        miniz_oxide::deflate::compress_to_vec_zlib(&raw, 6)
    } else {
        raw
    });
    let control = format!(
        "a=T,q=2,f=32,{}s={WIDTH},v={HEIGHT},i=7,c=2,r=1",
        if compress { "o=z," } else { "" }
    );

    let mut out = String::new();
    let mut chunks = payload.as_bytes().chunks(chunk.max(1)).peekable();
    let mut first = true;
    while let Some(part) = chunks.next() {
        out.push_str("\x1b_G");
        if first {
            out.push_str(&control);
            out.push(',');
            first = false;
        }
        out.push_str(&format!("m={};", u8::from(chunks.peek().is_some())));
        out.push_str(std::str::from_utf8(part).unwrap_or_default());
        out.push_str("\x1b\\");
    }
    out
}

/// The same shape as a sixel: one band, two colour registers, raster
/// attributes declaring the size. The transparent column is simply never
/// painted — sixel has no alpha to say it with.
///
/// The colours are pure rather than Primer's, and deliberately: a sixel
/// register is a *percentage* of 255, so most 8-bit values do not survive
/// the round trip. Choosing ones that do keeps the assertion exact instead
/// of approximate, which is the difference between a test that catches a
/// wrong colour and one that tolerates it.
fn sixel() -> String {
    let mut out = format!("\x1bP0;1;0q\"1;1;{WIDTH};{HEIGHT}");
    out.push_str("#0;2;0;100;0"); // green
    out.push_str("#1;2;0;0;100"); // blue
                                  // Both pixel rows sit in the one six-row band, so bits 0 and 1 are set:
                                  // `?` + 0b11 = `B`. `$` returns to the left margin between colours, and
                                  // `?` advances a column without painting it.
    out.push_str("#0BB$");
    out.push_str("#1??B$");
    out.push('-');
    out.push_str("\x1b\\");
    out
}

fn main() -> io::Result<()> {
    let mode = std::env::args().nth(1).unwrap_or_else(|| "kitty".into());
    let mut out = io::stdout();

    // A label above and below, at known rows, so a test can see that the
    // text layer left the image's cells alone.
    write!(out, "\x1b[H\x1b[2Jlabel above\r\n")?;
    // Row 2, column 5 (1-based), which is where every payload below lands.
    write!(out, "\x1b[2;5H")?;

    match mode.as_str() {
        "kitty" => write!(out, "{}", kitty(true, 4096))?,
        "kitty-plain" => write!(out, "{}", kitty(false, 4096))?,
        // Small enough that the payload takes three escapes.
        "kitty-chunks" => write!(out, "{}", kitty(false, 16))?,
        "sixel" => write!(out, "{}", sixel())?,
        "delete" => {
            write!(out, "{}", kitty(true, 4096))?;
            write!(out, "\x1b_Ga=d,d=I,i=7,q=2\x1b\\")?;
        }
        "none" => {}
        other => {
            write!(out, "unknown mode {other}")?;
        }
    }

    write!(out, "\x1b[4;1Hlabel below\r\ndone\r\n")?;
    out.flush()?;

    // The instant-exit guard: a child that writes and exits within its first
    // milliseconds can lose output to the OS pty teardown, so hold until the
    // test has asserted and releases us.
    let mut byte = [0u8; 1];
    let _ = io::stdin().read(&mut byte)?;
    Ok(())
}
