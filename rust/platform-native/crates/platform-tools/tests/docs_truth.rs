use serde_json::Value;
use std::{fs, path::PathBuf, process::Command};
const DOCS: &[&str] = &[
    "docs/harness/architecture.md",
    "CONTRIBUTING.md",
    "DEPLOY.md",
    "docs/deployment/operator-guide.md",
    "database/authority/README.md",
    "deploy/agent-runtime-source/README.md",
    "docs/design/enterprise-performance-contract.md",
    "rust/platform-native/crates/platform-migration-authority/README.md",
    "rust/platform-native/crates/platform-gateway/src/migration_host/bootstrap/README.md",
];
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let f = Self(std::env::temp_dir().join(format!("docs-truth-{}", uuid::Uuid::new_v4())));
        for file in DOCS {
            let path = f.0.join(file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(
                path,
                format!(
                    "# Source\n\n**Last verified:** {} (source only)\n",
                    chrono::Utc::now().date_naive()
                ),
            )
            .unwrap();
        }
        f
    }
    fn run(&self, success: bool) -> Value {
        let out = Command::new(env!("CARGO_BIN_EXE_platform-tools"))
            .args(["docs-truth-check", "--repo-root"])
            .arg(&self.0)
            .output()
            .unwrap();
        assert_eq!(
            out.status.success(),
            success,
            "{}",
            String::from_utf8_lossy(&out.stdout)
        );
        serde_json::from_slice(&out.stdout).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
#[test]
fn complete_reviewed_docs_pass_without_claiming_runtime() {
    let result = Fixture::new().run(true);
    assert_eq!(result["documents_checked"], DOCS.len());
    assert_eq!(result["runtime_acceptance"], "not_checked");
}
#[test]
fn broken_links_missing_scripts_and_unimplemented_commands_fail() {
    for text in [
        "[bad](missing.md)",
        "python scripts/harness/retired.py",
        "platform-tools invented-command",
        "platform-tools source-lock fake",
        "platform-tools source-lock json-get",
        "platform-tools source-candidate fake",
        "python -m database.authority verify",
    ] {
        let f = Fixture::new();
        let path = f.0.join(DOCS[0]);
        let mut doc = fs::read_to_string(&path).unwrap();
        doc.push_str(text);
        fs::write(path, doc).unwrap();
        assert!(!f.run(false)["errors"].as_array().unwrap().is_empty());
    }
}
#[test]
fn stale_dates_warn_but_missing_future_and_invalid_dates_fail() {
    let f = Fixture::new();
    fs::write(f.0.join(DOCS[0]), "**Last verified:** 2000-01-01\n").unwrap();
    assert_eq!(f.run(true)["warnings"].as_array().unwrap().len(), 1);
    for text in [
        "No review date",
        "**Last verified:** 9999-01-01",
        "**Last verified:** 2026-99-99",
    ] {
        fs::write(f.0.join(DOCS[0]), text).unwrap();
        f.run(false);
    }
}
#[test]
fn real_command_and_existing_local_material_pass() {
    let f = Fixture::new();
    let path = f.0.join(DOCS[0]);
    let mut text = fs::read_to_string(&path).unwrap();
    text.push_str("platform-tools source-lock validate\n[local](architecture.md)\n[official](https://example.invalid/)\n");
    fs::write(path, text).unwrap();
    f.run(true);
}
