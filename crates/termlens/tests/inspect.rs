//! End-to-end coverage for the `inspect` debugging example.

use std::path::PathBuf;
use std::process::{Command, Output};

fn inspect_bin() -> PathBuf {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let status = Command::new(cargo)
        .args(["build", "-p", "termlens", "--example", "inspect"])
        .status()
        .expect("failed to run cargo build for the inspect example");
    assert!(status.success(), "cargo build --example inspect failed");

    let test_exe = std::env::current_exe().expect("test executable path is available");
    let profile_dir = test_exe
        .parent()
        .and_then(|deps| deps.parent())
        .expect("test executable is under target/<profile>/deps");
    profile_dir
        .join("examples")
        .join(format!("inspect{}", std::env::consts::EXE_SUFFIX))
}

fn run_inspect(bin: &PathBuf, args: &[&str]) -> Output {
    Command::new(bin)
        .args(args)
        .output()
        .expect("failed to run the inspect example")
}

#[test]
fn inspect_runs_and_reports_cli_failures() {
    let bin = inspect_bin();

    let sized = run_inspect(&bin, &["--size", "12x3", "sh", "-c", "stty size"]);
    assert!(
        sized.status.success(),
        "inspect failed: {}",
        String::from_utf8_lossy(&sized.stderr)
    );
    let stdout = String::from_utf8_lossy(&sized.stdout);
    assert!(
        stdout.contains("3 12"),
        "terminal size missing from:\n{stdout}"
    );
    assert!(
        stdout.contains("--- exited: exit code 0 ---"),
        "exit status missing from:\n{stdout}"
    );

    let bad_size = run_inspect(&bin, &["--size", "12", "sh"]);
    assert_eq!(bad_size.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&bad_size.stderr).contains("expected e.g. 120x40"),
        "malformed-size error missing from:\n{}",
        String::from_utf8_lossy(&bad_size.stderr)
    );

    let missing_program = run_inspect(&bin, &["/definitely/not/a/program"]);
    assert_eq!(missing_program.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&missing_program.stderr).starts_with("inspect:"),
        "spawn error missing from:\n{}",
        String::from_utf8_lossy(&missing_program.stderr)
    );
}

#[test]
fn inspect_survives_a_reader_that_closes_early() {
    use std::io::Read;
    use std::process::Stdio;

    // 200x1000 cells is far more than a pipe holds, so once the read end is
    // closed the write gets EPIPE — which println! turned into a panic and
    // exit 101 (#223). A viewer piped into `head` must exit cleanly.
    let bin = inspect_bin();
    let mut child = Command::new(&bin)
        .args(["--size", "200x1000", "sh", "-c", "yes | head -n 2000"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn the inspect example");
    let mut stdout = child.stdout.take().expect("stdout is piped");
    let mut first = [0u8; 16];
    let _ = stdout.read(&mut first);
    drop(stdout);
    let status = child.wait().expect("inspect did not exit");
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .expect("stderr is piped")
        .read_to_string(&mut stderr)
        .expect("stderr is readable");
    assert!(!stderr.contains("panicked"), "inspect panicked:\n{stderr}");
    assert_eq!(status.code(), Some(0), "stderr:\n{stderr}");
}

/// `--help` is the first thing anyone types at an unfamiliar command; it
/// used to be spawned as a program called `--help` (#229). The usage text
/// has one home, so the no-program path prints the same words to stderr.
#[test]
fn inspect_prints_its_usage_for_help_and_for_a_missing_program() {
    let bin = inspect_bin();

    for flag in ["--help", "-h"] {
        let help = run_inspect(&bin, &[flag]);
        assert_eq!(
            help.status.code(),
            Some(0),
            "{flag} is a successful request"
        );
        let stdout = String::from_utf8_lossy(&help.stdout);
        assert!(
            stdout.starts_with(
                "usage: inspect [--size COLSxROWS] [--timeout SECONDS] [--idle MILLIS]"
            ),
            "{flag} must print the usage to stdout, got:\n{stdout}"
        );
        assert!(help.stderr.is_empty(), "{flag} wrote to stderr");
    }

    let version = run_inspect(&bin, &["--version"]);
    assert_eq!(version.status.code(), Some(0));
    assert!(
        String::from_utf8_lossy(&version.stdout)
            .contains(&format!("termlens {}", env!("CARGO_PKG_VERSION"))),
        "--version names the crate version"
    );

    let none = run_inspect(&bin, &[]);
    assert_eq!(
        none.status.code(),
        Some(1),
        "no program is still a usage error"
    );
    assert!(none.stdout.is_empty());
    assert!(
        String::from_utf8_lossy(&none.stderr).starts_with("usage: inspect"),
        "the same usage goes to stderr:\n{}",
        String::from_utf8_lossy(&none.stderr)
    );

    let unknown = run_inspect(&bin, &["--bogus", "sh"]);
    assert_eq!(unknown.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&unknown.stderr).contains("unknown option \"--bogus\""),
        "an unknown option is refused rather than spawned:\n{}",
        String::from_utf8_lossy(&unknown.stderr)
    );
}

/// Both timings are flags now (#236): a malformed value is rejected in one
/// line the way `--size` rejects one, and the deadline is honoured — a
/// program slower than the default five seconds can be cut off at one.
#[test]
fn inspect_takes_its_deadline_and_silence_window_from_flags() {
    let bin = inspect_bin();

    for (args, expect) in [
        (
            &["--timeout", "abc", "sh"][..],
            "bad --timeout \"abc\", expected e.g. 30",
        ),
        (
            &["--idle", "1.5", "sh"][..],
            "bad --idle \"1.5\", expected e.g. 1000",
        ),
        (&["--timeout"][..], "--timeout needs a SECONDS argument"),
        (&["--idle"][..], "--idle needs a MILLIS argument"),
    ] {
        let out = run_inspect(&bin, args);
        assert_eq!(out.status.code(), Some(1), "{args:?}");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains(expect), "{args:?}: got {stderr:?}");
        assert_eq!(
            stderr.lines().count(),
            1,
            "one line, like --size: {stderr:?}"
        );
    }

    // A one-second deadline against a program that sleeps for thirty:
    // inspect must report "still running" long before the default five
    // seconds would have, with the output painted before the deadline
    // still on the screen it prints.
    let started = std::time::Instant::now();
    let cut = run_inspect(
        &bin,
        &[
            "--timeout",
            "1",
            "--idle",
            "50",
            "sh",
            "-c",
            "echo painted; sleep 30",
        ],
    );
    let elapsed = started.elapsed();
    assert!(
        cut.status.success(),
        "{}",
        String::from_utf8_lossy(&cut.stderr)
    );
    let stdout = String::from_utf8_lossy(&cut.stdout);
    assert!(stdout.contains("painted"), "{stdout}");
    assert!(
        stdout.contains("--- still running at the deadline (killed on exit) ---"),
        "{stdout}"
    );
    assert!(
        elapsed < std::time::Duration::from_secs(4),
        "a 1s deadline took {elapsed:?}; the flag was not honoured"
    );
}

/// A relative program path is how `inspect` is pointed at something just
/// built (`inspect ./target/debug/myapp`), and it resolves only because a
/// child starts in the test process's working directory rather than in
/// `$HOME` (#215). The test pinning that default reads `pwd` inside a
/// shell; this one pins the mechanism the viewer actually relies on (#237).
#[cfg(unix)]
#[test]
fn inspect_resolves_a_relative_program_path_from_its_working_directory() {
    let bin = inspect_bin();
    let scratch = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("inspect-relative");
    std::fs::create_dir_all(&scratch).expect("scratch directory");
    // A real program linked into the scratch directory, rather than `sh -c`,
    // which resolves its own arguments and would test the shell instead of
    // termlens. A symlink rather than a copy: macOS refuses to run a system
    // binary copied out of `/bin` (its signature is trusted at that path
    // only), and a multi-call `echo` keeps its own name this way.
    let echo = scratch.join("echo");
    let _ = std::fs::remove_file(&echo);
    std::os::unix::fs::symlink("/bin/echo", &echo)
        .expect("link /bin/echo into the scratch directory");

    let out = Command::new(&bin)
        .current_dir(&scratch)
        .args(["./echo", "relative path resolved"])
        .output()
        .expect("failed to run the inspect example");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "inspect failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(stdout.contains("relative path resolved"), "{stdout}");
    assert!(stdout.contains("--- exited: exit code 0 ---"), "{stdout}");
}
