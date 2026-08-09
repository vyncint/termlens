//! termlens fixture: prints a fixed set of width-hostile lines and exits.
//!
//! Covers: plain ASCII, double-width CJK, emoji (including a ZWJ family and
//! a regional-indicator flag), Vietnamese in both NFC and NFD normalization
//! (the NFD line is spelled with explicit escapes so the source file's
//! encoding can never change the bytes), and a wide-vs-narrow width ruler.
//!
//! No timing, no randomness — print, wait for one line on stdin, exit 0.
//! The stdin guard exists because output written immediately before exit
//! can be discarded by macOS's pty teardown; the harness observes the
//! lines, then sends Enter to release the fixture.

fn main() {
    println!("ascii: the quick brown fox");
    println!("cjk: 你好 世界 漢字");
    println!("emoji: 🦀 crab, family 👩\u{200d}👩\u{200d}👧\u{200d}👦, flag 🇻🇳");
    println!("viet-nfc: Tiếng Việt — cà phê sữa đá");
    println!("viet-nfd: Tie\u{0302}\u{0301}ng Vie\u{0323}\u{0302}t");
    println!("width: |一二三| vs |abc|");
    println!("done");

    let mut guard = String::new();
    let _ = std::io::stdin().read_line(&mut guard);
}
