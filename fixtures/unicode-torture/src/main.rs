//! termlens fixture: prints a fixed set of width-hostile lines and exits.
//!
//! Covers: plain ASCII, double-width CJK, emoji (including a ZWJ family and
//! a regional-indicator flag), Vietnamese in both NFC and NFD normalization
//! (the NFD line is spelled with explicit escapes so the source file's
//! encoding can never change the bytes), a wide-vs-narrow width ruler, and
//! one line carrying a raw byte that is not UTF-8.
//!
//! No timing, no randomness — print, wait for one line on stdin, exit 0.
//! The stdin guard exists because output written immediately before exit
//! can be discarded by macOS's pty teardown; the harness observes the
//! lines, then sends Enter to release the fixture.

use std::io::Write;

fn main() {
    println!("ascii: the quick brown fox");
    println!("cjk: 你好 世界 漢字");
    println!("emoji: 🦀 crab, family 👩\u{200d}👩\u{200d}👧\u{200d}👦, flag 🇻🇳");
    println!("viet-nfc: Tiếng Việt — cà phê sữa đá");
    println!("viet-nfd: Tie\u{0302}\u{0301}ng Vie\u{0323}\u{0302}t");
    println!("width: |一二三| vs |abc|");
    // A byte that is not UTF-8: a Latin-1 `é` in what should have been
    // text. A string literal cannot hold it, so the bytes go out directly.
    // The grid must show U+FFFD here and keep `done` in the column it would
    // occupy had the byte decoded; the sanitizer used to drop it, shifting
    // every column after it left (#217, #230).
    std::io::stdout()
        .write_all(b"raw: caf\xe9 done\n")
        .expect("stdout is writable");
    println!("done");

    let mut guard = String::new();
    let _ = std::io::stdin().read_line(&mut guard);
}
