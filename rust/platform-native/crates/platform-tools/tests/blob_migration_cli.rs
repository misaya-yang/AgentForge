use std::{
    path::PathBuf,
    process::{Command, Output},
};
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("blob-migration-cli-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(path.join("source/dev/uploads/tenant-a/user")).unwrap();
        std::fs::write(path.join("source/dev/uploads/tenant-a/user/a"), b"hello").unwrap();
        Self(path)
    }
    fn run(&self, command: &str, extra: &[&str]) -> Output {
        let mut c = Command::new(env!("CARGO_BIN_EXE_blob-migrate"));
        c.arg(command)
            .arg("--root")
            .arg(self.0.join("source"))
            .arg("--manifest")
            .arg(self.0.join("manifest.json"));
        if command == "plan" {
            c.args(["--scope", "dev/uploads/tenant-a"]);
        } else {
            c.arg("--state")
                .arg(self.0.join("state.json"))
                .arg("--receipt")
                .arg(self.0.join("receipt.json"));
        }
        c.args(extra)
            .env_remove("BLOB_MIGRATION_S3_BUCKET")
            .env_remove("BLOB_MIGRATION_S3_ACCESS_KEY")
            .env_remove("BLOB_MIGRATION_S3_SECRET_KEY")
            .output()
            .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
#[test]
fn plans_and_copy_dry_run_need_no_credentials_or_output_mutations() {
    let f = Fixture::new();
    assert!(f.run("plan", &["--dry-run"]).status.success());
    assert!(!f.0.join("manifest.json").exists());
    assert!(f.run("plan", &[]).status.success());
    assert!(!f.run("plan", &[]).status.success());
    let result = f.run("copy", &["--dry-run"]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(!f.0.join("state.json").exists());
    assert!(!f.0.join("receipt.json").exists());
    assert!(!f.0.join("state.migration-lock").exists());
}
#[test]
fn failed_final_verify_invalidates_old_ready_receipt_before_source_check() {
    let f = Fixture::new();
    assert!(f.run("plan", &[]).status.success());
    std::fs::write(f.0.join("receipt.json"), br#"{"cutover_ready":true}"#).unwrap();
    std::fs::write(f.0.join("source/dev/uploads/tenant-a/user/a"), b"changed").unwrap();
    assert!(!f.run("verify", &["--source-quiesced"]).status.success());
    let receipt: serde_json::Value =
        serde_json::from_slice(&std::fs::read(f.0.join("receipt.json")).unwrap()).unwrap();
    assert_eq!(receipt["cutover_ready"], false);
    assert!(!f.0.join("state.migration-lock").exists());
}
#[test]
fn stale_lock_is_preserved_and_blocks_concurrent_run() {
    let f = Fixture::new();
    assert!(f.run("plan", &[]).status.success());
    std::fs::write(f.0.join("state.migration-lock"), b"operator-must-inspect").unwrap();
    assert!(!f.run("copy", &[]).status.success());
    assert_eq!(
        std::fs::read(f.0.join("state.migration-lock")).unwrap(),
        b"operator-must-inspect"
    );
}

#[test]
fn final_verification_requires_owner_metadata_snapshot() {
    let f = Fixture::new();
    assert!(f.run("plan", &[]).status.success());
    let output = f.run("verify", &["--source-quiesced"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("requires --metadata-references"));
    let receipt: serde_json::Value =
        serde_json::from_slice(&std::fs::read(f.0.join("receipt.json")).unwrap()).unwrap();
    assert_eq!(receipt["cutover_ready"], false);
}
