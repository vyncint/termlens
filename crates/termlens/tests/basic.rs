//! Integration tests against the `emit` fixture: spawning, waiting,
//! environment control, exit codes, and the failure modes (timeout / EOF).
//!
//! These run headless — CI runners have no TTY, the harness makes its own.
//!
//! Pattern note: every program that must *print something we assert on*
//! ends with a `--wait`, and we send Enter only after the assertion.
//! Output written immediately before exit can be discarded by macOS's PTY
//! teardown (docs/DESIGN.md §2); keeping the child alive until the harness
//! has seen the bytes makes these tests deterministic on every platform.

use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

use termlens::{Error, Key, Terminal};

mod common;

/// The `emit` fixture from the default builder; steps are documented in
/// `fixtures/emit/src/main.rs`.
fn emit(steps: &[&str]) -> termlens::Result<Terminal> {
    common::emit(steps)
}

#[test]
fn echo_reaches_the_screen_and_child_exits_cleanly() -> termlens::Result<()> {
    let mut t = emit(&["hello from a real PTY\n", "--wait"])?;
    t.wait_until(|s| s.contains("hello from a real PTY"))?;
    t.send(Key::Enter)?;
    let status = t.wait_exit()?;
    assert!(status.success(), "full status: {status}");
    assert_eq!(status.code(), Some(0));
    Ok(())
}

#[test]
fn exit_codes_are_reported() -> termlens::Result<()> {
    // The `--wait` keeps the exit from racing PTY setup; the line discipline
    // buffers our Enter even if it lands before the read starts.
    let mut t = emit(&["--wait", "--exit", "7"])?;
    t.send(Key::Enter)?;
    let status = t.wait_exit()?;
    assert!(!status.success());
    assert_eq!(status.code(), Some(7), "full status: {status}");

    // Idempotent: a second wait returns the cached status.
    assert_eq!(t.wait_exit()?, status);
    Ok(())
}

#[test]
#[cfg_attr(windows, ignore = "Windows has no signals")]
fn signal_deaths_are_reported_as_signals_not_exit_codes() -> termlens::Result<()> {
    let mut t = emit(&["--wait", "--kill-self"])?;
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
    let mut t = common::spawn_emit(
        Terminal::builder().env("TERMTEST_MARKER", "42"),
        &["marker=", "--env", "TERMTEST_MARKER", "--wait"],
    )?;
    t.wait_until(|s| s.contains("marker=42"))?;
    t.send(Key::Enter)?;
    t.wait_exit()?;
    Ok(())
}

#[test]
fn envs_accepts_common_pair_iterators() {
    let vec_vars = vec![(String::from("VEC_KEY"), String::from("vec-value"))];
    let map_vars = HashMap::from([("MAP_KEY", "map-value")]);

    let _ = Terminal::builder()
        .envs([("ARRAY_KEY", "array-value")])
        .envs(vec_vars)
        .envs(map_vars)
        .envs(std::env::vars().take(0));
}

#[test]
fn envs_and_env_preserve_order_and_duplicates() -> termlens::Result<()> {
    let mut t = common::spawn_emit(
        Terminal::builder()
            .env("VALUE", "env-first")
            .envs([("VALUE", "envs-first"), ("SECOND", "two")])
            .env("VALUE", "env-last")
            .envs([("THIRD", "three"), ("VALUE", "envs-last")]),
        &[
            "value=", "--env", "VALUE", " second=", "--env", "SECOND", " third=", "--env", "THIRD",
            "--wait",
        ],
    )?;
    t.wait_until(|s| s.contains("value=envs-last second=two third=three"))?;
    t.send(Key::Enter)?;
    t.wait_exit()?;
    Ok(())
}

#[test]
fn env_clear_hands_the_child_exactly_the_builder_environment() -> termlens::Result<()> {
    // Enumerate the whole environment rather than probing one name, so a
    // leaked variable cannot arrive unnoticed: SHELL did, filled in by the
    // PTY layer from the host's login shell (#221). The fixture prints its
    // own environment — no shell, which would add PWD and friends of its own
    // — and is spawned by absolute path, because PATH is gone.
    let mut t = common::spawn_emit(
        Terminal::builder().env_clear().env("MARKER", "1"),
        &["--environ"],
    )?;
    assert!(t.wait_exit()?.success());
    let screen = t.screen();
    let mut vars: Vec<String> = screen
        .text()
        .lines()
        .filter(|line| line.contains('='))
        .map(str::to_owned)
        .collect();
    vars.sort();
    assert_eq!(
        vars,
        ["MARKER=1", "SHELL=/bin/sh", "TERM=xterm-256color"],
        "the child's environment is not a function of the builder alone:\n{screen}"
    );
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
    let mut t = common::spawn_emit(
        Terminal::builder()
            .envs([("KEPT_BEFORE", "yes")])
            .env_clear()
            .envs([("KEPT_AFTER", "also")]),
        &[
            "home=",
            "--env",
            "HOME",
            " term=",
            "--env",
            "TERM",
            " before=",
            "--env",
            "KEPT_BEFORE",
            " after=",
            "--env",
            "KEPT_AFTER",
            "--wait",
        ],
    )?;
    t.wait_until(|s| s.contains("before=yes after=also"))?;
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
    let mut t = common::spawn_emit(
        Terminal::builder().env("TERM", "vt100"),
        &["term=", "--env", "TERM", "--wait"],
    )?;
    t.wait_until(|s| s.contains("term=vt100"))?;
    t.send(Key::Enter)?;
    t.wait_exit()?;
    Ok(())
}

#[test]
fn send_str_and_enter_round_trip_through_the_line_discipline() -> termlens::Result<()> {
    // The line discipline echoes what is typed onto row 0 and moves to row
    // 1; the fixture then writes the line it read, with a suffix the echo
    // cannot have produced — so the wait proves the round trip, not the echo.
    let mut t = emit(&["--echo-line", " back", "--wait"])?;
    t.send_str("hello")?;
    t.send(Key::Enter)?;
    t.wait_until(|s| s.contains("hello back"))?;
    t.send(Key::Enter)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}

#[test]
fn timeout_error_embeds_the_screen_dump() {
    let mut t = common::spawn_emit(
        Terminal::builder().timeout(Duration::from_millis(400)),
        &["something visible\n", "--echo"],
    )
    .unwrap();
    // `--echo` keeps the terminal open forever; the predicate can never hold.
    let err = t.wait_until(|s| s.contains("never printed")).unwrap_err();

    let Error::Timeout { ref screen, .. } = err else {
        panic!("expected Error::Timeout, got: {err}");
    };
    assert!(screen.contains("something visible"));

    let msg = err.to_string();
    assert!(msg.contains("timed out after 400ms"), "{msg}");
    assert!(msg.contains("--- screen at timeout ---"), "{msg}");
    assert!(msg.contains("something visible"), "{msg}");
    // Drop now kills the still-running child — no zombies.
}

#[test]
fn waits_fail_fast_on_eof_instead_of_burning_the_timeout() {
    let mut t = common::spawn_emit(
        Terminal::builder().timeout(Duration::from_secs(30)),
        &["bye\n", "--wait"],
    )
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
    let mut t = emit(&["a", "--sleep", "1.5s", "b", "--wait"])?;
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
