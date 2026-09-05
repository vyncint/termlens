//! `termlens::bin!` from the one place it is designed for: the integration
//! tests of the package that owns the binary, where Cargo sets
//! `CARGO_BIN_EXE_hello-tui`. termlens's own tests cannot use the macro —
//! the crate has no binaries — which is why this test lives in a fixture.

use std::time::Duration;

use termlens::Key;

/// The bare form: the binary at 80x24, a cleared environment, a five-second
/// deadline, and a `Result` like `spawn`'s.
#[test]
fn bin_spawns_this_package_s_binary_under_the_defaults() -> termlens::Result<()> {
    let mut t = termlens::bin!("hello-tui")?;
    // The bottom-right corner is the last byte the fixture draws.
    t.wait_until(|s| s.contains("╯"))?;
    let s = t.screen();
    assert_eq!(s.size(), (80, 24), "{s}");
    assert!(s.contains("status: ready"), "{s}");
    assert!(s.alternate_screen(), "{s}");
    t.send(Key::Char('q'))?;
    assert!(t.wait_exit()?.success());
    Ok(())
}

/// Builder calls follow the name and override the defaults; a trailing
/// comma is allowed, since a list of calls tends to grow one per line.
#[test]
fn bin_takes_builder_calls_after_the_name() -> termlens::Result<()> {
    let mut t = termlens::bin!(
        "hello-tui",
        size(100, 30),
        timeout(Duration::from_secs(20)),
        env("HELLO_TUI_UNUSED", "1"),
    )?;
    t.wait_until(|s| s.contains("╯"))?;
    let s = t.screen();
    assert_eq!(s.size(), (100, 30), "{s}");
    assert!(s.contains("status: ready"), "{s}");
    t.send(Key::Char('q'))?;
    assert!(t.wait_exit()?.success());
    Ok(())
}
