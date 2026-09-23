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

/// A comparison of two differently sized screens says so in colour mode
/// too (#365): the plain rendering's second header line, the one naming
/// the overlap, used to be dropped by `colored()`'s prefix filter.
#[test]
fn diff_color_always_keeps_the_overlap_note_and_paints_the_rows() -> termlens::Result<()> {
    let mut t = termlens::bin!(
        "termlens",
        args([
            "diff",
            "--color",
            "always",
            &data("overlap-before.snap"),
            &data("overlap-after.snap")
        ])
    )?;
    assert_eq!(t.wait_exit()?.code(), Some(1), "{}", t.screen());
    let s = t.screen();
    assert!(
        s.contains("compared over the 4x2 overlap; the rest is clipped"),
        "the note saying the comparison was partial:\n{s}"
    );
    // The rows are still painted: red for what the before screen showed,
    // green for what the after one shows.
    let (row, col) = s.find("abXX").expect("the after side");
    let two = s.cell(row, col + 2).expect("a changed cell");
    assert_eq!(two.contents(), "X");
    assert_eq!(two.style().fg, Color::Indexed(2), "{}", s.with_styles());
    // The before side's changed cells are two blanks here, and ConPTY does
    // not carry a foreground-only run written around isolated spaces across
    // the line boundary: on Windows the blanks arrive uncoloured and the
    // next row's red run is two cells wider (seen in the styles of run
    // 35188093990). The exact attribution is pinned where the stream
    // survives, and the note above is asserted everywhere.
    #[cfg(not(windows))]
    {
        let (row, col) = s.find("│ab  │").expect("the before side");
        let blank = s.cell(row, col + 4).expect("a painted blank");
        assert_eq!(blank.style().fg, Color::Indexed(1), "{}", s.with_styles());
    }

    // An empty diff is still the one line, with no colour to go with it.
    let mut t = termlens::bin!(
        "termlens",
        args([
            "diff",
            "--color",
            "always",
            &data("overlap-before.snap"),
            &data("overlap-before.snap")
        ])
    )?;
    assert_eq!(t.wait_exit()?.code(), Some(0));
    let s = t.screen();
    assert!(s.contains("no difference"), "{s}");
    assert!(
        s.find_by(|c| c.style().fg != Color::Default).is_none(),
        "{}",
        s.with_styles()
    );
    Ok(())
}

/// `--color` with no WHEN used to print the raw Option — `None` when the
/// flag ended the line, `Some("before.snap")` when it quietly ate the
/// operand after it — internals that name neither the flag's requirement
/// nor the mistake (#452).
#[test]
fn diff_names_a_missing_or_unknown_color_value() -> termlens::Result<()> {
    // The operand after the flag is taken as the value and refused as one
    // quoted word, exactly as `--color=pink` always was.
    let out = with_stdin(
        &[
            "diff",
            "--color",
            &data("before.snap"),
            &data("after.snap.new"),
        ],
        "",
    );
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    // The operand is the value, quoted and alone — `data()`'s absolute path
    // Debug-escapes its backslashes on Windows, so the shape is pinned at
    // the front and the eaten operand at the back, `Some(` nowhere.
    assert!(
        stderr.starts_with(r#"termlens: --color takes auto, always or never, got ""#)
            && stderr.contains("before.snap")
            && !stderr.contains("Some("),
        "the operand named as the value it was taken for: {stderr}"
    );

    // The flag ending the line has no value to quote, so it says it needs
    // one — the shape every other flag's missing argument takes.
    let out = with_stdin(
        &[
            "diff",
            &data("before.snap"),
            &data("after.snap.new"),
            "--color",
        ],
        "",
    );
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--color needs a WHEN argument"),
        "the missing value, not `None`: {stderr}"
    );

    // The `=` spelling keeps its diagnostic, from the list it now shares.
    let out = with_stdin(
        &[
            "diff",
            "--color=pink",
            &data("before.snap"),
            &data("after.snap.new"),
        ],
        "",
    );
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains(r#"--color takes auto, always or never, got "pink""#),
        "the offending word alone: {stderr}"
    );
    Ok(())
}

/// The three good WHENs, in both spellings of the flag (#452): `always`
/// paints even a pipe, `never` keeps it plain with the `^` marker line,
/// and `auto` is plain in a pipe — the same bytes as `never` there.
#[test]
fn diff_accepts_the_three_color_words_in_both_spellings() -> termlens::Result<()> {
    let before = data("before.snap");
    let after = data("after.snap.new");
    for word in ["auto", "always", "never"] {
        let inline = format!("--color={word}");
        // One flag, two spellings: `--color never` and `--color=never`
        // parse the same three words.
        for args in [
            vec!["diff", "--color", word, before.as_str(), after.as_str()],
            vec!["diff", inline.as_str(), before.as_str(), after.as_str()],
        ] {
            let out = with_stdin(&args, "");
            assert_eq!(out.status.code(), Some(1), "{word}: {out:?}");
            let stdout = String::from_utf8_lossy(&out.stdout);
            if word == "always" {
                assert!(stdout.contains('\x1b'), "painted: {stdout}");
            } else {
                assert!(stdout.contains('^'), "plain, marker line and all: {stdout}");
            }
        }
    }

    // In a pipe `auto` is `never`: same bytes, no escape to be found.
    let never = with_stdin(&["diff", "--color", "never", &before, &after], "");
    let auto = with_stdin(&["diff", "--color=auto", &before, &after], "");
    assert_eq!(never.stdout, auto.stdout, "a pipe is not a terminal");
    assert!(
        !auto.stdout.contains(&0x1b),
        "no ANSI in a pipe: {}",
        String::from_utf8_lossy(&auto.stdout)
    );
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

/// `render --json` writes what `render --text` reads (#373): the format-1
/// JSON document `docs/STABILITY.md` promises. The CLI could read the
/// format and not write it, so the two promised saved-screen formats
/// converted in one direction only.
#[test]
fn render_json_writes_the_document_render_text_reads_back() -> termlens::Result<()> {
    use std::process::Command;
    let bin = env!("CARGO_BIN_EXE_termlens");

    let json = Command::new(bin)
        .args(["render", "--json", &data("before.snap")])
        .output()?;
    assert_eq!(json.status.code(), Some(0), "{json:?}");
    let document = String::from_utf8(json.stdout).expect("utf-8 JSON");
    let value: serde_json::Value = serde_json::from_str(&document).expect("a JSON document");
    assert_eq!(value["format"], 1, "{document}");
    // The library reads its own document back, byte for byte.
    let screen: Screen = serde_json::from_str(&document).expect("a termlens Screen");
    assert_eq!(screen.size(), (30, 4));
    assert_eq!(
        document,
        format!(
            "{}\n",
            serde_json::to_string_pretty(&screen).expect("serialize")
        )
    );

    // text -> json -> text: the round trip through the two formats lands on
    // the bytes the text rendering prints.
    let from_text = Command::new(bin)
        .args(["render", "--text", &data("before.snap")])
        .output()?;
    assert_eq!(from_text.status.code(), Some(0), "{from_text:?}");
    let back = with_stdin(&["render", "--text", "-"], &document);
    assert_eq!(back.status.code(), Some(0), "{back:?}");
    assert_eq!(
        back.stdout, from_text.stdout,
        "json -> text is the same screen"
    );

    // --out carries exactly the bytes stdout did, and stdin is an input
    // like any path: `render --json -` is `render --json before.snap`.
    let dir = std::env::temp_dir().join(format!("termlens-render-json-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    let written = dir.join("screen.json");
    let out = Command::new(bin)
        .args([
            "render",
            "--json",
            "--out",
            written.to_str().expect("utf-8 path"),
            &data("before.snap"),
        ])
        .output()?;
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert!(out.stdout.is_empty(), "--out means not stdout: {out:?}");
    assert_eq!(std::fs::read(&written)?, document.as_bytes());
    let piped = with_stdin(&["render", "--json", "-"], &document);
    assert_eq!(piped.status.code(), Some(0), "{piped:?}");
    assert_eq!(piped.stdout, document.as_bytes(), "stdin is a path's equal");

    // A JSON file renders through --json unchanged — format 1 in, the same
    // document out.
    let again = Command::new(bin)
        .args(["render", "--json", written.to_str().expect("utf-8 path")])
        .output()?;
    assert_eq!(again.status.code(), Some(0), "{again:?}");
    assert_eq!(
        again.stdout,
        document.as_bytes(),
        "the JSON twin round-trips through the CLI"
    );

    let help = Command::new(bin).args(["render", "--help"]).output()?;
    let usage = String::from_utf8_lossy(&help.stdout);
    let first = usage.lines().next().expect("a usage line");
    assert!(
        first.contains("--json"),
        "the usage line lists --json: {first}"
    );

    let _ = std::fs::remove_dir_all(&dir);
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

/// A flag typed in place of a subcommand is an unknown option, not an
/// unknown command (#475). `-v` and `--verbose` are the two most likely
/// wrong guesses at this CLI, and both are flags. `nonesuch` stays a
/// command — `check-cli-contract.sh` pins the exit, this pins the word.
#[test]
fn a_top_level_flag_is_an_unknown_option_not_a_command() {
    use std::process::Command;
    let bin = env!("CARGO_BIN_EXE_termlens");
    for flag in ["--verbose", "-v"] {
        let out = Command::new(bin).arg(flag).output().expect("spawn");
        assert_eq!(out.status.code(), Some(2), "{flag}: {out:?}");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(
            stderr,
            format!("termlens: unknown option {flag:?} (try --help)\n"),
            "{flag}"
        );
    }

    let out = Command::new(bin).arg("nonesuch").output().expect("spawn");
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "termlens: unknown command \"nonesuch\" (try --help)\n"
    );
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

    // The `=` spelling reaches the same parse, so it keeps the same
    // diagnostic, naming the flag the user typed and not the whole token.
    let mut t = termlens::bin!(
        "termlens",
        env("PATH", &path),
        args(["inspect", "--size=12", "sh"])
    )?;
    assert_eq!(t.wait_exit()?.code(), Some(2));
    assert!(
        t.screen()
            .contains("bad --size \"12\", expected e.g. 120x40"),
        "{}",
        t.screen()
    );
    Ok(())
}

/// The `--flag=value` spelling beside the two-argument one, as `diff
/// --color=` and `render --out=` already have it (#366).
#[test]
#[cfg_attr(windows, ignore = "the program under inspection is a POSIX shell")]
fn inspect_accepts_the_flag_equals_value_spelling() -> termlens::Result<()> {
    let path = std::env::var("PATH").unwrap_or_default();
    let mut t = termlens::bin!(
        "termlens",
        env("PATH", &path),
        args([
            "inspect",
            "--size=60x3",
            "--timeout=5",
            "--idle=100",
            "sh",
            "-c",
            "printf 'hi there'"
        ])
    )?;
    assert_eq!(t.wait_exit()?.code(), Some(0), "{}", t.screen());
    let s = t.screen();
    assert!(s.contains("size: 60x3") && s.contains("hi there"), "{s}");
    Ok(())
}

/// `--flag=value` is for the flags that take a value; a value attached to a
/// flag that takes none is refused with the whole token named, as `render`
/// refuses `--svg=1`. Splitting every long flag would apply `--inherit-env`
/// and drop the `nonsense`, which is the silent-wrong-answer shape the
/// spelling was added to avoid (#366).
#[test]
fn inspect_refuses_a_value_on_a_flag_that_takes_none() -> termlens::Result<()> {
    let mut t = termlens::bin!(
        "termlens",
        args(["inspect", "--inherit-env=nonsense", "/bin/echo", "hi"])
    )?;
    let status = t.wait_exit()?;
    assert_eq!(status.code(), Some(2), "{}", t.screen());
    let s = t.screen();
    assert!(
        s.contains(r#"unknown option "--inherit-env=nonsense""#),
        "the diagnostic names the whole token: {s}"
    );
    Ok(())
}

/// `--env=KEY=VALUE` splits on the *first* `=` only, and `--cwd=PATH` runs
/// the program where it says, exactly as their two-argument forms do (#366).
#[test]
#[cfg_attr(windows, ignore = "the program under inspection is a POSIX shell")]
fn inspect_env_equals_splits_on_the_first_equals() -> termlens::Result<()> {
    let path = std::env::var("PATH").unwrap_or_default();
    // A directory made here, so the assertion is not about /tmp's own name
    // on a platform that symlinks it (macOS: /tmp -> /private/tmp).
    let dir = std::env::temp_dir().join(format!("termlens-eq-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    let real = std::fs::canonicalize(&dir)?;
    // Wide enough that the path is one row, as in the two-argument test.
    let cwd = format!("--cwd={}", dir.display());
    let mut t = termlens::bin!(
        "termlens",
        env("PATH", &path),
        args([
            "inspect",
            "--size=200x3",
            &cwd,
            "--env=A=b=c",
            "sh",
            "-c",
            "echo \"[$A] $(pwd)\""
        ])
    )?;
    assert_eq!(t.wait_exit()?.code(), Some(0), "{}", t.screen());
    let s = t.screen();
    assert!(s.contains("[b=c]"), "the value keeps its second `=`: {s}");
    assert!(
        s.contains(real.to_str().expect("utf-8 path")),
        "the program ran in {}: {s}",
        real.display()
    );
    let _ = std::fs::remove_dir_all(&dir);
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
    let mut t = termlens::bin!("termlens", args(["inspect", "--help"]))?;
    assert_eq!(t.wait_exit()?.code(), Some(0));
    let help = t.screen().to_string();
    // The help has to say which rendering a redirect gets, or the split
    // #454 and #478 introduced is undiscoverable: both flags behave one
    // way at a prompt and another through a pipe. Needles chosen to sit
    // within one line of the 80-column help, so this pins the claim
    // rather than the line breaks.
    assert!(
        help.contains("--ansi") && help.contains("gets a saved screen"),
        "`inspect --help` must say a redirect gets a saved screen:\n{help}"
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
    assert!(
        stdout.contains("styles:"),
        "a redirect keeps the styles block: {stdout:?}"
    );
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

/// `inspect --ansi > file` is still a saved screen (#478). `--ansi` paints
/// on a terminal; a pipe writes `with_styles` so `render` and `diff` read
/// the file back, including the colour the flag was asked to keep.
#[test]
#[cfg_attr(windows, ignore = "the program under inspection is a POSIX shell")]
fn inspect_ansi_redirect_is_a_saved_screen() -> termlens::Result<()> {
    use std::process::Command;
    let bin = env!("CARGO_BIN_EXE_termlens");
    let out = Command::new(bin)
        .args([
            "inspect",
            "--size",
            "30x3",
            "--ansi",
            "sh",
            "-c",
            r#"printf '\033[1;31mred\033[0m'"#,
        ])
        .output()?;
    assert!(out.status.success(), "{out:?}");
    let stdout = String::from_utf8(out.stdout).expect("utf-8");
    assert!(
        !stdout.contains('\u{1b}'),
        "a redirect must not write C0: {stdout:?}"
    );
    let parsed = Screen::parse(&stdout).expect("stdout is a saved screen");
    assert_eq!(parsed.size(), (30, 3));
    assert_eq!(parsed.find("red"), Some((0, 0)));
    let cell = parsed.cell(0, 0).expect("the painted cell");
    assert_eq!(
        cell.style().fg,
        Color::Indexed(1),
        "{}",
        parsed.with_styles()
    );
    assert!(cell.style().bold, "{}", parsed.with_styles());
    assert!(
        stdout.contains("styles:"),
        "colour survives as the styles block: {stdout:?}"
    );

    let path = std::env::temp_dir().join(format!(
        "termlens-cli-inspect-ansi-{}.txt",
        std::process::id()
    ));
    std::fs::write(&path, &stdout)?;
    let file = path.to_str().unwrap();
    let render = Command::new(bin)
        .args(["render", "--text", file])
        .output()?;
    assert_eq!(render.status.code(), Some(0), "{render:?}");
    let diff = Command::new(bin).args(["diff", file, file]).output()?;
    assert_eq!(diff.status.code(), Some(0), "{diff:?}");
    let _ = std::fs::remove_file(&path);
    Ok(())
}

/// `--ansi` on a terminal still paints, so a person who asked for colour
/// sees it rather than a `styles:` block. Through a PTY, stdout *is* a
/// terminal, and the outer emulator consumes the SGR — the cell is red.
#[test]
#[cfg_attr(windows, ignore = "the program under inspection is a POSIX shell")]
fn inspect_ansi_paints_on_a_terminal() -> termlens::Result<()> {
    let path = std::env::var("PATH").unwrap_or_default();
    let mut t = termlens::bin!(
        "termlens",
        env("PATH", &path),
        args([
            "inspect",
            "--size",
            "30x3",
            "--ansi",
            "sh",
            "-c",
            r#"printf '\033[1;31mred\033[0m'"#
        ])
    )?;
    assert_eq!(t.wait_exit()?.code(), Some(0), "{}", t.screen());
    let s = t.screen();
    assert!(s.contains("red"), "{s}");
    assert!(
        !s.contains("styles:"),
        "a terminal is painted, not described: {s}"
    );
    let (row, col) = s.find("red").expect("the painted word");
    let cell = s.cell(row, col).expect("the painted cell");
    assert_eq!(cell.style().fg, Color::Indexed(1), "{}", s.with_styles());
    assert!(cell.style().bold, "{}", s.with_styles());
    Ok(())
}

/// The other half of #454's split: a terminal is a person looking, so it
/// gets the plain text it always got. Without this, "a redirect keeps its
/// styles" collapses into "everything carries a styles block", which is
/// the noise #454 weighed and did not want at a prompt — and nothing else
/// here would notice the difference, because every other test in this file
/// reads `inspect` through a pipe.
#[test]
#[cfg_attr(windows, ignore = "the program under inspection is a POSIX shell")]
fn inspect_on_a_terminal_stays_plain() -> termlens::Result<()> {
    let path = std::env::var("PATH").unwrap_or_default();
    let mut t = termlens::bin!(
        "termlens",
        env("PATH", &path),
        args([
            "inspect",
            "--size",
            "30x3",
            "sh",
            "-c",
            r#"printf '\033[1;31mred\033[0m'"#
        ])
    )?;
    assert_eq!(t.wait_exit()?.code(), Some(0), "{}", t.screen());
    let s = t.screen();
    assert!(s.contains("red"), "{s}");
    assert!(
        !s.contains("styles:"),
        "a terminal gets what it always got, not a saved screen: {s}"
    );
    Ok(())
}

/// `inspect … > file` used to write the plain rendering, so two screens
/// that differed only in colour compared as the same picture (#454).
/// The redirect is a `with_styles` snapshot now: `diff` names the run,
/// and the file still round-trips through `render` — including one a
/// 0.10 inspect saved, trailer on stdout.
#[test]
#[cfg_attr(windows, ignore = "the program under inspection is a POSIX shell")]
fn inspect_redirect_keeps_styles_so_diff_sees_a_colour_change() -> termlens::Result<()> {
    use std::process::Command;
    let bin = env!("CARGO_BIN_EXE_termlens");
    let inspect = |sgr: &str| {
        Command::new(bin)
            .args([
                "inspect",
                "--size",
                "20x2",
                "sh",
                "-c",
                &format!(r#"printf '\033[{sgr}mred\033[0m'"#),
            ])
            .output()
            .expect("inspect ran")
    };
    let red = inspect("1;31");
    let green = inspect("1;32");
    assert!(red.status.success(), "{red:?}");
    assert!(green.status.success(), "{green:?}");
    let red_out = String::from_utf8(red.stdout).expect("utf-8");
    let green_out = String::from_utf8(green.stdout).expect("utf-8");
    assert!(
        red_out.contains("styles:") && green_out.contains("styles:"),
        "a redirect keeps the styles block:\n{red_out}\n{green_out}"
    );

    let red_screen = Screen::parse(&red_out).expect("red is a saved screen");
    let green_screen = Screen::parse(&green_out).expect("green is a saved screen");
    let red_cell = red_screen.cell(0, 0).expect("the painted cell");
    let green_cell = green_screen.cell(0, 0).expect("the painted cell");
    assert_eq!(
        red_cell.style().fg,
        Color::Indexed(1),
        "{}",
        red_screen.with_styles()
    );
    assert_eq!(
        green_cell.style().fg,
        Color::Indexed(2),
        "{}",
        green_screen.with_styles()
    );
    assert!(red_cell.style().bold && green_cell.style().bold);

    let dir = std::env::temp_dir().join(format!(
        "termlens-cli-inspect-styles-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir)?;
    let red_path = dir.join("red.snap");
    let green_path = dir.join("green.snap");
    std::fs::write(&red_path, &red_out)?;
    std::fs::write(&green_path, &green_out)?;
    let red_file = red_path.to_str().unwrap();
    let green_file = green_path.to_str().unwrap();

    let diff = Command::new(bin)
        .args(["diff", "--color", "never", red_file, green_file])
        .output()?;
    assert_eq!(diff.status.code(), Some(1), "{diff:?}");
    let diff_out = String::from_utf8_lossy(&diff.stdout);
    assert!(
        diff_out.contains("styles:") && diff_out.contains("fg=1") && diff_out.contains("fg=2"),
        "diff names the colour change: {diff_out}"
    );

    let text = Command::new(bin)
        .args(["render", "--text", red_file])
        .output()?;
    assert_eq!(text.status.code(), Some(0), "{text:?}");
    assert!(
        String::from_utf8_lossy(&text.stdout).contains("styles:"),
        "{text:?}"
    );
    let json = Command::new(bin)
        .args(["render", "--json", red_file])
        .output()?;
    assert_eq!(json.status.code(), Some(0), "{json:?}");
    let document = String::from_utf8(json.stdout).expect("utf-8 JSON");
    let from_json: Screen = serde_json::from_str(&document).expect("a termlens Screen");
    assert_eq!(
        from_json.cell(0, 0).expect("the painted cell").style().fg,
        Color::Indexed(1),
        "{}",
        from_json.with_styles()
    );

    // A file a 0.10 inspect saved — trailer on stdout — still reads.
    let old = dir.join("old.snap");
    std::fs::write(&old, format!("{red_out}--- exited: exit code 0 ---\n"))?;
    let render = Command::new(bin)
        .args(["render", "--text", old.to_str().unwrap()])
        .output()?;
    assert_eq!(render.status.code(), Some(0), "{render:?}");
    let against_old = Command::new(bin)
        .args(["diff", red_file, old.to_str().unwrap()])
        .output()?;
    assert_eq!(
        against_old.status.code(),
        Some(0),
        "the trailer is not a row: {against_old:?}"
    );

    let _ = std::fs::remove_dir_all(&dir);
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

/// An empty value is what a shell hands over when a variable is unset, so
/// `--out=$DEST` with `DEST` never assigned is the way this arrives. Every
/// other `=`-spelled flag names itself when handed one; `--out=` reported
/// the filesystem's complaint about `""` instead (#451).
#[test]
fn render_empty_out_value_names_the_flag() -> termlens::Result<()> {
    use std::process::Command;
    let bin = env!("CARGO_BIN_EXE_termlens");
    let out = Command::new(bin)
        .args(["render", "--text", "--out=", &data("before.snap")])
        .output()?;
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(stderr, "termlens: --out needs a PATH argument\n");
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

/// `--out -` is stdout, the same stream every other `-` operand already
/// is (#469). `--out` used to take it as a filename, so `render --out -`
/// wrote `./-` and printed nothing — exit 0, and a file most shells make
/// awkward to delete. `./-` is still a file named `-`.
#[test]
fn render_out_dash_writes_to_stdout_and_creates_no_file() -> termlens::Result<()> {
    use std::process::Command;
    let bin = env!("CARGO_BIN_EXE_termlens");
    let dir = std::env::temp_dir().join(format!("termlens-render-out-dash-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    let snap = data("before.snap");
    let dash = dir.join("-");

    let to_stdout = Command::new(bin)
        .args(["render", "--text", &snap])
        .output()?;
    assert_eq!(to_stdout.status.code(), Some(0), "{to_stdout:?}");

    // Both spellings arrive at the same string; the cwd is the temp dir so
    // a regression that writes `./-` is visible here, not in the suite root.
    let spaced = Command::new(bin)
        .current_dir(&dir)
        .args(["render", "--text", "--out", "-", &snap])
        .output()?;
    assert_eq!(spaced.status.code(), Some(0), "{spaced:?}");
    assert_eq!(
        spaced.stdout, to_stdout.stdout,
        "the same bytes stdout would have carried"
    );
    assert!(
        !dash.exists(),
        "--out - created a file named -: {}",
        dash.display()
    );

    let equals = Command::new(bin)
        .current_dir(&dir)
        .args(["render", "--text", "--out=-", &snap])
        .output()?;
    assert_eq!(equals.status.code(), Some(0), "{equals:?}");
    assert_eq!(
        equals.stdout, to_stdout.stdout,
        "--out=- is the same stream"
    );
    assert!(
        !dash.exists(),
        "--out=- created a file named -: {}",
        dash.display()
    );

    let out = Command::new(bin)
        .current_dir(&dir)
        .args(["render", "--text", "--out", "./-", &snap])
        .output()?;
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert!(out.stdout.is_empty(), "--out ./- means not stdout: {out:?}");
    assert_eq!(
        std::fs::read(&dash)?,
        to_stdout.stdout,
        "./- is a file named -"
    );

    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

/// The operand is the whole input: two of them are ambiguous, and the last
/// silently winning is how a stale path renders a screen nobody named —
/// with `--out` there is nothing on screen to reveal it (#364).
#[test]
fn render_refuses_a_second_operand() -> termlens::Result<()> {
    use std::process::Command;
    let bin = env!("CARGO_BIN_EXE_termlens");
    let dir = std::env::temp_dir().join(format!("termlens-render-operands-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    let a = dir.join("a.snap");
    let b = dir.join("b.snap");
    std::fs::write(&a, "size: 10x1  cursor: 0,0\nhi\n")?;
    std::fs::write(&b, "size: 10x1  cursor: 0,0\nbye\n")?;

    let out = Command::new(bin)
        .args([
            "render",
            "--text",
            a.to_str().expect("utf-8 path"),
            b.to_str().expect("utf-8 path"),
        ])
        .output()?;
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("usage: termlens render"), "{stderr}");
    assert!(
        !String::from_utf8_lossy(&out.stdout).contains("bye"),
        "the last operand must not win: {out:?}"
    );

    // `--out` is where the bug was invisible, so this is the case that
    // matters: exit 2 and no file, not the wrong screen in the right one.
    let shot = dir.join("shot.svg");
    let out = Command::new(bin)
        .args([
            "render",
            "--svg",
            "--out",
            shot.to_str().expect("utf-8 path"),
            a.to_str().expect("utf-8 path"),
            b.to_str().expect("utf-8 path"),
        ])
        .output()?;
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    assert!(
        !shot.exists(),
        "a refused render left a file behind: {}",
        shot.display()
    );

    // A `-` beside a path is refused before stdin is read: the diagnostic
    // is the usage, not a complaint about what the pipe carried.
    let out = with_stdin(
        &["render", "--text", "-", a.to_str().expect("utf-8 path")],
        "not a saved screen\n",
    );
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("usage: termlens render"), "{stderr}");
    assert!(!stderr.contains("<stdin>"), "stdin was read: {stderr}");

    // And one operand remains exactly the command it was.
    let out = Command::new(bin)
        .args(["render", "--text", a.to_str().expect("utf-8 path")])
        .output()?;
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("hi"),
        "{out:?}"
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

/// `inspect` exists so a test sees the screen its own program will see, so
/// the child environment starts bare; `--inherit-env` is the explicit
/// opt-in to the caller's (#367). A regression in either direction is
/// silent: the screen still renders, it is simply the wrong one.
#[test]
#[cfg_attr(windows, ignore = "the program under inspection is a POSIX shell")]
fn inspect_clears_the_caller_env_unless_inherit_env_is_passed() -> termlens::Result<()> {
    let path = std::env::var("PATH").unwrap_or_default();
    let mut t = termlens::bin!(
        "termlens",
        env("PATH", &path),
        env("TERMLENS_INSPECT_CALLER", "zzz"),
        args([
            "inspect",
            "--size",
            "60x3",
            "sh",
            "-c",
            "echo \"[$TERMLENS_INSPECT_CALLER]\""
        ])
    )?;
    assert_eq!(t.wait_exit()?.code(), Some(0), "{}", t.screen());
    let s = t.screen();
    assert!(s.contains("[]"), "absent without --inherit-env: {s}");
    assert!(!s.contains("zzz"), "the caller's variable leaked in: {s}");

    let mut t = termlens::bin!(
        "termlens",
        env("PATH", &path),
        env("TERMLENS_INSPECT_CALLER", "zzz"),
        args([
            "inspect",
            "--size",
            "60x3",
            "--inherit-env",
            "sh",
            "-c",
            "echo \"[$TERMLENS_INSPECT_CALLER]\""
        ])
    )?;
    assert_eq!(t.wait_exit()?.code(), Some(0), "{}", t.screen());
    assert!(t.screen().contains("[zzz]"), "{}", t.screen());
    Ok(())
}

/// `--env KEY=VALUE` sets variables in the otherwise bare child
/// environment, and splits on the **first** `=` so a value may contain
/// more of them (#367).
#[test]
#[cfg_attr(windows, ignore = "the program under inspection is a POSIX shell")]
fn inspect_env_sets_a_variable_and_splits_on_the_first_equals() -> termlens::Result<()> {
    let path = std::env::var("PATH").unwrap_or_default();
    let mut t = termlens::bin!(
        "termlens",
        env("PATH", &path),
        args([
            "inspect",
            "--size",
            "60x3",
            "--env",
            "TERMLENS_INSPECT_SET=zzz",
            "sh",
            "-c",
            "echo \"[$TERMLENS_INSPECT_SET]\""
        ])
    )?;
    assert_eq!(t.wait_exit()?.code(), Some(0), "{}", t.screen());
    assert!(t.screen().contains("[zzz]"), "{}", t.screen());

    // The first `=`, not the last: `A=b=c` means `A` is `b=c`.
    let mut t = termlens::bin!(
        "termlens",
        env("PATH", &path),
        args([
            "inspect",
            "--size",
            "60x3",
            "--env",
            "TERMLENS_INSPECT_SPLIT=b=c",
            "sh",
            "-c",
            "echo \"[$TERMLENS_INSPECT_SPLIT]\""
        ])
    )?;
    assert_eq!(t.wait_exit()?.code(), Some(0), "{}", t.screen());
    assert!(t.screen().contains("[b=c]"), "{}", t.screen());

    // The case where the two splits disagree: an empty key is refused, so
    // this cannot pass under both `split_once` and `rsplit_once` the way
    // the `[b=c]` assertion above can — the child sees `A=b=c` either way
    // and the shell splits it itself. This is the assertion that bites.
    let mut t = termlens::bin!(
        "termlens",
        env("PATH", &path),
        args(["inspect", "--size", "60x3", "--env", "=a=b", "sh", "-c", "echo hi"])
    )?;
    assert_eq!(t.wait_exit()?.code(), Some(2), "{}", t.screen());
    assert!(
        t.screen()
            .contains(r#"bad --env "=a=b", expected e.g. NO_COLOR=1"#),
        "an empty key is refused with the flag's own diagnostic: {}",
        t.screen()
    );
    Ok(())
}

/// `--idle` is the settle window that ends the wait, and `--timeout` is its
/// one bound: output that arrives after the window, or after the deadline,
/// is not on the snapshot (#367, #374). A regression here would be someone
/// else's flake, so this test pins both sides of the window.
#[test]
#[cfg_attr(windows, ignore = "the program under inspection is a POSIX shell")]
fn inspect_idle_window_decides_what_the_deadline_snapshot_holds() -> termlens::Result<()> {
    let path = std::env::var("PATH").unwrap_or_default();
    // `first` lands well before the 3s deadline, `second` four seconds in.
    // The two sides: a 300 ms window is satisfied a moment after `first`,
    // ending the wait there with `first` only; a five-second window can
    // never be satisfied inside the deadline, so the deadline ends the wait
    // — still with `first` only, because `second` lands a second after it.
    // Neither side races the 4 s mark that way.
    // The trailing `sleep` keeps the child alive after its last write: a
    // write-then-exit races the platform's PTY teardown and the final bytes
    // can be lost (docs/DESIGN.md).
    let child = "printf first; sleep 4; printf ' second'; sleep 30";
    let run = |idle: &str| {
        termlens::bin!(
            "termlens",
            env("PATH", &path),
            // The harness deadline also covers the spawn (CONTRIBUTING §3),
            // so it is generous rather than tight around the CLI's own 3s.
            timeout(std::time::Duration::from_secs(30)),
            args([
                "inspect",
                "--size",
                "60x3",
                "--timeout",
                "3",
                "--idle",
                idle,
                "sh",
                "-c",
                child
            ])
        )
    };

    let mut t = run("300")?;
    assert_eq!(t.wait_exit()?.code(), Some(0), "{}", t.screen());
    let s = t.screen();
    assert!(s.contains("first"), "{s}");
    assert!(
        !s.contains("second"),
        "the gap sits outside the window: {s}"
    );

    let started = std::time::Instant::now();
    let mut t = run("5000")?;
    assert_eq!(t.wait_exit()?.code(), Some(0), "{}", t.screen());
    assert!(
        !t.screen().contains("second"),
        "the deadline ends the wait before the overdue window: {}",
        t.screen()
    );
    // The deadline is the one bound: resolving there, not three seconds then
    // a five-second window on top (which used to hold `second`, four seconds
    // in, and return around eight). The ceiling is generous; the spawn is
    // covered by the harness deadline (CONTRIBUTING §3).
    assert!(
        started.elapsed() < std::time::Duration::from_secs(6),
        "returned after {:?}: the deadline did not bound the idle window",
        started.elapsed()
    );
    Ok(())
}

/// `--timeout` is the other half of the same pair: a program that outlives
/// it is snapshotted where it stands and reported as still running, not
/// treated as an error (#367). `second` is what separates a 3s deadline
/// from the 5s default: the default would hold it, the flag must not. The
/// harness deadline is generous because it also covers the spawn
/// (CONTRIBUTING §3).
///
/// `--idle 5000` is what keeps this about `--timeout` since #374: the wait
/// now ends on the silence window too, and this child goes quiet the
/// instant it prints `first`, so the default 300ms window would end the
/// wait at 300ms whatever the deadline said — and the test would pass
/// against a `--timeout` that did nothing at all. A window above the
/// deadline can never be satisfied inside it, so the deadline is once again
/// the thing being measured, and the trailer says so.
#[test]
#[cfg_attr(windows, ignore = "the program under inspection is a POSIX shell")]
fn inspect_timeout_snapshots_a_program_that_outlives_it() -> termlens::Result<()> {
    let path = std::env::var("PATH").unwrap_or_default();
    let mut t = termlens::bin!(
        "termlens",
        env("PATH", &path),
        timeout(std::time::Duration::from_secs(30)),
        args([
            "inspect",
            "--size",
            "60x3",
            "--timeout",
            "3",
            "--idle",
            "5000",
            "sh",
            "-c",
            "printf first; sleep 4; printf ' second'; sleep 30"
        ])
    )?;
    assert_eq!(t.wait_exit()?.code(), Some(0), "{}", t.screen());
    let s = t.screen();
    assert!(s.contains("first"), "the pre-deadline output is kept: {s}");
    assert!(
        !s.contains("second"),
        "output past the 3s deadline is not the snapshot: {s}"
    );
    assert!(
        s.contains("--- still running at the deadline (killed on exit) ---"),
        "{s}"
    );
    Ok(())
}

/// The idle window ends the wait, not the deadline (#374). A program that
/// paints once and sleeps is a TUI in miniature: `--idle 200` must end the
/// wait, where the sequential version charged the whole `--timeout` and
/// only then looked at `--idle` — six seconds for a screen complete in a
/// millisecond. The ceiling is generous, and the harness deadline sixty
/// times it, so this goes red only if the wait itself moved; the harness
/// timeout also covers the spawn (CONTRIBUTING §3).
#[test]
#[cfg_attr(windows, ignore = "the program under inspection is a POSIX shell")]
fn inspect_resolves_on_the_idle_window_before_the_deadline() -> termlens::Result<()> {
    let path = std::env::var("PATH").unwrap_or_default();
    let started = std::time::Instant::now();
    let mut t = termlens::bin!(
        "termlens",
        timeout(std::time::Duration::from_secs(60)),
        env("PATH", &path),
        args([
            "inspect",
            "--size",
            "20x3",
            "--idle",
            "200",
            "--timeout",
            "30",
            "sh",
            "-c",
            "printf READY; sleep 300"
        ])
    )?;
    assert_eq!(t.wait_exit()?.code(), Some(0), "{}", t.screen());
    let elapsed = started.elapsed();
    let s = t.screen();
    assert!(
        s.contains("READY"),
        "the output before idleness is kept: {s}"
    );
    assert!(
        s.contains("--- still running (killed on exit) ---"),
        "the program never exited, so the trailer says so — and the window \
         ended the wait, so it does not claim the deadline did: {s}"
    );
    assert!(
        elapsed < std::time::Duration::from_secs(10),
        "took {elapsed:?}: the 200ms idle window did not end the wait before the 30s deadline"
    );
    Ok(())
}

/// The other arm of the race (#374): an exit still wins, promptly and with
/// the exited trailer, even when `--idle` is far longer than the deadline —
/// the fix must not turn a program that prints and exits into one held for
/// the silence window.
#[test]
#[cfg_attr(windows, ignore = "the program under inspection is a POSIX shell")]
fn inspect_reports_an_exit_without_waiting_for_the_idle_window() -> termlens::Result<()> {
    let path = std::env::var("PATH").unwrap_or_default();
    let started = std::time::Instant::now();
    let mut t = termlens::bin!(
        "termlens",
        timeout(std::time::Duration::from_secs(30)),
        env("PATH", &path),
        args([
            "inspect",
            "--size",
            "20x3",
            "--idle",
            "30000",
            "--timeout",
            "30",
            "sh",
            "-c",
            "printf 'done here'"
        ])
    )?;
    assert_eq!(t.wait_exit()?.code(), Some(0), "{}", t.screen());
    let elapsed = started.elapsed();
    let s = t.screen();
    assert!(
        s.contains("done here"),
        "the final output is the screen: {s}"
    );
    assert!(s.contains("--- exited: exit code 0 ---"), "{s}");
    assert!(
        elapsed < std::time::Duration::from_secs(10),
        "took {elapsed:?} for a program that exited at once"
    );
    Ok(())
}

/// The deadline is still the bound when the idle window never opens (#374):
/// a program that keeps talking is snapshotted where it stands and reported
/// as still running, and the wait is not cut short by the fix. The 1s
/// deadline is the wait that must expire, and the child keeps it from
/// opening — the outer deadline stays generous all the same.
#[test]
#[cfg_attr(windows, ignore = "the program under inspection is a POSIX shell")]
fn inspect_chatter_waits_for_the_deadline_and_says_still_running() -> termlens::Result<()> {
    let path = std::env::var("PATH").unwrap_or_default();
    let started = std::time::Instant::now();
    let mut t = termlens::bin!(
        "termlens",
        timeout(std::time::Duration::from_secs(30)),
        env("PATH", &path),
        args([
            "inspect",
            "--size",
            "20x3",
            "--idle",
            "500",
            "--timeout",
            "1",
            "sh",
            "-c",
            "while :; do printf x; sleep 0.01; done"
        ])
    )?;
    assert_eq!(t.wait_exit()?.code(), Some(0), "{}", t.screen());
    let elapsed = started.elapsed();
    let s = t.screen();
    assert!(
        s.contains("--- still running at the deadline (killed on exit) ---"),
        "{s}"
    );
    assert!(
        elapsed >= std::time::Duration::from_millis(800),
        "returned after {elapsed:?}: the 1s deadline was not waited"
    );
    Ok(())
}

/// A child that closes its terminal but keeps running is not an exited
/// child: the reap that answers "exited" never comes, so the trailer must
/// say still running (#374). The reap grace a genuinely exited child is
/// given must not mislabel this one.
///
/// On Linux the EOF ends the wait: closing the last descriptor on the
/// terminal is one, at once. On macOS no EOF comes while the child lives —
/// the terminal is still its controlling terminal — and a child that never
/// printed starts no silence window, so the deadline ends it, and says so.
#[test]
#[cfg_attr(windows, ignore = "the program under inspection is a POSIX shell")]
fn inspect_reports_still_running_when_the_child_closes_its_terminal() -> termlens::Result<()> {
    use std::process::Command;
    // The short deadline goes where it must expire, and only there
    // (CONTRIBUTING §3).
    let (timeout, trailer) = if cfg!(target_os = "macos") {
        (
            "2",
            "--- still running at the deadline (killed on exit) ---",
        )
    } else {
        ("30", "--- still running (killed on exit) ---")
    };
    let bin = env!("CARGO_BIN_EXE_termlens");
    let started = std::time::Instant::now();
    let out = Command::new(bin)
        .args([
            "inspect",
            "--size",
            "20x3",
            "--timeout",
            timeout,
            "sh",
            "-c",
            "exec 0<&- 1>&- 2>&-; exec sleep 30",
        ])
        .output()?;
    assert!(out.status.success(), "{out:?}");
    let stderr = String::from_utf8(out.stderr).expect("utf-8");
    assert_eq!(stderr.trim_end(), trailer, "{stderr:?}");
    // On Linux it returned on the EOF, not the 30s deadline: the reap grace
    // is the only wait left, and the ceiling is generous for the spawn.
    if !cfg!(target_os = "macos") {
        assert!(
            started.elapsed() < std::time::Duration::from_secs(10),
            "returned after {:?}: the EOF did not end the wait",
            started.elapsed()
        );
    }
    Ok(())
}
