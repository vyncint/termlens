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

/// Writing to a child that has stopped reading must fail at the
/// deadline with a diagnosis, not block forever. Before this, a large
/// `send_str` into a non-reading child blocked indefinitely — no
/// deadline applied to writes at all, and the eventual failure (when
/// the child died) described the teardown rather than the real cause.
#[test]
fn writing_to_a_child_that_is_not_reading_fails_at_the_deadline() {
    let start = Instant::now();
    let panicked = std::panic::catch_unwind(|| {
        let mut t = Terminal::builder()
            .size(80, 24)
            .env_clear()
            .timeout(Duration::from_millis(700))
            // The child stops itself: a stopped process cannot drain its
            // input, which fills the PTY buffer for certain. (A single
            // huge write is not a reliable way to provoke this — macOS
            // absorbs one of those and blocks on the *small repeated*
            // writes instead, which is exactly the shape typed input
            // has.)
            .args(["-c", "stty -icanon -echo; kill -STOP $$; sleep 30"])
            .spawn("/bin/sh")
            .expect("spawn");
        std::thread::sleep(Duration::from_millis(200));
        for _ in 0..5000 {
            t.send_str("xxxxxxxx");
        }
    })
    .expect_err("the write must fail loudly");

    let message = panicked
        .downcast_ref::<String>()
        .map_or_else(|| "<non-string panic>".to_owned(), Clone::clone);
    assert!(
        message.contains("not reading its input"),
        "the panic must name the real cause: {message}"
    );
    assert!(
        message.contains("--- screen ---"),
        "the panic must carry the screen: {message}"
    );
    assert!(
        start.elapsed() < Duration::from_secs(10),
        "the write blocked past its deadline: {:?}",
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
