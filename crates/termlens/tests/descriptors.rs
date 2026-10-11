//! What a child inherits from the test process: its terminal, and nothing
//! termlens holds. A binary of its own, because the assertions are about the
//! whole process's descriptor table, which tests running beside them would
//! change.
#![cfg(unix)]

use std::collections::BTreeSet;
use std::os::fd::AsRawFd;
use std::process::Command;
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use termlens::Terminal;

/// The tests below measure and change one descriptor table, so they take
/// turns: one leaves a descriptor inheritable that the other would count.
static TABLE: Mutex<()> = Mutex::new(());

/// The descriptors an ordinary child of this process has open, as `ls`
/// lists them. The child's own listing descriptor is among them, the same
/// one each time.
fn in_an_ordinary_child() -> BTreeSet<u32> {
    let out = Command::new("ls").arg("/dev/fd").output().expect("ls runs");
    assert!(out.status.success(), "{out:?}");
    String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .map(|fd| fd.parse().expect("a descriptor number"))
        .collect()
}

/// A shell command that prints whether descriptor `fd` is open in it.
fn probe(fd: i32) -> String {
    format!("if [ -e /dev/fd/{fd} ]; then echo inherited; else echo closed; fi")
}

/// A terminal alive in the test process adds nothing to what the process's
/// other children inherit. The writer termlens duplicates from the master
/// was a plain `dup`, so every child started while a terminal lived held
/// that master open.
#[test]
fn a_live_terminal_leaks_no_descriptor_into_an_ordinary_child() -> termlens::Result<()> {
    let _table = TABLE.lock().unwrap_or_else(PoisonError::into_inner);
    let before = in_an_ordinary_child();
    let t = Terminal::builder()
        .timeout(Duration::from_secs(10))
        .spawn("cat")?;
    let during = in_an_ordinary_child();
    drop(t);
    assert_eq!(
        during, before,
        "a descriptor of the live terminal reached an unrelated child"
    );
    Ok(())
}

/// A child of termlens gets stdio and nothing else, even from a test process
/// holding a descriptor that any other child would inherit. The PTY layer
/// closed every such descriptor between fork and exec. termlens, which now
/// spawns the child itself, marks them close-on-exec instead, so that a
/// failed exec can still report through std's pipe (#563).
#[test]
fn a_termlens_child_inherits_nothing_but_its_terminal() -> termlens::Result<()> {
    let _table = TABLE.lock().unwrap_or_else(PoisonError::into_inner);
    let file = std::fs::File::open(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"))?;
    let fd = file.as_raw_fd();
    // SAFETY: `fd` is open for as long as `file` lives, and F_SETFD changes
    // only its descriptor flags.
    #[allow(unsafe_code)]
    let cleared = unsafe { libc::fcntl(fd, libc::F_SETFD, 0) };
    assert_eq!(cleared, 0, "clearing close-on-exec on {fd}");

    let ordinary = Command::new("sh")
        .args(["-c", &probe(fd)])
        .output()
        .expect("sh runs");
    assert_eq!(
        String::from_utf8_lossy(&ordinary.stdout).trim(),
        "inherited",
        "an ordinary child must inherit {fd}, or this test proves nothing"
    );

    let mut t = Terminal::builder()
        .timeout(Duration::from_secs(10))
        .args(["-c", &probe(fd)])
        .spawn("sh")?;
    assert!(t.wait_exit()?.success());
    let screen = t.screen().text();
    assert_eq!(screen.trim(), "closed", "descriptor {fd} reached the child");
    drop(file);
    Ok(())
}
