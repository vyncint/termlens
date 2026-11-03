//! A test process that ignores SIGCHLD, so the kernel reaps its children
//! itself and termlens never gets an exit status (#630).
//!
//! A binary of its own, because the setting is process-wide: every other
//! test here would run under it, and std's own `Command::output`, which the
//! fixture build uses, cannot reap either. So the program is `sh`, and the
//! one test sets the disposition before anything is spawned.

#![cfg(unix)]

use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use termlens::{Error, Signal, Terminal};

/// Far under `Drop`'s two-second reap grace, which teardown used to wait
/// out in full for a child the kernel had already reaped, and far over
/// what a drop takes.
const PROMPT: Duration = Duration::from_secs(1);

fn sh(script: &str) -> termlens::Result<Terminal> {
    Terminal::builder()
        .timeout(Duration::from_secs(10))
        .args(["-c", script])
        .spawn("sh")
}

/// Drop `t`, and how long that took.
fn timed_drop(t: Terminal) -> Duration {
    let start = Instant::now();
    drop(t);
    start.elapsed()
}

/// With SIGCHLD ignored, an exited child is gone the moment it exits:
/// `try_wait` reports ECHILD. Teardown read that as a child still running,
/// so it sent SIGHUP and SIGKILL to a pid that might belong to another
/// process by then, and waited out its whole reap grace, for every terminal.
/// Now it is taken as reaped: `wait_exit` says why there is no status,
/// `signal` refuses, and a drop returns at once.
#[test]
fn a_child_the_kernel_reaped_is_neither_waited_for_nor_signalled() {
    // SAFETY: setting a signal's disposition to SIG_IGN installs no
    // handler and touches no memory. Nothing in this binary has spawned yet.
    #[allow(unsafe_code)]
    unsafe {
        libc::signal(libc::SIGCHLD, libc::SIG_IGN);
    }
    let (done, finished) = mpsc::channel();
    thread::spawn(move || {
        let _ = done.send(the_cases());
    });
    match finished.recv_timeout(Duration::from_secs(60)) {
        Ok(outcome) => outcome.expect("the terminals spawn and draw"),
        Err(mpsc::RecvTimeoutError::Timeout) => panic!("teardown did not finish in 60s"),
        Err(mpsc::RecvTimeoutError::Disconnected) => panic!("a case panicked; see above"),
    }
}

fn the_cases() -> termlens::Result<()> {
    // Exited, and waited for: the wait names the cause.
    let mut t = sh("echo done")?;
    t.wait_until(|s| s.contains("done"))?;
    match t.wait_exit() {
        Err(Error::Io(error)) => {
            let message = error.to_string();
            assert!(message.contains("SIGCHLD is ignored"), "{message}");
        }
        other => panic!("the kernel reaped the child, so there is no status: {other:?}"),
    }
    match t.signal(Signal::Term) {
        Err(Error::Input(message)) => assert!(message.contains("reaped"), "{message}"),
        other => panic!("the pid may be another process's now: {other:?}"),
    }
    let took = timed_drop(t);
    assert!(took < PROMPT, "drop after wait_exit took {took:?}");

    // Exited, and never waited for. EOF comes as the child closes the
    // terminal, which is before the kernel has reaped it, so `signal` is not
    // asked here: it may find the child still exiting, and rightly deliver.
    let mut t = sh("echo done")?;
    match t.wait_until(|_| false) {
        Err(Error::Eof { .. }) => {}
        other => panic!("the child exits and the terminal closes: {other:?}"),
    }
    let took = timed_drop(t);
    assert!(took < PROMPT, "drop of an exited child took {took:?}");

    // Still running: the hang-up ends it, and the kernel's reap is the
    // child gone, not a child that ignored the hang-up and needs SIGKILL.
    let mut t = sh("echo up; read _")?;
    t.wait_until(|s| s.contains("up"))?;
    let took = timed_drop(t);
    assert!(took < PROMPT, "drop of a running child took {took:?}");
    Ok(())
}
