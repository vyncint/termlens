//! Query replies under load: a startup batch of probes must be answered in
//! full, and an application that genuinely never reads must be diagnosed
//! rather than quietly shorted.

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
    // 200 and 400 are the sizes the issue measured losses at; 1000 is where
    // it recorded 285 of 1000 answered.
    for n in [50usize, 200, 400, 1000] {
        assert_eq!(answered(n)?, n, "asked {n} probes back to back");
    }
    Ok(())
}

/// An application that never reads its input is a different case, and the one
/// the bounded queue exists for. It must be *told*, in every wait error,
/// rather than quietly shorted — a discarded answer that surfaces nowhere is
/// indistinguishable from a query never asked.
#[test]
fn an_application_that_never_reads_is_named_in_the_error() -> termlens::Result<()> {
    // Thousands of queries, and a child that never reads a byte back.
    let script = "stty -icanon -echo; i=0; \
                  while [ $i -lt 4000 ]; do printf '\\033[6n'; i=$((i+1)); done; \
                  printf ASKED; while true; do sleep 5; done";
    let mut t = Terminal::builder()
        .size(80, 6)
        .timeout(Duration::from_millis(900))
        .args(["-c", script])
        .spawn("/bin/sh")?;

    let err = t
        .wait_until(|s| s.contains("NEVER-APPEARS"))
        .expect_err("must time out");
    let msg = err.to_string();
    assert!(
        msg.contains("not reading its input"),
        "the cause must be named: {msg}"
    );
    assert!(
        msg.contains("could not be delivered"),
        "and the undelivered replies counted: {msg}"
    );
    Ok(())
}
