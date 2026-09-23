//! Run any command inside termlens and print its rendered screen — a
//! debugging tool for "what does my app actually look like in the grid?".
//!
//! ```sh
//! cargo run --example inspect -- ls -la
//! cargo run --example inspect -- --size 120x40 htop
//! cargo run --example inspect -- --timeout 30 ./target/debug/slow-app
//! cargo run --example inspect -- --env NO_COLOR=1 my-app
//! ```
//!
//! The wait ends on whichever comes first: the program exits, or its
//! output has been silent for a window (`--idle`, 300ms by default) —
//! bounded by the deadline (`--timeout`, five seconds by default). Five
//! seconds is what a test suite wants, where a deadline exists to turn a
//! hang into a readable failure; a person at a terminal pointing this at an
//! application that loads a large file or compiles before it draws is
//! willing to wait longer, which is what the flag is for. The silence
//! window has the same shape: an application that paints in bursts wider
//! than 300ms is snapshotted mid-render unless it is widened.
//!
//! The screen alone goes to stdout and the trailer to stderr, so
//! `inspect … > file` writes a saved screen. Exit code 0 means inspect ran
//! and printed a screen; the trailer says what the program did — its exit
//! status, or that it was still running when the wait ended. Exit code 2 means
//! inspect itself could not run: bad arguments, or a program that could
//! not be spawned. A viewer, not a gate: the program's own status is
//! reported, not propagated.

use std::io::{self, IsTerminal, Write};
use std::path::Path;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use termlens::Terminal;

/// The one copy of the usage text: `--help` prints it to stdout and exits
/// 0, a missing program prints it to stderr and exits 2 (#229).
/// How long to give the reap after the EOF that ended the idle wait: the
/// kernel can report the terminal's close a scheduling hair before the
/// child's status is collectable, and the trailer should say exited when
/// the program did exit. `termlens inspect` gives the same width, for the
/// same reason (#374).
const REAP_GRACE: Duration = Duration::from_millis(500);

const USAGE: &str = "\
usage: inspect [--size COLSxROWS] [--timeout SECONDS] [--idle MILLIS]
               [--cwd PATH] [--inherit-env] [--ansi]
               [--env KEY=VALUE]... <program> [args…]

Runs <program> in an 80x24 pseudo-terminal (or --size) and prints the
rendered screen. The wait ends on whichever comes first: the program
exits, or its output has been silent for --idle milliseconds (default
300), bounded by --timeout (default 5 seconds); a program still running
when it ends is killed.
The child environment is cleared by default except for PATH; --inherit-env
keeps the caller's environment, and repeatable --env sets selected values.
--cwd runs the program in PATH, which must be an existing directory.

The screen goes to stdout and nothing else does, so `inspect … > file`
saves a screen; the trailer that says what the program did — its exit
status, or that it was still running when the wait ended — goes to stderr.
Exit code 0: a screen was printed. Exit code 2: inspect itself could not
run — bad arguments, or a program that could not be spawned.

  -h, --help     print this text
      --version  print the version
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

/// Whether the program has put anything on the grid yet: a visible
/// character, or a cursor that has moved. A blank screen with the cursor at
/// the origin is what a PTY looks like before its program writes a byte —
/// and it is also what a program that clears and homes without painting
/// looks like, which is why that case waits for the exit or the deadline,
/// exactly as every release up to 0.11.2 did.
fn shows_something(screen: &termlens::Screen) -> bool {
    let (row, col, _) = screen.cursor();
    (row, col) != (0, 0) || !screen.text().trim().is_empty()
}

/// Two jobs, as in the command this mirrors: a terminal is a person
/// looking (plain text, or `--ansi` painted), anything else is a saved
/// screen somebody means to read back, so it gets `with_styles` — the
/// only one of the two text renderings that carries colour (#454, #478).
fn render(screen: &termlens::Screen, ansi: bool) -> String {
    if !io::stdout().is_terminal() {
        screen.with_styles().to_string()
    } else if ansi {
        let header = screen.to_string();
        let header = header.lines().next().unwrap_or_default();
        format!("{header}\n{}", screen.to_ansi())
    } else {
        screen.to_string()
    }
}

/// One diagnostic shape for every failure of the tool itself, and the exit
/// code 2 the CLI promises for the same failures.
fn fail(message: &str) -> ExitCode {
    eprintln!("inspect: {message}");
    ExitCode::from(2)
}

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1).peekable();

    let mut size = (80u16, 24u16);
    let mut timeout = Duration::from_secs(5);
    let mut idle = Duration::from_millis(300);
    let mut inherit_env = false;
    let mut ansi = false;
    let mut env = Vec::new();
    let mut cwd: Option<String> = None;

    // Options come before the program; everything after it is the
    // program's own, however flag-like it looks.
    while args.peek().is_some_and(|a| a.starts_with('-') && a != "-") {
        let flag = args.next().unwrap_or_default();
        // `--flag=value` for the flags that take a value, mirroring the
        // command this example follows: a value attached to a flag that
        // takes none (`--inherit-env=nonsense`) falls through to the
        // catch-all, which reports the whole token.
        let (name, inline) = match flag.split_once('=') {
            Some((name, value))
                if matches!(name, "--size" | "--timeout" | "--idle" | "--cwd" | "--env") =>
            {
                (name, Some(value))
            }
            _ => (flag.as_str(), None),
        };
        let mut args = inline.map(str::to_owned).into_iter().chain(args.by_ref());
        let parsed = match name {
            "-h" | "--help" => {
                println!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            "--version" => {
                println!("termlens {}", env!("CARGO_PKG_VERSION"));
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
            "--inherit-env" => {
                inherit_env = true;
                Ok(())
            }
            // The screen in colour, for a person: every cell painted through
            // the SGR its style derives to, so what the program showed is
            // shown rather than described by a `styles:` block.
            "--ansi" => {
                ansi = true;
                Ok(())
            }
            // Checked here rather than left to the builder so the diagnostic
            // names the flag the user typed; the builder refuses it too, with
            // a message about `current_dir` the caller never wrote (#312).
            "--cwd" => {
                take(&mut args, "--cwd", "PATH", "/tmp", |s| Some(s.to_owned())).and_then(|dir| {
                    if Path::new(&dir).is_dir() {
                        cwd = Some(dir);
                        Ok(())
                    } else {
                        Err(format!("bad --cwd {dir:?}, not an existing directory"))
                    }
                })
            }
            "--env" => take(&mut args, "--env", "KEY=VALUE", "NO_COLOR=1", |s| {
                let (key, value) = s.split_once('=')?;
                (!key.is_empty()).then(|| (key.to_owned(), value.to_owned()))
            })
            .map(|pair| env.push(pair)),
            other => Err(format!("unknown option {other:?} (try --help)")),
        };
        if let Err(message) = parsed {
            return fail(&message);
        }
    }

    let Some(program) = args.next() else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };

    let mut builder = Terminal::builder()
        .size(size.0, size.1)
        .timeout(timeout)
        .args(args);
    if !inherit_env {
        builder = builder.env_clear();
        if let Some(path) = std::env::var_os("PATH") {
            builder = builder.env("PATH", path);
        }
    }
    for (key, value) in env {
        builder = builder.env(key, value);
    }
    if let Some(dir) = cwd {
        builder = builder.current_dir(dir);
    }

    let mut t = match builder.spawn(&program) {
        Ok(t) => t,
        Err(e) => return fail(&e.to_string()),
    };

    // The screen to stdout, the trailer to stderr (#340): `inspect … > file`
    // then saves exactly a screen, while a human at a terminal still sees
    // both. `termlens inspect` splits the same two streams the same way.
    // Resolve on whichever comes first — the program exits, or its output
    // has been silent for `idle` — under the one deadline (#374).
    // `wait_idle_for` is that race rather than half of it: it also returns
    // on the EOF that says the child is gone. The reaping probe then says
    // which arm fired, and only reaps a child that has already exited, so
    // an exit lands on its own trailer with the finished screen. Two
    // still-running trailers, because "at the deadline" is true of only one
    // of the two ways the wait can end.
    let reap = |t: &mut termlens::Terminal| match t.wait_exit_for(REAP_GRACE) {
        Ok(status) => format!("--- exited: {status} ---"),
        Err(termlens::Error::Timeout { .. }) => "--- still running (killed on exit) ---".to_owned(),
        // Not "still running": the OS wait itself failed, and saying so is
        // the difference between a slow program and a broken harness.
        Err(e) => format!("--- waiting for the program failed: {e} ---"),
    };
    let at_the_deadline = || "--- still running at the deadline (killed on exit) ---".to_owned();
    // The silence window starts at the program's first output, not at its
    // spawn. `wait_idle_for` measures silence from the last byte, and before
    // the first byte that is the spawn — so a program slower than `idle` to
    // print anything (a cold `sh` under ConPTY, a JVM, anything that works
    // before it paints) came back as a blank screen at exit 0, killed. That
    // is the one outcome that reads as "this program shows nothing", and
    // 0.11.2, which waited for the exit, never produced it. So wait first
    // for something to be silent *after* — or for the program to go, which
    // is `Eof` and keeps `inspect true` instant — and only then race the
    // exit against the silence, in whatever is left of the one deadline.
    let deadline = Instant::now() + timeout;
    let trailer = match t.wait_until_for(shows_something, timeout) {
        Ok(()) => {
            let remaining = deadline.saturating_duration_since(Instant::now());
            match t.wait_idle_for(idle, remaining) {
                Ok(()) => reap(&mut t),
                Err(termlens::Error::Timeout { .. }) => at_the_deadline(),
                Err(e) => format!("--- waiting for the program failed: {e} ---"),
            }
        }
        Err(termlens::Error::Eof { .. }) => reap(&mut t),
        Err(termlens::Error::Timeout { .. }) => at_the_deadline(),
        Err(e) => format!("--- waiting for the program failed: {e} ---"),
    };
    // One write, and a reader that closed early (`inspect … | head`) is a
    // clean exit rather than a panic on a broken pipe (#223).
    let mut stdout = io::stdout().lock();
    let code = match stdout
        .write_all(render(&t.screen(), ansi).as_bytes())
        .and_then(|()| stdout.write_all(b"\n"))
        .and_then(|()| stdout.flush())
    {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) if e.kind() == io::ErrorKind::BrokenPipe => ExitCode::SUCCESS,
        Err(e) => fail(&format!("writing the screen failed: {e}")),
    };
    eprintln!("{trailer}");
    code
}
