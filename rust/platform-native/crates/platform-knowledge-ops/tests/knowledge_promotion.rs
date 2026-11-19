//! Fixture behavior tests; a fixture decision must never authorize quality promotion.
use ai_platform_knowledge_ops::promotion;
use std::{
    path::{Path, PathBuf},
    sync::atomic::{AtomicUsize, Ordering},
};
// Rebuild only synthetic test receipts in temporary storage when evaluator source
// changes. Tracked frozen receipts and production baseline pins remain untouched.
fn root() -> PathBuf {
    static ROOT: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    ROOT.get_or_init(|| {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../..")
            .canonicalize()
            .unwrap();
        let root =
            std::env::temp_dir().join(format!("knowledge-gate-fixtures-{}", std::process::id()));
        fn copy_tree(source: &Path, destination: &Path) {
            std::fs::create_dir_all(destination).unwrap();
            for entry in std::fs::read_dir(source).unwrap() {
                let entry = entry.unwrap();
                if entry.file_type().unwrap().is_dir() {
                    copy_tree(&entry.path(), &destination.join(entry.file_name()));
                } else {
                    std::fs::copy(entry.path(), destination.join(entry.file_name())).unwrap();
                }
            }
        }
        for relative in [
            "tests/fixtures/eval/knowledge/promotion",
            "tests/fixtures/eval/rag/golden",
        ] {
            copy_tree(&repo.join(relative), &root.join(relative));
        }
        let directory = root.join("tests/fixtures/eval/knowledge/promotion");
        for entry in std::fs::read_dir(&directory).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let mut value: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            if value.get("evaluator_sha256").is_some() {
                assert!(
                    value["provenance"] == "fixture" || value["schema"] == "knowledge-promotion/v1"
                );
                value["evaluator_sha256"] = promotion::evaluator_sha256().into();
                std::fs::write(&path, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
            }
        }
        let path = directory.join("policy-fixture.json");
        let mut policy: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        policy["baseline"]["sha256"] =
            promotion::sha256(&std::fs::read(directory.join("baseline-fixture.json")).unwrap())
                .into();
        std::fs::write(path, serde_json::to_vec_pretty(&policy).unwrap()).unwrap();
        root
    })
    .clone()
}
fn now() -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::parse_from_rfc3339("2026-10-17T00:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc)
}
fn fixture(name: &str) -> PathBuf {
    root()
        .join("tests/fixtures/eval/knowledge/promotion")
        .join(name)
}
fn check(name: &str, mode: bool) -> Result<promotion::Decision, String> {
    promotion::evaluate(
        &root(),
        &fixture("policy-fixture.json"),
        &fixture(name),
        &"1".repeat(40),
        mode,
        now(),
    )
}
#[test]
fn positive_fixture_computes_metrics_but_never_promotes() {
    let d = check("candidate-pass.json", true).unwrap();
    assert!(d.eligible && d.fixture_only && !d.promotable);
    assert!(d.cases > 1);
    assert!(d.retrieval_v2_promotions.is_empty());
    assert_eq!(d.metrics["retrieval-starter/native-hybrid"]["mrr"], 1.0);
}
#[test]
fn critical_case_regression_cannot_hide_behind_averages() {
    assert!(
        check("candidate-regression.json", true)
            .unwrap_err()
            .contains("regression")
    );
}
#[test]
fn reversed_judge_scores_fail_calibration() {
    assert!(
        check("candidate-bad-judge.json", true)
            .unwrap_err()
            .contains("calibration")
    );
}
#[test]
fn fixture_never_counts_as_measured_quality() {
    assert!(
        check("candidate-pass.json", false)
            .unwrap_err()
            .contains("fixtures/replays")
    );
}
struct Temp(PathBuf);
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}
fn mutated<F: FnOnce(&mut serde_json::Value)>(f: F) -> Temp {
    static SEQ: AtomicUsize = AtomicUsize::new(0);
    let mut value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(fixture("candidate-pass.json")).unwrap()).unwrap();
    f(&mut value);
    let path = std::env::temp_dir().join(format!(
        "knowledge-promotion-{}-{}.json",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
    Temp(path)
}
#[test]
fn source_evaluator_dataset_and_profile_coverage_are_bound() {
    for (key, value) in [
        ("source_sha", "0".repeat(40)),
        ("evaluator_sha256", "0".repeat(64)),
    ] {
        let t = mutated(|v| v[key] = value.into());
        assert!(
            promotion::evaluate(
                &root(),
                &fixture("policy-fixture.json"),
                &t.0,
                &"1".repeat(40),
                true,
                now()
            )
            .is_err()
        );
    }
    let t = mutated(|v| v["profiles"].as_array_mut().unwrap().clear());
    assert!(
        promotion::evaluate(
            &root(),
            &fixture("policy-fixture.json"),
            &t.0,
            &"1".repeat(40),
            true,
            now()
        )
        .is_err()
    );
    let t = mutated(|v| v["profiles"][0]["dataset_version"] = "other-dataset".into());
    assert!(
        promotion::evaluate(
            &root(),
            &fixture("policy-fixture.json"),
            &t.0,
            &"1".repeat(40),
            true,
            now()
        )
        .is_err()
    );
    let t = mutated(|v| v["profiles"][0]["observations"]["sha256"] = "0".repeat(64).into());
    assert!(
        promotion::evaluate(
            &root(),
            &fixture("policy-fixture.json"),
            &t.0,
            &"1".repeat(40),
            true,
            now()
        )
        .is_err()
    );
}
#[test]
fn stale_human_review_blocks_judge_use() {
    let future = now() + chrono::Duration::days(100);
    assert!(
        promotion::evaluate(
            &root(),
            &fixture("policy-fixture.json"),
            &fixture("candidate-pass.json"),
            &"1".repeat(40),
            true,
            future
        )
        .unwrap_err()
        .contains("overdue")
    );
}
