//! The `TERMLENS_ARTIFACT_DIR` hook (#251): a wait that fails writes the
//! screen it embeds to that directory, in the shape the `report` action and
//! `termlens-cli` read back. Alone in this binary because the variable is
//! process-wide.

use std::time::Duration;

use termlens::Screen;

mod common;

#[test]
fn a_failed_wait_writes_its_screen_to_the_artifact_dir() -> termlens::Result<()> {
    let dir = std::env::temp_dir().join(format!("termlens-artifact-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    // Edition 2021: this is a safe call, and this test is alone in its
    // process. The hook reads the variable at each failure, so setting it
    // after the terminal spawned is fine.
    std::env::set_var("TERMLENS_ARTIFACT_DIR", &dir);

    let mut t = common::emit(&["--raw", r"\e[1mReady\e[0m", "--wait"])?;
    t.wait_until(|s| s.contains("Ready"))?;
    let err = t
        .wait_until_for(|s| s.contains("never"), Duration::from_millis(100))
        .expect_err("the wait must time out");
    assert!(matches!(err, termlens::Error::Timeout { .. }));

    let mut files: Vec<_> = std::fs::read_dir(&dir)?
        .map(|e| e.expect("a directory entry").path())
        .collect();
    files.sort();
    assert_eq!(files.len(), 1, "{files:?}");
    let name = files[0].file_name().unwrap().to_string_lossy().into_owned();
    // `<test>-<n>.screen.<ext>`: the thread name is this test's path.
    assert!(
        name.starts_with("a_failed_wait_writes_its_screen_to_the_artifact_dir-1.screen."),
        "{name}"
    );
    let body = std::fs::read_to_string(&files[0])?;
    let saved: Screen = if cfg!(feature = "serde") {
        assert!(name.ends_with(".json"), "{name}");
        #[cfg(feature = "serde")]
        {
            serde_json::from_str(&body).expect("the file is a Screen")
        }
        #[cfg(not(feature = "serde"))]
        unreachable!()
    } else {
        assert!(name.ends_with(".txt"), "{name}");
        Screen::parse(&body)?
    };
    assert!(saved.diff(err.screen().unwrap()).is_empty());
    assert!(saved.cell(0, 0).unwrap().style().bold, "styles come along");

    std::env::remove_var("TERMLENS_ARTIFACT_DIR");
    let _ = std::fs::remove_dir_all(&dir);
    t.send(termlens::Key::Enter)?;
    t.wait_exit()?;
    Ok(())
}
