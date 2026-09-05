//! Run any command inside termlens and print its rendered screen — a
//! debugging tool for "what does my app actually look like in the grid?".
//!
//! ```sh
//! cargo run --example inspect -- ls -la
//! cargo run --example inspect -- --size 120x40 htop
//! cargo run --example inspect -- --timeout 30 ./target/debug/slow-app
//! ```
//!
//! Waits for the program to exit (up to the deadline, `--timeout`, five
//! seconds by default) or, if it keeps running, for a window of output
//! silence (`--idle`, 300ms by default) — then prints the screen. Five
//! seconds is what a test suite wants, where a deadline exists to turn a
//! hang into a readable failure; a person at a terminal pointing this at an
//! application that loads a large file or compiles before it draws is
//! willing to wait longer, which is what the flag is for. The silence
//! window has the same shape: an application that paints in bursts wider
//! than 300ms is snapshotted mid-render unless it is widened.
//!
//! Exit code 0 means inspect ran and printed a screen; the trailer under
//! the screen says what the program did — its exit status, or that it was
//! still running at the deadline. Exit code 1 means inspect itself could
//! not run: bad arguments, or a program that could not be spawned. A
//! viewer, not a gate: the program's own status is reported, not
//! propagated.

use std::io::{self, Write};
use std::process::ExitCode;
use std::time::Duration;

use termlens::Terminal;

/// The one copy of the usage text: `--help` prints it to stdout and exits
/// 0, a missing program prints it to stderr and exits 1 (#229).
const USAGE: &str = "\
usage: inspect [--size COLSxROWS] [--timeout SECONDS] [--idle MILLIS] <program> [args…]

Runs <program> in an 80x24 pseudo-terminal (or --size), waits for it to
exit or for the deadline (--timeout, default 5 seconds), and prints the
rendered screen. A program still running at the deadline is snapshotted
after --idle milliseconds (default 300) of output silence, then killed.

Exit code 0: a screen was printed; the trailer under it says what the
program did. Exit code 1: inspect itself could not run — bad arguments,
or a program that could not be spawned.

  -h, --help     print this text
      --version  print the termlens version this example was built from
  --             end of options; the program name follows";

/// The value after `flag`, or the one-line diagnostic every flag shares:
/// a missing value names the kind expected, a malformed one shows an
/// example — the shape `--size` set, so `--timeout` and `--idle` read the
/// same way (#236).
fn take<T>(
    args: &mut impl Iterator<Item = String>,
    flag: &str,
    kind: &str,
    example: &str,
    parse: impl Fn(&str) -> Option<T>,
) -> Result<T, String> {
    let Some(raw) = args.next() else {
        return Err(format!("{flag} needs a {kind} argument"));
    };
    parse(&raw).ok_or_else(|| format!("bad {flag} {raw:?}, expected e.g. {example}"))
}

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1).peekable();

    let mut size = (80u16, 24u16);
    let mut timeout = Duration::from_secs(5);
    let mut idle = Duration::from_millis(300);

    // Options come before the program; everything after it is the
    // program's own, however flag-like it looks.
    while args.peek().is_some_and(|a| a.starts_with('-') && a != "-") {
        let flag = args.next().unwrap_or_default();
        let parsed = match flag.as_str() {
            "-h" | "--help" => {
                println!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            "--version" => {
                println!("inspect (termlens {})", env!("CARGO_PKG_VERSION"));
                return ExitCode::SUCCESS;
            }
            "--" => break,
            "--size" => take(&mut args, "--size", "COLSxROWS", "120x40", |spec| {
                let (c, r) = spec.split_once('x')?;
                Some((c.parse().ok()?, r.parse().ok()?))
            })
            .map(|s| size = s),
            "--timeout" => take(&mut args, "--timeout", "SECONDS", "30", |s| s.parse().ok())
                .map(|secs| timeout = Duration::from_secs(secs)),
            "--idle" => take(&mut args, "--idle", "MILLIS", "1000", |s| s.parse().ok())
                .map(|millis| idle = Duration::from_millis(millis)),
            other => Err(format!("unknown option {other:?} (try --help)")),
        };
        if let Err(message) = parsed {
            eprintln!("inspect: {message}");
            return ExitCode::FAILURE;
        }
    }

    let Some(program) = args.next() else {
        eprintln!("{USAGE}");
        return ExitCode::FAILURE;
    };

    let mut t = match Terminal::builder()
        .size(size.0, size.1)
        .timeout(timeout)
        .args(args)
        .spawn(&program)
    {
        Ok(t) => t,
        Err(e) => {
            eprintln!("inspect: {e}");
            return ExitCode::FAILURE;
        }
    };

    let mut out = String::new();
    match t.wait_exit() {
        Ok(status) => {
            out.push_str(&t.screen().to_string());
            out.push_str(&format!("\n--- exited: {status} ---\n"));
        }
        Err(termlens::Error::Timeout { .. }) => {
            // Still running at the deadline: settle on a quiet screen
            // instead. The settle is bounded by the deadline too, unless the
            // silence window asked for is itself longer than that.
            let _ = t.wait_idle_for(idle, timeout.max(idle));
            out.push_str(&t.screen().to_string());
            out.push_str("\n--- still running at the deadline (killed on exit) ---\n");
        }
        Err(e) => {
            // Not "still running": the OS wait itself failed, and saying so
            // is the difference between a slow program and a broken harness.
            out.push_str(&t.screen().to_string());
            out.push_str(&format!("\n--- waiting for the program failed: {e} ---\n"));
        }
    }
    // One write, and a reader that closed early (`inspect … | head`) is a
    // clean exit rather than a panic on a broken pipe (#223).
    let mut stdout = io::stdout().lock();
    if let Err(e) = stdout
        .write_all(out.as_bytes())
        .and_then(|()| stdout.flush())
    {
        if e.kind() == io::ErrorKind::BrokenPipe {
            return ExitCode::SUCCESS;
        }
        eprintln!("inspect: writing the screen failed: {e}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}
