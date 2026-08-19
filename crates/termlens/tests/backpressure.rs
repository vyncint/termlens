//! Query replies under load: a startup batch of probes must be answered in
//! full.
//!
//! The other half of this story — what a harness can know about an
//! application that never reads its replies — lives in `drain.rs`, next to
//! the deadlock it must never cause.

use std::time::Duration;

use termlens::{Key, Terminal};

/// Ask `n` cursor-position queries back to back, then read everything, and
/// count the answers. Each DSR reply carries exactly one `R`.
fn answered(n: usize) -> termlens::Result<usize> {
    let script = format!(
        "stty -icanon -echo min 0 time 30; i=0; \
         while [ $i -lt {n} ]; do printf '\\033[6n'; i=$((i+1)); done; \
         printf ASKED; \
         got=$(dd bs=1 count=100000 2>/dev/null | tr -cd 'R' | wc -c | tr -d ' '); \
         printf ' GOT[%s] DONE' \"$got\"; read guard"
    );
    let mut t = Terminal::builder()
        .size(80, 6)
        .timeout(Duration::from_secs(30))
        .args(["-c", &script])
        .spawn("/bin/sh")?;
    t.wait_until(|s| s.contains("DONE"))?;
    let text = t.screen().text();
    let count = text
        .split("GOT[")
        .nth(1)
        .and_then(|s| s.split(']').next())
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(usize::MAX);
    t.send(Key::Enter)?;
    assert!(t.wait_exit()?.success());
    Ok(count)
}

/// Batch-probe-then-read is a legitimate startup pattern, and it used to lose
/// answers: 200 queries asked back to back returned 173. Nothing was blocked
/// — the same 200 queries a millisecond apart were all answered — so the
/// queue was filling because the reader could build replies faster than the
/// writer issued one `write(2)` each.
#[test]
fn a_batch_of_probes_is_answered_in_full() -> termlens::Result<()> {
    // 200 and 400 are the sizes the issue measured losses at (94 and 161
    // answered).
    //
    // The ceiling here is the terminal's own input queue, not ours: 400 DSR
    // replies are ~2.4 KB, inside Linux's 4 KB `N_TTY_BUF_SIZE`, while 1000
    // would be ~6 KB and could not be delivered before the application read
    // — a real terminal blocks there too. What this pins is that nothing is
    // lost while the answers still fit, which is the bug.
    for n in [50usize, 200, 400] {
        assert_eq!(answered(n)?, n, "asked {n} probes back to back");
    }
    Ok(())
}
