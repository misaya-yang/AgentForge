//! Verify the actual source-contract ZIP/runtime-image loader without Git or
//! historical generator executables. Frozen provenance facts remain untouched.
use ai_platform_migration_authority::{
    DEFAULT_BASELINE_ID, commands, manifest, runner::AuthorityPaths,
};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};
struct Package(PathBuf);
impl Drop for Package {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn copy_artifacts(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let p = entry.path();
        if entry.file_type().unwrap().is_dir() {
            copy_artifacts(&p, &to.join(entry.file_name()));
        } else if p.extension().is_none_or(|e| e != "py") {
            fs::copy(&p, to.join(entry.file_name())).unwrap();
        }
    }
}
#[test]
fn packaged_native_loader_keeps_historical_identity_without_python_preimages() {
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../../database");
    let package = Package(
        std::env::temp_dir().join(format!("authority-native-package-{}", uuid::Uuid::new_v4())),
    );
    let database = package.0.join("database");
    // Production Docker/ZIP supplies artifacts, not the source Git object database.
    for name in ["baselines", "migrations", "bootstrap"] {
        copy_artifacts(&source.join(name), &database.join(name));
    }
    assert!(!package.0.join(".git").exists());
    let paths = AuthorityPaths {
        database_dir: database,
    };
    let original = source
        .join("baselines")
        .join(DEFAULT_BASELINE_ID)
        .join("manifest.json");
    let original_bytes = fs::read(&original).unwrap();
    let historical: serde_json::Value = serde_json::from_slice(&original_bytes).unwrap();
    assert_eq!(
        historical["generator"],
        "scripts/database/freeze_baseline.py"
    );
    assert!(
        !package
            .0
            .join(historical["generator"].as_str().unwrap())
            .exists()
    );
    assert!(commands::baseline_ready(&paths, DEFAULT_BASELINE_ID).unwrap());
    let (baseline, sha) = commands::load_baseline(&paths, DEFAULT_BASELINE_ID).unwrap();
    assert_eq!(sha, manifest::file_sha256(&original).unwrap());
    assert_eq!(
        baseline.generator,
        historical["generator"].as_str().unwrap()
    );
    assert_eq!(
        baseline.source_git_sha,
        historical["source_git_sha"].as_str().unwrap()
    );
    assert_eq!(
        fs::read(
            paths
                .baseline_dir(DEFAULT_BASELINE_ID)
                .join("manifest.json")
        )
        .unwrap(),
        original_bytes
    );
    let epoch =
        manifest::load_epoch_manifest(&paths.epoch_dir(DEFAULT_BASELINE_ID).join("manifest.yml"))
            .unwrap();
    assert_eq!(epoch.epoch, 28);
    assert_eq!(epoch.changes.len(), 28);
    let mut applied = BTreeMap::new();
    assert_eq!(
        commands::expected_epoch_fingerprints(&paths, &baseline, &applied).unwrap(),
        baseline.fingerprints()
    );
    for change in epoch.changes {
        applied.insert(change.sequence, change.sha256);
        assert_eq!(
            commands::expected_epoch_fingerprints(&paths, &baseline, &applied)
                .unwrap()
                .len(),
            4
        );
    }
}
