//! Real validator CLI against synthetic configs. Actual Docker renders are a separate L0 gate.
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};
#[path = "support/compose.rs"]
mod compose;
fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../..")
        .canonicalize()
        .unwrap()
}
fn check(config: &Value, mode: &str, source_build: bool, success: bool) {
    let file = std::env::temp_dir().join(format!("compose-identity-{}.json", uuid::Uuid::new_v4()));
    fs::write(&file, serde_json::to_vec(config).unwrap()).unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_platform-tools"));
    command
        .args(["compose-image-check", "--repo-root"])
        .arg(root())
        .args(["--config"])
        .arg(&file)
        .args(["--mode", mode]);
    if source_build {
        command.arg("--source-build");
    }
    let output = command.output().unwrap();
    fs::remove_file(file).unwrap();
    assert_eq!(
        output.status.success(),
        success,
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    // Never echo the input, even when invalid configuration includes private values.
    assert!(!String::from_utf8_lossy(&output.stderr).contains("private-canary"));
    if success {
        let receipt: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert!(
            receipt["artifact_acceptance"]
                .as_str()
                .unwrap()
                .starts_with("not_checked")
        );
    }
}
#[test]
fn full_scale_and_compact_role_identity() {
    let mut config = compose::rendered(&root());
    check(&config, "full", false, true);
    check(&config, "scale", false, true);
    config["services"]
        .as_object_mut()
        .unwrap()
        .remove("knowledge-worker");
    check(&config, "compact", false, true);
    check(&config, "full", false, false);
}
#[test]
fn image_collisions_and_role_drift_fail_closed() {
    for service in [
        "frontend",
        "gateway",
        "migrate",
        "agent-runtime",
        "agent-capability-worker",
    ] {
        let mut config = compose::rendered(&root());
        config["services"][service]["image"] =
            config["services"]["knowledge-service"]["image"].clone();
        check(&config, "full", false, false);
    }
    let mut config = compose::rendered(&root());
    config["services"]["knowledge-worker"]["image"] = json!("example/knowledge:older");
    check(&config, "full", false, false);
}
#[test]
fn stale_pair_unknown_alias_and_kernel_revision_fail_closed() {
    for service in ["agent-runtime", "agent-capability-worker"] {
        let mut config = compose::rendered(&root());
        config["services"][service]["image"] = json!("private-canary:old-or-unadmitted");
        check(&config, "full", false, false);
    }
    let mut config = compose::rendered(&root());
    config["services"]["agent-runtime"]["environment"]["AI_PLATFORM_AGENT_RUNTIME_KERNEL_REVISION"] =
        json!("obsolete");
    check(&config, "full", false, false);
}
#[test]
fn malformed_or_unresolved_configs_are_rejected() {
    for config in [json!({}), json!({"services":[]}), json!({"services":{}})] {
        check(&config, "full", false, false);
    }
    for image in [Value::Null, json!(""), json!("${KNOWLEDGE_IMAGE}")] {
        let mut config = compose::rendered(&root());
        config["services"]["frontend"]["image"] = image;
        check(&config, "full", false, false);
    }
}
#[test]
fn opt_in_build_targets_cannot_overwrite_another_code_unit() {
    let mut config = compose::rendered(&root());
    for (service, context, dockerfile, target) in [
        (
            "gateway",
            ".",
            "deploy/platform-native/Dockerfile",
            "runtime",
        ),
        ("frontend", "web", "Dockerfile", ""),
        (
            "knowledge-service",
            ".",
            "deploy/platform-native/Dockerfile",
            "knowledge",
        ),
        (
            "knowledge-worker",
            ".",
            "deploy/platform-native/Dockerfile",
            "knowledge",
        ),
        (
            "migrate",
            ".",
            "deploy/platform-native/Dockerfile",
            "migrate",
        ),
    ] {
        config["services"][service]["build"] = json!({"context":root().join(context).canonicalize().unwrap(),"dockerfile":dockerfile,"target":target});
    }
    check(&config, "full", true, true);
    check(&config, "full", false, false);
    for service in ["agent-runtime", "agent-capability-worker"] {
        config["services"][service]["build"] = json!({"context":root(),"dockerfile":"Dockerfile"});
        check(&config, "full", true, false);
        config["services"][service]
            .as_object_mut()
            .unwrap()
            .remove("build");
    }
    config["services"]["frontend"]["build"]["target"] = json!("builder");
    check(&config, "full", true, false);
    config["services"]["frontend"]["build"] =
        config["services"]["knowledge-service"]["build"].clone();
    check(&config, "full", true, false);
}
