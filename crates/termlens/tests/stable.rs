//! `wait_stable` and `snapshot_after`: settling on the *picture* rather
//! than on silence, and the three rules for race-free waits as one call.

use std::time::{Duration, Instant};

use termlens::{Error, Key, Terminal};

fn sh(script: &str) -> termlens::Result<Terminal> {
    Terminal::builder()
        .size(40, 6)
        .env_clear()
        .timeout(Duration::from_secs(10))
        .args(["-c", script])
        .spawn("/bin/sh")
}

/// Two paints two seconds apart: the settle ends between them, so the
/// screen returned shows the first and not the second — the instant that
/// held still, not a later `screen()`.
#[test]
fn snapshot_after_returns_the_screen_once_it_holds_still() -> termlens::Result<()> {
    let mut t = sh("printf first; sleep 2; printf ' second'; read _")?;
    let screen = t.snapshot_after(|s| s.contains("first"))?;
    assert_eq!(screen.row_text(0).trim_end(), "first", "{screen}");
    t.wait_until(|s| s.contains("second"))?;
    t.send(Key::Enter)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}

/// The predicate half fails first, with `wait_until`'s own message: a
/// snapshot was never taken of a screen that never showed the thing.
#[test]
fn snapshot_after_fails_on_the_predicate_before_it_settles() -> termlens::Result<()> {
    let mut t = sh("printf other; read _")?;
    let err = t
        .snapshot_after_for(|s| s.contains("never"), Duration::from_millis(300))
        .unwrap_err();
    match &err {
        Error::Timeout { waiting_for, .. } => assert!(
            waiting_for.starts_with("the screen predicate to hold"),
            "{waiting_for}"
        ),
        other => panic!("expected a timeout, got {other:?}"),
    }
    t.send(Key::Enter)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}

/// Bells without end, and not one cell changes. `wait_idle` cannot see
/// silence through them; `wait_stable` settles, and the screen it hands
/// back carries the bell count as of the newest byte, not as of the paint.
///
/// The bell loop is a shell builtin with no `sleep`: a process spawned per
/// iteration stalls for hundreds of milliseconds on a loaded machine, and
/// a stall is real silence — the stress workflow found exactly that. The
/// `wait_idle` expectation is made load-proof the same way the deadline
/// tests are: two seconds of silence cannot be observed inside a 400ms
/// deadline unless the stream had already been silent for most of it.
#[test]
fn wait_stable_ignores_output_that_changes_no_cell() -> termlens::Result<()> {
    let mut t = sh(r"printf noisy; while :; do printf '\a'; done")?;
    t.wait_until(|s| s.contains("noisy") && s.bells() > 0)?;

    let idle = t.wait_idle_for(Duration::from_secs(2), Duration::from_millis(400));
    assert!(
        matches!(idle, Err(Error::Timeout { .. })),
        "silence never comes, so wait_idle must time out: {idle:?}"
    );

    let start = Instant::now();
    let screen = t.wait_stable(Duration::from_millis(100))?;
    assert_eq!(screen.row_text(0).trim_end(), "noisy", "{screen}");
    assert!(
        screen.bells() >= 1,
        "the returned screen is a current observation"
    );
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "settled long after the picture stopped changing: {:?}",
        start.elapsed()
    );
    Ok(())
}

/// A picture that keeps changing never settles, and the timeout says what
/// was waited for, with the deadline that applied.
///
/// `seq` scrolls a new number onto the screen as fast as the PTY takes
/// them — one process, no per-iteration spawn to stall under load — and
/// the stillness asked for (2s) is far longer than the deadline (400ms),
/// so the wait can only succeed if the child had already been stalled for
/// most of two seconds when it began. A version of this test that asked
/// for 150ms of stillness against a shell loop with `sleep 0.02` was found
/// by the stress workflow: at 16 threads the loop's `sleep` took longer
/// than 150ms to spawn, the picture genuinely held still, and
/// `wait_stable` was right to say so.
#[test]
fn wait_stable_times_out_while_the_picture_keeps_changing() -> termlens::Result<()> {
    let mut t = sh("seq 1 100000000")?;
    // Any digit: which numbers are on screen when a snapshot lands is
    // whatever the flood happens to show, so no particular one is waited for.
    t.wait_until(|s| s.text().chars().any(|c| c.is_ascii_digit()))?;
    let err = t
        .wait_stable_for(Duration::from_secs(2), Duration::from_millis(400))
        .unwrap_err();
    match err {
        Error::Timeout {
            waiting_for,
            timeout,
            ..
        } => {
            assert!(
                waiting_for.starts_with("the screen to hold still for 2s"),
                "{waiting_for}"
            );
            assert_eq!(timeout, Duration::from_millis(400));
        }
        other => panic!("expected a timeout, got {other:?}"),
    }
    Ok(())
}

/// A still picture inside an open DEC 2026 update is a half-painted frame,
/// not a settled one — the same guarantee `wait_idle` gives — and the
/// message names the real state. Closing the update lets the same picture
/// settle at once.
#[test]
fn wait_stable_does_not_settle_inside_an_open_synchronized_update() -> termlens::Result<()> {
    let mut t = sh(r"printf '\033[?2026hhalf'; read _; printf '\033[?2026l'; read _")?;
    t.wait_until(|s| s.contains("half"))?;
    let err = t
        .wait_stable_for(Duration::from_millis(50), Duration::from_millis(400))
        .unwrap_err();
    match &err {
        Error::Timeout { waiting_for, .. } => assert!(
            waiting_for.contains("unfinished DEC 2026 synchronized update"),
            "{waiting_for}"
        ),
        other => panic!("expected a timeout, got {other:?}"),
    }

    t.send(Key::Enter)?;
    let screen = t.wait_stable(Duration::from_millis(50))?;
    assert!(screen.contains("half"), "{screen}");
    t.send(Key::Enter)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}

/// Stillness before the call counts, and so does EOF: a grid that has been
/// quiet longer than `quiet` returns without waiting it out again, and an
/// exited child's final screen is still by definition.
#[test]
fn a_screen_that_already_holds_still_settles_at_once() -> termlens::Result<()> {
    let mut t = sh("printf settled; read _")?;
    t.wait_until(|s| s.contains("settled"))?;
    std::thread::sleep(Duration::from_millis(1200));
    let start = Instant::now();
    let screen = t.wait_stable(Duration::from_secs(1))?;
    // Waiting the stillness out again would take a full second; an
    // immediate return takes far less even on a loaded machine.
    assert!(
        start.elapsed() < Duration::from_millis(500),
        "a 1s stillness already elapsed was waited out again: {:?}",
        start.elapsed()
    );
    assert!(screen.contains("settled"), "{screen}");

    t.send(Key::Enter)?;
    assert!(t.wait_exit()?.success());
    let screen = t.wait_stable(Duration::from_secs(5))?;
    assert!(screen.contains("settled"), "EOF is still: {screen}");
    Ok(())
}
