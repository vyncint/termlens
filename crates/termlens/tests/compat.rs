//! The saved-screen formats stay readable (#327): every file under
//! `tests/compat/<version>/` was written by that published release, is
//! never edited, and must parse into the screen it was written from.
//!
//! Three checks per shape. The text file round-trips byte for byte — parse
//! it, render it the way it was rendered (`Display`, or `with_styles`
//! when it carries a block), compare. The JSON twin deserialises and shows
//! the same picture as the text (`Screen::diff` is empty), and re-serialises
//! to the file's own document plus the `format` field a 0.10 file lacks —
//! so every field of the out-of-band state is pinned too, not only the
//! grid. `crates/termlens-cli/tests/cli.rs` adds the fourth: `termlens
//! render --text` exits 0 on each file.
//!
//! The JSON checks need the `serde` feature; the text checks run in every
//! configuration, which is where a `cfg(feature = "serde")` mistake in the
//! parser would show.

use std::fs;
use std::path::{Path, PathBuf};

use termlens::Screen;

/// `tests/compat/`, and every version directory in it, sorted.
fn versions() -> Vec<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/compat");
    let mut dirs: Vec<PathBuf> = fs::read_dir(&root)
        .expect("tests/compat exists")
        .map(|entry| entry.expect("a directory entry").path())
        .filter(|path| path.is_dir())
        .collect();
    dirs.sort();
    assert!(
        !dirs.is_empty(),
        "no version directories under {}",
        root.display()
    );
    dirs
}

/// The six shapes every version directory carries, so a directory that is
/// missing one fails rather than silently pinning less.
const SHAPES: [&str; 6] = [
    "plain",
    "styled",
    "hidden",
    "wide",
    "masked",
    "text-of-styled",
];

fn read(path: &Path) -> String {
    fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
        // A checkout on Windows may have given the file CRLF endings; the
        // format is line-based and the byte comparison below must not be
        // about line endings.
        .replace("\r\n", "\n")
}

/// Parse a text file and render it back the way it was written: with the
/// `styles:` block when the file has one, plain `Display` otherwise. The
/// header's row count decides where the grid ends (#296), so "has a block"
/// is decided the same way the parser decides it — by what follows the
/// declared rows — and not by searching for the word.
fn round_trip(text: &str) -> (Screen, String) {
    let screen = Screen::parse(text).expect("a corpus file parses");
    let rows = usize::from(screen.rows());
    let after_grid = text
        .lines()
        .skip(1 + rows)
        .any(|line| !line.trim().is_empty());
    let rendered = if after_grid {
        format!("{}\n", screen.with_styles())
    } else {
        format!("{screen}\n")
    };
    (screen, rendered)
}

#[test]
fn every_text_file_written_by_a_published_release_round_trips_byte_for_byte() {
    for dir in versions() {
        for shape in SHAPES {
            let path = dir.join(format!("{shape}.txt"));
            let text = read(&path);
            let (_, rendered) = round_trip(&text);
            assert_eq!(
                rendered,
                text,
                "{} does not survive parse -> render",
                path.display()
            );
        }
    }
}

#[test]
fn a_hand_edited_corpus_file_would_fail() {
    // The check above can fail: a header field, a grid character and a
    // style token each changed in memory, and each is caught.
    let dir = &versions()[0];
    let text = read(&dir.join("styled.txt"));
    let (_, rendered) = round_trip(&text);
    assert_eq!(rendered, text, "the unedited file round-trips");
    for (from, to) in [
        ("cursor: 2,17", "cursor: 2,18"),
        ("myapp", "myapq"),
        ("fg=6", "fg=7"),
    ] {
        let edited = text.replacen(from, to, 1);
        assert_ne!(edited, text, "{from} is in the file");
        let (_, rendered) = round_trip(&edited);
        assert_eq!(
            rendered, edited,
            "an edit that is itself valid still round-trips"
        );
        assert_ne!(rendered, text, "…and no longer equals the frozen file");
    }
    // An edit that breaks the format is refused rather than read as far as
    // it goes.
    let truncated = text.replacen("styles:", "style:", 1);
    assert!(Screen::parse(&truncated).is_err());
}

#[cfg(feature = "serde")]
mod json {
    use super::*;

    #[test]
    fn every_json_twin_reads_as_the_same_picture_as_its_text() {
        for dir in versions() {
            for shape in SHAPES {
                let text_path = dir.join(format!("{shape}.txt"));
                let json_path = dir.join(format!("{shape}.json"));
                let (from_text, _) = round_trip(&read(&text_path));
                let from_json: Screen = serde_json::from_str(&read(&json_path))
                    .unwrap_or_else(|e| panic!("{}: {e}", json_path.display()));
                let diff = from_text.diff(&from_json);
                if shape == "text-of-styled" {
                    // The text format is style-blind and this shape's text
                    // was written without its block, so the JSON twin
                    // carries styles the text cannot: the same text, and a
                    // diff that is styles only.
                    assert_eq!(from_json.to_string(), from_text.to_string());
                    assert_eq!(diff.changed_rows().collect::<Vec<_>>(), [0], "{diff}");
                    assert_eq!(
                        diff.style_changes().collect::<Vec<_>>(),
                        [(0, "(none)", "0-2 fg=1")],
                        "{diff}"
                    );
                    continue;
                }
                assert!(
                    diff.is_empty(),
                    "{} and {} show different pictures:\n{diff}",
                    text_path.display(),
                    json_path.display()
                );
            }
        }
    }

    /// The whole document, not only the grid: a `Screen` read from a
    /// corpus file serialises back to that file's JSON, with the one
    /// addition every reader of a 0.10 file makes — `"format": 1`, which
    /// 0.10 did not write and which is what its shape is (#329).
    #[test]
    fn every_json_twin_re_serialises_to_itself_plus_the_format_field() {
        for dir in versions() {
            for shape in SHAPES {
                let path = dir.join(format!("{shape}.json"));
                let mut file: serde_json::Value = serde_json::from_str(&read(&path)).unwrap();
                let screen: Screen = serde_json::from_value(file.clone()).unwrap();
                let object = file.as_object_mut().expect("a JSON object");
                object.entry("format").or_insert(serde_json::json!(1));
                assert_eq!(
                    serde_json::to_value(&screen).unwrap(),
                    file,
                    "{} does not survive deserialize -> serialize",
                    path.display()
                );
            }
        }
    }

    #[test]
    fn a_0_10_json_file_has_no_format_field_and_a_0_11_one_does() {
        for dir in versions() {
            let version = dir.file_name().unwrap().to_string_lossy().into_owned();
            let file: serde_json::Value =
                serde_json::from_str(&read(&dir.join("plain.json"))).unwrap();
            // 0.10 wrote no field; every later release writes `1`.
            let one = serde_json::json!(1);
            let expected = (!version.starts_with("0.10.")).then_some(&one);
            assert_eq!(file.get("format"), expected, "{version}");
        }
    }
}

/// Writes a new version directory from *this* tree, for the release that
/// this tree is about to become (docs/RELEASING.md). Ignored: it is a tool,
/// not a check, and it is run once per release by hand —
///
/// ```sh
/// TERMLENS_WRITE_CORPUS=0.11.0 cargo test -p termlens --features serde \
///     --test compat write_corpus -- --ignored
/// ```
///
/// The shapes are the six the README lists, produced the way the 0.10.1
/// directory was (a `sh -c 'printf …; read _'` per shape, so the corpus
/// does not depend on a fixture binary that itself moves). It refuses to
/// overwrite: a directory that exists is a published release's record.
#[cfg(all(unix, feature = "serde"))]
#[test]
#[ignore = "writes tests/compat/<version>/; run by hand at release time with TERMLENS_WRITE_CORPUS set"]
fn write_corpus() -> termlens::Result<()> {
    use std::time::Duration;
    use termlens::Terminal;

    let Ok(version) = std::env::var("TERMLENS_WRITE_CORPUS") else {
        panic!("set TERMLENS_WRITE_CORPUS=<version> to write a corpus directory");
    };
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/compat")
        .join(&version);
    assert!(
        !dir.exists(),
        "{} exists; a published release's corpus is never rewritten",
        dir.display()
    );
    fs::create_dir_all(&dir).expect("create the version directory");

    let sh = |size: (u16, u16), script: &str| -> termlens::Result<Terminal> {
        Terminal::builder()
            .size(size.0, size.1)
            .env_clear()
            .timeout(Duration::from_secs(20))
            .arg("-c")
            .arg(format!("{script}; read _"))
            .spawn("/bin/sh")
    };
    let write = |name: &str, text: String, screen: &Screen| {
        fs::write(dir.join(format!("{name}.txt")), format!("{text}\n")).expect("write text");
        let json = serde_json::to_string_pretty(screen).expect("serialize");
        fs::write(dir.join(format!("{name}.json")), format!("{json}\n")).expect("write json");
    };

    let mut t = sh((20, 4), "printf 'hello, world\\nsecond line'")?;
    t.wait_until(|s| s.contains("second line"))?;
    let s = t.screen();
    write("plain", s.to_string(), &s);

    let mut t = sh((30, 4), "printf '\\033[1;36mmyapp\\033[0m  \\033[7m> Alpha\\033[0m\\n\\033[2mnote\\033[0m \\033[3mit\\033[0m \\033[4;33mul\\033[0m \\033[5mbl\\033[0m \\033[8mhid\\033[0m \\033[9mst\\033[0m\\n\\033[48;2;30;30;46mrgb bg\\033[0m \\033[38;5;208mfg208\\033[0m DONE'")?;
    t.wait_until(|s| s.contains("DONE"))?;
    let s = t.screen();
    write("styled", s.with_styles().to_string(), &s);

    let mut t = sh((20, 3), "printf 'cursor gone\\033[?25l'")?;
    t.wait_until(|s| s.contains("cursor gone") && !s.cursor_visible())?;
    let s = t.screen();
    write("hidden", s.with_styles().to_string(), &s);

    let mut t = sh((12, 3), "printf '東京 e\u{301} 😀!\\nabcdefghij東\\nEND'")?;
    t.wait_until(|s| s.contains("END"))?;
    let s = t.screen();
    write("wide", s.with_styles().to_string(), &s);

    let mut t = sh(
        (30, 3),
        "printf 'pw: \\033[8mhunter2\\033[28m at 12:34:56\\n\\033[1mid=\\033[0m 4711 DONE'",
    )?;
    t.wait_until(|s| s.contains("DONE"))?;
    let s = t
        .screen()
        .mask_matching("12:34:56", '▒')
        .mask_rect(4..11, 0..1);
    write("masked", s.with_styles().to_string(), &s);

    let mut t = sh((16, 2), "printf '\\033[31mred\\033[0m and plain'")?;
    t.wait_until(|s| s.contains("plain"))?;
    let s = t.screen();
    write("text-of-styled", s.to_string(), &s);
    Ok(())
}
