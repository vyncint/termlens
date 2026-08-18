//! The drain must never block on a write it makes itself.
//!
//! The reader thread answers terminal queries. If it blocks writing a
//! reply into a full PTY input queue, it stops draining the master, the
//! child then blocks writing into a full output buffer, and neither side
//! can proceed — a permanent hang with no test input involved. This is
//! the one failure the harness must never produce, since a hung harness
//! cannot report anything at all.

use std::time::{Duration, Instant};

use termlens::{Error, Terminal};

/// ~1000 cursor-position queries generate ~8 KB of replies — past the
/// noncanonical tty buffer — from a child that never reads its input.
const FLOOD: &str = r"stty -icanon -echo; i=0; while [ $i -lt 1000 ]; do printf '\033[6n'; i=$((i+1)); done; printf 'DONE\n'; sleep 3";

#[test]
fn a_child_that_never_reads_its_replies_cannot_wedge_the_drain() {
    let start = Instant::now();
    let mut t = Terminal::builder()
        .size(80, 24)
        .env_clear()
        // answer_queries defaults to true: this is the default config.
        .timeout(Duration::from_secs(10))
        .args(["-c", FLOOD])
        .spawn("/bin/sh")
        .expect("spawn");

    // The child's own output must keep arriving: the drain is alive.
    t.wait_until(|s| s.contains("DONE"))
        .expect("the drain kept running, so DONE arrived");

    drop(t);
    assert!(
        start.elapsed() < Duration::from_secs(10),
        "teardown took {:?} — the drain or the reap is blocking",
        start.elapsed()
    );
}

/// Undeliverable replies are evidence, not silence: the diagnosis says
/// the application is not reading its input.
#[test]
fn undelivered_replies_are_named_in_the_diagnosis() {
    let mut t = Terminal::builder()
        .size(80, 24)
        .env_clear()
        .timeout(Duration::from_secs(10))
        .args(["-c", FLOOD])
        .spawn("/bin/sh")
        .expect("spawn");

    // Let the whole flood land first. The child prints DONE *after* its
    // last query, so once that is on screen the drain has read every
    // reply-generating byte and the queue has long since overflowed. Racing
    // a wall-clock deadline against the flood instead would make this test
    // sensitive to how fast the reader thread happens to be.
    t.wait_until(|s| s.contains("DONE"))
        .expect("the drain kept running");

    let err = t
        .wait_until_for(|s| s.contains("never-appears"), Duration::from_millis(200))
        .expect_err("the predicate can never hold");
    let message = err.to_string();
    assert!(matches!(err, Error::Timeout { .. }), "got: {err}");
    assert!(
        message.contains("not reading its input"),
        "the backlog should be diagnosed: {message}"
    );
}

// Not automated here: that a write into a *full* PTY buffer gives up at
// the terminal's deadline instead of blocking forever. Provoking one
// means getting a kernel to stop absorbing writes, and the platforms
// disagree at every turn — macOS swallows a single 256 KiB write in
// 13ms but blocks on small repeated ones; Linux absorbs far more before
// blocking, and keeps the master writable even after the child is gone.
// Four attempts produced four behaviours and no stable gate, and a
// flaky test for a hang is worse than none: it teaches people to rerun
// CI.
//
// The guarantee itself was verified by hand on both platforms; the
// ubuntu run of this branch printed exactly:
//
//     termlens: failed to send literal text to `/bin/sh -c ...` (the
//     application is not reading its input, and the PTY buffer is full
//     — no progress in 700ms)
//     --- screen ---
//
// The mechanism that produces it — every write acknowledged by the
// writer thread within the terminal's deadline — is exercised by every
// other test in the suite, since all typed input now travels that path.

/// The ordinary case must be untouched: an application that reads its
/// replies still gets every one of them.
#[test]
fn replies_still_reach_an_application_that_reads_them() -> termlens::Result<()> {
    let mut t = Terminal::builder()
        .timeout(Duration::from_secs(10))
        .args([
            "-c",
            concat!(
                r"stty -icanon -echo; printf 'abc\033[6n'; ",
                r#"reply=$(head -c 6 | tr '\033' 'E'); "#,
                r#"printf '\nunblocked:%s' "$reply"; read guard"#
            ),
        ])
        .spawn("/bin/sh")?;
    t.wait_until(|s| s.contains("unblocked:E[1;4R"))?;
    t.send(termlens::Key::Enter);
    assert!(t.wait_exit()?.success());
    Ok(())
}
