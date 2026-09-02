//! Run any command inside termlens and print its rendered screen — a
//! debugging tool for "what does my app actually look like in the grid?".
//!
//! ```sh
//! cargo run --example inspect -- ls -la
//! cargo run --example inspect -- --size 120x40 htop
//! ```
//!
//! Waits for the program to exit (up to the timeout) or, if it keeps
//! running, for 300ms of output silence — then prints the screen.
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

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1).peekable();

    let mut size = (80u16, 24u16);
    if args.peek().map(String::as_str) == Some("--size") {
        args.next();
        let Some(spec) = args.next() else {
            eprintln!("--size needs a COLSxROWS argument");
            return ExitCode::FAILURE;
        };
        match spec.split_once('x') {
            Some((c, r)) => match (c.parse(), r.parse()) {
                (Ok(c), Ok(r)) => size = (c, r),
                _ => {
                    eprintln!("bad --size {spec:?}, expected e.g. 120x40");
                    return ExitCode::FAILURE;
                }
            },
            None => {
                eprintln!("bad --size {spec:?}, expected e.g. 120x40");
                return ExitCode::FAILURE;
            }
        }
    }

    let Some(program) = args.next() else {
        eprintln!("usage: inspect [--size COLSxROWS] <program> [args…]");
        return ExitCode::FAILURE;
    };

    let mut t = match Terminal::builder()
        .size(size.0, size.1)
        .timeout(Duration::from_secs(5))
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
            // Still running at the deadline: settle on a quiet screen instead.
            let _ = t.wait_idle(Duration::from_millis(300));
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
