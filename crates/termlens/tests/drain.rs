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
        .timeout(Duration::from_millis(600))
        .args(["-c", FLOOD])
        .spawn("/bin/sh")
        .expect("spawn");

    let err = t
        .wait_until(|s| s.contains("never-appears"))
        .expect_err("the predicate can never hold");
    let message = err.to_string();
    assert!(matches!(err, Error::Timeout { .. }), "got: {err}");
    assert!(
        message.contains("not reading its input"),
        "the backlog should be diagnosed: {message}"
    );
}

/// Writes into a terminal whose child is gone fail loudly and promptly,
/// rather than blocking or silently doing nothing.
///
/// The related guarantee — that a write into a *full* PTY buffer gives
/// up at the terminal's deadline instead of blocking forever — is
/// deliberately not automated here. Provoking a full buffer means
/// getting a kernel to stop absorbing writes, and the volume and timing
/// that achieve it differ so much between macOS and Linux that three
/// attempts produced three different behaviours, none of them a stable
/// gate. It was verified by hand on both platforms instead:
///
/// ```text
/// termlens: failed to send literal text to `/bin/sh -c …` (the application is
/// not reading its input, and the PTY buffer is full — no progress in 700ms)
/// --- screen ---
/// ```
#[test]
fn writing_after_the_child_is_gone_fails_loudly() {
    let start = Instant::now();
    let panicked = std::panic::catch_unwind(|| {
        let mut t = Terminal::builder()
            .size(80, 24)
            .env_clear()
            .timeout(Duration::from_millis(700))
            .args(["-c", "exit 0"])
            .spawn("/bin/sh")
            .expect("spawn");
        t.wait_exit().expect("the child exits immediately");
        // The terminal is torn down; typing into it is a broken test and
        // must say so rather than no-op.
        for _ in 0..200 {
            t.send_str("xxxxxxxx");
        }
    })
    .expect_err("writing to a dead terminal must fail");

    let message = panicked
        .downcast_ref::<String>()
        .map_or_else(|| "<non-string panic>".to_owned(), Clone::clone);
    assert!(
        message.contains("failed to send"),
        "the panic must say what could not be sent: {message}"
    );
    assert!(
        message.contains("--- screen ---"),
        "the panic must carry the screen: {message}"
    );
    assert!(
        start.elapsed() < Duration::from_secs(20),
        "the write should fail promptly, not hang: {:?}",
        start.elapsed()
    );
}

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
