//! Source assertion ports: test_migration_authority_credentials.py and
//! test_migration_authority_baseline_manifest.py. No ambient DB access.
use ai_platform_migration_authority::{credentials, manifest};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};
const ID: &str = "2026_08_post_kb_v1";
struct Repo(PathBuf);
impl Repo {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("authority-parity-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        let r = Self(root);
        r.git(&["init", "-q"]);
        r.git(&["config", "user.email", "fixture@example.invalid"]);
        r.git(&["config", "user.name", "Offline provenance fixture"]);
        r
    }
    fn git(&self, args: &[&str]) -> String {
        let out = Command::new("git")
            .args(args)
            .current_dir(&self.0)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    }
    fn write(&self, path: &str, bytes: impl AsRef<[u8]>) -> PathBuf {
        let p = self.0.join(path);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, bytes).unwrap();
        p
    }
    fn commit(&self) {
        self.git(&["add", "."]);
        self.git(&["-c", "commit.gpgsign=false", "commit", "-qm", "fixture"]);
    }
    fn freeze(&self, initial_epoch: &str) -> (PathBuf, manifest::BaselineManifest) {
        self.write(
            "scripts/freeze.rs",
            "// immutable native fixture generator\n",
        );
        if initial_epoch != "missing" {
            let changes = if initial_epoch == "one" {
                vec![self.change(1)]
            } else {
                vec![]
            };
            self.epochs(&changes);
        }
        let path = self.baseline("pending-live-freeze", &"a".repeat(40));
        self.commit();
        let source = self.git(&["rev-parse", "HEAD"]);
        self.baseline("frozen", &source);
        self.commit();
        let baseline = manifest::load_baseline_manifest(&path).unwrap();
        (path, baseline)
    }
    fn baseline(&self, state: &str, source: &str) -> PathBuf {
        let mut sql = serde_json::Map::new();
        let mut policies = serde_json::Map::new();
        for name in [
            "cutover_convergence.sql",
            "grants.sql",
            "init.sql",
            "reference_data.sql",
            "verify.sql",
        ] {
            let p = self.write(
                &format!("database/baselines/{ID}/{name}"),
                format!("-- {name}\n"),
            );
            sql.insert(name.into(), json!(manifest::file_sha256(&p).unwrap()));
        }
        for name in [
            "data-access-inventory.json",
            "ownership-policy.json",
            "grants-policy.json",
        ] {
            let p = self.write(&format!("database/baselines/{ID}/{name}"), "{}\n");
            policies.insert(name.into(), json!(manifest::file_sha256(&p).unwrap()));
        }
        self.write(
            &format!("database/baselines/{ID}/manifest.json"),
            serde_json::to_vec(&json!({
                "schema":"migration-authority/baseline-manifest/v1", "state":state,
                "baseline_id":ID, "schema_revision":"112", "source_git_sha":source,
                "last_legacy_change":"112_kb_document_progress_retention.sql",
                "structural_sha256":"1".repeat(64),"acl_sha256":"2".repeat(64),
                "extensions_sha256":"3".repeat(64),"reference_data_sha256":"4".repeat(64),
                "generator":"scripts/freeze.rs", "generated_at":"2026-08-30T00:00:00Z",
                "postgres_version":"16", "files_sha256":sql,"policy_files_sha256":policies,
                "reference_data":[]
            }))
            .unwrap(),
        )
    }
    fn change(&self, sequence: usize) -> Value {
        let file = format!("{sequence:03}_probe{sequence}.sql");
        let p = self.write(
            &format!("database/migrations/{ID}/{file}"),
            format!("SELECT {sequence};\n"),
        );
        json!({"sequence":sequence,"name":format!("probe{sequence}"),"file":file,
            "sha256":manifest::file_sha256(&p).unwrap(),"owner":"owner","transaction_mode":"transactional",
            "rollback_class":"forward-fix-only","preconditions":["SELECT TRUE"],"postconditions":["SELECT TRUE"],
            "timeout_seconds":300,"lock_budget_seconds":10,"resume_handler":null,"repair_handler":null,"notes":"original"})
    }
    fn epochs(&self, changes: &[Value]) -> PathBuf {
        self.write(
            &format!("database/migrations/{ID}/manifest.yml"),
            serde_json::to_vec(
                &json!({"baseline_id":ID,"epoch":changes.len().max(1),"changes":changes}),
            )
            .unwrap(),
        )
    }
}
impl Drop for Repo {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn rejected(path: &Path, baseline: &manifest::BaselineManifest, repo: &Repo, needle: &str) {
    let error = manifest::verify_baseline_git_provenance(path, baseline, &repo.0)
        .unwrap_err()
        .to_string();
    assert!(error.contains(needle), "expected {needle}: {error}");
}
#[test]
fn provenance_requires_prior_unchanged_source_and_rejects_fake_or_self_containing_commit() {
    let repo = Repo::new();
    let (path, baseline) = repo.freeze("missing");
    manifest::verify_baseline_git_provenance(&path, &baseline, &repo.0).unwrap();
    let mut fake = baseline.clone();
    fake.source_git_sha = "0".repeat(40);
    rejected(&path, &fake, &repo, "exact resolvable commit");
    fake.source_git_sha = repo.git(&["rev-parse", "HEAD"]);
    rejected(&path, &fake, &repo, "pending freeze manifest");
    repo.write("scripts/freeze.rs", "// drift\n");
    repo.commit();
    rejected(&path, &baseline, &repo, "inputs differ");
}
#[test]
fn provenance_allows_append_only_epochs_and_rejects_metadata_sql_and_committed_tampering() {
    for initial in ["missing", "empty", "one"] {
        let repo = Repo::new();
        let (path, baseline) = repo.freeze(initial);
        let mut changes = vec![repo.change(1)];
        if initial != "one" {
            repo.epochs(&changes);
            repo.commit();
        }
        changes.push(repo.change(2));
        let epoch = repo.epochs(&changes);
        manifest::verify_baseline_git_provenance(&path, &baseline, &repo.0).unwrap();
        fs::remove_file(&epoch).unwrap();
        assert!(manifest::verify_baseline_git_provenance(&path, &baseline, &repo.0).is_err());
        changes[0]["notes"] = json!("changed metadata");
        repo.epochs(&changes);
        rejected(&path, &baseline, &repo, "immutable");
        changes[0]["notes"] = json!("original");
        let sql = repo.write(
            &format!("database/migrations/{ID}/001_probe1.sql"),
            "SELECT 999;\n",
        );
        repo.epochs(&changes);
        rejected(&path, &baseline, &repo, "checksum");
        changes[0]["sha256"] = json!(manifest::file_sha256(&sql).unwrap());
        repo.epochs(&changes);
        rejected(&path, &baseline, &repo, "immutable");
        repo.commit();
        rejected(&path, &baseline, &repo, "immutable");
    }
}
#[test]
fn baseline_rejects_checksum_drift_and_unreviewed_sql() {
    let repo = Repo::new();
    let path = repo.baseline("frozen", &"a".repeat(40));
    let baseline = manifest::load_baseline_manifest(&path).unwrap();
    for (name, hash) in baseline.files_sha256 {
        assert_eq!(
            manifest::file_sha256(&path.parent().unwrap().join(name)).unwrap(),
            hash
        );
    }
    repo.write(&format!("database/baselines/{ID}/init.sql"), "-- drift\n");
    assert!(
        manifest::load_baseline_manifest(&path)
            .unwrap_err()
            .to_string()
            .contains("checksum")
    );
    repo.baseline("frozen", &"a".repeat(40));
    repo.write(
        &format!("database/baselines/{ID}/unreviewed.sql"),
        "SELECT 1;\n",
    );
    assert!(manifest::load_baseline_manifest(&path).is_err());
}
// Each environment scenario runs in a fresh process, avoiding unsafe process-global
// environment mutation under the parallel Rust test harness.
#[test]
fn credential_subprocess_probe() {
    let Ok(mode) = std::env::var("AUTHORITY_PARITY_CHILD") else {
        return;
    };
    match mode.as_str() {
        "resolved" => {
            for principal in credentials::ALL_PRINCIPALS {
                assert_eq!(
                    credentials::dsn_for_principal(principal).unwrap(),
                    format!("postgresql://{principal}.example.invalid/gateway")
                );
            }
            assert!(credentials::dsn_for_principal("unknown").is_err());
        }
        "missing" | "empty" => {
            let error = credentials::dsn_for_principal("migrator")
                .unwrap_err()
                .to_string();
            assert!(error.contains("shared or inferred"));
            assert!(!error.contains("postgresql://"));
        }
        "duplicate" => {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let error = rt
                .block_on(credentials::verify_role_connections("ai_gateway_", None))
                .unwrap_err()
                .to_string();
            assert!(error.contains("DSNs must be distinct"), "{error}");
            assert!(!error.contains("postgresql://"));
        }
        _ => panic!("unknown isolated scenario"),
    }
}
#[test]
fn role_dsn_resolution_has_no_shared_fallback_and_rejects_reuse_without_disclosure() {
    for mode in ["resolved", "missing", "empty", "duplicate"] {
        let mut child = Command::new(std::env::current_exe().unwrap());
        child
            .args(["--exact", "credential_subprocess_probe", "--nocapture"])
            .env_clear()
            .env("AUTHORITY_PARITY_CHILD", mode);
        for principal in credentials::ALL_PRINCIPALS {
            child.env(
                format!("AI_GATEWAY_DATABASE_{}_DSN", principal.to_ascii_uppercase()),
                format!("postgresql://{principal}.example.invalid/gateway"),
            );
        }
        child
            .env(
                "DATABASE_URL",
                "postgresql://shared.example.invalid/gateway",
            )
            .env("PGUSER", "shared")
            .env("PGDATABASE", "gateway");
        if mode == "missing" {
            child.env_remove("AI_GATEWAY_DATABASE_MIGRATOR_DSN");
        }
        if mode == "empty" {
            child.env("AI_GATEWAY_DATABASE_MIGRATOR_DSN", "");
        }
        if mode == "duplicate" {
            child.env(
                "AI_GATEWAY_DATABASE_RUNTIME_DSN",
                "postgresql://gateway.example.invalid/gateway",
            );
        }
        let output = child.output().unwrap();
        assert!(
            output.status.success(),
            "{mode}: {} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
#[tokio::test]
#[ignore = "Requires explicitly provisioned isolated seven-role PostgreSQL fixture; no production or ambient .env"]
async fn actual_role_connections_verify_all_effective_users_and_reject_wrong_database() {
    assert_eq!(
        std::env::var("AUTHORITY_PARITY_ALLOW_ISOLATED_DB").as_deref(),
        Ok("1")
    );
    let expected = std::env::var("AUTHORITY_PARITY_EXPECTED_DATABASE")
        .expect("explicit isolated database name");
    assert!(
        expected.starts_with("authority_parity_"),
        "isolated fixture name required"
    );
    let prefix = credentials::role_prefix_from_env().unwrap();
    assert_eq!(
        credentials::verify_role_connections(&prefix, Some(&expected))
            .await
            .unwrap(),
        credentials::ALL_PRINCIPALS.len()
    );
    let wrong = format!("{expected}_wrong");
    let error = credentials::verify_role_connections(&prefix, Some(&wrong))
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("expected database"));
}
