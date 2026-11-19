//! Native translation of tests/harness/test_kb_release_evidence_gate.py.
use ai_platform_openapi::kb_release::evaluate;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};
static NEXT: AtomicU64 = AtomicU64::new(0);
fn digest(p: &Path) -> String {
    format!("{:x}", Sha256::digest(fs::read(p).unwrap()))
}
fn write(p: &Path, v: &Value) {
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, serde_json::to_vec(v).unwrap()).unwrap();
}
struct Fixture {
    root: PathBuf,
    m: PathBuf,
    c: PathBuf,
    p: PathBuf,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "kb-evidence-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(root.join("fixtures")).unwrap();
        let (m, c, p) = (
            root.join("fixtures/manifest.json"),
            root.join("fixtures/golden.jsonl"),
            root.join("reports/release-pointer.json"),
        );
        let review = json!({"status":"approved","reviewer":"reviewer@example.test","reviewed_at":"2026-08-29T13:00:00Z"});
        let rows=(0..200).map(|i|json!({"case_id":format!("case-{i:03}"),"track":"retrieval_only","query":format!("query {i}"),"relevance":{format!("segment-{i}"):3},"metadata":{"version":"v1","provenance":{"kind":if i<100{"real"}else{"synthetic"},"source_ref":format!("source-{i}")},"review":review}}).to_string()).collect::<Vec<_>>().join("\n")+"\n";
        fs::write(&c, rows).unwrap();
        write(
            &m,
            &json!({"version":"v1","files":{"golden.jsonl":{"sha256":digest(&c)}}}),
        );
        let b = root.join("reports/baseline.json");
        write(
            &b,
            &json!({"release_evidence":{"schema_version":"kb-baseline-evidence/v1","dataset_id":"dataset-real-corpus","golden_version":"v1","golden_manifest_sha256":digest(&m),"review":review},"provenance":{"expectations":{"sha256":digest(&c)},"observations":{"sha256":"a".repeat(64)}},"retrieval":{"all":{"metrics":{"5":{"hit_rate":0.8,"mrr":0.7,"ndcg_at_k":0.75,"recall_at_k":0.85,"num_queries":200}}}}}),
        );
        write(
            &p,
            &json!({"schema_version":"kb-release-evidence/v1","release_key":"current","golden_version":"v1","golden_manifest":"fixtures/manifest.json","golden_manifest_sha256":digest(&m),"baseline_report":"reports/baseline.json","baseline_report_sha256":digest(&b),"dataset_id":"dataset-real-corpus","review":review}),
        );
        Self { root, m, c, p }
    }
    fn report(&self) -> Value {
        evaluate(&self.root, &self.m, &self.c, &self.p)
    }
}
fn has(v: &Value, code: &str) -> bool {
    v["reasons"]
        .as_array()
        .unwrap()
        .iter()
        .any(|r| r["code"] == code)
}
#[test]
fn complete_reviewed_and_bound_evidence_passes() {
    let f = Fixture::new();
    let r = f.report();
    assert_eq!(r["status"], "PASS", "{r}");
    assert_eq!(r["source_counts"], json!({"real":100,"synthetic":100}));
}
#[test]
fn manifest_tamper_is_blocked() {
    let f = Fixture::new();
    let mut b = fs::read(&f.c).unwrap();
    b.push(b'\n');
    fs::write(&f.c, b).unwrap();
    assert!(has(&f.report(), "manifest_hash_mismatch"));
}
#[test]
fn missing_cases_fail_closed_without_exception() {
    let f = Fixture::new();
    fs::remove_file(&f.c).unwrap();
    let r = f.report();
    for code in [
        "manifest_file_missing",
        "cases_missing",
        "baseline_expectations_hash",
    ] {
        assert!(has(&r, code), "{r}");
    }
}
#[test]
fn unreviewed_case_and_wrong_source_mix_are_blocked() {
    let f = Fixture::new();
    let rows = fs::read_to_string(&f.c)
        .unwrap()
        .lines()
        .enumerate()
        .map(|(i, s)| {
            let mut v: Value = serde_json::from_str(s).unwrap();
            v["metadata"]["provenance"]["kind"] = json!(if i < 190 { "synthetic" } else { "real" });
            if i == 0 {
                v["metadata"]["review"]["status"] = json!("pending");
            }
            v.to_string()
        })
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(&f.c, rows).unwrap();
    let r = f.report();
    assert!(has(&r, "case_review_incomplete"));
    assert!(has(&r, "source_mix_out_of_range"));
}
#[test]
fn baseline_review_and_dataset_binding_are_independent_blocks() {
    let f = Fixture::new();
    let b = f.root.join("reports/baseline.json");
    let mut v: Value = serde_json::from_slice(&fs::read(&b).unwrap()).unwrap();
    v["release_evidence"]["review"]["status"] = json!("pending");
    v["release_evidence"]["dataset_id"] = json!("different-dataset");
    write(&b, &v);
    let r = f.report();
    assert!(has(&r, "baseline_review"));
    assert!(has(&r, "baseline_dataset_binding"));
}
#[test]
fn release_pointer_cannot_escape_repository() {
    let f = Fixture::new();
    let mut p: Value = serde_json::from_slice(&fs::read(&f.p).unwrap()).unwrap();
    p["baseline_report"] = json!("../outside.json");
    write(&f.p, &p);
    assert!(has(&f.report(), "baseline_report_path"));
}
#[test]
fn current_seed_assets_are_explicitly_blocked() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../..");
    let r = evaluate(
        &root,
        &root.join("tests/fixtures/eval/rag/golden/manifest.json"),
        &root.join("tests/fixtures/eval/rag/golden/kb_golden_qa_v1.jsonl"),
        &root.join("reports/kb-eval-baseline/release-pointer.json"),
    );
    assert_eq!(r["status"], "BLOCKED");
    assert_eq!(r["case_count"], 18);
    for code in [
        "case_count_out_of_range",
        "case_provenance_incomplete",
        "case_review_incomplete",
        "release_pointer_missing",
    ] {
        assert!(has(&r, code), "{r}");
    }
}
