//! Shared helpers for integration tests (not a test binary itself).

use std::path::PathBuf;
use std::process::Command;
use std::sync::Mutex;

/// Fixture names already rebuilt by this test process.
static BUILT: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Path to a fixture binary (a sibling workspace member, so
/// `CARGO_BIN_EXE_*` is unavailable). Always runs `cargo build -p`
/// once per fixture per test process: `cargo test` links test
/// harnesses into `deps/` but does not refresh the plain bin artifact,
/// so an existing `target/<profile>/<name>` can be stale and would
/// silently test old fixture code. The build no-ops in milliseconds
/// when everything is fresh (and stress.yml prebuilds --all-targets).
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
    let bin = target_dir.join(profile).join(name);

    let mut built = BUILT.lock().unwrap();
    if !built.iter().any(|b| b == name) {
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
