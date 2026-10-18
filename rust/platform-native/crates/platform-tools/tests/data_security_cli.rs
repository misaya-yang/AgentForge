use std::process::Command;
#[test]
fn source_validation_is_offline_and_probe_requires_explicit_private_inputs() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../..");
    let result = Command::new(env!("CARGO_BIN_EXE_platform-data-security"))
        .args(["validate", "--repo-root"])
        .arg(root)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(value["runtime_acceptance"], false);
    let result = Command::new(env!("CARGO_BIN_EXE_platform-data-security"))
        .args([
            "probe",
            "--role",
            "gateway",
            "--tenant",
            "tenant-a",
            "--environment-prefix",
            "UNSET_TEST_DATA_16333",
        ])
        .output()
        .unwrap();
    assert!(!result.status.success());
}
#[test]
fn incomplete_private_material_never_validates() {
    let root = std::env::temp_dir().join(format!("data-security-cli-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("INCOMPLETE"), b"fixture").unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_platform-data-security"))
        .args(["validate-material", "--directory"])
        .arg(&root)
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(root.join("INCOMPLETE").exists());
    std::fs::remove_dir_all(root).unwrap();
}
#[cfg(unix)]
#[test]
fn local_env_cli_rejects_secret_conflicts_without_export_or_subprocess_fallback() {
    use std::io::Write;
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    let parent = std::env::temp_dir().canonicalize().unwrap();
    let root = parent.join(format!("data-import-cli-{}", uuid::Uuid::new_v4()));
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&root)
        .unwrap();
    let input = root.join("private.env");
    let mut file = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&input)
        .unwrap();
    writeln!(file, "AI_GATEWAY_DATABASE_ADMIN_DSN='postgresql://postgres:do-not-print-import-sentinel@127.0.0.1:5432/gateway'\nAI_GATEWAY_DATABASE_ADMIN_DSN=conflict\nPROVIDER_API_KEY=provider-sentinel").unwrap();
    drop(file);
    let output = root.join("release");
    let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../..");
    let result = Command::new(env!("CARGO_BIN_EXE_platform-data-security"))
        .args(["prepare-local-env", "--repo-root"])
        .arg(repo)
        .arg("--local-env")
        .arg(&input)
        .arg("--directory")
        .arg(&output)
        .args([
            "--database",
            "gateway",
            "--client-cidr",
            "10.71.10.0/24",
            "--backend-cidr",
            "10.71.11.0/24",
            "--openssl",
            "/nonexistent-openssl",
        ])
        .output()
        .unwrap();
    assert!(!result.status.success());
    let stdout = String::from_utf8_lossy(&result.stdout);
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(!stdout.contains("sentinel") && !stderr.contains("sentinel"));
    assert!(stderr.contains("duplicate local database assignment"));
    assert!(!output.exists());
    assert!(input.exists());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn aggregate_pool_budget_is_executable_offline_and_exhaustion_is_refused() {
    let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../..");
    let fixture = repo.join("deploy/data/pgbouncer/connection-budget.example.json");
    let command = || Command::new(env!("CARGO_BIN_EXE_platform-data-security"));
    let out = command()
        .arg("pool-budget")
        .arg("--repo-root")
        .arg(&repo)
        .arg("--budget")
        .arg(&fixture)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["total_backend_bound"], 60);
    assert_eq!(v["replica_capacity_acceptance"], false);
    let path = std::env::temp_dir().join(format!("data-budget-{}.json", uuid::Uuid::new_v4()));
    let mut bad: serde_json::Value =
        serde_json::from_slice(&std::fs::read(fixture).unwrap()).unwrap();
    bad["poolers"][0]["replicas"] = serde_json::json!(16);
    std::fs::write(&path, serde_json::to_vec(&bad).unwrap()).unwrap();
    let out = command()
        .arg("pool-budget")
        .arg("--repo-root")
        .arg(repo)
        .arg("--budget")
        .arg(&path)
        .output()
        .unwrap();
    assert!(!out.status.success());
    std::fs::remove_file(path).unwrap();
}
