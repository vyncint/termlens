//! The `TERMLENS_ARTIFACT_DIR` hook (#251): a wait that fails writes the
//! screen it embeds to that directory, in the shape the `report` action and
//! `termlens-cli` read back. Alone in this binary because the variable is
//! process-wide: the second test records nothing itself, only in the
//! processes it starts.

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
    assert!(matches!(err, termlens::Error::Timeout { .. }), "{err}");

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
    let diff = saved.diff(err.screen().unwrap());
    assert!(diff.is_empty(), "{diff}");
    assert!(saved.cell(0, 0).unwrap().style().bold, "styles come along");

    std::env::remove_var("TERMLENS_ARTIFACT_DIR");
    let _ = std::fs::remove_dir_all(&dir);
    t.send(termlens::Key::Enter)?;
    t.wait_exit()?;
    Ok(())
}

/// A saved screen, in whichever format this build writes.
fn read_screen(path: &std::path::Path) -> termlens::Result<Screen> {
    let body = std::fs::read_to_string(path)?;
    #[cfg(feature = "serde")]
    return Ok(serde_json::from_str(&body).expect("the file is a Screen"));
    #[cfg(not(feature = "serde"))]
    Screen::parse(&body)
}

/// Two processes whose failing tests have the same name leave two files
/// (#711). The counter in a file's name is per process and `<test>` is the
/// test's path inside its binary, so two test binaries with a `smoke` test,
/// or a test that cargo-nextest retried, both wrote `smoke-1.screen.*`, and
/// the second screen replaced the first. Here this binary runs this test
/// twice into one directory, as two processes, each leaving a screen that
/// says which run it was. Only those processes record anything, and each
/// is given the directory, so the variable the test above sets in this
/// process does not reach them.
#[test]
fn same_named_failures_in_two_processes_leave_two_files() -> termlens::Result<()> {
    const NAME: &str = "same_named_failures_in_two_processes_leave_two_files";
    const RUN: &str = "TERMLENS_ARTIFACT_RUN";
    if std::env::var_os(RUN).is_some() {
        let mut t = common::emit(&["run=", "--env", RUN, " .", "--wait"])?;
        t.wait_until(|s| s.contains(" ."))?;
        let err = t
            .wait_until_for(|s| s.contains("never"), Duration::from_millis(100))
            .expect_err("the wait must time out");
        assert!(matches!(err, termlens::Error::Timeout { .. }), "{err}");
        t.send(termlens::Key::Enter)?;
        t.wait_exit()?;
        return Ok(());
    }

    let dir = std::env::temp_dir().join(format!("termlens-artifact-two-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    for run in ["first", "second"] {
        let out = common::rerun_self(&["emit"])?
            .args(["--exact", NAME, "--nocapture"])
            .env("TERMLENS_ARTIFACT_DIR", &dir)
            .env(RUN, run)
            .output()?;
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success() && stdout.contains("1 passed"),
            "{run}: {}\n{stdout}\n{}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
    }

    let mut files: Vec<_> = std::fs::read_dir(&dir)?
        .map(|e| e.expect("a directory entry").path())
        .collect();
    files.sort();
    let names: Vec<_> = files
        .iter()
        .map(|f| f.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    let ext = if cfg!(feature = "serde") {
        "json"
    } else {
        "txt"
    };
    // The names the report action looks for, the second one numbered on.
    assert_eq!(
        names,
        [
            format!("{NAME}-1.screen.{ext}"),
            format!("{NAME}-2.screen.{ext}")
        ],
    );
    for (file, run) in files.iter().zip(["first", "second"]) {
        let screen = read_screen(file)?;
        assert!(
            screen.contains(&format!("run={run} .")),
            "{}:\n{screen}",
            file.display()
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}
