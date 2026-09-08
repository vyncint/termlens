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
