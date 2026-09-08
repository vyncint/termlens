use std::time::Duration;

use termlens::{Key, Terminal};

mod common;
use common as util;

/// Keep the README's first example compile-checked against a real fixture.
/// The README uses `myapp`; this test uses the equivalent workspace fixture so
/// Cargo can build and run it in CI.
#[test]
fn readme_example_compiles_and_runs() -> termlens::Result<()> {
    let mut t = Terminal::builder()
        .size(80, 24)
        .env_clear()
        .timeout(Duration::from_secs(5))
        .spawn(util::fixture_bin("hello-tui"))?;

    // The README's most distinctive line: wait for the marker, settle,
    // snapshot with styles — the macro makes the three decisions rule 2 of
    // `docs/DESIGN.md` §2 used to ask the reader to make by hand. The macro
    // is behind the default `insta` feature, so the no-default-features leg
    // takes the low-level spelling with the same three steps.
    #[cfg(feature = "insta")]
    termlens::assert_screen_snapshot!(t, after = |s| s.contains("status: ready"));
    #[cfg(not(feature = "insta"))]
    insta::assert_snapshot!(t
        .snapshot_after(|s| s.contains("status: ready"))?
        .with_styles());

    t.send(Key::Char('q'))?;
    assert!(t.wait_exit()?.success());
    Ok(())
}
