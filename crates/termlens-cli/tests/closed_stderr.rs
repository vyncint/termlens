//! A stderr nobody reads any more — `termlens … 2>&1 | true` — made
//! `eprintln!` panic, exit 101, outside the 0/1/2 the CLI promises (#594).
//!
//! A binary of its own, like the library's `descriptors.rs`, because what
//! it asserts is a fact about the whole process's descriptor table. The
//! test closes its read end of termlens's stderr pipe and expects the
//! write to find no reader. In a binary shared with other tests, a sibling
//! test that forks while the pipe exists holds a copy of the read end until
//! its child execs; termlens's write then succeeds and inspect exits 0. The
//! stress workflow caught that once at 16 threads, and a sibling whose
//! child waits 3 s before exec reproduces it every time.

/// The trailer cannot be delivered, so inspect says so with exit 2; a
/// diagnostic that cannot be delivered keeps its exit 2.
#[cfg(unix)]
#[test]
fn a_closed_stderr_is_exit_two_not_a_panic() -> termlens::Result<()> {
    use std::process::{Command, Stdio};
    let bin = env!("CARGO_BIN_EXE_termlens");
    for args in [
        &["inspect", "sh", "-c", "echo hi"][..],
        &["--bogus"][..],
        &["render", "--text", "missing.snap"][..],
    ] {
        let mut child = Command::new(bin)
            .args(args)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()?;
        // Close the read end before termlens writes anything to it.
        drop(child.stderr.take());
        let status = child.wait()?;
        assert_eq!(status.code(), Some(2), "termlens {args:?}");
    }
    Ok(())
}
