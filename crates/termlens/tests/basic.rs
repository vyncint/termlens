//! Integration tests against `/bin/sh`: spawning, waiting, environment
//! control, exit codes, and the failure modes (timeout / EOF).
//!
//! These run headless — CI runners have no TTY, the harness makes its own.
//!
//! Pattern note: every script that must *print something we assert on*
//! ends with a `read` guard, and we send Enter only after the assertion.
//! Output written immediately before exit can be discarded by macOS's PTY
//! teardown (docs/DESIGN.md §2); keeping the child alive until the harness
//! has seen the bytes makes these tests deterministic on every platform.

use std::time::{Duration, Instant};

use termlens::{Error, Key, Terminal};

const SH: &str = "/bin/sh";

fn sh(script: &str) -> termlens::Result<Terminal> {
    Terminal::builder().args(["-c", script]).spawn(SH)
}

#[test]
fn echo_reaches_the_screen_and_child_exits_cleanly() -> termlens::Result<()> {
    let mut t = sh("echo hello from a real PTY; read guard")?;
    t.wait_until(|s| s.contains("hello from a real PTY"))?;
    t.send(Key::Enter)?;
    let status = t.wait_exit()?;
    assert!(status.success(), "full status: {status}");
    assert_eq!(status.code(), Some(0));
    Ok(())
}

#[test]
fn exit_codes_are_reported() -> termlens::Result<()> {
    // The `read` keeps the exit from racing PTY setup; the line discipline
    // buffers our Enter even if it lands before `read` starts.
    let mut t = sh("read guard; exit 7")?;
    t.send(Key::Enter)?;
    let status = t.wait_exit()?;
    assert!(!status.success());
    assert_eq!(status.code(), Some(7), "full status: {status}");

    // Idempotent: a second wait returns the cached status.
    assert_eq!(t.wait_exit()?, status);
    Ok(())
}

#[test]
fn signal_deaths_are_reported_as_signals_not_exit_codes() -> termlens::Result<()> {
    let mut t = sh("read guard; kill -TERM $$")?;
    t.send(Key::Enter)?;
    let status = t.wait_exit()?;
    assert!(!status.success());
    // strsignal spelling differs per libc ("Terminated" / "Terminated: 15"),
    // but the word is stable on both CI platforms.
    let signal = status.signal().unwrap_or_else(|| {
        panic!("expected a signal death, got: {status}");
    });
    assert!(signal.contains("Terminated"), "signal was: {signal}");
    Ok(())
}

#[test]
fn env_vars_reach_the_child() -> termlens::Result<()> {
    let mut t = Terminal::builder()
        .env("TERMTEST_MARKER", "42")
        .args(["-c", r#"echo "marker=$TERMTEST_MARKER"; read guard"#])
        .spawn(SH)?;
    t.wait_until(|s| s.contains("marker=42"))?;
    t.send(Key::Enter)?;
    t.wait_exit()?;
    Ok(())
}

#[test]
fn env_clear_blocks_inheritance_but_keeps_explicit_vars_and_term() -> termlens::Result<()> {
    // Probe HOME, not PATH: shells synthesize a compiled-in default PATH
    // when none is inherited, so PATH can't distinguish "inherited" from
    // "defaulted". HOME is always set for the test process and never
    // synthesized by a non-interactive shell.
    assert!(
        std::env::var_os("HOME").is_some(),
        "test needs HOME in the parent env"
    );
    let mut t = Terminal::builder()
        .env_clear()
        .env("KEPT", "yes")
        .args([
            "-c",
            r#"echo "home=${HOME:-unset} term=$TERM kept=$KEPT"; read guard"#,
        ])
        .spawn(SH)?;
    t.wait_until(|s| s.contains("kept=yes"))?;
    let screen = t.screen();
    assert!(
        screen.contains("home=unset"),
        "HOME leaked through env_clear:\n{screen}"
    );
    assert!(
        screen.contains("term=xterm-256color"),
        "default TERM missing:\n{screen}"
    );
    t.send(Key::Enter)?;
    t.wait_exit()?;
    Ok(())
}

#[test]
fn explicit_term_overrides_the_default() -> termlens::Result<()> {
    let mut t = Terminal::builder()
        .env("TERM", "vt100")
        .args(["-c", r#"echo "term=$TERM"; read guard"#])
        .spawn(SH)?;
    t.wait_until(|s| s.contains("term=vt100"))?;
    t.send(Key::Enter)?;
    t.wait_exit()?;
    Ok(())
}

#[test]
fn send_str_and_enter_round_trip_through_the_line_discipline() -> termlens::Result<()> {
    let mut t = sh(r#"read line; echo "got: $line"; read guard"#)?;
    t.send_str("hello")?;
    t.send(Key::Enter)?;
    t.wait_until(|s| s.contains("got: hello"))?;
    t.send(Key::Enter)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}

#[test]
fn timeout_error_embeds_the_screen_dump() {
    let mut t = Terminal::builder()
        .timeout(Duration::from_millis(400))
        .args(["-c", "echo something visible; exec cat"])
        .spawn(SH)
        .unwrap();
    // `cat` keeps the terminal open forever; the predicate can never hold.
    let err = t.wait_until(|s| s.contains("never printed")).unwrap_err();

    let Error::Timeout { ref screen, .. } = err else {
        panic!("expected Error::Timeout, got: {err}");
    };
    assert!(screen.contains("something visible"));

    let msg = err.to_string();
    assert!(msg.contains("timed out after 400ms"), "{msg}");
    assert!(msg.contains("--- screen at timeout ---"), "{msg}");
    assert!(msg.contains("something visible"), "{msg}");
    // Drop now kills the still-running `cat` — no zombies.
}

#[test]
fn waits_fail_fast_on_eof_instead_of_burning_the_timeout() {
    let mut t = Terminal::builder()
        .timeout(Duration::from_secs(30))
        .args(["-c", "echo bye; read guard"])
        .spawn(SH)
        .unwrap();
    // Deterministic sequencing: observe the output, then let the child
    // exit, then wait for something that can never appear.
    t.wait_until(|s| s.contains("bye")).unwrap();
    t.send(Key::Enter).unwrap();

    let start = Instant::now();
    let err = t.wait_until(|s| s.contains("never printed")).unwrap_err();
    let elapsed = start.elapsed();

    assert!(matches!(err, Error::Eof { .. }), "expected Eof, got: {err}");
    assert!(err.to_string().contains("--- final screen ---"));
    assert!(err.screen().unwrap().contains("bye"));
    assert!(
        elapsed < Duration::from_secs(10),
        "EOF should fail fast, took {elapsed:?}"
    );
}

#[test]
fn wait_idle_resolves_in_output_gaps() -> termlens::Result<()> {
    let mut t = sh("printf a; sleep 1.5; printf b; read guard")?;
    t.wait_until(|s| s.contains("a"))?;
    t.wait_idle(Duration::from_millis(200))?;

    let screen = t.screen();
    assert!(screen.contains("a"));
    assert!(
        !screen.contains("b"),
        "wait_idle resolved too late:\n{screen}"
    );

    t.wait_until(|s| s.contains("b"))?;
    t.send(Key::Enter)?;
    t.wait_exit()?;
    Ok(())
}

#[test]
fn spawn_failure_surfaces_instead_of_hanging() {
    // Depending on the platform, exec failure is reported at spawn time or
    // as a fast, non-zero child exit. Both are fine; hanging is not.
    match Terminal::builder().spawn("/definitely/not/a/real/binary") {
        Err(_) => {}
        Ok(mut t) => {
            let status = t.wait_exit().expect("child should exit, not hang");
            assert!(!status.success());
        }
    }
}
