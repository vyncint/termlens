//! termlens fixture: writes exactly the bytes its arguments describe, in
//! order, then waits, sleeps or exits as told — the program under test
//! wherever the suite used to run `sh -c 'printf …; read _'` (#249).
//!
//! A shell was a variable in every test that was not about the shell: which
//! `sh` runs decides how `printf` reads `\033`, what `read` does at EOF, how
//! fast a loop spins — and whether there is a shell at all. This is the same
//! program on every platform, with an argument per step and no quoting to
//! decode. Steps apply left to right:
//!
//! ```text
//! TEXT             literal text — any argument that is not a step below
//! NL  CR           a newline / a carriage return
//! --text WORD      literal text that happens to spell a step name
//! --esc BYTES      ESC followed by BYTES          `--esc '(0'`  is  ESC ( 0
//! --csi BYTES      ESC [ followed by BYTES         `--csi '?2026h'`
//! --raw SPEC       bytes with escapes: \e \n \r \t \a \\ and \xNN
//! --sleep DUR      pause for DUR: `250ms`, `1.5s`, `2s`
//! --wait           read one line from stdin and discard it — "hold the
//!                  terminal open until the test sends Enter"
//! --echo-line      read one line from stdin and write it back, without
//!                  its newline
//! --echo           copy stdin to stdout, line by line, until EOF
//! --seq N          the integers 1..=N, one per line
//! --cwd            the current directory, as the process sees it
//! --pid            this process's id, in decimal
//! --exit CODE      exit now with CODE
//! --loop           run the steps before it once, then the steps after it
//!                  forever
//! ```
//!
//! Every emitting step is one `write_all` and a flush, so a test that wants
//! two writes says so with two steps. `--wait` at EOF exits 0: a harness that
//! closed the terminal has finished with it.
//!
//! Fixture rules: **no timing but the explicit `--sleep`, and std only.** A
//! dependency would make this a second thing the suite tests; a clock would
//! make it a second source of flakiness. Nothing here reads the terminal's
//! modes or sets them — a step that needs raw mode is a different fixture.

use std::io::{self, BufRead, Write};
use std::process;
use std::time::Duration;

#[derive(Debug, Clone)]
enum Step {
    Write(Vec<u8>),
    Sleep(Duration),
    Wait,
    EchoLine,
    Echo,
    Seq(u64),
    Cwd,
    Pid,
    Exit(i32),
}

fn usage(reason: &str) -> ! {
    eprintln!("emit: {reason}");
    eprintln!("see the crate doc in fixtures/emit/src/main.rs for the steps");
    process::exit(2)
}

/// Decode `--raw`: `\e` `\n` `\r` `\t` `\a` `\\` and `\xNN`; every other
/// byte is itself. A lone or malformed escape is an error, not a guess.
fn raw(spec: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(spec.len());
    let mut chars = spec.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            let mut buf = [0u8; 4];
            out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            continue;
        }
        match chars.next() {
            Some('e') => out.push(0x1b),
            Some('n') => out.push(b'\n'),
            Some('r') => out.push(b'\r'),
            Some('t') => out.push(b'\t'),
            Some('a') => out.push(0x07),
            Some('\\') => out.push(b'\\'),
            Some('x') => {
                let hex: String = chars.by_ref().take(2).collect();
                match u8::from_str_radix(&hex, 16) {
                    Ok(b) if hex.len() == 2 => out.push(b),
                    _ => usage(&format!("--raw: `\\x{hex}` is not two hex digits")),
                }
            }
            other => usage(&format!(
                "--raw: unknown escape `\\{}`",
                other.map_or(String::new(), String::from)
            )),
        }
    }
    out
}

/// `250ms`, `1.5s`, `2s`.
fn duration(spec: &str) -> Duration {
    let (number, unit) = spec.strip_suffix("ms").map_or_else(
        || (spec.strip_suffix('s').unwrap_or(spec), 1.0),
        |n| (n, 0.001),
    );
    match number.parse::<f64>() {
        Ok(v) if v.is_finite() && v >= 0.0 => Duration::from_secs_f64(v * unit),
        _ => usage(&format!(
            "--sleep: `{spec}` is not a duration like 250ms or 1.5s"
        )),
    }
}

/// The steps, and the index the forever-loop starts at, if there is one.
fn parse(args: impl Iterator<Item = String>) -> (Vec<Step>, Option<usize>) {
    let mut steps = Vec::new();
    let mut loop_from = None;
    let mut args = args;
    while let Some(arg) = args.next() {
        let mut next = |flag: &str| {
            args.next()
                .unwrap_or_else(|| usage(&format!("{flag} needs a value")))
        };
        let step = match arg.as_str() {
            "NL" => Step::Write(b"\n".to_vec()),
            "CR" => Step::Write(b"\r".to_vec()),
            "--text" => Step::Write(next("--text").into_bytes()),
            "--esc" => {
                let mut b = vec![0x1b];
                b.extend_from_slice(next("--esc").as_bytes());
                Step::Write(b)
            }
            "--csi" => {
                let mut b = b"\x1b[".to_vec();
                b.extend_from_slice(next("--csi").as_bytes());
                Step::Write(b)
            }
            "--raw" => Step::Write(raw(&next("--raw"))),
            "--sleep" => Step::Sleep(duration(&next("--sleep"))),
            "--wait" => Step::Wait,
            "--echo-line" => Step::EchoLine,
            "--echo" => Step::Echo,
            "--seq" => Step::Seq(
                next("--seq")
                    .parse()
                    .unwrap_or_else(|_| usage("--seq needs a count")),
            ),
            "--cwd" => Step::Cwd,
            "--pid" => Step::Pid,
            "--exit" => Step::Exit(
                next("--exit")
                    .parse()
                    .unwrap_or_else(|_| usage("--exit needs an exit code")),
            ),
            "--loop" => {
                loop_from = Some(steps.len());
                continue;
            }
            other if other.starts_with("--") => usage(&format!("unknown step `{other}`")),
            text => Step::Write(text.as_bytes().to_vec()),
        };
        steps.push(step);
    }
    (steps, loop_from)
}

/// Read one line from stdin. `None` at EOF — the terminal is gone.
fn line(stdin: &mut impl BufRead) -> io::Result<Option<String>> {
    let mut s = String::new();
    if stdin.read_line(&mut s)? == 0 {
        return Ok(None);
    }
    while s.ends_with('\n') || s.ends_with('\r') {
        s.pop();
    }
    Ok(Some(s))
}

fn run(steps: &[Step], out: &mut impl Write, stdin: &mut impl BufRead) -> io::Result<()> {
    for step in steps {
        match step {
            Step::Write(bytes) => out.write_all(bytes)?,
            Step::Sleep(d) => std::thread::sleep(*d),
            Step::Wait => {
                if line(stdin)?.is_none() {
                    process::exit(0);
                }
            }
            Step::EchoLine => match line(stdin)? {
                Some(l) => out.write_all(l.as_bytes())?,
                None => process::exit(0),
            },
            Step::Echo => {
                let mut buf = String::new();
                while stdin.read_line(&mut buf)? > 0 {
                    out.write_all(buf.as_bytes())?;
                    out.flush()?;
                    buf.clear();
                }
            }
            Step::Seq(n) => {
                for i in 1..=*n {
                    writeln!(out, "{i}")?;
                }
            }
            Step::Cwd => {
                let dir = std::env::current_dir()?;
                out.write_all(dir.to_string_lossy().as_bytes())?;
            }
            Step::Pid => write!(out, "{}", process::id())?,
            Step::Exit(code) => {
                out.flush()?;
                process::exit(*code);
            }
        }
        out.flush()?;
    }
    Ok(())
}

fn main() {
    let (steps, loop_from) = parse(std::env::args().skip(1));
    let stdout = io::stdout();
    let mut out = stdout.lock();
    let stdin = io::stdin();
    let mut stdin = stdin.lock();
    // A write that fails is the terminal going away under us — the harness
    // has torn down. Nothing to report to, so nothing to report.
    let result = match loop_from {
        Some(from) => run(&steps[..from], &mut out, &mut stdin).and_then(|()| loop {
            run(&steps[from..], &mut out, &mut stdin)?;
        }),
        None => run(&steps, &mut out, &mut stdin),
    };
    if result.is_err() {
        process::exit(0);
    }
}
