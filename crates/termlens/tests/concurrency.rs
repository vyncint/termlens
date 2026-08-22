//! Many terminals at once, which is what `cargo test` does by default.
//!
//! A suite runs one test per core, so a sixteen-core machine opens sixteen
//! PTYs at once and closes them just as fast. On macOS that is not merely
//! concurrent, it is a *queue*: devices are torn down with `revoke()` and
//! recycled, and asking for one faster than the kernel returns it fails with
//! `ENXIO` — "Device not configured", which reads like a broken machine.
//!
//! The stress workflow found it at sixteen threads on macOS while Linux ran
//! the same suite twenty-five times over without noticing. This is that
//! failure written down, so it is reproducible on demand rather than once in
//! a while in whichever shard drew the short straw.

use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use termlens::{Key, Terminal};

/// More than any runner has cores, and more than the default `--test-threads`
/// on the largest machine anyone is likely to be sitting at.
const AT_ONCE: usize = 24;

#[test]
fn two_dozen_terminals_open_at_once() {
    let (report, results) = mpsc::channel();
    let mut threads = Vec::with_capacity(AT_ONCE);

    for index in 0..AT_ONCE {
        let report = report.clone();
        threads.push(thread::spawn(move || {
            let outcome = (|| -> termlens::Result<()> {
                let mut terminal = Terminal::builder()
                    .size(40, 10)
                    .env_clear()
                    .timeout(Duration::from_secs(30))
                    .arg("-c")
                    // The `read` is the instant-exit guard: a child that
                    // writes and dies inside a millisecond can lose its
                    // output to the PTY teardown.
                    .arg(format!("printf 'terminal {index}\\n'; read _"))
                    .spawn("/bin/sh")?;
                terminal.wait_until(|screen| screen.contains(&format!("terminal {index}")))?;
                terminal.send(Key::Enter)?;
                terminal.wait_exit()?;
                Ok(())
            })();
            report
                .send((index, outcome))
                .expect("the collector is alive");
        }));
    }
    drop(report);

    // Collected rather than asserted per thread, so one failure names itself
    // instead of arriving as a panic from a thread nobody is watching.
    let mut failures = Vec::new();
    for (index, outcome) in results {
        if let Err(error) = outcome {
            failures.push(format!("terminal {index}: {error}"));
        }
    }
    for thread in threads {
        thread.join().expect("no thread panicked");
    }

    assert!(
        failures.is_empty(),
        "{} of {AT_ONCE} terminals failed to run:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// The same pressure applied in waves rather than all at once: open, tear
/// down, open again. Recycling is what macOS is slow at, so churn is a
/// different shape of the same question from a single wide burst.
#[test]
fn terminals_recycle_without_running_out_of_devices() {
    for round in 0..8 {
        let mut open = Vec::new();
        for index in 0..6 {
            let mut terminal = Terminal::builder()
                .size(20, 5)
                .env_clear()
                .timeout(Duration::from_secs(30))
                .arg("-c")
                .arg(format!("printf 'round {round} {index}\\n'; read _"))
                .spawn("/bin/sh")
                .unwrap_or_else(|error| panic!("round {round}, terminal {index}: {error}"));
            terminal
                .wait_until(|screen| screen.contains(&format!("round {round} {index}")))
                .unwrap_or_else(|error| panic!("round {round}, terminal {index}: {error}"));
            open.push(terminal);
        }
        // Dropped together, which is the burst of teardowns the next round's
        // opens have to survive.
        drop(open);
    }
}
