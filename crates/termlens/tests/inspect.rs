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
