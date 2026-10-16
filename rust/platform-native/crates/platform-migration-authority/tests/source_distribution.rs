use ai_platform_migration_authority::{
    DEFAULT_BASELINE_ID, commands,
    distribution::{self, SourceMode},
    runner::AuthorityPaths,
};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};
struct Package(PathBuf);
impl Package {
    fn new() -> Self {
        fn copy(from: &Path, to: &Path) {
            fs::create_dir_all(to).unwrap();
            for entry in fs::read_dir(from).unwrap() {
                let entry = entry.unwrap();
                if entry.file_type().unwrap().is_dir() {
                    copy(&entry.path(), &to.join(entry.file_name()));
                } else {
                    fs::copy(entry.path(), to.join(entry.file_name())).unwrap();
                }
            }
        }
        let root =
            std::env::temp_dir().join(format!("authority-distribution-{}", uuid::Uuid::new_v4()));
        copy(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../../database"),
            &root.join("database"),
        );
        Self(root)
    }
    fn database(&self) -> PathBuf {
        self.0.join("database")
    }
    fn paths(&self) -> AuthorityPaths {
        AuthorityPaths {
            database_dir: self.database(),
        }
    }
}
impl Drop for Package {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
#[test]
fn distribution_installation_does_not_depend_on_unrelated_git_history() {
    let package = Package::new();
    fs::create_dir(package.0.join(".git")).unwrap();
    let (baseline, _) = commands::load_baseline_with_mode(
        &package.paths(),
        DEFAULT_BASELINE_ID,
        SourceMode::Distribution,
    )
    .unwrap();
    assert_eq!(
        baseline.source_git_sha,
        "386344c6c15e92547cabe32b07fad304afbf4a98"
    );
    assert!(
        commands::load_baseline_with_mode(
            &package.paths(),
            DEFAULT_BASELINE_ID,
            SourceMode::Development
        )
        .is_err()
    );
}
#[test]
fn changed_or_extra_schema_inputs_are_rejected() {
    let package = Package::new();
    let original = package.database().join("bootstrap/extensions.sql");
    let bytes = fs::read(&original).unwrap();
    fs::write(&original, [bytes.as_slice(), b"\n-- changed\n"].concat()).unwrap();
    assert!(distribution::verify(&package.database(), DEFAULT_BASELINE_ID).is_err());
    fs::write(&original, bytes).unwrap();
    fs::write(
        package.database().join("migrations/999_unreviewed.sql"),
        "SELECT 1",
    )
    .unwrap();
    assert!(distribution::verify(&package.database(), DEFAULT_BASELINE_ID).is_err());
}
#[test]
fn a_self_consistent_replacement_lock_cannot_replace_the_release_pin() {
    let package = Package::new();
    let lock = package
        .database()
        .join("authority/package-authority-lock.json");
    let mut value: serde_json::Value = serde_json::from_slice(&fs::read(&lock).unwrap()).unwrap();
    value["baseline_id"] = "unreviewed".into();
    fs::write(lock, serde_json::to_vec(&value).unwrap()).unwrap();
    assert!(distribution::verify(&package.database(), "unreviewed").is_err());
}
#[cfg(unix)]
#[test]
fn symlinks_outside_the_schema_suffix_filter_are_rejected() {
    let package = Package::new();
    std::os::unix::fs::symlink("/tmp", package.database().join("bootstrap/hidden-link.txt"))
        .unwrap();
    assert!(distribution::verify(&package.database(), DEFAULT_BASELINE_ID).is_err());
}
#[test]
fn missing_baseline_is_refused_before_a_database_connection_or_legacy_fallback() {
    let package = Package::new();
    fs::remove_file(
        package
            .database()
            .join("baselines")
            .join(DEFAULT_BASELINE_ID)
            .join("init.sql"),
    )
    .unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_database-authority"))
        .arg("migrate")
        .env_clear()
        .env("AI_GATEWAY_DATABASE_DIR", package.database())
        .env("AI_GATEWAY_AUTHORITY_SOURCE_MODE", "distribution")
        .env(
            "AI_GATEWAY_DATABASE_MIGRATOR_DSN",
            "postgresql://unreachable:fixture@127.0.0.1:1/unreachable",
        )
        .output()
        .unwrap();
    assert!(!result.status.success());
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        stderr.contains("distribution"),
        "source validation must precede connection"
    );
    assert!(!stderr.contains("database authority operation failed"));
    assert!(!String::from_utf8_lossy(&result.stdout).contains("compatibility mode"));
}
