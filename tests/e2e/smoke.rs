//! Binary-contract smoke tests: no server, no provider, no shared state.

use nuo_test_support::{Cli, Sandbox};

#[test]
fn cli_reports_version_and_diagnoses_an_empty_sandbox_without_spawning() {
    let sandbox = Sandbox::new().unwrap();
    let bin = nuo_test_support::binary_path().unwrap();

    let version = Cli::new(&bin, &sandbox).arg("--version").output().unwrap();
    assert!(
        version.status.success(),
        "`nuo --version` failed: {}",
        String::from_utf8_lossy(&version.stderr)
    );
    let stdout = String::from_utf8(version.stdout).unwrap();
    assert!(
        stdout.trim().starts_with("nuo "),
        "version output: {stdout:?}"
    );
    assert!(
        stdout.contains(env!("CARGO_PKG_VERSION")),
        "version output must carry the workspace version: {stdout:?}"
    );

    // `status` is explicitly read-only: it must report an empty instance and
    // must not spawn a server just to answer.
    let status = Cli::new(&bin, &sandbox)
        .args(["status", "--json", "--diagnostic"])
        .output()
        .unwrap();
    assert!(
        status.status.success(),
        "`nuo status --json --diagnostic` failed: {}",
        String::from_utf8_lossy(&status.stderr)
    );
    let json: serde_json::Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_eq!(json["discovery_valid"].as_bool(), Some(false));
    assert!(json["discovery_record"].is_null());
    assert!(
        !sandbox.discovery_path().exists(),
        "`nuo status` must never start a server"
    );
}
