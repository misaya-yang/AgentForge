//! Source profile gate safety matrix, with native fixture sources and no application startup.
use ai_platform_openapi::{cli::evaluate, profiles, snapshot::safe_output};
use serde_json::{Value, json};
use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicUsize, Ordering},
};
static NEXT: AtomicUsize = AtomicUsize::new(0);
struct Fixture {
    root: PathBuf,
    published: Value,
    ownership: Value,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "native-openapi-profile-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(root.join("rust/platform-native")).unwrap();
        fs::write(
            root.join("rust/platform-native/owner.rs"),
            "pub fn implemented() {}",
        )
        .unwrap();
        let published = json!({"openapi":"3.1.0","paths":{"/legacy":{"get":{"operationId":"legacy"}},"/native":{"get":{"operationId":"native","security":[{"NativeBearer":[]}]}}},"components":{"securitySchemes":{"NativeBearer":{"type":"http","scheme":"bearer"}}}});
        let ownership = json!({"version":1,"native_only":[{"path":"/native","operations":{"get":"native"},"source":"rust/platform-native/owner.rs","source_evidence":["pub fn implemented()"]}],"native_security_schemes":["NativeBearer"],"python_paths":["/legacy"]});
        Self {
            root,
            published,
            ownership,
        }
    }
    fn files(&self) -> (PathBuf, PathBuf, PathBuf) {
        let baseline = self.root.join("published.json");
        let candidate = self.root.join("candidate.json");
        let ownership = self.root.join("ownership.json");
        fs::write(&baseline, self.published.to_string()).unwrap();
        fs::write(&candidate, self.published.to_string()).unwrap();
        fs::write(&ownership, self.ownership.to_string()).unwrap();
        (baseline, candidate, ownership)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
#[test]
fn projections_never_drop_native_operations_from_native_profile() {
    let f = Fixture::new();
    profiles::validate_ownership(&f.published, &f.ownership, &f.root).unwrap();
    assert_eq!(
        profiles::project(&f.published, "native", &f.ownership).unwrap(),
        f.published
    );
    assert!(profiles::project(&f.published, "python-hybrid", &f.ownership).is_err());
    assert!(profiles::project(&f.published, "unknown", &f.ownership).is_err());
}
#[test]
fn ownership_invalidity_fails_closed() {
    let f = Fixture::new();
    for (pointer, value) in [
        ("/version", json!(2)),
        ("/native_only/0/operations/get", json!("stale")),
        (
            "/native_only/0/source_evidence",
            json!(["missing implementation"]),
        ),
        ("/python_paths", json!(["/legacy", "/native"])),
        ("/native_security_schemes", json!(["unknown"])),
    ] {
        let mut ownership = f.ownership.clone();
        *ownership.pointer_mut(pointer).unwrap() = value;
        assert!(
            profiles::validate_ownership(&f.published, &ownership, &f.root).is_err(),
            "{pointer}"
        );
    }
}
#[test]
fn identical_artifact_is_not_native_verification() {
    let f = Fixture::new();
    let (base, candidate, owner) = f.files();
    let (evidence, code) = evaluate(&base, Some(&candidate), &owner, &f.root, "native");
    assert_eq!(code, 2);
    assert_eq!(evidence["comparison_result"], "pass");
    assert_eq!(evidence["native_runtime_verified"], false);
    let (evidence, code) = evaluate(&base, Some(&candidate), &owner, &f.root, "published");
    assert_eq!(code, 0);
    assert_eq!(evidence["result_kind"], "artifact-comparison-only");
}
#[test]
fn missing_candidate_and_baseline_self_comparison_rejected() {
    let f = Fixture::new();
    let (base, _, owner) = f.files();
    assert_eq!(evaluate(&base, None, &owner, &f.root, "native").1, 2);
    assert_eq!(
        evaluate(&base, Some(&base), &owner, &f.root, "published").1,
        2
    );
}
#[test]
fn source_tree_outputs_and_unknown_public_paths_rejected() {
    let mut f = Fixture::new();
    assert!(safe_output(&f.root.join("new-output.json"), &f.root).is_err());
    f.published["paths"]["/unknown"] = json!({"get":{}});
    assert!(profiles::validate_ownership(&f.published, &f.ownership, &f.root).is_err());
}
#[test]
fn declaration_drift_includes_additions() {
    let prior = json!({"x-native-contract":{"scope":"native-only"},"paths":{}});
    let mut current = prior.clone();
    current["paths"]["/new"] = json!({});
    assert_eq!(
        profiles::declaration_drift(&prior, &current).unwrap(),
        vec!["paths"]
    );
    assert!(profiles::declaration_drift(&json!({}), &current).is_err());
}
