//! `termlens` — the harness at a shell prompt (#255): `inspect` runs a
//! program in a PTY and prints its screen, `diff` compares two saved
//! screens cell by cell, `render` turns one into SVG, HTML, ANSI, the text
//! format or JSON.
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

use std::borrow::Cow;
use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::io::{self, IsTerminal, Read, Write};
use std::path::Path;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use termlens::{Screen, ScreenDiff, Terminal};

const USAGE: &str = "\
usage: termlens <command> [options]

commands:
  inspect [options] <program> [args…]  run a program in a PTY and print its screen
  diff [--color WHEN] [--] <a> <b>     compare two saved screens; exit 1 when they differ
  render --svg|--html|--ansi|--text|--json [--out PATH] [--] <a>  render a saved screen

A saved screen is the snapshot text format termlens prints — what `inspect`
writes to stdout, an insta .snap with or without its header, the block a
wait error prints, a TERMLENS_ARTIFACT_DIR file — or the JSON the crate's
`serde` feature writes. `diff` and `render` read `-` as standard input; only
one of diff's two operands can be, since stdin is read once. Either refuses
an input larger than 1 GiB, which no saved screen is.

Exit code 0: the command ran (diff: the screens are the same picture).
Exit code 1: diff found a difference. Exit code 2: termlens itself could not
run — bad arguments, an unreadable file, a program that could not be spawned.

  -h, --help     print this text (also after a command)
      --version  print the version";

const INSPECT_USAGE: &str = "\
usage: termlens inspect [--size COLSxROWS] [--timeout SECONDS] [--idle MILLIS]
                        [--cwd DIR] [--inherit-env] [--ansi]
                        [--env KEY=VALUE]... [--] <program> [args…]

Runs <program> in an 80x24 pseudo-terminal (or --size) and prints the
rendered screen. The wait ends on whichever comes first: the program
exits, or, once it has drawn something, its output has been silent for
--idle milliseconds (default 300). --timeout (default 5 seconds) bounds
both, and a program still running when the wait ends is killed.
The child environment is cleared except for PATH, and gets
TERM=xterm-256color and SHELL=/bin/sh; --inherit-env keeps the caller's
environment instead, but for TERM, and repeatable --env sets selected
values, TERM included.
--cwd runs the program in DIR, which must be an existing directory. A
relative DIR is taken from where inspect runs, and a relative program
path such as ./myapp is then found inside DIR.
--ansi paints the screen in colour on a terminal.
-- ends the options; the program name follows. Options already stop at
the first argument that does not begin with `-`, so this matters only for
a program whose name does. Every option that takes a value also accepts
it attached with `=`, as in --size=100x30 or --env=KEY=VALUE.

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
usage: termlens diff [--color auto|always|never] [--] <a> <b>

Either <a> or <b> may be `-`, meaning standard input — not both, since
stdin is read once. -- ends the options, so a file whose name begins with
`-` can follow it; a `-` after it is still standard input. An input larger
than 1 GiB is refused, since no saved screen is that large.

Parses two saved screens and prints what changed from <a> to <b>: the rows
that differ side by side, the size and cursor deltas, the style runs before
and after. On a terminal the changed cells are coloured (red in <a>, green
in <b>) unless --color never or NO_COLOR is set; in a pipe the rendering is
the plain one Screen::diff prints in a CI log. Every option that takes a
value also accepts it attached with `=`, as in --color=never.

Exit code 0 when the two are the same picture, 1 when they differ, 2 when
diff itself could not run — bad arguments, or a file that is missing or not
a saved screen.";

const RENDER_USAGE: &str = "\
usage: termlens render (--svg | --html | --ansi | --text | --json) [--out PATH] [--] <a>

Prints a saved screen as an SVG image, an HTML fragment, the screen in ANSI
colour for a terminal, the plain text format with its styles: block, or the
format-1 JSON document the crate's `serde` feature writes.

<a> may be `-`, meaning standard input. -- ends the options, so a file
whose name begins with `-` can follow it; a `-` after it is still standard
input. An input larger than 1 GiB is refused, since no saved screen is that
large. --out writes to PATH instead of stdout; `--out -` is standard
output, and a file named `-` is still `./-`.
--out creates nothing when the render fails — unlike a shell redirect,
which truncates the file before termlens runs. Every option that takes a
value also accepts it attached with `=`, as in --out=screen.svg.

Exit code 0: the screen was rendered. Exit code 2: render itself could not
run — bad arguments, a file that is missing or not a saved screen, or an
--out path that cannot be written.";

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
    // Best effort: a closed or full stderr loses the message, not the
    // exit code (#594).
    let _ = report(&format!("termlens: {message}"));
    ExitCode::from(2)
}

/// Write one line to stderr. `eprintln!` panics when the write fails — a
/// stderr piped into `true` or redirected to `/dev/full` — and a panic is
/// exit 101, a code this CLI does not promise (#594). The caller decides
/// whether a lost line matters.
fn report(line: &str) -> io::Result<()> {
    let mut stderr = io::stderr().lock();
    writeln!(stderr, "{line}").and_then(|()| stderr.flush())
}

/// The usage text on stderr and exit code 2, for a command line that is
/// missing what it needs.
fn usage_error(usage: &str) -> ExitCode {
    let _ = report(usage);
    ExitCode::from(2)
}

/// termlens's own options and operands, which must be UTF-8. Only the
/// program `inspect` runs and its arguments may be anything the OS allows,
/// and those never pass through here (#594).
fn utf8(arg: OsString) -> Result<String, String> {
    arg.into_string()
        .map_err(|arg| format!("{arg:?} is not valid UTF-8; termlens's own arguments must be"))
}

/// The one-line string every `--version` flag prints.
fn version() -> String {
    format!("termlens {}\n", env!("CARGO_PKG_VERSION"))
}

fn main() -> ExitCode {
    // `args_os`, not `args`, which panics on an argument that is not UTF-8
    // (#594): `inspect` passes the program's arguments through untouched,
    // and everything else is checked and refused with exit code 2.
    let mut args = std::env::args_os().skip(1);
    let Some(command) = args.next() else {
        return usage_error(USAGE);
    };
    let command = match utf8(command) {
        Ok(command) => command,
        Err(e) => return fail(&e),
    };
    let rest: Vec<OsString> = args.collect();
    if command == "inspect" {
        return inspect(rest);
    }
    let rest = match rest.into_iter().map(utf8).collect::<Result<Vec<_>, _>>() {
        Ok(rest) => rest,
        Err(e) => return fail(&e),
    };
    match command.as_str() {
        "-h" | "--help" => print(&format!("{USAGE}\n")),
        "--version" => print(&version()),
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
    read(open(path)?)
}

/// An operand that has been opened and measured and not yet read: the
/// first half of [`load`]. The halves are apart so that `diff` can learn
/// that one operand will never be read before it waits on the other (#715).
enum Source<'a> {
    /// `-`: standard input, which has nothing to open.
    Stdin,
    /// A file, with its length when it is a regular one. A pipe or a
    /// device (`/dev/zero`) says 0 and is bounded by the read itself.
    File {
        path: &'a str,
        file: std::fs::File,
        len: usize,
    },
}

/// The refusal of an input past [`MAX_INPUT`], for an operand named `name`.
fn too_large(name: &str) -> String {
    format!(
        "{name}: larger than {} GiB, which no saved screen is",
        MAX_INPUT >> 30
    )
}

/// Open the operand at `path` and find what can be known of it without
/// reading it: that it is there, that it is not a directory, how long it is.
fn open(path: &str) -> Result<Source<'_>, String> {
    if path == STDIN {
        return Ok(Source::Stdin);
    }
    let file = std::fs::File::open(path).map_err(|e| format!("{path}: {e}"))?;
    // A regular file says how long it is, so one past the bound is refused
    // before a byte is read, and one inside it is read into a buffer of its
    // own size.
    let len = match file.metadata() {
        Ok(meta) if meta.is_file() => meta.len(),
        // A directory opens like a file on Unix and refuses its first read.
        // That read waits on nobody, so it is made here and the error is the
        // operating system's own, the one `render --text /` has always
        // printed. Where a directory can be read, it is no screen either way.
        Ok(meta) if meta.is_dir() => match (&file).read(&mut [0; 1]) {
            Ok(_) => 0,
            Err(e) => return Err(format!("{path}: {e}")),
        },
        _ => 0,
    };
    if len > MAX_INPUT as u64 {
        return Err(too_large(path));
    }
    Ok(Source::File {
        path,
        file,
        len: len as usize,
    })
}

/// Read an opened operand to its end and parse it: the second half of
/// [`load`].
fn read(source: Source<'_>) -> Result<Screen, String> {
    let (name, raw) = match source {
        Source::Stdin => (STDIN_NAME, read_bounded(io::stdin().lock(), 0, MAX_INPUT)),
        Source::File { path, file, len } => (path, read_bounded(file, len, MAX_INPUT)),
    };
    let Some(raw) = raw.map_err(|e| format!("{name}: {e}"))? else {
        return Err(too_large(name));
    };
    let text = decode(raw, MAX_INPUT).map_err(|e| match e {
        // The words `read_to_string` used, which this read replaced.
        NotText::Utf8 => format!("{name}: stream did not contain valid UTF-8"),
        NotText::Utf16 => format!("{name}: stream did not contain valid UTF-16"),
        NotText::TooLarge => format!(
            "{name}: larger than {} GiB as UTF-8, which no saved screen is",
            MAX_INPUT >> 30
        ),
        // As a refused reservation reads when the bytes are read.
        NotText::Memory => format!("{name}: {}", io::Error::from(io::ErrorKind::OutOfMemory)),
    })?;
    // A byte-order mark says how the file was encoded and is no part of the
    // screen. Left in, it made the header's first word `\u{feff}size:` and
    // hid the `{` that tells JSON from the text format (#716).
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    // A checkout on Windows may have given the file CRLF endings; the
    // formats are line-based and neither cares. Only copied when there is
    // one, since the input may be hundreds of megabytes.
    let text = if text.contains("\r\n") {
        Cow::Owned(text.replace("\r\n", "\n"))
    } else {
        Cow::Borrowed(text)
    };
    let body = strip_insta_header(&text).trim_start_matches('\n');
    if body.starts_with('{') {
        return serde_json::from_str(body)
            .map_err(|e| format!("{name}: not a termlens Screen: {e}"));
    }
    Screen::parse(strip_inspect_trailer(body)).map_err(|e| format!("{name}: {e}"))
}

/// Why an input's bytes are not text.
#[derive(Debug, PartialEq, Eq)]
enum NotText {
    /// Not UTF-8, with no UTF-16 byte-order mark in front.
    Utf8,
    /// A UTF-16 byte-order mark, then half a code unit or a lone surrogate.
    Utf16,
    /// UTF-16 inside the bound as bytes and past it as UTF-8.
    TooLarge,
    /// The allocator refused the room the text needs.
    Memory,
}

/// The text of an input's bytes: UTF-8, or UTF-16 behind its byte-order
/// mark, in either byte order (#716). Windows PowerShell 5.1 re-encodes
/// what a program prints, so `termlens inspect app > a.snap` there writes
/// UTF-16LE with a mark, and the CLI could not read back its own output
/// from the default Windows shell. The mark stays at the head of the text
/// for the caller to strip, as a UTF-8 file's own mark does.
///
/// Nothing is read as UTF-16 without the mark: `FF` is in no UTF-8 text,
/// so the mark cannot be mistaken, and anything else would be a guess.
///
/// UTF-16 grows by half when its characters are three bytes of UTF-8, so
/// the bound on the bytes read is not a bound on the text. The text is
/// measured before any room is reserved for it and refused past `limit`
/// ([`MAX_INPUT`] outside the tests), and the room is the measured size,
/// reserved fallibly: a refusal is an error to report, as in
/// [`read_bounded`].
fn decode(raw: Vec<u8>, limit: usize) -> Result<String, NotText> {
    let big_endian = match raw.get(..2) {
        Some([0xff, 0xfe]) => false,
        Some([0xfe, 0xff]) => true,
        _ => return String::from_utf8(raw).map_err(|_| NotText::Utf8),
    };
    if raw.len() % 2 != 0 {
        return Err(NotText::Utf16);
    }
    let units = || {
        raw.chunks_exact(2).map(|unit| {
            let unit = [unit[0], unit[1]];
            if big_endian {
                u16::from_be_bytes(unit)
            } else {
                u16::from_le_bytes(unit)
            }
        })
    };
    let mut len = 0;
    for decoded in char::decode_utf16(units()) {
        len += decoded.map_err(|_| NotText::Utf16)?.len_utf8();
        if len > limit {
            return Err(NotText::TooLarge);
        }
    }
    let mut text = String::new();
    text.try_reserve_exact(len).map_err(|_| NotText::Memory)?;
    // Every unit decoded in the pass above, so nothing is replaced here.
    text.extend(char::decode_utf16(units()).map(|c| c.unwrap_or(char::REPLACEMENT_CHARACTER)));
    Ok(text)
}

/// The dash operand: standard input where a screen is read, standard
/// output where one is written (`render --out -`, #469). One constant,
/// because it is one convention — a reader who meets `-` in either
/// position should not have to ask which stream is meant.
const STDIN: &str = "-";
/// What a diagnostic calls it, since `-: ...` reads as a stray flag.
const STDIN_NAME: &str = "<stdin>";

/// The most `diff` and `render` read of one input, in bytes (#653). Without
/// a bound, `render --text /dev/zero` or `yes | render --text -` grew until
/// the allocator or the OOM killer stopped it.
///
/// A saved screen has a ceiling of its own — at most 1000x1000 cells since
/// #583 — but the bound has to sit above the largest file termlens itself
/// writes, or a release would refuse its own output: a 1000x1000 screen with
/// every cell differently styled and carrying combining marks is about
/// 80 MB in the text format and about 600 MB as the pretty-printed JSON
/// `render --json` writes. 1 GiB clears that with room for a JSON writer
/// that indents more.
const MAX_INPUT: usize = 1 << 30;

/// Read all of `reader`, or `None` once it holds more than `limit` bytes
/// ([`MAX_INPUT`] outside the tests). `hint` is the expected length (a
/// regular file's size, else 0).
///
/// The buffer never grows past `limit + 1` bytes: each round reads at
/// most what the buffer already has room for, and the room is doubled up to
/// that cap, so the refusal costs the bound and not the next power of two a
/// plain `read_to_end` would have reserved for the extra byte. A reservation
/// the allocator refuses is an error to report, not an abort.
fn read_bounded(mut reader: impl Read, hint: usize, limit: usize) -> io::Result<Option<Vec<u8>>> {
    let cap = limit + 1;
    let mut buf: Vec<u8> = Vec::new();
    // One more than the hint, so a file read in full is seen to end
    // without growing the buffer for the probe.
    buf.try_reserve_exact(hint.saturating_add(1).max(8 * 1024).min(cap))?;
    loop {
        let room = buf.capacity() - buf.len();
        let read = (&mut reader).take(room as u64).read_to_end(&mut buf)?;
        if buf.len() > limit {
            return Ok(None);
        }
        if read < room {
            return Ok(Some(buf));
        }
        let grown = buf.capacity().saturating_mul(2).min(cap);
        buf.try_reserve_exact(grown - buf.len())?;
    }
}

/// How long `inspect` gives the reap to land after the wait ends, so the
/// trailer says exited when the program did exit. Only ever paid in full by
/// a child that is genuinely still running, and only after the wait itself
/// has ended.
///
/// On Unix the exit closes the terminal, the EOF ends the wait, and the
/// kernel can report that close a scheduling hair before the child's status
/// is collectable — the stress run caught the gap (#374, iteration 3 of 25
/// on a loaded 16-thread runner). 500 ms, the width the library gives its
/// own post-reap drain, covers it.
///
/// Windows needs longer (#501). ConPTY never reports the terminal closing,
/// so a program that has just exited ends the wait by the silence window,
/// not an EOF, and the reap then has to cover everything between the
/// program's last byte and its exit status: an MSYS `sh` under load took
/// more than the 300 + 500 ms that left it, and was reported still running.
const REAP_GRACE: Duration = if cfg!(windows) {
    Duration::from_secs(2)
} else {
    Duration::from_millis(500)
};

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
            // The end of the options (#650), as for `inspect`: everything
            // after it is an operand, so a file whose name begins with `-`
            // can be named. A `-` after it is still standard input.
            "--" => {
                files.extend(args.by_ref());
                break;
            }
            other if other.starts_with('-') && other != STDIN => {
                return fail(&format!(
                    "unknown option {other:?} (try `termlens diff --help`)"
                ));
            }
            _ => files.push(arg),
        }
    }
    let [a, b] = files.as_slice() else {
        return usage_error(DIFF_USAGE);
    };
    // Standard input can be consumed once, so it can be one operand and not
    // both — said plainly here rather than left to look like an empty second
    // screen further down (#317).
    if a.as_str() == STDIN && b.as_str() == STDIN {
        return fail("only one of the two screens can be `-`: stdin is read once");
    }
    // Left first, so `diff a.snap -` and `diff - b.snap` both read the pipe
    // at the point the argument order says they do — and a left operand
    // that cannot be read is reported before the right one is touched, so
    // `diff missing.snap -` does not sit waiting for a stdin it will never
    // use (#651).
    //
    // The mirror takes one step out of that order (#715). With `-` on the
    // left, the right operand is opened and measured before standard input
    // is read, so `diff - missing.snap` does not wait either; the two are
    // then read in argument order all the same.
    let right = if a.as_str() == STDIN {
        match open(b) {
            Ok(source) => Some(source),
            Err(e) => return fail(&e),
        }
    } else {
        None
    };
    let before = match load(a) {
        Ok(screen) => screen,
        Err(e) => return fail(&e),
    };
    let after = match right.map_or_else(|| load(b), read) {
        Ok(screen) => screen,
        Err(e) => return fail(&e),
    };
    let diff = before.diff(&after);
    let rendered = if color.enabled() {
        colored(&before, &after, &diff)
    } else {
        diff.to_string()
    };
    // Exactly one newline at the end, whichever rendering this is: the
    // plain one already ends its last line, "no difference" and the
    // coloured one do not. Adding one regardless left a blank line after
    // every plain diff, in a pipe and in the report action's summary (#652).
    let code = if rendered.ends_with('\n') {
        print(&rendered)
    } else {
        print(&format!("{rendered}\n"))
    };
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
    // Collected once: asking the diff about every painted cell made a
    // full-screen change quadratic (#595).
    let changed: HashSet<(u16, u16)> = diff.cells().map(|(row, col, _, _)| (row, col)).collect();
    // The overlap's columns, as the plain rendering clips its rows to them
    // (#649): painting each screen across its own width printed the wider
    // side whole, past the note saying the rest is clipped.
    let cols = before.cols().min(after.cols());
    for row in changed_rows {
        // The row's cells in the overlap, each as `(text, changed)`. A wide
        // character whose second column falls past the overlap does not fit
        // in it and is one blank, as in the plain rendering.
        let cells = |screen: &Screen| -> Vec<(String, bool)> {
            let mut cells = Vec::new();
            for col in 0..cols {
                let Some(cell) = screen.cell(row, col) else {
                    continue;
                };
                if cell.is_wide_continuation() {
                    continue;
                }
                let text = if cell.contents().is_empty() || (cell.is_wide() && col + 1 >= cols) {
                    " "
                } else {
                    cell.contents()
                };
                cells.push((text.to_owned(), changed.contains(&(row, col))));
            }
            cells
        };
        let paint = |cells: &[(String, bool)], sgr: &str| -> String {
            cells
                .iter()
                .map(|(text, changed)| {
                    if *changed {
                        format!("\x1b[{sgr}m{text}\x1b[0m")
                    } else {
                        text.clone()
                    }
                })
                .collect()
        };
        // The before side keeps the overlap's full width so the separator
        // stays in one column down the listing. The after side is trimmed
        // like a row, by cell: a trailing blank is trimmed painted or not,
        // since a coloured blank shows nothing, and trimming the painted
        // string instead left its `\x1b[32m \x1b[0m` runs behind.
        let mut after_cells = cells(after);
        while after_cells.last().is_some_and(|(text, _)| text == " ") {
            after_cells.pop();
        }
        out.push_str(&format!(
            "\n{row:>3} │{}│{}",
            paint(&cells(before), "31"),
            paint(&after_cells, "32")
        ));
    }
    // The trailer: the unchanged-row count and the style runs, as printed.
    for line in lines.filter(|l| l.starts_with('…') || l.starts_with("styles:")) {
        out.push('\n');
        out.push_str(line);
    }
    out
}

// ---------------------------------------------------------------------- render

/// A contradictory `render` request: the reason on one line, then the usage,
/// and exit 2 — what a second operand already gets, with the conflict named
/// so the reader does not have to find it in their own command line.
/// Returned before anything is opened, so `--out` creates no file.
fn render_usage_error(reason: &str) -> ExitCode {
    let _ = report(&format!("termlens: {reason}\n\n{RENDER_USAGE}"));
    ExitCode::from(2)
}

fn render(args: &[String]) -> ExitCode {
    let mut format: Option<&str> = None;
    let mut files = Vec::new();
    let mut out_path: Option<&str> = None;
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => return print(&format!("{RENDER_USAGE}\n")),
            "--version" => return print(&version()),
            // One format and one `--out`, for the reason a second operand is
            // refused below (#364): the last one winning is silent, and in a
            // script the two usually come from two variables, so the loser
            // is the one somebody meant (#450). A repeat of the *same* flag
            // is refused too — it is harmless on its own, but it is the same
            // shape as the mistake, and one rule is easier to state.
            "--svg" | "--html" | "--ansi" | "--text" | "--json" => {
                if let Some(first) = format {
                    return render_usage_error(&if first == arg {
                        format!("{arg} is given twice; render writes one format")
                    } else {
                        format!("{first} and {arg} are two formats; render writes one")
                    });
                }
                format = Some(arg.as_str());
            }
            "--out" => match args.next() {
                // The same refusal as the `--out=` spelling below (#451):
                // an empty PATH is a missing one, not a file named "" (#594).
                Some(path) if path.is_empty() => return fail("--out needs a PATH argument"),
                Some(path) => {
                    if out_path.is_some() {
                        return render_usage_error("--out is given twice; render writes one file");
                    }
                    out_path = Some(path.as_str());
                }
                None => return fail("--out needs a PATH argument"),
            },
            other if other.starts_with("--out=") => {
                let path = &other["--out=".len()..];
                if path.is_empty() {
                    return fail("--out needs a PATH argument");
                }
                if out_path.is_some() {
                    return render_usage_error("--out is given twice; render writes one file");
                }
                out_path = Some(path);
            }
            // The end of the options (#650); see `diff`.
            "--" => {
                files.extend(args.by_ref());
                break;
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
        return usage_error(RENDER_USAGE);
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
    args: &mut impl Iterator<Item = OsString>,
    flag: &str,
    kind: &str,
    example: &str,
    parse: impl Fn(&str) -> Option<T>,
) -> Result<T, String> {
    let Some(raw) = args.next() else {
        return Err(format!("{flag} needs a {kind} argument"));
    };
    let raw = utf8(raw)?;
    parse(&raw).ok_or_else(|| format!("bad {flag} {raw:?}, expected e.g. {example}"))
}

/// The `--timeout` a deadline can be built from. `Instant + Duration`
/// panics on overflow, so a value the clock cannot add — `u64::MAX` seconds
/// — is refused here, naming the flag, rather than panicking after the
/// program was spawned (#594). The hour of headroom covers the grace
/// periods added on top of it later.
fn timeout_secs(spec: &str) -> Option<Duration> {
    let timeout = Duration::from_secs(spec.parse().ok()?);
    Instant::now().checked_add(timeout.checked_add(Duration::from_secs(3600))?)?;
    Some(timeout)
}

/// Whether an argument is an option: it begins with `-` and is not `-`.
fn is_option(arg: &OsStr) -> bool {
    arg.as_encoded_bytes().starts_with(b"-") && arg != STDIN
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

fn inspect(args: Vec<OsString>) -> ExitCode {
    // The library writes every screen-carrying error to
    // `TERMLENS_ARTIFACT_DIR`, which is right for a failing test. Here a
    // program that exits (`Eof`) or is still running at the deadline
    // (`Timeout`) is a run that ended normally, so the variable is taken
    // out of this process's environment before anything reads it — no
    // thread exists yet — and handed to the program itself when it
    // inherits the environment (#596).
    let artifact_dir = std::env::var_os("TERMLENS_ARTIFACT_DIR");
    std::env::remove_var("TERMLENS_ARTIFACT_DIR");
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
    while args.peek().is_some_and(|a| is_option(a)) {
        let flag = match utf8(args.next().unwrap_or_default()) {
            Ok(flag) => flag,
            Err(e) => return fail(&e),
        };
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
        let mut args = inline.map(OsString::from).into_iter().chain(args.by_ref());
        let parsed = match name {
            "-h" | "--help" => return print(&format!("{INSPECT_USAGE}\n")),
            "--version" => return print(&version()),
            "--" => break,
            "--size" => take(&mut args, "--size", "COLSxROWS", "120x40", |spec| {
                let (c, r) = spec.split_once('x')?;
                Some((c.parse().ok()?, r.parse().ok()?))
            })
            .map(|s| size = s),
            "--timeout" => {
                take(&mut args, "--timeout", "SECONDS", "30", timeout_secs).map(|t| timeout = t)
            }
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
                take(&mut args, "--cwd", "DIR", "/tmp", |s| Some(s.to_owned())).and_then(|dir| {
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
        return usage_error(INSPECT_USAGE);
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
    } else if let Some(dir) = artifact_dir {
        builder = builder.env("TERMLENS_ARTIFACT_DIR", dir);
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
    // The trailer is what inspect reports about the program; if stderr
    // cannot take it, say so with the exit code rather than a panic (#594).
    if report(&trailer).is_err() {
        return ExitCode::from(2);
    }
    code
}

#[cfg(test)]
mod tests {
    use super::{decode, read_bounded, NotText};

    /// `text` as UTF-16 behind its byte-order mark.
    fn utf16(text: &str, big_endian: bool) -> Vec<u8> {
        std::iter::once(0xfeff)
            .chain(text.encode_utf16())
            .flat_map(|unit| {
                if big_endian {
                    unit.to_be_bytes()
                } else {
                    unit.to_le_bytes()
                }
            })
            .collect()
    }

    /// UTF-16 is read behind its mark in either byte order, surrogate pairs
    /// included, and comes back with the mark for the caller to strip, as
    /// marked UTF-8 does. Without a mark the bytes are UTF-8 or nothing
    /// (#716).
    #[test]
    fn decode_reads_utf8_and_utf16_behind_its_mark() {
        let text = "size: 4x1  cursor: 0,0\n汉字 \u{1f600}";
        assert_eq!(
            decode(text.as_bytes().to_vec(), 1 << 20).as_deref(),
            Ok(text)
        );
        let marked = format!("\u{feff}{text}");
        assert_eq!(
            decode(marked.as_bytes().to_vec(), 1 << 20).as_deref(),
            Ok(marked.as_str())
        );
        for big_endian in [false, true] {
            assert_eq!(
                decode(utf16(text, big_endian), 1 << 20).as_deref(),
                Ok(marked.as_str()),
                "big-endian: {big_endian}"
            );
        }
        // The same units without the mark are not UTF-8, and are not guessed at.
        assert_eq!(
            decode(utf16(text, false)[2..].to_vec(), 1 << 20),
            Err(NotText::Utf8)
        );
        assert_eq!(decode(vec![b'a', 0xff], 1 << 20), Err(NotText::Utf8));
        assert_eq!(decode(Vec::new(), 1 << 20).as_deref(), Ok(""));
        assert_eq!(decode(vec![0xff, 0xfe], 1 << 20).as_deref(), Ok("\u{feff}"));
    }

    /// Half a code unit, or a surrogate without its other half, is not
    /// UTF-16, and is refused rather than read as U+FFFD (#716).
    #[test]
    fn decode_refuses_broken_utf16() {
        let mut odd = utf16("ok", false);
        odd.push(b'!');
        assert_eq!(decode(odd, 1 << 20), Err(NotText::Utf16));
        for lone in [[0x3d, 0xd8], [0x00, 0xde]] {
            let mut bytes = utf16("ok", false);
            bytes.extend(lone);
            bytes.extend(utf16("ok", false).split_off(2));
            assert_eq!(decode(bytes, 1 << 20), Err(NotText::Utf16), "{lone:02x?}");
        }
    }

    /// The bound is on the text as UTF-8, not only on the bytes read: ten
    /// Han characters are 22 bytes of UTF-16 with the mark and 33 of UTF-8,
    /// so a limit between the two refuses them, and one byte more reads
    /// them (#716).
    #[test]
    fn decode_bounds_utf16_by_its_utf8_length() {
        let bytes = utf16(&"汉".repeat(10), false);
        assert_eq!(bytes.len(), 22);
        assert_eq!(decode(bytes.clone(), 32), Err(NotText::TooLarge));
        let text = decode(bytes, 33).expect("exactly the limit is read");
        assert_eq!(text.len(), 33);
        // The room is what the text measured, not the half again that the
        // bytes could have come to: 18 bytes of ASCII need 11, not 27.
        let ascii = decode(utf16("abcdefgh", false), 1 << 20).expect("ASCII");
        assert!(ascii.capacity() <= 11, "{}", ascii.capacity());
    }

    /// A reader that hands out at most `chunk` bytes per call, so the
    /// bounded read is exercised across many short reads, as a pipe gives.
    struct Trickle<'a> {
        data: &'a [u8],
        chunk: usize,
    }

    impl std::io::Read for Trickle<'_> {
        fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
            let n = self.data.len().min(self.chunk).min(out.len());
            out[..n].copy_from_slice(&self.data[..n]);
            self.data = &self.data[n..];
            Ok(n)
        }
    }

    /// The bound is inclusive, one byte past it is refused, and the buffer
    /// never reserves more than one byte past it on the way (#653).
    #[test]
    fn read_bounded_takes_the_limit_and_refuses_one_byte_more() {
        let limit = 100_000;
        let data = vec![b'x'; limit + 1];
        for chunk in [1, 7, 4096, limit + 1] {
            for hint in [0, limit, limit + 1, usize::MAX] {
                let whole = read_bounded(
                    Trickle {
                        data: &data[..limit],
                        chunk,
                    },
                    hint,
                    limit,
                )
                .expect("an in-memory read")
                .expect("exactly the limit is read");
                assert_eq!(whole.len(), limit);
                assert!(
                    whole.capacity() <= limit + 1,
                    "{} for {hint}",
                    whole.capacity()
                );
                let over = read_bounded(Trickle { data: &data, chunk }, hint, limit)
                    .expect("an in-memory read");
                assert!(over.is_none(), "{chunk}/{hint}");
            }
        }
        let empty = read_bounded(&b""[..], 0, limit).unwrap().unwrap();
        assert!(empty.is_empty());
        let small = read_bounded(&b"size"[..], 4, limit).unwrap().unwrap();
        assert_eq!(small, b"size");
    }
}
