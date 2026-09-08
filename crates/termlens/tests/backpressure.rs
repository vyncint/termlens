//! Query replies under load: a startup batch of probes must be answered in
//! full.
//!
//! The other half of this story — what a harness can know about an
//! application that never reads its replies — lives in `drain.rs`, next to
//! the deadlock it must never cause.
//!

use std::time::Duration;

use termlens::{Key, Terminal};

mod common;

/// Ask `n` cursor-position queries back to back, then read everything, and
/// count the answers. Each DSR reply carries exactly one `R`.
///
/// Read in **one continuous read of exactly the expected byte count**, and
/// the shape of that read is load-bearing in both directions — stress taught
/// me twice.
///
/// A single read sized by a *timeout* stops at the first gap longer than its
/// `VTIME`, so on a busy runner it ends early and measures scheduling: 310 of
/// 400 on macOS. Replacing it with a retry loop was worse, and for a reason
/// worth remembering — each retry was a `fork`+`exec` of `dd`, and **nothing
/// read the terminal in between**, so on a slow runner the input queue
/// overflowed in those gaps and the kernel discarded: 235 of 400.
///
/// Exactly-sized works because every reply here is `ESC[1;1R`, six bytes,
/// identical — the cursor cannot move while the queries are being asked,
/// since a DSR query prints nothing. So `--read-count` returns the instant
/// it has them all, with no gap for the queue to overflow through, and the
/// terminal's deadline bounds it if answers really are missing.
fn answered(n: usize) -> termlens::Result<usize> {
    let queries = r"\e[6n".repeat(n);
    let expected = (n * 6).to_string();
    let mut t = common::spawn_emit(
        Terminal::builder()
            .size(80, 6)
            .timeout(Duration::from_secs(30)),
        &[
            "--raw-mode",
            "--raw",
            &queries,
            "ASKED GOT[",
            "--read-count",
            &expected,
            "R",
            "] DONE",
            "--wait",
        ],
    )?;
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
    // 400 is also the size that failed on a loaded macOS runner at 235
    // answered, twice, while the queue was bounded by slots rather than
    // bytes — see `fine_grained_arrival_is_answered_in_full` for why.
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

/// The shape that defeated a slot-counted queue: queries arriving across
/// *many small reads* instead of one.
///
/// Batching per read fixed the original loss on a fast machine, where a
/// startup batch lands in a single read. On a slow one the application's
/// writes dribble out, the same 400 queries arrive in hundreds of reads, and
/// 64 slots ran out again — 235 of 400 on a loaded macOS runner, at the same
/// number twice. One `--raw` step per query reproduces that arrival shape on
/// purpose: every step is its own `write(2)`.
#[test]
fn fine_grained_arrival_is_answered_in_full() -> termlens::Result<()> {
    for n in [100usize, 400] {
        let expected = (n * 6).to_string();
        let mut steps: Vec<&str> = vec!["--raw-mode"];
        for _ in 0..n {
            steps.extend(["--raw", r"\e[6n"]);
        }
        steps.extend([
            "ASKED GOT[",
            "--read-count",
            &expected,
            "R",
            "] DONE",
            "--wait",
        ]);
        let mut t = common::spawn_emit(
            Terminal::builder()
                .size(80, 6)
                .timeout(Duration::from_secs(40)),
            &steps,
        )?;
        t.wait_until(|s| s.contains("DONE"))?;
        let text = t.screen().text();
        let got: usize = text
            .split("GOT[")
            .nth(1)
            .and_then(|s| s.split(']').next())
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(usize::MAX);
        assert_eq!(got, n, "one read per query must not lose answers");
        t.send(Key::Enter)?;
        assert!(t.wait_exit()?.success());
    }
    Ok(())
}
