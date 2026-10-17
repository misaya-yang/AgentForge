use serde_json::Value;
use std::process::Command;

#[test]
fn local_cli_ignores_ambient_parallelism_and_writes_the_observed_receipt() {
    let path = std::env::temp_dir().join(format!("ci-capacity-{}.json", uuid::Uuid::new_v4()));
    let output = Command::new(env!("CARGO_BIN_EXE_platform-tools"))
        .args(["ci-capacity", "--evidence-out"])
        .arg(&path)
        .env("CI", "false")
        .env("CARGO_BUILD_JOBS", "128")
        .env("AI_PLATFORM_CI_CAPACITY_PROFILE", "hosted")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["hosted"], false);
    assert_eq!(value["cargo_build_jobs"], 1);
    assert_eq!(value["nextest_test_threads"], 1);
    let saved: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(saved, value);
    std::fs::remove_file(path).unwrap();
}

#[test]
fn cli_cannot_spoof_resource_measurements() {
    let output = Command::new(env!("CARGO_BIN_EXE_platform-tools"))
        .args(["ci-capacity", "--cpus", "128"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("resource facts cannot be overridden")
    );
}
