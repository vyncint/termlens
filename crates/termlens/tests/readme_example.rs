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

    // The README waits on "Ready" and snapshots. The fixture's last byte is
    // the bottom-right corner, so the corner is waited on as well — rule 2
    // of `docs/DESIGN.md` §2: a whole-screen snapshot must wait on the last
    // thing the application paints, or it races the rest of the frame at a
    // chunk boundary.
    t.wait_until(|screen| screen.contains("status: ready") && screen.contains("╯"))?;
    // The README's most distinctive line, compiled against the `insta` the
    // crate itself dev-depends on. A dev-dependency is present in every
    // feature configuration, so this needs no `cfg(feature = "insta")`: the
    // `--no-default-features` CI job builds and runs it too.
    insta::assert_snapshot!(t.screen());

    t.send(Key::Char('q'))?;
    assert!(t.wait_exit()?.success());
    Ok(())
}
