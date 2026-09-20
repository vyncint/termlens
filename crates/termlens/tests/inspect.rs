//! End-to-end coverage for the `inspect` debugging example, and for its
//! agreement with `termlens inspect`, the command it mirrors (#480).

use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::OnceLock;

/// Built once per test process, whichever test asks first. Every test used
/// to run its own `cargo build`, and two of them in parallel could race a
/// relink: one unlinks and rewrites the example while the other's
/// `Command::new` finds nothing there — `NotFound`, once, on a macOS runner
/// (#278). Same shape as `common::fixture_bin`'s guard, for the same reason.
fn inspect_bin() -> &'static PathBuf {
    static BIN: OnceLock<PathBuf> = OnceLock::new();
    BIN.get_or_init(|| {
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
    })
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

    let sized = run_inspect(bin, &["--size", "12x3", "sh", "-c", "stty size"]);
    assert!(
        sized.status.success(),
        "inspect failed: {}",
        String::from_utf8_lossy(&sized.stderr)
    );
    let stdout = String::from_utf8_lossy(&sized.stdout);
    let stderr = String::from_utf8_lossy(&sized.stderr);
    assert!(
        stdout.contains("3 12"),
        "terminal size missing from:\n{stdout}"
    );
    assert!(
        stderr.contains("--- exited: exit code 0 ---"),
        "exit status missing from:\n{stderr}"
    );
    assert!(
        !stdout.contains("---"),
        "the trailer belongs on stderr, so stdout stays a saved screen:\n{stdout}"
    );
    termlens::Screen::parse(&stdout).expect("stdout is a saved screen");

    // `--ansi` on a pipe is still a saved screen (#478): the example
    // follows the command, so a redirect must parse, with the colour
    // kept as a `styles:` block rather than as C0.
    let ansi = run_inspect(
        bin,
        &[
            "--size",
            "30x3",
            "--ansi",
            "sh",
            "-c",
            r#"printf '\033[1;31mred\033[0m'"#,
        ],
    );
    assert!(
        ansi.status.success(),
        "inspect --ansi failed: {}",
        String::from_utf8_lossy(&ansi.stderr)
    );
    let stdout = String::from_utf8_lossy(&ansi.stdout);
    assert!(
        !stdout.contains('\u{1b}'),
        "a redirect must not write C0:\n{stdout}"
    );
    let parsed = termlens::Screen::parse(&stdout).expect("stdout is a saved screen");
    assert_eq!(parsed.find("red"), Some((0, 0)));
    let cell = parsed.cell(0, 0).expect("the painted cell");
    assert_eq!(cell.style().fg, termlens::Color::Indexed(1));
    assert!(cell.style().bold);

    let bad_size = run_inspect(bin, &["--size", "12", "sh"]);
    assert_eq!(bad_size.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&bad_size.stderr).contains("expected e.g. 120x40"),
        "malformed-size error missing from:\n{}",
        String::from_utf8_lossy(&bad_size.stderr)
    );

    let bad_cwd = run_inspect(bin, &["--cwd", "/definitely/not/a/directory", "sh"]);
    assert_eq!(bad_cwd.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&bad_cwd.stderr);
    assert!(
        stderr.contains("bad --cwd") && stderr.contains("not an existing directory"),
        "malformed-cwd error missing from:\n{stderr}"
    );

    let missing_program = run_inspect(bin, &["/definitely/not/a/program"]);
    assert_eq!(missing_program.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&missing_program.stderr).starts_with("inspect:"),
        "spawn error missing from:\n{}",
        String::from_utf8_lossy(&missing_program.stderr)
    );
}

#[test]
fn inspect_clears_and_selectively_sets_the_child_environment() {
    let bin = inspect_bin();
    let output = Command::new(bin)
        .env("TERMLENS_INSPECT_LEAK", "secret")
        .args(["--env", "KEPT=yes"])
        .args(["sh", "-c", "printf ${TERMLENS_INSPECT_LEAK-unset}:$KEPT"])
        .output()
        .expect("inspect runs");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("unset:yes"),
        "cleared/selected environment missing from:\n{stdout}"
    );

    let inherited = Command::new(bin)
        .env("TERMLENS_INSPECT_INHERITED", "yes")
        .args(["--inherit-env"])
        .args(["sh", "-c", "printf inherited:$TERMLENS_INSPECT_INHERITED"])
        .output()
        .expect("inspect runs");
    let stdout = String::from_utf8_lossy(&inherited.stdout);
    assert!(
        stdout.contains("inherited:yes"),
        "inherited environment missing from:\n{stdout}"
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
    let mut child = Command::new(bin)
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
        let help = run_inspect(bin, &[flag]);
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

    let version = run_inspect(bin, &["--version"]);
    assert_eq!(version.status.code(), Some(0));
    assert_eq!(
        String::from_utf8_lossy(&version.stdout),
        format!("termlens {}\n", env!("CARGO_PKG_VERSION")),
        "--version prints the CLI's one version line"
    );

    let none = run_inspect(bin, &[]);
    assert_eq!(
        none.status.code(),
        Some(2),
        "no program is still a usage error"
    );
    assert!(none.stdout.is_empty());
    assert!(
        String::from_utf8_lossy(&none.stderr).starts_with("usage: inspect"),
        "the same usage goes to stderr:\n{}",
        String::from_utf8_lossy(&none.stderr)
    );

    let unknown = run_inspect(bin, &["--bogus", "sh"]);
    assert_eq!(unknown.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&unknown.stderr).contains("unknown option \"--bogus\""),
        "an unknown option is refused rather than spawned:\n{}",
        String::from_utf8_lossy(&unknown.stderr)
    );

    // `--flag=value` is for the flags that take a value; a value on a flag
    // that takes none is refused with the whole token named, so the example
    // and the command agree on the spelling they both gained (#366).
    let sized = run_inspect(bin, &["--size=12x3", "sh", "-c", "stty size"]);
    assert!(
        sized.status.success(),
        "the = spelling works as the two-argument form:\n{}",
        String::from_utf8_lossy(&sized.stderr)
    );
    let stdout = String::from_utf8_lossy(&sized.stdout);
    assert!(
        stdout.contains("3 12"),
        "spawned at the given size:\n{stdout}"
    );

    let valued = run_inspect(bin, &["--inherit-env=nonsense", "sh"]);
    assert_eq!(valued.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&valued.stderr)
            .contains("unknown option \"--inherit-env=nonsense\""),
        "a value on a value-less flag names the whole token:\n{}",
        String::from_utf8_lossy(&valued.stderr)
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
        let out = run_inspect(bin, args);
        assert_eq!(out.status.code(), Some(2), "{args:?}");
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
    // seconds would have, with the output painted before the wait ended
    // still on the screen it prints. The 50ms silence window is what ends
    // it now (#374), inside the 1s bound rather than at it.
    let started = std::time::Instant::now();
    let cut = run_inspect(
        bin,
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
    let stderr = String::from_utf8_lossy(&cut.stderr);
    assert!(stdout.contains("painted"), "{stdout}");
    assert!(
        stderr.contains("--- still running (killed on exit) ---"),
        "the 50ms window ended the wait, so the trailer does not claim the \
         1s deadline did: {stderr}"
    );
    assert!(
        !stdout.contains("---"),
        "the trailer belongs on stderr:\n{stdout}"
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

    let out = Command::new(bin)
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
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("--- exited: exit code 0 ---"),
        "exit status missing from stderr"
    );

    // `--cwd` moves the child without moving the viewer (#312): started
    // wherever the test runs, the relative program resolves in the scratch
    // directory all the same.
    let out = Command::new(bin)
        .args([
            "--cwd",
            scratch.to_str().expect("utf-8 scratch path"),
            "./echo",
            "cwd resolved",
        ])
        .output()
        .expect("failed to run the inspect example");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "inspect failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(stdout.contains("cwd resolved"), "{stdout}");
    assert!(
        !stdout.contains("---"),
        "the trailer belongs on stderr:\n{stdout}"
    );
}

// ------------------------------------------------- parity with the command
//
// `examples/inspect.rs` is kept in step with `termlens inspect` by hand: the
// two files carry separate copies of the flag parser, the usage text,
// `REAP_GRACE` and the wait, and it has already drifted twice (#443, #465).
// The tests below run both binaries on the same program and compare what the
// contract promises — the screen on stdout, the trailer on stderr, the exit
// code — so a change to one that the other did not follow fails here rather
// than in review.
//
// The usage text and the diagnostics are deliberately not compared: they
// name the tool (`inspect` vs `termlens inspect`) and may legitimately differ
// in prose (#453). Exit codes and the stream a failure uses are the contract.

/// `termlens inspect`, built once per test process for the same reason
/// `inspect_bin` is (#278). It lives in `termlens-cli`, so `cargo test -p
/// termlens` does not build it; the test asks for it by name.
fn command_bin() -> &'static PathBuf {
    static BIN: OnceLock<PathBuf> = OnceLock::new();
    BIN.get_or_init(|| {
        let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
        let status = Command::new(cargo)
            .args(["build", "-p", "termlens-cli", "--bin", "termlens"])
            .status()
            .expect("failed to run cargo build for the termlens command");
        assert!(status.success(), "cargo build --bin termlens failed");

        let test_exe = std::env::current_exe().expect("test executable path is available");
        let profile_dir = test_exe
            .parent()
            .and_then(|deps| deps.parent())
            .expect("test executable is under target/<profile>/deps");
        profile_dir.join(format!("termlens{}", std::env::consts::EXE_SUFFIX))
    })
}

/// The example and `termlens inspect` run on the same arguments, side by
/// side: both children are alive at once, so a pair costs the slower of the
/// two rather than the sum.
fn run_both(args: &[&str]) -> (Output, Output) {
    let example = inspect_bin();
    let command = command_bin();
    std::thread::scope(|scope| {
        let example = scope.spawn(|| run_inspect(example, args));
        let command = scope.spawn(|| {
            Command::new(command)
                .arg("inspect")
                .args(args)
                .output()
                .expect("failed to run termlens inspect")
        });
        (
            example.join().expect("the example run panicked"),
            command.join().expect("the command run panicked"),
        )
    })
}

/// Run both on `args` and require the same screen, the same trailer and the
/// same exit code. Returns the shared `(stdout, stderr)` so a caller can also
/// pin what they are — two binaries that agree on nothing useful (both
/// failing to spawn, say) must not pass as agreeing.
fn agree(args: &[&str]) -> (String, String) {
    let (example, command) = run_both(args);
    let text = |bytes: &[u8]| String::from_utf8_lossy(bytes).into_owned();
    let (ex_out, ex_err) = (text(&example.stdout), text(&example.stderr));
    let (cmd_out, cmd_err) = (text(&command.stdout), text(&command.stderr));
    assert_eq!(
        example.status.code(),
        command.status.code(),
        "exit codes differ for {args:?}\nexample stderr:\n{ex_err}\ncommand stderr:\n{cmd_err}"
    );
    assert_eq!(
        ex_out, cmd_out,
        "the screens on stdout differ for {args:?}\nexample:\n{ex_out}\ncommand:\n{cmd_out}"
    );
    assert_eq!(
        ex_err, cmd_err,
        "the trailers on stderr differ for {args:?}\nexample: {ex_err:?}\ncommand: {cmd_err:?}"
    );
    (ex_out, ex_err)
}

/// A program that exits has one trailer, and the screen holds its finished
/// output. Nothing here waits on a clock: the EOF that says the child is
/// gone ends the wait.
#[test]
#[cfg_attr(windows, ignore = "the program under inspection is a POSIX shell")]
fn inspect_example_and_command_agree_on_a_program_that_exits() {
    for (args, screen, trailer) in [
        (
            &["--size", "30x3", "sh", "-c", "printf hello"][..],
            "hello",
            "--- exited: exit code 0 ---\n",
        ),
        // A non-zero status is reported in the trailer, not propagated.
        (
            &["--size", "30x3", "sh", "-c", "printf bye; exit 3"][..],
            "bye",
            "--- exited: exit code 3 ---\n",
        ),
        // Styled output: a redirect is a saved screen, colour kept in a
        // `styles:` block, on both sides (#454, #478).
        (
            &[
                "--size",
                "30x3",
                "--ansi",
                "sh",
                "-c",
                r#"printf '\033[1;31mred\033[0m'"#,
            ][..],
            "red",
            "--- exited: exit code 0 ---\n",
        ),
        // The `--flag=value` spelling, which both gained together (#366).
        (
            &["--size=12x3", "sh", "-c", "stty size"][..],
            "3 12",
            "--- exited: exit code 0 ---\n",
        ),
    ] {
        let (stdout, stderr) = agree(args);
        assert!(
            stdout.contains(screen),
            "{args:?}: {screen:?} missing from the screen:\n{stdout}"
        );
        assert_eq!(stderr, trailer, "{args:?}");
        termlens::Screen::parse(&stdout).expect("stdout is a saved screen");
    }
}

/// The wait ends on whichever comes first — the program exits, or its output
/// has been silent for `--idle` — under one deadline (#465). A program that
/// paints and then sits still is resolved by the silence, long before the
/// deadline, and says so: "still running", without claiming the deadline.
///
/// This is the case the sequential wait got wrong: it spent the whole
/// `--timeout` first, so it reported the deadline for a screen that had been
/// complete for a second. The silence window is far above what a loaded
/// runner needs to start `sh` and print a word (CONTRIBUTING §3), and the
/// deadline far above the window, so neither is what the test measures.
#[test]
#[cfg_attr(windows, ignore = "the program under inspection is a POSIX shell")]
fn inspect_example_and_command_agree_when_the_silence_window_ends_the_wait() {
    let (stdout, stderr) = agree(&[
        "--size",
        "30x3",
        "--idle",
        "1000",
        "--timeout",
        "10",
        "sh",
        "-c",
        "echo painted; sleep 30",
    ]);
    assert!(stdout.contains("painted"), "{stdout}");
    assert_eq!(
        stderr, "--- still running (killed on exit) ---\n",
        "the silence window, not the deadline, ended the wait"
    );
}

/// The other way a wait can end: output that never goes quiet for `--idle`
/// runs into the deadline, and only that trailer says so (#465). Here the
/// silence window is the larger of the two, so the deadline is the only thing
/// that can end it.
#[test]
#[cfg_attr(windows, ignore = "the program under inspection is a POSIX shell")]
fn inspect_example_and_command_agree_when_the_deadline_ends_the_wait() {
    let (stdout, stderr) = agree(&[
        "--size",
        "30x3",
        "--idle",
        "30000",
        "--timeout",
        "3",
        "sh",
        "-c",
        "echo painted; sleep 30",
    ]);
    assert!(stdout.contains("painted"), "{stdout}");
    assert_eq!(
        stderr, "--- still running at the deadline (killed on exit) ---\n",
        "the deadline ended the wait"
    );
}

/// A child that closes its terminal but keeps running is not an exited
/// child: the EOF ends the wait, the reap that would say "exited" never
/// comes, and the reap grace both sides give a genuinely exited child must
/// not mislabel this one (#374, #465). The EOF is immediate, so nothing here
/// depends on a clock.
#[test]
#[cfg_attr(windows, ignore = "the program under inspection is a POSIX shell")]
fn inspect_example_and_command_agree_on_a_child_that_closes_its_terminal() {
    let (_, stderr) = agree(&[
        "--size",
        "20x3",
        "--timeout",
        "30",
        "sh",
        "-c",
        "exec 0<&- 1>&- 2>&-; exec sleep 30",
    ]);
    assert_eq!(stderr, "--- still running (killed on exit) ---\n");
}

/// Inspect itself failing is exit code 2 with nothing on stdout, whichever
/// way it fails: a flag it does not know, a value it cannot read, a flag
/// missing its value, no program at all, or a program that cannot be spawned.
/// The wording is each tool's own and is not compared.
#[test]
fn inspect_example_and_command_agree_on_exit_codes_when_inspect_cannot_run() {
    for args in [
        &["--bogus", "sh"][..],
        &["--inherit-env=nonsense", "sh"][..],
        &["--size", "12", "sh"][..],
        &["--timeout", "abc", "sh"][..],
        &["--idle"][..],
        &["--cwd", "/definitely/not/a/directory", "sh"][..],
        &[][..],
        &["/definitely/not/a/program"][..],
    ] {
        let (example, command) = run_both(args);
        assert_eq!(example.status.code(), Some(2), "example on {args:?}");
        assert_eq!(command.status.code(), Some(2), "command on {args:?}");
        assert!(example.stdout.is_empty(), "example stdout on {args:?}");
        assert!(command.stdout.is_empty(), "command stdout on {args:?}");
        assert!(!example.stderr.is_empty(), "example stderr on {args:?}");
        assert!(!command.stderr.is_empty(), "command stderr on {args:?}");
    }
}

/// `--help` and `--version` succeed on stdout with nothing on stderr, and
/// `--version` is the one line both print. The usage text itself is not
/// compared: it names the tool, and the two legitimately differ (#453).
#[test]
fn inspect_example_and_command_agree_on_help_and_version() {
    for flag in ["--help", "-h"] {
        let (example, command) = run_both(&[flag]);
        assert_eq!(example.status.code(), Some(0), "example {flag}");
        assert_eq!(command.status.code(), Some(0), "command {flag}");
        assert!(example.stderr.is_empty(), "example {flag} wrote to stderr");
        assert!(command.stderr.is_empty(), "command {flag} wrote to stderr");
        assert!(
            String::from_utf8_lossy(&example.stdout).starts_with("usage: inspect "),
            "example {flag}"
        );
        assert!(
            String::from_utf8_lossy(&command.stdout).starts_with("usage: termlens inspect "),
            "command {flag}"
        );
    }
    let (stdout, _) = agree(&["--version"]);
    assert_eq!(stdout, format!("termlens {}\n", env!("CARGO_PKG_VERSION")));
}
