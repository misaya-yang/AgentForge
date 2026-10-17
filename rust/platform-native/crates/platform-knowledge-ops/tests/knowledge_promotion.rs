//! Fixture behavior tests; a fixture decision must never authorize quality promotion.
use ai_platform_knowledge_ops::promotion;
use std::{
    path::{Path, PathBuf},
    sync::atomic::{AtomicUsize, Ordering},
};
fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../..")
        .canonicalize()
        .unwrap()
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
