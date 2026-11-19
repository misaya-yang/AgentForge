//! CLI checks are offline: no accepted service, process inspector or provider.
use serde_json::Value;
use std::process::Command;
fn binary() -> Command {
    Command::new(env!("CARGO_BIN_EXE_platform-performance-baseline"))
}
#[test]
fn plan_without_accepted_build_is_explicitly_blocked() {
    let output = binary().arg("plan").output().unwrap();
    assert!(output.status.success());
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["status"], "BLOCKED");
    assert_eq!(report["acceptance"], false);
    assert_eq!(report["network_executed"], false);
}
#[test]
fn unknown_options_and_run_without_fresh_output_fail() {
    assert!(
        !binary()
            .args(["plan", "--disable-auth"])
            .status()
            .unwrap()
            .success()
    );
    assert!(!binary().arg("run").status().unwrap().success());
}
#[test]
fn failed_plan_directory_is_retained_and_not_reused() {
    let directory =
        std::env::temp_dir().join(format!("performance-cli-test-{}", uuid::Uuid::new_v4()));
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(4)
        .unwrap();
    let args = [
        "plan",
        "--repo-root",
        root.to_str().unwrap(),
        "--output",
        directory.to_str().unwrap(),
    ];
    let output = binary().args(args).output().unwrap();
    assert!(output.status.success());
    let before = std::fs::read(directory.join("report.json")).unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&before).unwrap()["status"],
        "BLOCKED"
    );
    assert!(!binary().args(args).output().unwrap().status.success());
    assert_eq!(
        before,
        std::fs::read(directory.join("report.json")).unwrap()
    );
    std::fs::remove_dir_all(directory).unwrap();
}
