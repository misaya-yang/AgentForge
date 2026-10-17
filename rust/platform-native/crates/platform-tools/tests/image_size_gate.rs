//! Exercise the real CLI; no Docker, daemon or image pull is required.
use serde_json::{Value, json};
use std::{fs, path::PathBuf, process::Command};
const ID: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("image-size-gate-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(root.join("deploy/platform-native")).unwrap();
        let targets: std::collections::BTreeMap<_, _> = [
            "runtime",
            "knowledge",
            "migrate",
            "sdk-worker",
            "local-node",
            "tools",
            "release",
            "frontend",
            "code-interpreter",
        ]
        .into_iter()
        .map(|t| (t, 1))
        .collect();
        fs::write(root.join("deploy/platform-native/image-size-budgets.json"), serde_json::to_vec(&json!({"schema_version":"ai-platform/image-size-budgets/v1","metric":"docker-image-inspect-size-bytes","targets":targets})).unwrap()).unwrap();
        Self(root)
    }
    fn check(&self, input: &str, extra: &[&str], pass: bool) {
        let inspect = self.0.join("inspect.json");
        fs::write(&inspect, input).unwrap();
        let output = Command::new(env!("CARGO_BIN_EXE_platform-tools"))
            .args(["image-size-check", "--repo-root"])
            .arg(&self.0)
            .args(["--inspect"])
            .arg(inspect)
            .args([
                "--target",
                "runtime",
                "--platform",
                "linux/amd64",
                "--image-id",
                ID,
            ])
            .args(extra)
            .output()
            .unwrap();
        assert_eq!(
            output.status.success(),
            pass,
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        for bytes in [&output.stdout, &output.stderr] {
            assert!(!String::from_utf8_lossy(bytes).contains("private-canary"));
        }
        if pass {
            let receipt: Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(receipt["measured_bytes"], 1048576);
            assert_eq!(receipt["image_id"], ID);
            assert_eq!(receipt["runtime_security_acceptance"], "not_checked");
            assert_eq!(receipt["policy_sha256"].as_str().unwrap().len(), 64);
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn image(size: u64) -> Value {
    json!([{"Id":ID,"Size":size,"Os":"linux","Architecture":"amd64","Config":{"Env":["private-canary"]}}])
}
#[test]
fn exact_limit_passes_and_one_byte_over_fails_without_leaking_inspect_env() {
    let f = Fixture::new();
    f.check(&image(1048576).to_string(), &[], true);
    f.check(&image(1048577).to_string(), &[], false);
}
#[test]
fn absent_duplicate_and_mismatched_images_fail_closed() {
    let f = Fixture::new();
    let valid = image(1048576);
    f.check("[]", &[], false);
    f.check("{}", &[], false);
    f.check(&json!([valid[0], valid[0]]).to_string(), &[], false);
    let mut wrong = valid.clone();
    wrong[0]["Id"] =
        json!("sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
    f.check(&wrong.to_string(), &[], false);
    let mut wrong = valid.clone();
    wrong[0]["Architecture"] = json!("arm64");
    f.check(&wrong.to_string(), &[], false);
    let mut wrong = valid;
    wrong[0].as_object_mut().unwrap().remove("Size");
    f.check(&wrong.to_string(), &[], false);
}
#[test]
fn duplicate_json_keys_and_cli_overrides_are_rejected() {
    let f = Fixture::new();
    f.check(
        &format!(
            r#"[{{"Id":"{ID}","Size":1048577,"Size":1,"Os":"linux","Architecture":"amd64"}}]"#
        ),
        &[],
        false,
    );
    let valid = image(1048576).to_string();
    for extra in [
        vec!["--target", "runtime"],
        vec!["--limit", "9999999999"],
        vec!["--skip"],
        vec!["unexpected"],
    ] {
        f.check(&valid, &extra, false);
    }
}
