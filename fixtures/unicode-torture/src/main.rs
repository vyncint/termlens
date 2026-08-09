//! termtest fixture: prints a fixed set of width-hostile lines and exits.
//!
//! Covers: plain ASCII, double-width CJK, emoji (including a ZWJ family and
//! a regional-indicator flag), Vietnamese in both NFC and NFD normalization
//! (the NFD line is spelled with explicit escapes so the source file's
//! encoding can never change the bytes), and a wide-vs-narrow width ruler.
//!
//! No TTY interaction, no timing, no randomness — print and exit 0.

fn main() {
    println!("ascii: the quick brown fox");
    println!("cjk: 你好 世界 漢字");
    println!("emoji: 🦀 crab, family 👩\u{200d}👩\u{200d}👧\u{200d}👦, flag 🇻🇳");
    println!("viet-nfc: Tiếng Việt — cà phê sữa đá");
    println!("viet-nfd: Tie\u{0302}\u{0301}ng Vie\u{0323}\u{0302}t");
    println!("width: |一二三| vs |abc|");
    println!("done");
}
