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

    t.wait_until(|screen| screen.contains("status: ready"))?;
    assert!(t.screen().contains("status: ready"));

    t.send(Key::Char('q'))?;
    assert!(t.wait_exit()?.success());
    Ok(())
}
