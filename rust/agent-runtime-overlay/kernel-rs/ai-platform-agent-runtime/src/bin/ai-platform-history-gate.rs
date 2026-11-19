//! Offline release gate using the exact production persistence decoder.
//! No provider, database mutation, Cargo subprocess or private payload output.
#[path = "../postgres_store/history_codec.rs"]
mod history_codec;

use std::{collections::BTreeSet, path::{Path, PathBuf}};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use history_codec::{WriterIdentity, decode_rollout, encode_rollout, persisted_writer, writer_identity};
type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
const SCENARIOS: [&str; 8] = ["session_start", "ordinary_turn", "tool_roundtrip", "approval_pause", "cancelled_turn", "failed_turn", "compacted_resume", "subagent_history"];
const MAX_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Corpus {
    schema: String,
    candidate: WriterIdentity,
    previous: Option<WriterIdentity>,
    cases: Vec<Case>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Case {
    id: String,
    direction: String,
    scenario: String,
    file: String,
    sha256: String,
    item_count: usize,
    source_receipt: String,
    source_receipt_sha256: String,
    capture_receipt: String,
    capture_receipt_sha256: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Capture {
    schema: String,
    method: String,
    captured_at: String,
    writer: WriterIdentity,
    image_id: String,
    history_sha256: String,
    source_receipt_sha256: String,
    item_count: usize,
}
#[derive(Serialize)]
struct Observed {
    scenario: String,
    sha256: String,
    items: usize,
    legacy_items: usize,
}
fn check(ok: bool, message: &'static str) -> Result<()> {
    if !ok { return Err(message.into()); }
    Ok(())
}
fn digest(bytes: &[u8]) -> String { format!("{:x}", Sha256::digest(bytes)) }
fn pinned_file(root: &Path, relative: &str, expected: &str) -> Result<Vec<u8>> {
    check(history_codec::valid_digest(expected, 64), "invalid_corpus_hash")?;
    let path = Path::new(relative);
    check(!path.is_absolute() && !path.components().any(|c| matches!(c, std::path::Component::ParentDir)), "unsafe_corpus_path")?;
    let path = root.join(path).canonicalize()?;
    check(path.starts_with(root), "corpus_path_escaped_root")?;
    let meta = std::fs::metadata(&path)?;
    check(meta.is_file() && meta.len() <= MAX_BYTES, "invalid_corpus_file_size")?;
    let bytes = std::fs::read(path)?;
    check(digest(&bytes) == expected, "corpus_hash_mismatch")?;
    Ok(bytes)
}
fn validate_source_receipt(bytes: &[u8], writer: &WriterIdentity) -> Result<()> {
    let receipt: Value = serde_json::from_slice(bytes)?;
    check(receipt.pointer("/source/upstream_sha").and_then(Value::as_str) == Some(writer.upstream_revision.as_str())
        && receipt.pointer("/schema_bundle/sha256").and_then(Value::as_str) == Some(writer.schema_sha256.as_str()), "capture_source_identity_mismatch")
}
fn verify_scenario(scenario: &str, values: &[Value]) -> Result<()> {
    let native = |kind: &str| values.iter().any(|v| v["type"] == kind);
    let event = |kind: &str| values.iter().any(|v| v["type"] == "event_msg" && v["payload"]["type"] == kind);
    let response = |kind: &str| values.iter().any(|v| v["type"] == "response_item" && v["payload"]["type"] == kind);
    let matches = match scenario {
        "session_start" => native("session_meta"),
        "ordinary_turn" => event("task_started") && event("task_complete") && response("message"),
        "tool_roundtrip" => response("function_call") && response("function_call_output"),
        "approval_pause" => values.iter().any(|v| v["type"] == "response_item" && v["payload"]["type"] == "function_call"
            && !values.iter().any(|o| o["type"] == "response_item" && o["payload"]["type"] == "function_call_output"
                && o["payload"]["call_id"] == v["payload"]["call_id"])),
        "cancelled_turn" => event("turn_aborted"),
        "failed_turn" => values.iter().any(|v| v["type"] == "event_msg" && v["payload"]["type"] == "task_complete"
            && v["payload"].get("error").is_some_and(|e| !e.is_null())),
        "compacted_resume" => native("compacted") && native("turn_context"),
        "subagent_history" => values.iter().any(|v| v["type"] == "turn_context"
            && v["payload"].get("root_turn_id").is_some_and(|id| id.is_string())),
        _ => false,
    };
    check(matches, "corpus_scenario_not_demonstrated")
}
fn run_gate(manifest: &Path, source_lock: &Path, direction: &str) -> Result<Value> {
    check(["forward", "rollback"].contains(&direction), "invalid_direction")?;
    let manifest_bytes = std::fs::read(manifest)?;
    check(manifest_bytes.len() as u64 <= MAX_BYTES, "manifest_too_large")?;
    let corpus: Corpus = serde_json::from_slice(&manifest_bytes)?;
    check(corpus.schema == "ai-platform/history-corpus/v1", "invalid_corpus_schema")?;
    corpus.candidate.validate()?;
    let previous = corpus.previous.as_ref().ok_or("missing_authenticated_n_minus_one_identity")?;
    previous.validate()?;
    check(previous != &corpus.candidate, "previous_identity_must_differ")?;
    let lock_bytes = std::fs::read(source_lock)?;
    let lock: Value = serde_json::from_slice(&lock_bytes)?;
    check(lock.pointer("/source/upstream_sha").and_then(Value::as_str) == Some(corpus.candidate.upstream_revision.as_str())
        && lock.pointer("/build/app_server_schema_sha256").and_then(Value::as_str) == Some(corpus.candidate.schema_sha256.as_str()), "candidate_source_lock_mismatch")?;
    let decoder = writer_identity()?;
    let expected_decoder = if direction == "forward" { &corpus.candidate } else { previous };
    let expected_writer = if direction == "forward" { previous } else { &corpus.candidate };
    check(&decoder == expected_decoder, "wrong_compiled_decoder_identity")?;
    let root = manifest.parent().ok_or("manifest_parent_missing")?.canonicalize()?;
    let mut seen = BTreeSet::new();
    let mut coverage = BTreeSet::new();
    let mut observations = Vec::new();
    for case in corpus.cases.iter().filter(|c| c.direction == direction) {
        check(!case.id.is_empty() && seen.insert(case.id.clone()), "duplicate_or_missing_case_id")?;
        check(SCENARIOS.contains(&case.scenario.as_str()) && case.item_count > 0, "invalid_case_scenario_or_count")?;
        let bytes = pinned_file(&root, &case.file, &case.sha256)?;
        let receipt = pinned_file(&root, &case.source_receipt, &case.source_receipt_sha256)?;
        validate_source_receipt(&receipt, expected_writer)?;
        let capture_bytes = pinned_file(&root, &case.capture_receipt, &case.capture_receipt_sha256)?;
        let capture: Capture = serde_json::from_slice(&capture_bytes)?;
        check(capture.schema == "ai-platform/history-capture/v1" && capture.method == "postgresql_rollout_items"
            && capture.writer == *expected_writer && capture.history_sha256 == case.sha256
            && capture.source_receipt_sha256 == case.source_receipt_sha256
            && capture.item_count == case.item_count, "invalid_capture_provenance")?;
        check(capture.image_id.strip_prefix("sha256:").is_some_and(|d| history_codec::valid_digest(d, 64)), "missing_capture_image_identity")?;
        chrono::DateTime::parse_from_rfc3339(&capture.captured_at)?;
        let mut values = Vec::new();
        let mut legacy = 0;
        let mut expected_writer_items = 0;
        for line in std::str::from_utf8(&bytes)?.lines() {
            check(!line.trim().is_empty(), "blank_corpus_record")?;
            let raw: Value = serde_json::from_str(line)?;
            if let Some(writer) = persisted_writer(&raw)? {
                // A captured full history may contain several older writers.
                // Validate every stamp; the pinned capture identifies the actual
                // release which exported this immutable stream.
                expected_writer_items += usize::from(&writer == expected_writer);
            } else { legacy += 1; }
            let native = decode_rollout(raw)?;
            let canonical = serde_json::to_value(&native)?;
            let roundtrip = decode_rollout(encode_rollout(&native)?)?;
            check(serde_json::to_value(roundtrip)? == canonical, "history_roundtrip_changed_semantics")?;
            values.push(canonical);
        }
        check(values.len() == case.item_count, "corpus_item_count_mismatch")?;
        if direction == "rollback" {
            check(expected_writer_items > 0, "rollback_corpus_contains_no_candidate_written_items")?;
        }
        verify_scenario(&case.scenario, &values)?;
        coverage.insert(case.scenario.clone());
        observations.push(Observed { scenario: case.scenario.clone(), sha256: case.sha256.clone(), items: values.len(), legacy_items: legacy });
    }
    check(SCENARIOS.iter().all(|s| coverage.contains(*s)), "required_n_minus_one_scenario_missing")?;
    Ok(json!({"schema":"ai-platform/history-gate/v1","result":"PASS","direction":direction,
        "decoder":decoder,"candidate":corpus.candidate,"previous":previous,
        "source_lock_sha256":digest(&lock_bytes),"manifest_sha256":digest(&manifest_bytes),
        "observations":observations,"skipped":0,"scope":"native rollout decode and semantic roundtrip only; not DB restore or live replay acceptance"}))
}
fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args == ["--help"] {
        println!("ai-platform-history-gate --manifest PATH --source-lock PATH --direction forward|rollback --receipt /external/PATH");
        return;
    }
    if args.len() != 8 || args[0] != "--manifest" || args[2] != "--source-lock"
        || args[4] != "--direction" || args[6] != "--receipt" || !Path::new(&args[7]).is_absolute() {
        eprintln!("history-gate: explicit manifest/source-lock/direction and absolute receipt are required");
        std::process::exit(2);
    }
    let report = PathBuf::from(&args[7]);
    let result = run_gate(Path::new(&args[1]), Path::new(&args[3]), &args[5]);
    let value = match &result {
        Ok(value) => value.clone(),
        Err(_) => json!({"schema":"ai-platform/history-gate/v1","result":"BLOCKED","direction":args[5],
            "reason":"missing, invalid or incompatible authenticated corpus/decoder; raw history is never logged","skipped":0}),
    };
    let written = (|| -> Result<()> {
        std::fs::create_dir_all(report.parent().ok_or("report_parent_missing")?)?;
        std::fs::write(&report, format!("{}\n", serde_json::to_string_pretty(&value)?))?;
        Ok(())
    })();
    if result.is_err() || written.is_err() {
        eprintln!("history-gate: BLOCKED; inspect the redacted receipt and corpus provenance");
        std::process::exit(1);
    }
    println!("history-gate: PASS ({})", args[5]);
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn absent_previous_and_empty_coverage_cannot_pass_release() {
        let root = tempfile::tempdir().unwrap();
        let manifest = root.path().join("corpus.json");
        let lock = root.path().join("lock.json");
        let candidate = writer_identity().unwrap();
        let mut value = json!({"schema":"ai-platform/history-corpus/v1",
            "candidate":candidate,"previous":null,"cases":[]});
        std::fs::write(&manifest, serde_json::to_vec(&value).unwrap()).unwrap();
        assert_eq!(run_gate(&manifest, &lock, "forward").unwrap_err().to_string(),
            "missing_authenticated_n_minus_one_identity");
        // Synthetic negative metadata only; never historical fixture evidence.
        value["previous"] = json!({"format":history_codec::FORMAT,
            "upstream_revision":"0".repeat(40),"schema_sha256":"1".repeat(64)});
        std::fs::write(&manifest, serde_json::to_vec(&value).unwrap()).unwrap();
        std::fs::write(&lock, serde_json::to_vec(&json!({
            "source":{"upstream_sha":candidate.upstream_revision},
            "build":{"app_server_schema_sha256":candidate.schema_sha256}})).unwrap()).unwrap();
        assert_eq!(run_gate(&manifest, &lock, "forward").unwrap_err().to_string(),
            "required_n_minus_one_scenario_missing");
    }

    #[test]
    fn pinned_corpus_rejects_escape_and_tampering() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("record"), b"old").unwrap();
        assert!(pinned_file(root.path(), "record", &digest(b"old")).is_ok());
        assert!(pinned_file(root.path(), "record", &digest(b"changed")).is_err());
        assert!(pinned_file(root.path(), "../record", &digest(b"old")).is_err());
        assert!(pinned_file(root.path(), "/record", &digest(b"old")).is_err());
    }
    #[test]
    fn scenario_labels_cannot_replace_actual_history() {
        assert!(verify_scenario("tool_roundtrip", &[]).is_err());
        assert!(verify_scenario("failed_turn", &[json!({"type":"event_msg","payload":{"type":"task_complete","error":null}})]).is_err());
        assert!(verify_scenario("cancelled_turn", &[json!({"type":"event_msg","payload":{"type":"turn_aborted"}})]).is_ok());
    }
}
