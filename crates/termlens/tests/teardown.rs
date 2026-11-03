//! Teardown of a terminal whose application left its input unread.
//!
//! A binary of its own, because the failure it guards against is a `drop`
//! that never returns while holding the process-wide PTY lock: every other
//! spawn and every reader thread in the binary would wait on it, and a
//! regression must fail this file, not stall whichever test drew the lock
//! next (#628).

#![cfg(unix)]

use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use termlens::{Error, Key, Signal, Terminal};

mod common;

/// Long enough for a loaded runner, short enough that a hang reads as a
/// failure here and not as the CI job's own limit.
const WATCHDOG: Duration = Duration::from_secs(30);

/// Run `work` on a thread of its own and fail, naming `what`, if it has not
/// finished by [`WATCHDOG`]. The hung thread is left behind: the harness
/// exits all the same once every test has reported.
fn within_watchdog<T: Send + 'static>(what: &str, work: impl FnOnce() -> T + Send + 'static) -> T {
    let (done, finished) = mpsc::channel();
    thread::spawn(move || {
        let _ = done.send(work());
    });
    match finished.recv_timeout(WATCHDOG) {
        Ok(value) => value,
        Err(mpsc::RecvTimeoutError::Timeout) => {
            panic!("{what} did not return in {WATCHDOG:?}")
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => panic!("{what} panicked; see above"),
    }
}

/// An application in raw mode that never reads, sent more than the PTY's
/// input queue holds. The send gives up (#590), which is the reportable
/// outcome, and the queue is then full for good.
fn full_input_queue() -> termlens::Result<Terminal> {
    let mut t = common::spawn_emit(
        // The deadline under test is the send's; the waits name their own.
        Terminal::builder().timeout(Duration::from_millis(500)),
        &["--raw-mode", "--on-term", "bye", "3", "ready", "--idle"],
    )?;
    t.wait_until_for(|s| s.contains("ready"), Duration::from_secs(10))?;
    match t.send_str(&"x".repeat(50_000)) {
        Err(Error::Write { .. }) => {}
        other => panic!("the application is not reading, so the send gives up: {other:?}"),
    }
    Ok(t)
}

/// Wait until this process holds no PTY master. The writer thread's own
/// write was asleep on the full queue too, and kept its duplicate of the
/// master, and with it the PTY, open for the life of the process; it ends
/// once teardown has emptied the queue. Its end is asynchronous, so this
/// polls the descriptor table for the one fact it can see, and the
/// watchdog bounds it. Linux only, where `/proc` names a master.
#[cfg(target_os = "linux")]
fn masters_all_close() {
    let masters = || {
        std::fs::read_dir("/proc/self/fd")
            .expect("/proc/self/fd lists")
            .filter_map(|fd| std::fs::read_link(fd.ok()?.path()).ok())
            .filter(|target| target.to_string_lossy().ends_with("ptmx"))
            .count()
    };
    while masters() > 0 {
        thread::yield_now();
    }
}

/// Dropping the terminal returns, and releases the lock: a spawn after it
/// gets through. portable-pty's writer sends an EOT as it is dropped, and
/// that write blocked on the full queue forever, inside the lock, with the
/// child long gone (#628). Both ways in: the child exited and reaped before
/// the drop, and the child still running when the drop stops it.
#[test]
fn a_terminal_whose_input_queue_is_full_drops_and_releases_the_lock() -> termlens::Result<()> {
    // The fixture is built before the watchdog starts counting.
    let _ = common::fixture_bin("emit");
    for exited_first in [true, false] {
        let mut t = full_input_queue()?;
        if exited_first {
            t.signal(Signal::Term)?;
            let status = t.wait_exit_for(Duration::from_secs(10))?;
            assert_eq!(status.code(), Some(3), "{status}");
        }
        within_watchdog("dropping the terminal", move || drop(t));
        #[cfg(target_os = "linux")]
        within_watchdog("the PTY closing", masters_all_close);
        within_watchdog("a spawn after the drop", || -> termlens::Result<()> {
            let mut next = common::spawn_emit(
                Terminal::builder().timeout(Duration::from_secs(10)),
                &["next", "--wait"],
            )?;
            next.wait_until(|s| s.contains("next"))?;
            next.send(Key::Enter)?;
            assert!(next.wait_exit()?.success());
            Ok(())
        })?;
    }
    Ok(())
}
