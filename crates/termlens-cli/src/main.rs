//! `termlens` — the harness at a shell prompt (#255): `inspect` runs a
//! program in a PTY and prints its screen, `diff` compares two saved
//! screens cell by cell, `render` turns one into ANSI, SVG or HTML.
//!
//! A saved screen is the snapshot text format of `docs/DESIGN.md` §3 — an
//! insta `.snap` with or without its header, the block a wait error prints,
//! a `TERMLENS_ARTIFACT_DIR` file — or the JSON the crate's `serde` feature
//! writes. `Screen::parse` reads the first back; this binary only decides
//! which of the two a file is. `diff` and `render` take `-` for standard
//! input, and `render --out PATH` writes there instead of to stdout.
//!
//! Exit codes: 0 ran; `diff` exits 1 when the screens differ; 2 means the
//! command itself could not run — bad arguments, an unreadable file, a
//! program that could not be spawned. `inspect` is a viewer, not a gate:
//! the program's own exit status is reported on stderr under the screen,
//! not propagated — and the screen alone goes to stdout, so `inspect … >
//! file` is a saved screen `diff` and `render` read back (#340).

use std::io::{self, IsTerminal, Read, Write};
use std::path::Path;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use termlens::{Screen, ScreenDiff, Terminal};

const USAGE: &str = "\
usage: termlens <command> [options]

commands:
  inspect [options] <program> [args…]  run a program in a PTY and print its screen
  diff [--color WHEN] <a> <b>          compare two saved screens; exit 1 when they differ
  render --svg|--html|--ansi|--text|--json [--out PATH] <a>  render a saved screen

A saved screen is the snapshot text format termlens prints — what `inspect`
writes to stdout, an insta .snap with or without its header, the block a
wait error prints, a TERMLENS_ARTIFACT_DIR file — or the JSON the crate's
`serde` feature writes. `diff` and `render` read `-` as standard input; only
one of diff's two operands can be, since stdin is read once.

Exit code 0: the command ran (diff: the screens are the same picture).
Exit code 1: diff found a difference. Exit code 2: termlens itself could not
run — bad arguments, an unreadable file, a program that could not be spawned.

  -h, --help     print this text (also after a command)
      --version  print the version";

const INSPECT_USAGE: &str = "\
usage: termlens inspect [--size COLSxROWS] [--timeout SECONDS] [--idle MILLIS]
                        [--cwd PATH] [--inherit-env] [--ansi]
                        [--env KEY=VALUE]... <program> [args…]

Runs <program> in an 80x24 pseudo-terminal (or --size) and prints the
rendered screen. The wait ends on whichever comes first: the program
exits, or its output has been silent for --idle milliseconds (default
300), bounded by --timeout (default 5 seconds); a program still running
when it ends is killed. The child environment is cleared by default
except for PATH; --inherit-env keeps the caller's environment, and
repeatable --env sets selected values.
--cwd runs the program in PATH, which must be an existing directory.
--ansi paints the screen in colour on a terminal.

What goes to stdout depends on where stdout goes. A terminal gets what
you came to look at: the plain text, or the painted screen with --ansi.
Anything else — a redirect, a pipe — gets a saved screen with its
styles: block, because that is the rendering that carries colour and the
one `termlens diff` and `termlens render` read back. So a redirect never
loses a style, and never writes an escape those two would refuse.

The screen goes to stdout and nothing else does, so `inspect … > file`
saves a screen that `termlens diff` and `termlens render` read back. The
trailer that says what the program did — its exit status, or that it was
still running when the wait ended — goes to stderr. Exit code 2 means inspect
itself could not run: bad arguments, or a program that could not be spawned.";

const DIFF_USAGE: &str = "\
usage: termlens diff [--color auto|always|never] <a> <b>

Either <a> or <b> may be `-`, meaning standard input — not both, since
stdin is read once.

Parses two saved screens and prints what changed from <a> to <b>: the rows
that differ side by side, the size and cursor deltas, the style runs before
and after. On a terminal the changed cells are coloured (red in <a>, green
in <b>) unless --color never or NO_COLOR is set; in a pipe the rendering is
the plain one Screen::diff prints in a CI log. Exit code 0 when the two are
the same picture, 1 when they differ, 2 when a file could not be read.";

const RENDER_USAGE: &str = "\
usage: termlens render (--svg | --html | --ansi | --text | --json) [--out PATH] <a>

Prints a saved screen as an SVG image, an HTML fragment, the screen in ANSI
colour for a terminal, the plain text format with its styles: block, or the
format-1 JSON document the crate's `serde` feature writes.

<a> may be `-`, meaning standard input. --out writes to PATH instead of
stdout; `--out -` is standard output, and a file named `-` is still `./-`.
--out creates nothing when the render fails — unlike a shell redirect,
which truncates the file before termlens runs.";

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
        // A leading `-` is a flag, not a command name. Calling `--verbose`
        // an unknown command sent people to the subcommand list for the
        // two guesses this CLI does not have (#475). A bare `-` is not a
        // flag anywhere in this CLI — every subcommand reads it as standard
        // input — so it stays an unknown command.
        other if other.starts_with('-') && other != STDIN => {
            fail(&format!("unknown option {other:?} (try --help)"))
        }
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
///
/// `-` is standard input, the universal convention (#317): a saved screen
/// most often arrives on a pipe, out of a CI log or from the tool that made
/// it a moment earlier. Only the reading changes; the parser already took a
/// `&str` rather than a path.
fn load(path: &str) -> Result<Screen, String> {
    let raw = if path == STDIN {
        let mut raw = String::new();
        io::stdin()
            .read_to_string(&mut raw)
            .map_err(|e| format!("{STDIN_NAME}: {e}"))?;
        raw
    } else {
        std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?
    };
    // A checkout on Windows may have given the file CRLF endings; the
    // formats are line-based and neither cares.
    let raw = raw.replace("\r\n", "\n");
    let body = strip_insta_header(&raw).trim_start_matches('\n');
    if body.starts_with('{') {
        return serde_json::from_str(body)
            .map_err(|e| format!("{}: not a termlens Screen: {e}", name_of(path)));
    }
    Screen::parse(strip_inspect_trailer(body)).map_err(|e| format!("{}: {e}", name_of(path)))
}

/// The dash operand: standard input where a screen is read, standard
/// output where one is written (`render --out -`, #469). One constant,
/// because it is one convention — a reader who meets `-` in either
/// position should not have to ask which stream is meant.
const STDIN: &str = "-";
/// What a diagnostic calls it, since `-: ...` reads as a stray flag.
const STDIN_NAME: &str = "<stdin>";

/// How long `inspect` gives the reap to land after the EOF that ended the
/// idle wait. The kernel can report the terminal's close a scheduling hair
/// before the child's status becomes collectable — the stress run caught
/// the gap (#374, iteration 3 of 25 on a loaded 16-thread runner) — and the
/// trailer should say exited when the program did exit. The same width the
/// library gives its own post-reap drain, for the same reason. Only ever
/// paid by a child that is genuinely still running, and only after the wait
/// itself has ended.
const REAP_GRACE: Duration = Duration::from_millis(500);

/// A file operand as a diagnostic should name it.
fn name_of(path: &str) -> &str {
    if path == STDIN {
        STDIN_NAME
    } else {
        path
    }
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
/// non-blank line is dropped when it is one of the trailers `inspect`
/// writes — exactly those, so a grid row that happens to start with `---`
/// is left alone. `--- still running` is matched as a prefix so that both
/// of the two still-running trailers (#374) and the single one every
/// release up to 0.11.2 wrote are all stripped from a saved screen.
fn strip_inspect_trailer(text: &str) -> &str {
    let trimmed = text.trim_end_matches('\n');
    let start = trimmed.rfind('\n').map_or(0, |at| at + 1);
    let last = &trimmed[start..];
    let is_trailer = last.ends_with(" ---")
        && [
            "--- exited: ",
            "--- still running",
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

/// The WHEN word `--color` takes, in either spelling of the flag, or the
/// diagnostic naming anything else — one list of three words, so the two
/// spellings cannot drift apart (#452).
fn color_when(word: &str) -> Result<ColorWhen, String> {
    match word {
        "auto" => Ok(ColorWhen::Auto),
        "always" => Ok(ColorWhen::Always),
        "never" => Ok(ColorWhen::Never),
        other => Err(format!(
            "--color takes auto, always or never, got {other:?}"
        )),
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
                // A WHEN that is not there is its own diagnostic, in the
                // shape every other flag's missing value takes — not the
                // `Some("before.snap")`/`None` the raw Option printed while
                // quietly eating the operand (#452).
                let Some(word) = args.next() else {
                    return fail("--color needs a WHEN argument");
                };
                match color_when(word) {
                    Ok(when) => color = when,
                    Err(e) => return fail(&e),
                }
            }
            other if other.starts_with("--color=") => {
                match color_when(&other["--color=".len()..]) {
                    Ok(when) => color = when,
                    Err(e) => return fail(&e),
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
    // Standard input can be consumed once, so it can be one operand and not
    // both — said plainly here rather than left to look like an empty second
    // screen further down (#317).
    if a.as_str() == STDIN && b.as_str() == STDIN {
        return fail("only one of the two screens can be `-`: stdin is read once");
    }
    // Left first, so `diff a.snap -` and `diff - b.snap` both read the pipe
    // at the point the argument order says they do.
    let (before, after) = match (load(a), load(b)) {
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

/// Whether `line` is one of the plain rendering's changed-row lines, the
/// `{row:>3} │…│…` shape `ScreenDiff`'s `Display` writes
/// (`crates/termlens/src/screen/diff.rs`). The row number is right-aligned,
/// so before the first frame there are only digits and spaces and at least
/// one digit — which no header or trailer line is, so the shape is where
/// the colour rendering's body begins.
fn is_row_line(line: &str) -> bool {
    let Some((number, _)) = line.split_once('│') else {
        return false;
    };
    let Some(number) = number.strip_suffix(' ') else {
        return false;
    };
    number.chars().any(|c| c.is_ascii_digit())
        && number.chars().all(|c| c.is_ascii_digit() || c == ' ')
}

/// The diff for a terminal: the plain rendering's header and trailer, and
/// each changed row side by side with its changed cells painted — red for
/// what `before` showed, green for what `after` shows — in place of the
/// `^` marker line, which colour makes redundant.
///
/// This owns the body — rows are rebuilt from the screens, painted cell by
/// cell, and the marker lines dropped with them — and passes everything
/// else through as printed: the header (every line before the first row,
/// so the overlap note of a differently sized comparison is carried too,
/// #365) and the trailer (the unchanged-row count and the style runs).
fn colored(before: &Screen, after: &Screen, diff: &ScreenDiff) -> String {
    let plain = diff.to_string();
    if diff.is_empty() {
        return plain;
    }
    let mut lines = plain.lines();
    let mut out = String::new();
    // The header is every line before the first row: one today, two when
    // the sizes differ and the second says what was clipped. Found by the
    // row's shape rather than a line count, so a header line added later
    // is passed through without touching this. The row line itself is
    // consumed here and rebuilt below.
    for line in lines.by_ref() {
        if is_row_line(line) {
            break;
        }
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(line);
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
    let mut files = Vec::new();
    let mut out_path: Option<&str> = None;
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => return print(&format!("{RENDER_USAGE}\n")),
            "--version" => return print(&version()),
            "--svg" | "--html" | "--ansi" | "--text" | "--json" => format = Some(arg.as_str()),
            "--out" => match args.next() {
                Some(path) => out_path = Some(path.as_str()),
                None => return fail("--out needs a PATH argument"),
            },
            other if other.starts_with("--out=") => {
                let path = &other["--out=".len()..];
                if path.is_empty() {
                    return fail("--out needs a PATH argument");
                }
                out_path = Some(path);
            }
            other if other.starts_with('-') && other != STDIN => {
                return fail(&format!(
                    "unknown option {other:?} (try `termlens render --help`)"
                ));
            }
            _ => files.push(arg),
        }
    }
    // The operand is the whole input: two of them are ambiguous, and letting
    // the last win is how a stale path renders a screen nobody named (#364).
    let (Some(format), [file]) = (format, files.as_slice()) else {
        eprintln!("{RENDER_USAGE}");
        return ExitCode::from(2);
    };
    let screen = match load(file) {
        Ok(screen) => screen,
        Err(e) => return fail(&e),
    };
    let out = match format {
        "--svg" => screen.to_svg(),
        "--html" => screen.to_html(),
        "--ansi" => screen.to_ansi(),
        // Pretty-printed and newline-terminated, the shape the corpus
        // commits: a saved screen a human diffs, and one `load` reads back.
        "--json" => match serde_json::to_string_pretty(&screen) {
            Ok(json) => format!("{json}\n"),
            Err(e) => return fail(&format!("writing the JSON failed: {e}")),
        },
        _ => format!("{}\n", screen.with_styles()),
    };
    // The file is created here, after the screen parsed, and not before
    // (#313): `termlens render … > out.svg` truncates out.svg in the shell
    // before the process runs, so a failing render leaves an empty file
    // behind. --out cannot, because nothing is opened until there are bytes.
    // `--out -` is stdout, the same stream every other `-` operand already
    // is (#469). Treating it as a filename wrote `./-` and printed nothing.
    match out_path {
        Some(STDIN) | None => print(&out),
        Some(path) => match std::fs::write(path, &out) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => fail(&format!("{path}: {e}")),
        },
    }
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

/// What `inspect` writes to stdout, which is two different jobs.
///
/// A terminal is a person looking: the plain text they already read, or
/// with `--ansi` the header over the screen painted in colour. Anything
/// else — a redirect, a pipe — is a saved screen somebody means to read
/// back, so it gets `with_styles`: the text format *with* its `styles:`
/// block, which is the only one of the two that carries colour.
///
/// That split is what #454 and #478 each asked for from one side.
/// Without the block a redirect lost every style, so `diff` called bold
/// red and bold green the same picture (#454). With `to_ansi` a redirect
/// wrote C0 controls the snapshot format refuses, so `render` and `diff`
/// would not read back what `inspect --ansi > file` had just written
/// (#478). Both are the same mistake — writing the *viewing* rendering
/// to a file — and `is_terminal` is what tells the two jobs apart.
fn inspect_render(screen: &Screen, ansi: bool) -> String {
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

fn inspect(args: Vec<String>) -> ExitCode {
    let mut args = args.into_iter().peekable();
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
        // `--flag=value` is split only for the flags that take a value, so
        // each keeps its own diagnostic for the spelling and the whole token
        // stays in `flag` for the unknown-option message. A value attached
        // to a flag that takes none (`--inherit-env=nonsense`) falls through
        // to the catch-all and is refused there, as `render` refuses
        // `--svg=1`; splitting it would apply the flag and drop the value.
        let (name, inline) = match flag.split_once('=') {
            Some((name, value))
                if matches!(name, "--size" | "--timeout" | "--idle" | "--cwd" | "--env") =>
            {
                (name, Some(value))
            }
            _ => (flag.as_str(), None),
        };
        // The inline value first, then the next argument: `take` reads one
        // stream, so both spellings parse and diagnose identically.
        let mut args = inline.map(str::to_owned).into_iter().chain(args.by_ref());
        let parsed = match name {
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
            _ => Err(format!(
                "unknown option {flag:?} (try `termlens inspect --help`)"
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
    if let Some(dir) = cwd {
        builder = builder.current_dir(dir);
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
    //
    // Resolve on whichever comes first — the program exits, or its output
    // has been silent for `idle` — under the one deadline (#374). Waiting
    // for the exit first charged the full `--timeout` to every TUI that
    // never exits, however complete the screen already was. `wait_idle_for`
    // is that race, not half of it: it also resolves on the EOF that says
    // the child is gone, and its deadline is applied once. The reaping
    // probe then says which arm fired; it only reaps (and drains the final
    // bytes) when the child is already gone, so an exit lands on its own
    // trailer with the finished screen.
    // Two trailers for a child that outlived the wait, because the wait can
    // now end two ways and "at the deadline" is only true of one of them.
    // `wait_idle_for` returning `Ok` means the output went quiet *or* the
    // terminal reached EOF; neither is the deadline, and a TUI snapshotted
    // 200ms into a 30s budget must not claim otherwise.
    let still_running = "--- still running (killed on exit) ---";
    let at_the_deadline = "--- still running at the deadline (killed on exit) ---";
    let reap = |t: &mut termlens::Terminal| match t.wait_exit_for(REAP_GRACE) {
        Ok(status) => format!("--- exited: {status} ---"),
        Err(termlens::Error::Timeout { .. }) => still_running.to_owned(),
        Err(e) => format!("--- waiting for the program failed: {e} ---"),
    };
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
                // Output never went silent for `idle`: the deadline ended it.
                Err(termlens::Error::Timeout { .. }) => at_the_deadline.to_owned(),
                Err(e) => format!("--- waiting for the program failed: {e} ---"),
            }
        }
        // The program went without painting anything, as `true` does.
        Err(termlens::Error::Eof { .. }) => reap(&mut t),
        // Nothing appeared before the deadline.
        Err(termlens::Error::Timeout { .. }) => at_the_deadline.to_owned(),
        Err(e) => format!("--- waiting for the program failed: {e} ---"),
    };
    let code = print(&format!("{}\n", inspect_render(&t.screen(), ansi)));
    eprintln!("{trailer}");
    code
}
