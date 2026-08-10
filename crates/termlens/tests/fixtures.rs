//! Integration tests driving the deterministic fixture apps in `fixtures/`:
//! real TUI behavior (alternate screen, redraws, SIGWINCH) plus snapshot
//! coverage of the rendered grid.

use std::time::Duration;

use termlens::{Key, Terminal};

mod common;
use common as util;

fn spawn_fixture(name: &str) -> termlens::Result<Terminal> {
    Terminal::builder()
        .size(80, 24)
        .timeout(Duration::from_secs(30))
        .env_clear()
        .spawn(util::fixture_bin(name))
}

#[test]
fn hello_tui_draws_a_static_alt_screen_frame() -> termlens::Result<()> {
    let mut t = spawn_fixture("hello-tui")?;
    // The bottom-right corner is the LAST byte the fixture draws — once it
    // is on screen, the whole frame is. Waiting on an earlier line (like
    // "status: ready") would race the rest of the frame at chunk
    // boundaries and flake the snapshot below.
    t.wait_until(|s| s.contains("╯"))?;

    let screen = t.screen();
    assert!(screen.contains("status: ready"), "{screen}");
    let (_, _, cursor_visible) = screen.cursor();
    assert!(!cursor_visible, "hello-tui hides the cursor");
    assert_eq!(screen.find("hello-tui"), Some((1, 2)));
    insta::assert_snapshot!(t.screen());

    t.send(Key::Char('q'));
    assert!(t.wait_exit()?.success());
    Ok(())
}

#[test]
fn form_echo_round_trips_typed_input_and_special_keys() -> termlens::Result<()> {
    let mut t = spawn_fixture("form-echo")?;
    t.wait_until(|s| s.contains("form-echo ready"))?;

    t.send_str("hello");
    t.wait_until(|s| s.contains("input: hello"))?;

    // Each special key must round-trip: our xterm encoding -> PTY ->
    // crossterm's parser inside the fixture -> stable name on screen.
    for (key, name) in [
        (Key::Up, "last: up"),
        (Key::Down, "last: down"),
        (Key::Left, "last: left"),
        (Key::Right, "last: right"),
        (Key::Home, "last: home"),
        (Key::End, "last: end"),
        (Key::PageUp, "last: pageup"),
        (Key::PageDown, "last: pagedown"),
        (Key::Delete, "last: delete"),
        (Key::BackTab, "last: backtab"),
        (Key::F(1), "last: f:1"),
        (Key::F(5), "last: f:5"),
        (Key::F(12), "last: f:12"),
        (Key::Alt('x'), "last: alt:x"),
        (Key::Ctrl('a'), "last: ctrl:a"),
    ] {
        t.send(key);
        t.wait_until(|s| s.contains(name))?;
    }

    t.send(Key::Backspace);
    t.wait_until(|s| s.contains("input: hell") && s.contains("last: backspace"))?;

    t.send(Key::Enter);
    t.wait_until(|s| s.contains("submitted: hell"))?;
    insta::assert_snapshot!(t.screen());

    t.send(Key::Esc);
    assert!(t.wait_exit()?.success());
    Ok(())
}

#[test]
fn form_echo_reports_nonzero_exit_codes() -> termlens::Result<()> {
    let mut t = spawn_fixture("form-echo")?;
    t.wait_until(|s| s.contains("form-echo ready"))?;
    t.send(Key::Ctrl('x'));
    let status = t.wait_exit()?;
    assert!(!status.success());
    assert_eq!(status.code(), 42, "full status: {status}");
    Ok(())
}

#[test]
fn resize_reaches_the_child_as_sigwinch() -> termlens::Result<()> {
    let mut t = spawn_fixture("resize-echo")?;
    t.wait_until(|s| s.contains("size: 80x24"))?;

    t.resize(120, 40)?;
    t.wait_until(|s| s.contains("size: 120x40"))?;
    assert_eq!(t.screen().size(), (120, 40));

    t.resize(66, 20)?;
    t.wait_until(|s| s.contains("size: 66x20"))?;

    t.send(Key::Char('q'));
    assert!(t.wait_exit()?.success());
    Ok(())
}

#[test]
fn unicode_torture_renders_with_correct_widths() -> termlens::Result<()> {
    let mut t = spawn_fixture("unicode-torture")?;
    // Wait on the cursor, not on contains("done"): the predicate would turn
    // true before the trailing newline is processed, and the snapshot would
    // catch the cursor mid-line. After "done\r\n" the cursor rests at the
    // start of row 7 — that is the fixture's true "finished drawing" state.
    t.wait_until(|s| s.cursor() == (7, 0, true))?;

    let screen = t.screen();
    // "width: |一二三| vs |abc|" — "width: " is 7 columns, "|一二三|" is
    // 1 + 3×2 + 1 = 8, " vs " is 4: |abc| starts at column 19.
    assert_eq!(screen.find("|abc|"), Some((5, 19)));
    assert_eq!(screen.find("|一二三|"), Some((5, 7)));
    insta::assert_snapshot!(screen);

    // Release the fixture's stdin guard now that everything is asserted.
    t.send(Key::Enter);
    assert!(t.wait_exit()?.success());
    Ok(())
}
