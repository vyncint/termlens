//! `Screen::unsupported`: the sequences the emulator did not implement, so
//! a test can tell a plausible-looking wrong grid from a right one (#266).

use std::time::Duration;

use termlens::{Key, Terminal};

mod common;

fn emit(steps: &[&str]) -> termlens::Result<Terminal> {
    common::spawn_emit(
        Terminal::builder()
            .size(40, 4)
            .timeout(Duration::from_secs(10)),
        steps,
    )
}

fn shapes(t: &Terminal) -> Vec<String> {
    t.screen()
        .unsupported()
        .iter()
        .map(|s| s.to_string())
        .collect()
}

/// A sequence nobody honours is named, in the form the timeout messages
/// use, once however often it is sent.
#[test]
#[cfg_attr(
    windows,
    ignore = "ConPTY renders what it implements and drops the rest before termlens sees a byte, so the record there is the console's, not the application's (#149)"
)]
fn an_unimplemented_sequence_is_listed_once() -> termlens::Result<()> {
    let mut t = emit(&[
        "--csi", "20h", "--csi", "20h", "--esc", "D", "text", "--wait",
    ])?;
    t.wait_until(|s| s.contains("text"))?;
    assert_eq!(shapes(&t), ["^[[20h", "^[D"]);
    assert_eq!(t.screen().unsupported_overflow(), 0);
    t.send(Key::Enter)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}

/// Everything termlens renders or answers itself is not "unsupported",
/// even though the backend declines it: the list means what its name says.
#[test]
#[cfg_attr(
    windows,
    ignore = "ConPTY renders what it implements and drops the rest before termlens sees a byte, so the record there is the console's, not the application's (#149)"
)]
fn an_application_using_only_what_is_implemented_reports_nothing() -> termlens::Result<()> {
    let mut t = emit(&[
        "--raw",
        r"\e(0lqk\e(B\eH\e[3g\e[4h\e[4l\e[!p\e[2 q\e[?1004h\e[?2026h\e[?2026l\e[?1000h",
        "--raw",
        r"\e]8;;http://a/\e\\link\e]8;;\e\\\e[31mred\e[0m",
        " DONE",
        "--wait",
    ])?;
    t.wait_until(|s| s.contains("DONE"))?;
    assert_eq!(shapes(&t), Vec::<String>::new(), "{}", t.screen());
    t.send(Key::Enter)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}

/// A visual bell is a different event from an audible one, and a request to
/// resize the window is recorded rather than honoured.
#[test]
#[cfg_attr(
    windows,
    ignore = "ConPTY renders what it implements and drops the rest before termlens sees a byte, so the record there is the console's, not the application's (#149)"
)]
fn a_visual_bell_and_a_resize_request_are_observable() -> termlens::Result<()> {
    let mut t = emit(&["--esc", "g", "--csi", "8;10;60t", "DONE", "--wait"])?;
    t.wait_until(|s| s.contains("DONE"))?;
    let s = t.screen();
    assert_eq!(s.visual_bells(), 1, "{s}");
    assert_eq!(s.bells(), 0, "a flash is not a beep: {s}");
    assert_eq!(shapes(&t), ["^[[8;10;60t"]);
    assert_eq!(s.size(), (40, 4), "the grid is the test's to size: {s}");
    t.send(Key::Enter)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}

/// The record keeps 32 distinct shapes and counts the rest, so a stream
/// that invents thousands cannot grow a snapshot.
#[test]
#[cfg_attr(
    windows,
    ignore = "ConPTY renders what it implements and drops the rest before termlens sees a byte, so the record there is the console's, not the application's (#149)"
)]
fn the_record_is_bounded() -> termlens::Result<()> {
    let mut stream = String::new();
    for mode in 20..60u16 {
        stream.push_str(&format!(r"\e[{mode}h"));
    }
    let mut t = emit(&["--raw", &stream, "DONE", "--wait"])?;
    t.wait_until(|s| s.contains("DONE"))?;
    let s = t.screen();
    assert_eq!(s.unsupported().len(), 32, "{s}");
    assert_eq!(s.unsupported_overflow(), 8, "{s}");
    assert_eq!(&*s.unsupported()[0], "^[[20h");
    t.send(Key::Enter)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}
