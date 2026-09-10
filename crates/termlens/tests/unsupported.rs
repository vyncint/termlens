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

/// #320: an SGR the attribute shadow recovers is *implemented*, and naming
/// it here contradicted the cell on the same `Screen` — `Style::blink` said
/// the cell blinks while `unsupported()` said `^[[5m` was dropped. It also
/// made the natural check impossible: `assert!(s.unsupported().is_empty())`
/// could never pass for an application that blinks or strikes through.
///
/// Measured against 0.10.1 by `termlens-demo` driving a real ratatui
/// application, which is where the defect was found.
#[test]
#[cfg_attr(
    windows,
    ignore = "ConPTY renders what it implements and drops the rest before termlens sees a byte, so the record there is the console's, not the application's (#149)"
)]
fn the_sgrs_the_shadow_recovers_are_not_named_as_unimplemented() -> termlens::Result<()> {
    // Every parameter `shadow::sgr_substitute` carries, on and off.
    for param in [5u16, 6, 8, 9, 25, 28, 29] {
        let mut t = emit(&["--csi", &format!("{param}m"), "text", "--wait"])?;
        t.wait_until(|s| s.contains("text"))?;
        assert_eq!(
            shapes(&t),
            Vec::<String>::new(),
            "^[[{param}m is recovered by the shadow, so nothing is unimplemented"
        );
        t.send(Key::Enter)?;
        assert!(t.wait_exit()?.success());
    }

    // And the attribute really is on the cell — the half that makes the
    // above a fix rather than silence.
    let mut t = emit(&["--csi", "5m", "--csi", "9m", "marked", "--wait"])?;
    t.wait_until(|s| s.contains("marked"))?;
    let s = t.screen();
    let style = *s.cell(0, 0).expect("the cell").style();
    assert!(style.blink && style.strikethrough, "{style:?}");
    assert!(s.unsupported().is_empty(), "{:?}", shapes(&t));
    t.send(Key::Enter)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}

/// The other half of #320: an SGR nobody models is still named. `59` is
/// underline colour — the shadow does not carry it and neither does vt100 —
/// and it appeared in the original report alongside the four false ones.
#[test]
#[cfg_attr(
    windows,
    ignore = "ConPTY renders what it implements and drops the rest before termlens sees a byte, so the record there is the console's, not the application's (#149)"
)]
fn an_sgr_nobody_models_is_still_named() -> termlens::Result<()> {
    let mut t = emit(&["--csi", "59m", "text", "--wait"])?;
    t.wait_until(|s| s.contains("text"))?;
    assert_eq!(shapes(&t), ["^[[59m"]);
    t.send(Key::Enter)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}

/// A mixed SGR is reported whole, and this test says which parameter keeps
/// it in the record.
///
/// `^[[5;59m` carries one parameter the shadow recovers (`5`, blink) and one
/// nobody models (`59`). Dropping the sequence because *some* of it is
/// implemented would hide the gap, so the rule is all-or-nothing per
/// sequence: one unrecovered parameter keeps the whole shape.
///
/// The cost of that rule, measured rather than assumed: `^[[1;5;31m` is
/// still named even though bold, red *and* blink all reach the cell — `1`
/// and `31` are vt100's to implement, and this tracker knows what the
/// shadow recovers, not what the backend does. Narrowing it further means
/// enumerating vt100's own SGR surface, which is a bigger claim than a
/// patch should make.
#[test]
#[cfg_attr(
    windows,
    ignore = "ConPTY renders what it implements and drops the rest before termlens sees a byte, so the record there is the console's, not the application's (#149)"
)]
fn a_mixed_sgr_is_named_by_the_parameter_nobody_models() -> termlens::Result<()> {
    let mut t = emit(&["--csi", "5;59m", "text", "--wait"])?;
    t.wait_until(|s| s.contains("text"))?;
    let s = t.screen();
    assert_eq!(shapes(&t), ["^[[5;59m"], "59 keeps it: {s}");
    assert!(
        s.cell(0, 0).expect("the cell").style().blink,
        "and 5 still reached the cell: {s}"
    );
    t.send(Key::Enter)?;
    assert!(t.wait_exit()?.success());

    // The documented residue: everything here was applied, and it is still
    // named. A test says so rather than a reader discovering it.
    let mut t = emit(&["--csi", "1;5;31m", "text", "--wait"])?;
    t.wait_until(|s| s.contains("text"))?;
    let s = t.screen();
    let style = *s.cell(0, 0).expect("the cell").style();
    assert!(style.bold && style.blink, "all three applied: {style:?}");
    assert_eq!(
        shapes(&t),
        ["^[[1;5;31m"],
        "still named: the tracker knows the shadow's set, not vt100's"
    );
    t.send(Key::Enter)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}
