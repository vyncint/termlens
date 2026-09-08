//! What the PTY layer forwards, byte for byte. A diagnostic, not a test.
//!
//! On Unix a PTY is a pipe with a line discipline: what the child writes is
//! what the master reads. ConPTY is not — it renders the child's output into
//! a screen of its own and re-emits *that*, so the sequences arriving at
//! termlens's emulator on Windows are whatever ConPTY chose to say, not what
//! the application wrote. Which sequences survive decides which features
//! termlens can honestly claim there: the query responder, `wait_frame`,
//! graphics, links, character sets (#149, step 3).
//!
//! Nobody working on this crate has a Windows machine, so the questions are
//! asked all at once and answered by a CI run: `.github/workflows/windows.yml`
//! runs this with `--ignored --nocapture` and keeps the output. On Unix the
//! same probe prints every sequence forwarded verbatim, which is what makes
//! it possible to develop and trust the probe itself without Windows.
//!
//! Each case writes the bytes to a file and has the platform's own file
//! printer (`cat`, or `cmd /d /c type`) send them through the PTY: nothing
//! between the bytes and the console but a program every install has, and
//! no shell quoting to decode. The verdict per case is whether the exact
//! sent bytes appear somewhere in what the master read.

use std::io::{self, Read};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::Duration;

use portable_pty::{native_pty_system, CommandBuilder, PtySize};

struct Case {
    name: &'static str,
    /// What the feature needs to survive the round trip.
    why: &'static str,
    bytes: &'static [u8],
}

const CASES: &[Case] = &[
    Case {
        name: "plain text",
        why: "control: a case that cannot fail",
        bytes: b"hello",
    },
    Case {
        name: "SGR bold red",
        why: "style assertions",
        bytes: b"\x1b[1;31mred\x1b[0m",
    },
    Case {
        name: "DECSET 2026 frame",
        why: "wait_frame: a frame is the bracket, not the bytes inside it",
        bytes: b"\x1b[?2026hframe\x1b[?2026l",
    },
    Case {
        name: "DA1 query",
        why: "the query responder: absent here means ConPTY answered it itself",
        bytes: b"\x1b[c",
    },
    Case {
        name: "DSR 6 cursor position",
        why: "the query responder",
        bytes: b"\x1b[6n",
    },
    Case {
        name: "OSC 11 background query",
        why: "background_rgb reaches the child only if this reaches us",
        bytes: b"\x1b]11;?\x07",
    },
    Case {
        name: "XTGETTCAP TN",
        why: "the terminfo name reply",
        bytes: b"\x1bP+q544e\x1b\\",
    },
    Case {
        name: "OSC 8 hyperlink",
        why: "Screen::links",
        bytes: b"\x1b]8;;http://example.invalid\x1b\\label\x1b]8;;\x1b\\",
    },
    Case {
        name: "OSC 52 clipboard write",
        why: "Screen::clipboard",
        bytes: b"\x1b]52;c;aGVsbG8=\x07",
    },
    Case {
        name: "OSC 2 title",
        why: "Screen::title",
        bytes: b"\x1b]2;a title\x07",
    },
    Case {
        name: "DEC Special Graphics",
        why: "charset translation: box-drawing via ESC ( 0",
        bytes: b"\x1b(0lqk\x1b(B",
    },
    Case {
        name: "mouse tracking + SGR encoding",
        why: "Screen::mouse_modes",
        bytes: b"\x1b[?1000h\x1b[?1006h\x1b[?1006l\x1b[?1000l",
    },
    Case {
        name: "bracketed paste",
        why: "paste() wraps only when the child asked",
        bytes: b"\x1b[?2004h\x1b[?2004l",
    },
    Case {
        name: "focus reporting",
        why: "focus events",
        bytes: b"\x1b[?1004h\x1b[?1004l",
    },
    Case {
        name: "cursor shape DECSCUSR",
        why: "Screen::cursor_shape",
        bytes: b"\x1b[2 q",
    },
    Case {
        name: "kitty graphics",
        why: "GraphicsPayload: a 1x1 RGB transmit-and-display",
        bytes: b"\x1b_Ga=T,f=24,s=1,v=1;AAAA\x1b\\",
    },
    Case {
        name: "sixel",
        why: "GraphicsPayload",
        bytes: b"\x1bPq#0;2;0;0;0#0~-\x1b\\",
    },
    Case {
        name: "HTS then TBC 3",
        why: "tab stops (#262)",
        bytes: b"\x1bH\x1b[3g",
    },
    Case {
        name: "alternate screen",
        why: "the mode termlens reports",
        bytes: b"\x1b[?1049hinside\x1b[?1049l",
    },
    Case {
        name: "bell",
        why: "Screen::bells",
        bytes: b"\x07",
    },
    Case {
        name: "tab",
        why: "HT reaches the emulator as HT, not as spaces",
        bytes: b"a\tb",
    },
];

/// One temp file per case per process, so parallel probes never share one.
static SEQ: AtomicUsize = AtomicUsize::new(0);

fn other(e: impl std::fmt::Display) -> io::Error {
    io::Error::other(e.to_string())
}

/// Every byte visibly: ESC as `\e`, other controls as `\xNN`, printable
/// ASCII as itself. Non-ASCII is escaped too — the verdict is about bytes.
fn escape(bytes: &[u8]) -> String {
    let mut out = String::new();
    for &b in bytes {
        match b {
            0x1b => out.push_str("\\e"),
            0x20..=0x7e if b != b'\\' => out.push(b as char),
            _ => out.push_str(&format!("\\x{b:02x}")),
        }
    }
    out
}

/// Spawn `cmd`, read the master until it closes, and return every byte.
///
/// The master is closed only after the child has exited and had a moment
/// to be flushed: on Unix the reader then sees EOF or `EIO`; on Windows
/// closing the pseudoconsole is what ends the output pipe. The reader
/// thread is joined rather than abandoned so nothing outlives the case.
fn through_pty(mut cmd: CommandBuilder) -> io::Result<Vec<u8>> {
    let pty = native_pty_system();
    let pair = pty
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(other)?;
    let mut reader = pair.master.try_clone_reader().map_err(other)?;
    let collector = thread::spawn(move || {
        let mut out = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => out.extend_from_slice(&buf[..n]),
            }
        }
        out
    });
    cmd.env("TERM", "xterm-256color");
    let mut child = pair.slave.spawn_command(cmd).map_err(other)?;
    drop(pair.slave);
    child.wait()?;
    thread::sleep(Duration::from_millis(500));
    drop(pair.master);
    collector
        .join()
        .map_err(|_| io::Error::other("the reader thread panicked"))
}

/// The platform's own way to print a file, with no shell in between.
fn print_file(path: &PathBuf) -> CommandBuilder {
    let mut cmd = if cfg!(windows) {
        let mut c = CommandBuilder::new("cmd.exe");
        c.args(["/d", "/c", "type"]);
        c
    } else {
        CommandBuilder::new("cat")
    };
    cmd.arg(path);
    cmd
}

fn probe(case: &Case) -> io::Result<Vec<u8>> {
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "termlens-conpty-probe-{}-{n}.bin",
        std::process::id()
    ));
    std::fs::write(&path, case.bytes)?;
    let result = through_pty(print_file(&path));
    let _ = std::fs::remove_file(&path);
    result
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && haystack.windows(needle.len()).any(|w| w == needle)
}

/// Prints, per sequence, whether the master read exactly what the child
/// wrote. Fails only when a probe cannot run at all — a verdict of "absent"
/// is a finding, not a failure.
#[test]
#[ignore = "diagnostic: prints what the PTY layer forwards; run with --ignored --nocapture"]
fn what_the_pty_layer_forwards() -> io::Result<()> {
    println!();
    println!(
        "conpty probe on {} {} — {} cases",
        std::env::consts::OS,
        std::env::consts::ARCH,
        CASES.len()
    );
    println!("verdict  case                             why");
    let mut forwarded = 0;
    let mut details = Vec::new();
    for case in CASES {
        let got = probe(case)?;
        let ok = contains(&got, case.bytes);
        forwarded += usize::from(ok);
        println!(
            "{:<8} {:<32} {}",
            if ok { "verbatim" } else { "ABSENT" },
            case.name,
            case.why
        );
        if !ok {
            details.push((case.name, escape(case.bytes), escape(&got)));
        }
    }
    println!();
    println!("{forwarded}/{} forwarded verbatim", CASES.len());
    for (name, sent, got) in &details {
        println!();
        println!("[{name}]");
        println!("  sent: {sent}");
        println!("  read: {got}");
    }

    // The suite's other dependency on the host: whether a `sh` resolves at
    // all. Reported, not judged — #249 is what removes the dependency.
    let mut sh = CommandBuilder::new("sh");
    sh.args(["-c", "echo sh-ok"]);
    println!();
    match through_pty(sh) {
        Ok(out) if contains(&out, b"sh-ok") => println!("sh: resolves and runs"),
        Ok(out) => println!("sh: spawned, but printed {}", escape(&out)),
        Err(e) => println!("sh: does not spawn ({e})"),
    }
    Ok(())
}
