//! The drain must never block on a write it makes itself.
//!
//! The reader thread answers terminal queries. If it blocks writing a
//! reply into a full PTY input queue, it stops draining the master, the
//! child then blocks writing into a full output buffer, and neither side
//! can proceed — a permanent hang with no test input involved. This is
//! the one failure the harness must never produce, since a hung harness
//! cannot report anything at all.
//!
//! Every program here runs in `--raw-mode`, so replies are neither echoed
//! onto the grid nor held for a newline.

use std::time::{Duration, Instant};

use termlens::{Error, Terminal};

mod common;

/// ~1000 cursor-position queries generate ~6 KB of replies from a child
/// that never reads its input: past macOS's tty input queue, though within
/// the ~13.8 KB Linux takes in raw mode (see `docs/DESIGN.md` §1).
fn flood() -> termlens::Result<Terminal> {
    let queries = r"\e[6n".repeat(1000);
    common::spawn_emit(
        Terminal::builder()
            .size(80, 24)
            .env_clear()
            // answer_queries defaults to true: this is the default config.
            .timeout(Duration::from_secs(10)),
        &["--raw-mode", "--raw", &queries, "DONE\n", "--sleep", "3s"],
    )
}

#[test]
fn a_child_that_never_reads_its_replies_cannot_wedge_the_drain() {
    let start = Instant::now();
    let mut t = flood().expect("spawn");

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

/// Undeliverable replies are evidence, not silence — where the kernel lets
/// us have the evidence.
///
/// This used to hold on both platforms, and for the wrong reason: replies
/// were enqueued one per answer, so a flood overflowed our own 64-deep queue
/// and the drops were ours to count. Batching per read fixed that (a
/// well-behaved application was losing answers too), and it moved where the
/// truth lives.
///
/// Now the replies reach the kernel. In raw mode a write into a full
/// terminal input queue **blocks**, on Linux as on macOS, so the replies are
/// visibly stuck in our writer and counted. But how much the queue holds
/// differs: this flood's ~6 KB overflows macOS's, while Linux's raw-mode
/// queue takes about 13.8 KB, so there nothing is stuck and nothing is
/// named. (In canonical mode Linux's `n_tty` **discards** what does not fit
/// its 4 KB buffer, and that loss is invisible.) The assertion is scoped to
/// where this flood overflows the queue.
///
/// The trade is deliberate: a diagnosis for a pathological application, in
/// exchange for a well-behaved one actually getting its answers.
#[test]
fn undelivered_replies_are_named_where_the_kernel_makes_them_visible() {
    let mut t = flood().expect("spawn");

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
    // True everywhere: the failure is a timeout carrying the screen, so a CI
    // log shows what the application had managed to do.
    assert!(matches!(err, Error::Timeout { .. }), "got: {err}");
    assert!(
        err.screen().is_some_and(|s| s.contains("DONE")),
        "{message}"
    );

    // True only where this flood fills the queue and blocks the write.
    #[cfg(target_os = "macos")]
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
#[cfg_attr(
    windows,
    ignore = "ConPTY answers or eats the child's queries itself, so they never reach the responder (#149)"
)]
fn replies_still_reach_an_application_that_reads_them() -> termlens::Result<()> {
    let mut t = common::spawn_emit(
        Terminal::builder().timeout(Duration::from_secs(10)),
        &[
            "--raw-mode",
            "abc",
            "--csi",
            "6n",
            "\nunblocked:",
            "--read",
            "6",
            "--wait",
        ],
    )?;
    t.wait_until(|s| s.contains("unblocked:E[1;4R"))?;
    t.send(termlens::Key::Enter)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}

/// A send that gives up at the deadline is not withdrawn (#590). The
/// application here sleeps before it reads, so 200,000 bytes fill the
/// buffer and the send fails — and so does the next one, queued behind it.
/// Once the application reads, it receives both, in order. The error says
/// so, because "the application never got these bytes" would be false.
#[test]
#[cfg_attr(
    windows,
    ignore = "ConPTY buffers typed input on its own side, so a full queue is not reached the same way (#149)"
)]
fn a_send_that_gives_up_at_the_deadline_is_still_delivered() -> termlens::Result<()> {
    const N: usize = 200_000;
    let count = N.to_string();
    let mut t = common::spawn_emit(
        // The builder's deadline is the send's; every wait below names
        // its own, generous one.
        Terminal::builder().timeout(Duration::from_millis(500)),
        &[
            "--raw-mode",
            "ready",
            "NL",
            "--sleep",
            "2s",
            "--read-count",
            &count,
            "x",
            "NL",
            "--read-count",
            "1",
            "y",
            "NL",
            "--wait",
        ],
    )?;
    t.wait_until_for(|s| s.contains("ready"), Duration::from_secs(10))?;
    for input in ["x".repeat(N), "y".to_owned()] {
        match t.send_str(&input) {
            Err(Error::Write { what, .. }) => assert!(
                what.contains("stays queued and reaches the application if it reads again"),
                "{what}"
            ),
            other => panic!("the queue is not moving, so the send gives up: {other:?}"),
        }
    }
    // Both arrive, the x's first.
    t.wait_until_for(
        |s| s.contains(&count) && s.text().lines().nth(2) == Some("1"),
        Duration::from_secs(20),
    )?;
    t.send(termlens::Key::Enter)?;
    assert!(t.wait_exit_for(Duration::from_secs(10))?.success());
    Ok(())
}

/// A send blocked on a full input queue ends when the application exits,
/// and says so (#770). The write was waited for in one piece, and Linux
/// does not wake a master write when the other side closes, so the send
/// waited out the whole deadline and then reported "the application is not
/// reading its input … reaches the application if it reads again" about an
/// application that was gone. macOS fails such a write with EIO instead,
/// which said nothing of the exit either.
///
/// The application reads one byte of what is typed and exits with the rest
/// unread, so the send is under way when it goes, and a mebibyte is past
/// what either kernel's input queue holds: the write cannot finish.
#[test]
#[cfg_attr(
    windows,
    ignore = "ConPTY buffers typed input on its own side, so a full queue is not reached the same way (#149)"
)]
fn a_send_blocked_on_a_full_queue_ends_when_the_application_exits() -> termlens::Result<()> {
    const N: usize = 1 << 20;
    // The deadline the send must not wait out; the waits name their own.
    const DEADLINE: Duration = Duration::from_secs(20);
    let mut t = common::spawn_emit(
        Terminal::builder().timeout(DEADLINE),
        &["--raw-mode", "ready", "--skip", "1"],
    )?;
    t.wait_until_for(|s| s.contains("ready"), Duration::from_secs(10))?;
    let start = Instant::now();
    let sent = t.send_str(&"x".repeat(N));
    let took = start.elapsed();
    match sent {
        Err(Error::Write { what, .. }) => assert!(
            what.contains(&format!(
                "released the terminal (EOF) before this write of {N} bytes finished"
            )),
            "{what}"
        ),
        other => panic!("the application exited mid-send: {other:?}"),
    }
    assert!(
        took < DEADLINE / 2,
        "the send outlived the application by {took:?}, near its {DEADLINE:?} deadline"
    );
    // The next one is refused at once, as it was.
    match t.send_str("y") {
        Err(Error::Write { what, .. }) => assert!(what.contains("(EOF)"), "{what}"),
        other => panic!("the terminal is closed: {other:?}"),
    }
    Ok(())
}

/// A zero deadline does not make a send to an application that is reading
/// fail (#632). Every send waited for the writer thread's acknowledgement
/// for exactly the deadline, so with `Duration::ZERO` each one failed,
/// blaming a full buffer, while the application got every byte. The
/// acknowledgement now has a floor, and a full buffer is claimed only when a
/// write is seen blocked.
#[test]
fn a_zero_timeout_send_to_a_reading_application_succeeds() -> termlens::Result<()> {
    let mut t = common::spawn_emit(
        // The deadline under test is the send's; every wait names its own.
        Terminal::builder().timeout(Duration::ZERO),
        &["--raw-mode", "ready", "NL", "--echo"],
    )?;
    t.wait_until_for(|s| s.contains("ready"), Duration::from_secs(10))?;
    for n in 1..=10 {
        t.send_str(&format!("line-{n}\r"))?;
    }
    t.wait_until_for(|s| s.contains("line-10"), Duration::from_secs(10))?;
    Ok(())
}

/// On Windows a send has a deadline too (#771). There the master cannot
/// be duplicated, so no writer thread was started: a send wrote on the test
/// thread, under the lock the reader writes query replies under, with no
/// deadline at all. Whether ConPTY's input pipe ever fills, so that the
/// write blocks, nobody had measured; this asks it with an application that
/// never reads, and a watchdog, so that a write that never returns fails
/// here instead of holding the CI job to its own limit.
///
/// The measurement goes straight to stderr, past the test harness's
/// capture, so that the `windows` workflow's log records it whether this
/// passes or not.
#[cfg(windows)]
#[test]
fn a_send_to_an_application_that_never_reads_returns_by_its_deadline() -> termlens::Result<()> {
    use std::io::Write as _;
    use std::sync::mpsc;

    const SIZE: usize = 64 << 20;
    // The send's deadline, which the rustdoc promises it keeps.
    const DEADLINE: Duration = Duration::from_secs(1);
    // Far past that deadline: only a send that is not coming back reaches it.
    const WATCHDOG: Duration = Duration::from_secs(20);

    let mut t = common::spawn_emit(
        Terminal::builder().timeout(DEADLINE),
        &["ready", "--sleep", "60s"],
    )?;
    t.wait_until_for(|s| s.contains("ready"), Duration::from_secs(10))?;
    let (sent_tx, sent) = mpsc::channel();
    let (dropped_tx, dropped) = mpsc::channel();
    std::thread::spawn(move || {
        let start = Instant::now();
        let outcome = t.send_str(&"x".repeat(SIZE));
        let _ = sent_tx.send((start.elapsed(), outcome));
        let start = Instant::now();
        drop(t);
        let _ = dropped_tx.send(start.elapsed());
    });
    let (took, outcome) = match sent.recv_timeout(WATCHDOG) {
        Ok(sent) => sent,
        Err(mpsc::RecvTimeoutError::Timeout) => panic!(
            "send_str of {SIZE} bytes to a program that never reads, with a {DEADLINE:?} \
             deadline, did not return in {WATCHDOG:?}: the send has no deadline (#771)"
        ),
        Err(mpsc::RecvTimeoutError::Disconnected) => panic!("the send panicked; see above"),
    };
    let summary = match &outcome {
        Ok(()) => "Ok".to_owned(),
        Err(e) => e.to_string().lines().next().unwrap_or_default().to_owned(),
    };
    let _ = writeln!(
        std::io::stderr(),
        "#771 measurement: send_str({SIZE} bytes), deadline {DEADLINE:?}: {summary} after {took:?}"
    );
    match dropped.recv_timeout(WATCHDOG) {
        Ok(took) => {
            let _ = writeln!(std::io::stderr(), "#771 measurement: drop took {took:?}");
        }
        Err(mpsc::RecvTimeoutError::Timeout) => {
            panic!("dropping the terminal after the send did not return in {WATCHDOG:?}")
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => panic!("the drop panicked; see above"),
    }
    match outcome {
        // ConPTY took everything: the write never had to give up.
        Ok(()) => {}
        Err(Error::Write { what, .. }) => assert!(what.contains("not reading its input"), "{what}"),
        Err(other) => panic!("a send that cannot be written fails as Error::Write: {other}"),
    }
    // The send returns within a slice of its deadline, whatever the writer
    // thread is doing. Ten deadlines leave a loaded runner room, and still
    // fail a write that was merely slow, made on this thread as it was.
    assert!(
        took < DEADLINE * 10,
        "the send took {took:?} against a {DEADLINE:?} deadline"
    );
    Ok(())
}
