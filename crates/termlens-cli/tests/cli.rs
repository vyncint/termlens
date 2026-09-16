//! The CLI driven the way its users' programs are: through a PTY, by
//! termlens (#255). `diff` and `render` read the saved screens under
//! `tests/data`; `inspect` runs a shell.

use termlens::{Color, Screen};

fn data(name: &str) -> String {
    format!("{}/tests/data/{name}", env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn diff_exits_one_and_paints_the_changed_cells_on_a_terminal() -> termlens::Result<()> {
    let mut t = termlens::bin!(
        "termlens",
        args(["diff", &data("before.snap"), &data("after.snap.new")])
    )?;
    let status = t.wait_exit()?;
    assert_eq!(status.code(), Some(1), "{}", t.screen());
    let s = t.screen();
    assert!(s.contains("size: 30x4"), "{s}");
    // Row 2 changed in one cell: `1` red on the left, `2` green on the right.
    let (row, col) = s.find("Counter: 1").expect("the before side");
    let one = s.cell(row, col + 9).unwrap();
    assert_eq!(one.contents(), "1");
    assert_eq!(one.style().fg, Color::Indexed(1), "{}", s.with_styles());
    let (row, col) = s.find("Counter: 2").expect("the after side");
    assert_eq!(s.cell(row, col + 9).unwrap().style().fg, Color::Indexed(2));
    // The style runs before → after, as the plain rendering prints them.
    assert!(
        s.contains("styles: 0: 0-4 fg=6 bold; 7-13 reverse → 0-4 fg=6 bold"),
        "{s}"
    );
    assert!(s.contains("2 rows unchanged"), "{s}");
    Ok(())
}

#[test]
fn diff_stays_plain_when_asked_and_exits_zero_on_the_same_picture() -> termlens::Result<()> {
    let mut t = termlens::bin!(
        "termlens",
        args([
            "diff",
            "--color",
            "never",
            &data("before.snap"),
            &data("after.snap.new")
        ])
    )?;
    assert_eq!(t.wait_exit()?.code(), Some(1));
    let s = t.screen();
    assert!(
        s.contains("^"),
        "the marker line stands in for colour:\n{s}"
    );
    assert!(
        s.find_by(|c| c.style().fg != Color::Default).is_none(),
        "{}",
        s.with_styles()
    );

    let mut t = termlens::bin!(
        "termlens",
        args(["diff", &data("before.snap"), &data("before.snap")])
    )?;
    assert_eq!(t.wait_exit()?.code(), Some(0));
    assert!(t.screen().contains("no difference"), "{}", t.screen());
    Ok(())
}

#[test]
fn render_writes_svg_html_ansi_and_text() -> termlens::Result<()> {
    for (flag, needle) in [("--svg", "<svg"), ("--html", "<pre"), ("--text", "styles:")] {
        let mut t = termlens::bin!("termlens", args(["render", flag, &data("before.snap")]))?;
        assert_eq!(t.wait_exit()?.code(), Some(0), "{flag}: {}", t.screen());
        assert!(t.screen().contains(needle), "{flag}:\n{}", t.screen());
    }
    // ANSI is the screen itself, styles and all, painted into our PTY.
    let mut t = termlens::bin!("termlens", args(["render", "--ansi", &data("before.snap")]))?;
    assert_eq!(t.wait_exit()?.code(), Some(0));
    let s = t.screen();
    let (row, col) = s.find("myapp").expect("the title");
    let title = s.cell(row, col).unwrap().style();
    assert!(
        title.bold && title.fg == Color::Indexed(6),
        "{}",
        s.with_styles()
    );
    let (row, col) = s.find("secret").expect("the field");
    assert!(
        s.cell(row, col).unwrap().style().conceal,
        "{}",
        s.with_styles()
    );
    Ok(())
}

#[test]
fn a_file_that_is_not_a_screen_exits_two_and_names_it() -> termlens::Result<()> {
    // Wide, so the path in the diagnostic is not wrapped across two rows.
    let mut t = termlens::bin!(
        "termlens",
        size(200, 10),
        args(["render", "--svg", &data("missing.snap")])
    )?;
    assert_eq!(t.wait_exit()?.code(), Some(2));
    assert!(
        t.screen().contains("termlens: ") && t.screen().contains("missing.snap"),
        "{}",
        t.screen()
    );

    let mut t = termlens::bin!(
        "termlens",
        args(["diff", &data("cli.rs"), &data("before.snap")])
    )?;
    // tests/data/cli.rs does not exist either; the source file next door is
    // the one that parses to nothing.
    assert_eq!(t.wait_exit()?.code(), Some(2));

    let mut t = termlens::bin!("termlens", args(["frobnicate"]))?;
    assert_eq!(t.wait_exit()?.code(), Some(2));
    assert!(t.screen().contains("unknown command"), "{}", t.screen());
    Ok(())
}

#[test]
fn the_text_a_wait_error_prints_is_a_saved_screen_too() -> termlens::Result<()> {
    // No header, no styles block: the block from a CI log.
    let path = std::env::temp_dir().join(format!("termlens-cli-{}.txt", std::process::id()));
    std::fs::write(&path, "size: 10x2  cursor: hidden\nhello\n")?;
    let mut t = termlens::bin!(
        "termlens",
        args(["render", "--text", path.to_str().unwrap()])
    )?;
    assert_eq!(t.wait_exit()?.code(), Some(0), "{}", t.screen());
    let s = t.screen();
    assert!(
        s.contains("size: 10x2  cursor: hidden") && s.contains("(none)"),
        "{s}"
    );
    let _ = std::fs::remove_file(&path);
    Ok(())
}

#[test]
fn a_crlf_checkout_of_a_snap_still_parses() -> termlens::Result<()> {
    // Git on Windows may hand the file over with CRLF endings; the header
    // and the grid are line-based and must not care.
    // (Normalized first: on Windows the checkout may already be CRLF.)
    let crlf = std::fs::read_to_string(data("before.snap"))?
        .replace("\r\n", "\n")
        .replace('\n', "\r\n");
    let path = std::env::temp_dir().join(format!("termlens-cli-crlf-{}.snap", std::process::id()));
    std::fs::write(&path, crlf)?;
    let mut t = termlens::bin!(
        "termlens",
        args(["render", "--text", path.to_str().unwrap()])
    )?;
    assert_eq!(t.wait_exit()?.code(), Some(0), "{}", t.screen());
    assert!(t.screen().contains("myapp  > Alpha"), "{}", t.screen());
    let _ = std::fs::remove_file(&path);
    Ok(())
}

#[test]
#[cfg_attr(windows, ignore = "the program under inspection is a POSIX shell")]
fn inspect_prints_the_screen_and_the_exit_trailer() -> termlens::Result<()> {
    let path = std::env::var("PATH").unwrap_or_default();
    let mut t = termlens::bin!(
        "termlens",
        env("PATH", &path),
        args(["inspect", "--size", "20x3", "sh", "-c", "printf 'hi there'"])
    )?;
    assert_eq!(t.wait_exit()?.code(), Some(0), "{}", t.screen());
    let s = t.screen();
    assert!(s.contains("size: 20x3") && s.contains("hi there"), "{s}");
    assert!(s.contains("--- exited: exit code 0 ---"), "{s}");

    let mut t = termlens::bin!(
        "termlens",
        env("PATH", &path),
        args(["inspect", "--size", "12", "sh"])
    )?;
    assert_eq!(t.wait_exit()?.code(), Some(2));
    assert!(
        t.screen().contains("expected e.g. 120x40"),
        "{}",
        t.screen()
    );
    Ok(())
}

#[test]
fn help_and_version() -> termlens::Result<()> {
    let mut t = termlens::bin!("termlens", args(["--help"]))?;
    assert_eq!(t.wait_exit()?.code(), Some(0));
    assert!(
        t.screen().contains("usage: termlens <command>"),
        "{}",
        t.screen()
    );
    let mut t = termlens::bin!("termlens", args(["--version"]))?;
    assert_eq!(t.wait_exit()?.code(), Some(0));
    assert!(
        t.screen()
            .contains(concat!("termlens ", env!("CARGO_PKG_VERSION"))),
        "{}",
        t.screen()
    );
    let mut t = termlens::bin!("termlens")?;
    assert_eq!(t.wait_exit()?.code(), Some(2));
    // Unused otherwise, but the round trip through the library is the point.
    let _: Screen = Screen::parse("size: 1x1  cursor: 0,0\n")?;
    Ok(())
}

#[test]
fn subcommand_version_prints_same_string_as_top_level() -> termlens::Result<()> {
    // The top-level version string is the reference.
    let mut top = termlens::bin!("termlens", args(["--version"]))?;
    assert_eq!(top.wait_exit()?.code(), Some(0));
    let expected = concat!("termlens ", env!("CARGO_PKG_VERSION"));

    for subcommand in ["diff", "render", "inspect"] {
        let mut t = termlens::bin!("termlens", args([subcommand, "--version"]))?;
        assert_eq!(
            t.wait_exit()?.code(),
            Some(0),
            "`termlens {subcommand} --version` exited non-zero"
        );
        assert!(
            t.screen().contains(expected),
            "`termlens {subcommand} --version` output:\n{}",
            t.screen()
        );
    }
    Ok(())
}

/// `inspect … > file` is a saved screen (#340): the screen alone goes to
/// stdout and the trailer to stderr, so the file a redirect captures is
/// what `render` and `diff` read. Driven without a PTY here on purpose —
/// through one, both streams land on the same screen and the split is
/// invisible.
#[test]
#[cfg_attr(windows, ignore = "the program under inspection is a POSIX shell")]
fn inspect_stdout_is_a_saved_screen_and_the_trailer_is_on_stderr() -> termlens::Result<()> {
    use std::process::Command;
    let bin = env!("CARGO_BIN_EXE_termlens");
    let out = Command::new(bin)
        .args(["inspect", "--size", "20x3", "sh", "-c", "printf 'hi there'"])
        .output()?;
    assert!(out.status.success(), "{out:?}");
    let stdout = String::from_utf8(out.stdout).expect("utf-8");
    let stderr = String::from_utf8(out.stderr).expect("utf-8");
    assert!(stdout.starts_with("size: 20x3  cursor: "), "{stdout:?}");
    assert!(stdout.contains("hi there"), "{stdout:?}");
    assert!(!stdout.contains("---"), "no trailer on stdout: {stdout:?}");
    assert_eq!(
        stderr.trim_end(),
        "--- exited: exit code 0 ---",
        "{stderr:?}"
    );
    // What the library reads from it is the screen inspect saw.
    let parsed = Screen::parse(&stdout)?;
    assert_eq!(parsed.size(), (20, 3));
    assert_eq!(parsed.find("hi there"), Some((0, 0)));

    // And the CLI reads its own output back: render, and diff against itself.
    let path =
        std::env::temp_dir().join(format!("termlens-cli-inspect-{}.txt", std::process::id()));
    std::fs::write(&path, &stdout)?;
    let file = path.to_str().unwrap();
    let render = Command::new(bin)
        .args(["render", "--text", file])
        .output()?;
    assert_eq!(render.status.code(), Some(0), "{render:?}");
    let diff = Command::new(bin).args(["diff", file, file]).output()?;
    assert_eq!(diff.status.code(), Some(0), "{diff:?}");

    // A file a 0.10 inspect saved — trailer on stdout — still reads.
    let old = path.with_extension("old.txt");
    std::fs::write(&old, format!("{stdout}--- exited: exit code 0 ---\n"))?;
    let render = Command::new(bin)
        .args(["render", "--text", old.to_str().unwrap()])
        .output()?;
    assert_eq!(render.status.code(), Some(0), "{render:?}");
    let diff = Command::new(bin)
        .args(["diff", file, old.to_str().unwrap()])
        .output()?;
    assert_eq!(
        diff.status.code(),
        Some(0),
        "the trailer is not a row: {diff:?}"
    );
    // …while a grid row that merely starts with `---` is content.
    let dashes = path.with_extension("dashes.txt");
    std::fs::write(&dashes, "size: 20x2  cursor: 0,0\n--- not a trailer\n")?;
    let render = Command::new(bin)
        .args(["render", "--text", dashes.to_str().unwrap()])
        .output()?;
    assert_eq!(render.status.code(), Some(0), "{render:?}");
    assert!(
        String::from_utf8_lossy(&render.stdout).contains("--- not a trailer"),
        "{render:?}"
    );
    for p in [path, old, dashes] {
        let _ = std::fs::remove_file(p);
    }
    Ok(())
}

/// The compatibility corpus (#327): every saved screen a published release
/// wrote — text and JSON — is a file this CLI reads. The library's
/// `tests/compat.rs` holds the round trips; this is the fourth check it
/// names, from the tool's side.
#[test]
fn every_corpus_file_renders() -> termlens::Result<()> {
    use std::process::Command;
    let bin = env!("CARGO_BIN_EXE_termlens");
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../termlens/tests/compat");
    let mut files: Vec<std::path::PathBuf> = std::fs::read_dir(&root)?
        .map(|entry| entry.map(|e| e.path()))
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .filter(|p| p.is_dir())
        .flat_map(|dir| std::fs::read_dir(dir).expect("a version directory"))
        .map(|entry| entry.expect("a corpus file").path())
        .filter(|p| p.extension().is_some_and(|e| e == "txt" || e == "json"))
        .collect();
    files.sort();
    assert!(
        files.len() >= 12,
        "the 0.10.1 corpus alone is twelve files, found {}",
        files.len()
    );
    for file in files {
        let out = Command::new(bin)
            .args(["render", "--text", file.to_str().unwrap()])
            .output()?;
        assert_eq!(
            out.status.code(),
            Some(0),
            "{}: {}",
            file.display(),
            String::from_utf8_lossy(&out.stderr)
        );
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(text.starts_with("size: "), "{}: {text}", file.display());
    }
    Ok(())
}

/// Run the CLI with `input` on standard input, and without a PTY: a pipe is
/// the whole point of `-`, and through a PTY there is no EOF to read to.
fn with_stdin(args: &[&str], input: &str) -> std::process::Output {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let mut child = Command::new(env!("CARGO_BIN_EXE_termlens"))
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn termlens");
    // A broken pipe here is a *result*, not a failure. `diff - -` refuses its
    // arguments before reading anything, so the child can exit and close the
    // pipe before this write lands — the faster the refusal, the likelier it
    // is. The stress workflow found it on both Linux shards while it never
    // reproduced locally, because on an idle machine the bytes reach the
    // pipe buffer first. The child's exit code and stderr are what the
    // callers assert; whether it read the input is the child's business.
    let mut pipe = child.stdin.take().expect("a stdin pipe");
    match pipe.write_all(input.as_bytes()) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => {}
        Err(e) => panic!("write to the child's stdin: {e}"),
    }
    drop(pipe);
    child.wait_with_output().expect("the child's output")
}

/// A saved screen most often arrives on a pipe — out of a CI log, or from
/// the tool that made it a moment earlier — and `-` is the convention for
/// that (#317).
#[test]
fn render_and_diff_read_a_screen_from_stdin() -> termlens::Result<()> {
    let before = std::fs::read_to_string(data("before.snap"))?;
    let after = std::fs::read_to_string(data("after.snap.new"))?;

    // `render --text -` is `render --text before.snap`, byte for byte.
    let piped = with_stdin(&["render", "--text", "-"], &before);
    assert_eq!(piped.status.code(), Some(0), "{piped:?}");
    let from_file = std::process::Command::new(env!("CARGO_BIN_EXE_termlens"))
        .args(["render", "--text", &data("before.snap")])
        .output()?;
    assert_eq!(
        piped.stdout, from_file.stdout,
        "the pipe and the path are the same screen"
    );

    // `diff` takes it as either operand, and the direction is the argument
    // order, not which one came off the pipe.
    let right = with_stdin(
        &["diff", "--color", "never", &data("before.snap"), "-"],
        &after,
    );
    assert_eq!(right.status.code(), Some(1), "{right:?}");
    let left = with_stdin(
        &["diff", "--color", "never", "-", &data("after.snap.new")],
        &before,
    );
    assert_eq!(left.status.code(), Some(1), "{left:?}");
    assert_eq!(right.stdout, left.stdout, "same two screens, same diff");
    assert!(
        String::from_utf8_lossy(&right.stdout).contains("Counter: 1"),
        "{}",
        String::from_utf8_lossy(&right.stdout)
    );

    // And a pipe that is not a screen is named as stdin, not as a file `-`.
    let junk = with_stdin(&["render", "--text", "-"], "not a saved screen\n");
    assert_eq!(junk.status.code(), Some(2), "{junk:?}");
    let stderr = String::from_utf8_lossy(&junk.stderr);
    assert!(stderr.contains("<stdin>"), "{stderr}");
    Ok(())
}

/// Standard input is read once, so it can be one of `diff`'s operands and
/// not both — said plainly rather than left to look like an empty screen.
#[test]
fn diff_refuses_two_stdin_operands() -> termlens::Result<()> {
    let out = with_stdin(&["diff", "-", "-"], "size: 1x1  cursor: 0,0\n\n");
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("only one of the two screens can be `-`"),
        "one line, naming the reason: {stderr}"
    );
    assert_eq!(stderr.lines().count(), 1, "{stderr}");
    Ok(())
}

/// `render --out` exists because every caller was writing `> file.svg`, and
/// a redirect truncates the file before termlens runs — so a failing render
/// leaves an empty file where a bug report expected an image (#313).
#[test]
fn render_out_writes_the_file_and_creates_none_when_the_render_fails() -> termlens::Result<()> {
    use std::process::Command;
    let bin = env!("CARGO_BIN_EXE_termlens");
    let dir = std::env::temp_dir().join(format!("termlens-render-out-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;

    let written = dir.join("screen.svg");
    let out = Command::new(bin)
        .args([
            "render",
            "--svg",
            "--out",
            written.to_str().expect("utf-8 path"),
            &data("before.snap"),
        ])
        .output()?;
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert!(out.stdout.is_empty(), "--out means not stdout: {out:?}");
    let to_stdout = Command::new(bin)
        .args(["render", "--svg", &data("before.snap")])
        .output()?;
    assert_eq!(
        std::fs::read(&written)?,
        to_stdout.stdout,
        "the same bytes stdout would have carried"
    );

    // An unreadable input: exit 2, and nothing where the file would go.
    let junk = dir.join("junk.txt");
    std::fs::write(&junk, "not a saved screen\n")?;
    let missing = dir.join("never-written.svg");
    let out = Command::new(bin)
        .args([
            "render",
            "--svg",
            "--out",
            missing.to_str().expect("utf-8 path"),
            junk.to_str().expect("utf-8 path"),
        ])
        .output()?;
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    assert!(
        !missing.exists(),
        "a failing render left a file behind: {}",
        missing.display()
    );

    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

/// The working directory is part of "how the program is normally run", and
/// `TerminalBuilder::current_dir` had no way through to the command line
/// (#312).
#[test]
#[cfg_attr(windows, ignore = "the program under inspection is a POSIX shell")]
fn inspect_runs_the_program_where_cwd_says() -> termlens::Result<()> {
    let path = std::env::var("PATH").unwrap_or_default();
    // A directory made here, so the assertion is not about /tmp's own name
    // on a platform that symlinks it (macOS: /tmp -> /private/tmp).
    let dir = std::env::temp_dir().join(format!("termlens-cwd-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    let real = std::fs::canonicalize(&dir)?;
    // Wide enough that the path is one row. At 60 columns this went red on
    // the macOS leg and nowhere else: the temp directory there is
    // `/private/var/folders/36/tjdph2t965j8snz9_vkdnw0r0000gn/T/…`, which
    // wraps, and a wrapped needle is a test about the width rather than
    // about --cwd.
    let mut t = termlens::bin!(
        "termlens",
        env("PATH", &path),
        args([
            "inspect",
            "--size",
            "200x3",
            "--cwd",
            dir.to_str().expect("utf-8 path"),
            "sh",
            "-c",
            "pwd"
        ])
    )?;
    assert_eq!(t.wait_exit()?.code(), Some(0), "{}", t.screen());
    let s = t.screen();
    assert!(
        s.contains(real.to_str().expect("utf-8 path")),
        "the program ran in {}: {s}",
        real.display()
    );
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

/// A directory that is not there is a one-line diagnostic naming the flag,
/// and exit 2 — not a panic, and not the builder's message about a
/// `current_dir` the caller never wrote.
#[test]
fn inspect_refuses_a_cwd_that_is_not_a_directory() -> termlens::Result<()> {
    use std::process::Command;
    let missing = std::env::temp_dir().join("termlens-no-such-directory-here");
    let _ = std::fs::remove_dir_all(&missing);
    let out = Command::new(env!("CARGO_BIN_EXE_termlens"))
        .args([
            "inspect",
            "--cwd",
            missing.to_str().expect("utf-8 path"),
            "true",
        ])
        .output()?;
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(stderr.lines().count(), 1, "one line: {stderr}");
    assert!(stderr.contains("--cwd"), "it names the flag: {stderr}");
    assert!(
        stderr.contains("not an existing directory"),
        "and what was wrong with it: {stderr}"
    );
    Ok(())
}
