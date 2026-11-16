//! Shared helpers for integration tests (not a test binary itself).

use std::path::PathBuf;
use std::process::Command;
use std::sync::Mutex;

use termlens::{Terminal, TerminalBuilder};

/// Spawn the `emit` fixture with `steps` from a builder the caller has
/// already sized and timed: what `sh -c 'printf …; read _'` used to be,
/// with no shell deciding how `printf` reads an escape (#249). The steps
/// are documented in `fixtures/emit/src/steps.txt`, which is both the
/// fixture's module header and its `--help`; `check-emit-steps.sh` holds
/// that list against the steps the fixture implements.
///
/// This module is compiled into every test binary that declares it, and
/// not every one of them uses every helper.
#[allow(dead_code)]
pub(crate) fn spawn_emit(builder: TerminalBuilder, steps: &[&str]) -> termlens::Result<Terminal> {
    builder.args(steps).spawn(fixture_bin("emit"))
}

/// [`spawn_emit`] from the default builder.
#[allow(dead_code)]
pub(crate) fn emit(steps: &[&str]) -> termlens::Result<Terminal> {
    spawn_emit(Terminal::builder(), steps)
}

/// Write an executable script at `path` through `sh`, so that this process
/// never holds the file open for writing. A test binary forks all the time,
/// and a child forked while the file was open keeps a copy of the
/// descriptor until it execs. Running the script in that window fails with
/// ETXTBSY, "Text file busy", which is never the failure under test.
#[cfg(unix)]
#[allow(dead_code)]
pub(crate) fn write_script(path: &std::path::Path, contents: &str) {
    let status = Command::new("sh")
        .args(["-c", r#"printf '%s' "$2" > "$1" && chmod 755 "$1""#, "sh"])
        .arg(path)
        .arg(contents)
        .status()
        .expect("sh runs");
    assert!(status.success(), "writing {} failed", path.display());
}

/// Build one binary or example with Cargo and return the path Cargo gives
/// for it. `target` selects it (`["-p", "termlens", "--example", "inspect"]`)
/// and `name` is the target's name.
///
/// The path is read from Cargo's own `compiler-artifact` message. It used to
/// be worked out from where this test binary sits, two directories up from
/// `target/<profile>/deps/`, and Cargo 1.100 moved test binaries to
/// `target/<profile>/build/<package>/<hash>/out/`: every test that ran a
/// binary found that way failed with `NotFound`. Where Cargo keeps a test
/// binary is its own business; where it put what it was asked to build is
/// something it reports.
///
/// The build uses this test's profile, as [`fixture_bin`] does, so a
/// `--release` test run measures a release binary.
#[allow(dead_code)]
pub(crate) fn cargo_built(target: &[&str], name: &str) -> PathBuf {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let mut cmd = Command::new(cargo);
    cmd.arg("build").args(target);
    if !cfg!(debug_assertions) {
        cmd.arg("--release");
    }
    // The messages go to stdout; the build's own progress and any compile
    // error stay on stderr, where a failing test shows them.
    let out = cmd
        .arg("--message-format=json")
        .stderr(std::process::Stdio::inherit())
        .output()
        .expect("failed to run cargo build");
    assert!(out.status.success(), "cargo build {target:?} failed");
    let stdout = String::from_utf8(out.stdout).expect("cargo's messages are UTF-8");
    built_executable(&stdout, name)
        .unwrap_or_else(|| panic!("cargo build {target:?} reported no executable named {name}"))
}

/// The executable Cargo reports for the target `name` in a
/// `--message-format=json` stream: the `executable` of its
/// `compiler-artifact` message. A library, or a target that was not built,
/// has none.
#[allow(dead_code)]
pub(crate) fn built_executable(messages: &str, name: &str) -> Option<PathBuf> {
    messages
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|message| {
            message["reason"] == "compiler-artifact" && message["target"]["name"] == name
        })
        .filter_map(|message| message["executable"].as_str().map(PathBuf::from))
        .next_back()
}

/// Fixture names already rebuilt by this test process.
static BUILT: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Set on a run of this test binary that [`rerun_self`] started: the
/// process that started it has built every fixture already.
const FIXTURES_BUILT: &str = "TERMLENS_TEST_FIXTURES_BUILT";

/// A command that runs this test binary again, for a test that needs a
/// process of its own (its own environment, or a second writer of the same
/// artifact). The fixtures the run spawns are built here first, and the
/// run is told so, and builds none: its `cargo build` would relink each
/// one, removing the file before linking it again, while a test in this
/// process runs it, which then finds nothing there (#278 again, on three
/// macOS stress shards of the v0.11.9 release commit).
#[allow(dead_code)]
pub(crate) fn rerun_self(fixtures: &[&str]) -> std::io::Result<Command> {
    for name in fixtures {
        fixture_bin(name);
    }
    let mut cmd = Command::new(std::env::current_exe()?);
    cmd.env(FIXTURES_BUILT, "1");
    Ok(cmd)
}

/// Path to a fixture binary (a sibling workspace member, so
/// `CARGO_BIN_EXE_*` is unavailable). Always runs `cargo build -p`
/// once per fixture per test process: `cargo test` links test
/// harnesses into `deps/` but does not refresh the plain bin artifact,
/// so an existing `target/<profile>/<name>` can be stale and would
/// silently test old fixture code. The build no-ops in milliseconds
/// when everything is fresh (and stress.yml prebuilds --all-targets). A
/// run started by [`rerun_self`] builds nothing: its parent has.
pub(crate) fn fixture_bin(name: &str) -> PathBuf {
    let profile = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };
    let target_dir = std::env::var_os("CARGO_TARGET_DIR").map_or_else(
        || {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../..")
                .join("target")
        },
        PathBuf::from,
    );
    // `.exe` on Windows, nothing elsewhere — the same rule Cargo used to
    // name the artifact this path has to find.
    let bin = target_dir
        .join(profile)
        .join(format!("{name}{}", std::env::consts::EXE_SUFFIX));

    let mut built = BUILT.lock().unwrap();
    if !built.iter().any(|b| b == name) && std::env::var_os(FIXTURES_BUILT).is_none() {
        let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
        let mut cmd = Command::new(cargo);
        cmd.args(["build", "-p", name]);
        if profile == "release" {
            cmd.arg("--release");
        }
        let status = cmd.status().expect("failed to run cargo build");
        assert!(status.success(), "cargo build -p {name} failed");
        built.push(name.to_owned());
    }
    drop(built);

    assert!(bin.exists(), "fixture binary missing at {}", bin.display());
    bin
}
