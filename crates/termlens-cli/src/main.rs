//! `termlens` — the harness at a shell prompt (#255): `inspect` runs a
//! program in a PTY and prints its screen, `diff` compares two saved
//! screens cell by cell, `render` turns one into ANSI, SVG or HTML.
//!
//! A saved screen is the snapshot text format of `docs/DESIGN.md` §3 — an
//! insta `.snap` with or without its header, the block a wait error prints,
//! a `TERMLENS_ARTIFACT_DIR` file — or the JSON the crate's `serde` feature
//! writes. `Screen::parse` reads the first back; this binary only decides
//! which of the two a file is.
//!
//! Exit codes: 0 ran; `diff` exits 1 when the screens differ; 2 means the
//! command itself could not run — bad arguments, an unreadable file, a
//! program that could not be spawned. `inspect` is a viewer, not a gate:
//! the program's own exit status is reported on stderr under the screen,
//! not propagated — and the screen alone goes to stdout, so `inspect … >
//! file` is a saved screen `diff` and `render` read back (#340).

use std::io::{self, IsTerminal, Write};
use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

use termlens::{Screen, ScreenDiff, Terminal};

const USAGE: &str = "\
usage: termlens <command> [options]

commands:
  inspect [options] <program> [args…]  run a program in a PTY and print its screen
  diff [--color WHEN] <a> <b>          compare two saved screens; exit 1 when they differ
  render --svg|--html|--ansi|--text <a>  render a saved screen

A saved screen is the snapshot text format termlens prints — what `inspect`
writes to stdout, an insta .snap with or without its header, the block a
wait error prints, a TERMLENS_ARTIFACT_DIR file — or the JSON the crate's
`serde` feature writes.

Exit code 0: the command ran (diff: the screens are the same picture).
Exit code 1: diff found a difference. Exit code 2: termlens itself could not
run — bad arguments, an unreadable file, a program that could not be spawned.

  -h, --help     print this text (also after a command)
      --version  print the version";

const INSPECT_USAGE: &str = "\
usage: termlens inspect [--size COLSxROWS] [--timeout SECONDS] [--idle MILLIS]
                        [--inherit-env] [--ansi] [--env KEY=VALUE]... <program> [args…]

Runs <program> in an 80x24 pseudo-terminal (or --size), waits for it to
exit or for the deadline (--timeout, default 5 seconds), and prints the
rendered screen. A program still running at the deadline is snapshotted
after --idle milliseconds (default 300) of output silence, then killed.
The child environment is cleared by default except for PATH; --inherit-env
keeps the caller's environment, and repeatable --env sets selected values.
--ansi paints the screen in colour instead of the plain text format.

The screen goes to stdout and nothing else does, so `inspect … > file`
saves a screen that `termlens diff` and `termlens render` read back. The
trailer that says what the program did — its exit status, or that it was
still running at the deadline — goes to stderr. Exit code 2 means inspect
itself could not run: bad arguments, or a program that could not be spawned.";

const DIFF_USAGE: &str = "\
usage: termlens diff [--color auto|always|never] <a> <b>

Parses two saved screens and prints what changed from <a> to <b>: the rows
that differ side by side, the size and cursor deltas, the style runs before
and after. On a terminal the changed cells are coloured (red in <a>, green
in <b>) unless --color never or NO_COLOR is set; in a pipe the rendering is
the plain one Screen::diff prints in a CI log. Exit code 0 when the two are
the same picture, 1 when they differ, 2 when a file could not be read.";

const RENDER_USAGE: &str = "\
usage: termlens render (--svg | --html | --ansi | --text) <a>

Prints a saved screen as an SVG image, an HTML fragment, the screen in ANSI
colour for a terminal, or the plain text format with its styles: block.";

/// Where a rendering goes, so a reader that closed early (`termlens … |
/// head`) is a clean exit rather than a panic on a broken pipe.
fn print(out: &str) -> ExitCode {
    let mut stdout = io::stdout().lock();
    match stdout
        .write_all(out.as_bytes())
        .and_then(|()| stdout.flush())
    {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) if e.kind() == io::ErrorKind::BrokenPipe => ExitCode::SUCCESS,
        Err(e) => fail(&format!("writing the output failed: {e}")),
    }
}

/// One diagnostic shape for every failure of the tool itself.
fn fail(message: &str) -> ExitCode {
    eprintln!("termlens: {message}");
    ExitCode::from(2)
}

/// The one-line string every `--version` flag prints.
fn version() -> String {
    format!("termlens {}\n", env!("CARGO_PKG_VERSION"))
}

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let Some(command) = args.next() else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };
    let rest: Vec<String> = args.collect();
    match command.as_str() {
        "-h" | "--help" => print(&format!("{USAGE}\n")),
        "--version" => print(&version()),
        "inspect" => inspect(rest),
        "diff" => diff(&rest),
        "render" => render(&rest),
        other => fail(&format!("unknown command {other:?} (try --help)")),
    }
}

// ---------------------------------------------------------------- saved screens

/// Read a saved screen: an insta `.snap` (its `---` header dropped), the
/// text format (an `inspect` trailer from before 0.11 dropped), or JSON.
///
/// The two decorations this skips are the two termlens itself writes
/// around a screen, and `Screen::parse` accepts neither — the library
/// reads the format, the tool knows its own wrappers (`docs/DESIGN.md` §3).
fn load(path: &Path) -> Result<Screen, String> {
    let raw = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    // A checkout on Windows may have given the file CRLF endings; the
    // formats are line-based and neither cares.
    let raw = raw.replace("\r\n", "\n");
    let body = strip_insta_header(&raw).trim_start_matches('\n');
    if body.starts_with('{') {
        return serde_json::from_str(body)
            .map_err(|e| format!("{}: not a termlens Screen: {e}", path.display()));
    }
    Screen::parse(strip_inspect_trailer(body)).map_err(|e| format!("{}: {e}", path.display()))
}

/// insta writes `---\n<metadata>\n---\n` above the content; the content is
/// what was snapshotted.
fn strip_insta_header(text: &str) -> &str {
    text.strip_prefix("---\n")
        .and_then(|rest| rest.find("\n---\n").map(|end| &rest[end + 5..]))
        .unwrap_or(text)
}

/// `inspect` before 0.11 wrote its `--- exited: … ---` trailer to stdout,
/// so a screen saved with `> file` then carried it (#340). The trailer now
/// goes to stderr, and a file saved that way still reads: the last
/// non-blank line is dropped when it is one of the three trailers
/// `inspect` writes — exactly those, so a grid row that happens to start
/// with `---` is left alone.
fn strip_inspect_trailer(text: &str) -> &str {
    let trimmed = text.trim_end_matches('\n');
    let start = trimmed.rfind('\n').map_or(0, |at| at + 1);
    let last = &trimmed[start..];
    let is_trailer = last.ends_with(" ---")
        && [
            "--- exited: ",
            "--- still running at the deadline",
            "--- waiting for the program failed: ",
        ]
        .iter()
        .any(|prefix| last.starts_with(prefix));
    if is_trailer {
        &trimmed[..start]
    } else {
        text
    }
}

// ------------------------------------------------------------------------ diff

/// When `diff` colours its output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ColorWhen {
    Auto,
    Always,
    Never,
}

impl ColorWhen {
    fn enabled(self) -> bool {
        match self {
            ColorWhen::Always => true,
            ColorWhen::Never => false,
            ColorWhen::Auto => {
                io::stdout().is_terminal()
                    && std::env::var_os("NO_COLOR").is_none_or(|v| v.is_empty())
            }
        }
    }
}

fn diff(args: &[String]) -> ExitCode {
    let mut color = ColorWhen::Auto;
    let mut files = Vec::new();
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => return print(&format!("{DIFF_USAGE}\n")),
            "--version" => return print(&version()),
            "--color" => {
                color = match args.next().map(String::as_str) {
                    Some("auto") => ColorWhen::Auto,
                    Some("always") => ColorWhen::Always,
                    Some("never") => ColorWhen::Never,
                    other => {
                        return fail(&format!(
                            "--color takes auto, always or never, got {other:?}"
                        ))
                    }
                }
            }
            other if other.starts_with("--color=") => {
                color = match &other["--color=".len()..] {
                    "auto" => ColorWhen::Auto,
                    "always" => ColorWhen::Always,
                    "never" => ColorWhen::Never,
                    got => {
                        return fail(&format!("--color takes auto, always or never, got {got:?}"))
                    }
                }
            }
            other if other.starts_with('-') && other != "-" => {
                return fail(&format!(
                    "unknown option {other:?} (try `termlens diff --help`)"
                ));
            }
            _ => files.push(arg),
        }
    }
    let [a, b] = files.as_slice() else {
        eprintln!("{DIFF_USAGE}");
        return ExitCode::from(2);
    };
    let (before, after) = match (load(Path::new(a)), load(Path::new(b))) {
        (Ok(a), Ok(b)) => (a, b),
        (Err(e), _) | (_, Err(e)) => return fail(&e),
    };
    let diff = before.diff(&after);
    let rendered = if color.enabled() {
        colored(&before, &after, &diff)
    } else {
        diff.to_string()
    };
    let code = print(&format!("{rendered}\n"));
    if code != ExitCode::SUCCESS {
        return code;
    }
    if diff.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

/// The diff for a terminal: the plain rendering's header and trailer, and
/// each changed row side by side with its changed cells painted — red for
/// what `before` showed, green for what `after` shows — in place of the
/// `^` marker line, which colour makes redundant.
fn colored(before: &Screen, after: &Screen, diff: &ScreenDiff) -> String {
    let plain = diff.to_string();
    if diff.is_empty() {
        return plain;
    }
    let mut lines = plain.lines();
    let mut out = String::new();
    if let Some(header) = lines.next() {
        out.push_str(header);
    }
    let mut changed_rows: Vec<u16> = diff.cells().map(|(row, _, _, _)| row).collect();
    changed_rows.dedup();
    for row in changed_rows {
        let paint = |screen: &Screen, sgr: &str| -> String {
            let mut cells: Vec<String> = Vec::new();
            for col in 0..screen.cols() {
                let Some(cell) = screen.cell(row, col) else {
                    continue;
                };
                if cell.is_wide_continuation() {
                    continue;
                }
                let text = if cell.contents().is_empty() {
                    " "
                } else {
                    cell.contents()
                };
                let changed = diff.cells().any(|(r, c, _, _)| r == row && c == col);
                cells.push(if changed {
                    format!("\x1b[{sgr}m{text}\x1b[0m")
                } else {
                    text.to_owned()
                });
            }
            cells.concat()
        };
        // The before side keeps its full width so the separator stays in one
        // column down the listing; the after side is trimmed like a row.
        let after_row = paint(after, "32");
        let after_row = after_row.trim_end();
        out.push_str(&format!("\n{row:>3} │{}│{after_row}", paint(before, "31")));
    }
    // The trailer: the unchanged-row count and the style runs, as printed.
    for line in lines.filter(|l| l.starts_with('…') || l.starts_with("styles:")) {
        out.push('\n');
        out.push_str(line);
    }
    out
}

// ---------------------------------------------------------------------- render

fn render(args: &[String]) -> ExitCode {
    let mut format: Option<&str> = None;
    let mut file = None;
    for arg in args {
        match arg.as_str() {
            "-h" | "--help" => return print(&format!("{RENDER_USAGE}\n")),
            "--version" => return print(&version()),
            "--svg" | "--html" | "--ansi" | "--text" => format = Some(arg.as_str()),
            other if other.starts_with('-') && other != "-" => {
                return fail(&format!(
                    "unknown option {other:?} (try `termlens render --help`)"
                ));
            }
            _ => file = Some(arg),
        }
    }
    let (Some(format), Some(file)) = (format, file) else {
        eprintln!("{RENDER_USAGE}");
        return ExitCode::from(2);
    };
    let screen = match load(Path::new(file)) {
        Ok(screen) => screen,
        Err(e) => return fail(&e),
    };
    let out = match format {
        "--svg" => screen.to_svg(),
        "--html" => screen.to_html(),
        "--ansi" => screen.to_ansi(),
        _ => format!("{}\n", screen.with_styles()),
    };
    print(&out)
}

// --------------------------------------------------------------------- inspect

/// The value after `flag`, or the one-line diagnostic every flag shares.
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

/// The text rendering, or with `--ansi` the header over the screen in colour.
fn inspect_render(screen: &Screen, ansi: bool) -> String {
    if ansi {
        let header = screen.to_string();
        let header = header.lines().next().unwrap_or_default();
        format!("{header}\n{}", screen.to_ansi())
    } else {
        screen.to_string()
    }
}

fn inspect(args: Vec<String>) -> ExitCode {
    let mut args = args.into_iter().peekable();
    let mut size = (80u16, 24u16);
    let mut timeout = Duration::from_secs(5);
    let mut idle = Duration::from_millis(300);
    let mut inherit_env = false;
    let mut ansi = false;
    let mut env = Vec::new();

    // Options come before the program; everything after it is the
    // program's own, however flag-like it looks.
    while args.peek().is_some_and(|a| a.starts_with('-') && a != "-") {
        let flag = args.next().unwrap_or_default();
        let parsed = match flag.as_str() {
            "-h" | "--help" => return print(&format!("{INSPECT_USAGE}\n")),
            "--version" => return print(&version()),
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
            "--ansi" => {
                ansi = true;
                Ok(())
            }
            "--env" => take(&mut args, "--env", "KEY=VALUE", "NO_COLOR=1", |s| {
                let (key, value) = s.split_once('=')?;
                (!key.is_empty()).then(|| (key.to_owned(), value.to_owned()))
            })
            .map(|pair| env.push(pair)),
            other => Err(format!(
                "unknown option {other:?} (try `termlens inspect --help`)"
            )),
        };
        if let Err(message) = parsed {
            return fail(&message);
        }
    }

    let Some(program) = args.next() else {
        eprintln!("{INSPECT_USAGE}");
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

    let mut t = match builder.spawn(&program) {
        Ok(t) => t,
        Err(e) => return fail(&e.to_string()),
    };

    // The screen to stdout, the trailer to stderr (#340): `inspect … > file`
    // then saves exactly a screen, which `diff` and `render` read back,
    // while a human at a terminal still sees both. Before 0.11 the trailer
    // followed the screen on stdout and no CLI route produced a file the
    // CLI would accept.
    let trailer = match t.wait_exit() {
        Ok(status) => format!("--- exited: {status} ---"),
        Err(termlens::Error::Timeout { .. }) => {
            // Still running at the deadline: settle on a quiet screen
            // instead, bounded by the deadline too unless the silence window
            // asked for is itself longer.
            let _ = t.wait_idle_for(idle, timeout.max(idle));
            "--- still running at the deadline (killed on exit) ---".to_owned()
        }
        Err(e) => format!("--- waiting for the program failed: {e} ---"),
    };
    let code = print(&format!("{}\n", inspect_render(&t.screen(), ansi)));
    eprintln!("{trailer}");
    code
}
