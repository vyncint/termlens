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

    match t.wait_exit() {
        Ok(status) => {
            println!("{}", t.screen());
            println!("--- exited: {status} ---");
        }
        Err(_) => {
            // Still running: settle on a quiet screen instead.
            let _ = t.wait_idle(Duration::from_millis(300));
            println!("{}", t.screen());
            println!("--- still running (killed on exit) ---");
        }
    }
    ExitCode::SUCCESS
}
