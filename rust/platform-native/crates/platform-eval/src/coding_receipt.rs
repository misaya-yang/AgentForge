//! Fail-closed independently keyed coding host-test receipt verification.
use crate::{
    Result,
    canonical::{digest, sha256},
    seals::{finite, hash, integer, strict_keys},
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};
pub const SUITE_ID: &str = "general-agent.real-coding.v2";
pub const SCENARIO_ID: &str = "coding.parallel.settlement-retry";
pub const RECEIPT_SCHEMA: &str = "coding-agent-patch-validation/v2";
pub const HMAC_ENVIRONMENT_NAME: &str = "GENERAL_AGENT_CODING_HOST_TEST_HMAC_KEY";
fn strings(v: &Value) -> Result<Vec<String>> {
    let a = v.as_array().ok_or("expected string list")?;
    a.iter()
        .map(|v| {
            v.as_str()
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .ok_or_else(|| "expected nonempty string".into())
        })
        .collect()
}
pub fn validator_policy(contract_path: &Path, verifier_path: &Path) -> Result<Value> {
    for p in [contract_path, verifier_path] {
        let m = std::fs::symlink_metadata(p)?;
        if !m.is_file() || m.file_type().is_symlink() {
            return Err("coding policy source missing or symlinked".into());
        }
    }
    let raw = std::fs::read(contract_path)?;
    let c = crate::strict_json::parse(&raw)?;
    strict_keys(
        &c,
        &[
            "schema_version",
            "case_id",
            "allowed_changes",
            "required_investigation_areas",
            "required_diagnosis_evidence",
            "required_rejected_hint_ids",
            "max_changed_lines",
            "minimum_child_duration_ms",
            "minimum_parallel_overlap_ms",
            "sandbox_image_reference",
            "sandbox_image_config_digest",
            "sandbox_platform",
            "test_commands",
            "execution_boundary",
        ],
        &[],
        "validator contract",
    )?;
    if c["schema_version"] != "coding-agent-validator/v2" || c["case_id"] != SCENARIO_ID {
        return Err("unsupported coding validator contract".into());
    }
    let mut changes = strings(&c["allowed_changes"])?;
    changes.sort();
    if changes.is_empty() || changes.windows(2).any(|w| w[0] == w[1]) {
        return Err("validator allowed_changes malformed".into());
    }
    let max = integer(&c["max_changed_lines"], 1, 10000, "max_changed_lines")?;
    let overlap = finite(
        &c["minimum_parallel_overlap_ms"],
        25.,
        "minimum_parallel_overlap_ms",
    )?;
    let image = c["sandbox_image_reference"]
        .as_str()
        .ok_or("sandbox image must be digest pinned")?;
    let (_, image_hash) = image
        .rsplit_once("@sha256:")
        .ok_or("sandbox image must be digest pinned")?;
    hash(&json!(image_hash), "image digest")?;
    let config_id = c["sandbox_image_config_digest"]
        .as_str()
        .and_then(|s| s.strip_prefix("sha256:"))
        .ok_or("sandbox image config digest missing")?;
    hash(&json!(config_id), "image config digest")?;
    let platform = c["sandbox_platform"]
        .as_str()
        .filter(|p| ["linux/amd64", "linux/arm64"].contains(p))
        .ok_or("sandbox platform invalid")?;
    let commands = c["test_commands"]
        .as_array()
        .filter(|a| a.len() == 3)
        .ok_or("exactly three host tests required")?;
    for command in commands {
        if strings(command)?.is_empty() {
            return Err("empty host test argv".into());
        }
    }
    Ok(
        json!({"validator_contract_sha256":digest(&c),"validator_contract_file_sha256":sha256(&raw),"verifier_executable_sha256":sha256(&std::fs::read(verifier_path)?),"receipt_verifier_executable_sha256":sha256(include_bytes!("coding_receipt.rs")),"sandbox_image_reference":image,"sandbox_image_config_digest":format!("sha256:{config_id}"),"sandbox_platform":platform,"test_commands":commands,"allowed_changes":changes,"max_changed_lines":max,"minimum_parallel_overlap_ms":overlap}),
    )
}
pub struct Expected<'a> {
    pub scenario_contract_sha256: &'a str,
    pub observations_sha256: &'a str,
    pub collector_attestation_key_id: &'a str,
    pub trials: &'a BTreeMap<i64, Value>,
    pub source_artifacts: Option<&'a Value>,
    pub plugin_definitions: Option<&'a Value>,
    pub runtime_binding: Option<&'a Value>,
    pub raw_sse_artifact: Option<&'a Value>,
}
pub fn verify_receipt(
    path: &Path,
    key: &str,
    policy: &Value,
    expected: &Expected<'_>,
) -> Result<Value> {
    let (doc, file_hash) = crate::seals::read_private(path, 5_000_000)?;
    let payload = crate::seals::verify_document(&doc, key)?;
    verify_payload(&payload, &file_hash, key, policy, expected)
}
pub fn verify_payload(
    p: &Value,
    file_hash: &str,
    key: &str,
    policy: &Value,
    e: &Expected<'_>,
) -> Result<Value> {
    strict_keys(
        p,
        &[
            "schema_version",
            "suite_id",
            "scenario_id",
            "scenario_contract_sha256",
            "observations_sha256",
            "validator_contract_sha256",
            "verifier_executable_sha256",
            "source_artifacts",
            "plugin_definitions",
            "runtime_binding",
            "raw_sse_artifact",
            "collector_attestation_key_id",
            "trials",
            "three_run_policy_met",
            "validation_passed",
            "acceptance_eligible",
            "passed",
            "evidence_boundary",
        ],
        &[],
        "coding receipt",
    )?;
    if p["schema_version"] != RECEIPT_SCHEMA
        || p["suite_id"] != SUITE_ID
        || p["scenario_id"] != SCENARIO_ID
    {
        return Err("coding receipt identity does not match".into());
    }
    for (field, value) in [
        ("scenario_contract_sha256", e.scenario_contract_sha256),
        ("observations_sha256", e.observations_sha256),
        (
            "collector_attestation_key_id",
            e.collector_attestation_key_id,
        ),
    ] {
        if p[field] != value {
            return Err(format!("coding {field} binding does not match").into());
        }
    }
    for (field, value) in [
        ("source_artifacts", e.source_artifacts),
        ("plugin_definitions", e.plugin_definitions),
        ("runtime_binding", e.runtime_binding),
        ("raw_sse_artifact", e.raw_sse_artifact),
    ] {
        if value.is_some_and(|v| v != &p[field]) {
            return Err(format!("coding {field} binding does not match").into());
        }
    }
    for field in [
        "three_run_policy_met",
        "validation_passed",
        "acceptance_eligible",
        "passed",
    ] {
        if p[field] != true {
            return Err("coding receipt is non-live or not fully passed".into());
        }
    }
    for field in ["validator_contract_sha256", "verifier_executable_sha256"] {
        if p[field] != policy[field] {
            return Err(format!("coding {field} does not match").into());
        }
    }
    let trials = p["trials"]
        .as_array()
        .filter(|a| a.len() == 3)
        .ok_or("exactly three trials required")?;
    if e.trials.keys().copied().collect::<Vec<_>>() != [1, 2, 3] {
        return Err("expected trials must be 1, 2, 3".into());
    }
    let image_ref = policy["sandbox_image_reference"]
        .as_str()
        .ok_or("image reference missing")?;
    let image_id = policy["sandbox_image_config_digest"]
        .as_str()
        .ok_or("image config digest missing")?;
    let mut evidence = BTreeMap::new();
    for t in trials {
        strict_keys(
            t,
            &[
                "trial",
                "observation_sha256",
                "accepted",
                "execution_checks",
                "timeline",
                "patch",
                "sandbox_image",
                "host_test_receipts",
                "errors",
            ],
            &[],
            "coding trial",
        )?;
        let n = integer(&t["trial"], 1, 3, "trial")?;
        if evidence.contains_key(&n) {
            return Err("duplicate coding trial".into());
        }
        let expected = &e.trials[&n];
        if t["observation_sha256"] != expected["observation_sha256"] {
            return Err("coding trial binds another observation".into());
        }
        let checks = t["execution_checks"]
            .as_array()
            .filter(|a| !a.is_empty())
            .ok_or("missing execution checks")?;
        if expected["execution_checks"].is_array()
            && t["execution_checks"] != expected["execution_checks"]
        {
            return Err("execution checks do not match".into());
        }
        for c in checks {
            if c["passed"] != true || !c["check_id"].is_string() {
                return Err("execution checks not passed".into());
            }
            hash(&c["observed_sha256"], "execution observed digest")?;
        }
        if t["accepted"] != true || t["errors"] != json!([]) {
            return Err("coding trial not accepted".into());
        }
        let timeline = &t["timeline"];
        strict_keys(
            timeline,
            &[
                "overlap_ms",
                "spawn_call_id_sha256",
                "child_terminal_sha256s",
                "last_child_terminal_event_index",
                "spawn_result_event_index",
                "final_json_event_index",
                "parent_terminal_event_index",
            ],
            &[],
            "timeline",
        )?;
        if finite(&timeline["overlap_ms"], 25., "overlap")?
            < finite(
                &policy["minimum_parallel_overlap_ms"],
                25.,
                "policy overlap",
            )?
        {
            return Err("overlap below policy".into());
        }
        hash(&timeline["spawn_call_id_sha256"], "spawn call digest")?;
        let children = timeline["child_terminal_sha256s"]
            .as_array()
            .filter(|a| a.len() == 2 && a[0] != a[1])
            .ok_or("child digests malformed")?;
        for child in children {
            hash(child, "child terminal digest")?;
        }
        let order = [
            "last_child_terminal_event_index",
            "spawn_result_event_index",
            "final_json_event_index",
            "parent_terminal_event_index",
        ]
        .into_iter()
        .map(|f| integer(&timeline[f], 0, 1_000_000, f))
        .collect::<Result<Vec<_>>>()?;
        if order.windows(2).any(|w| w[0] >= w[1]) {
            return Err("coding synthesis order invalid".into());
        }
        let patch = &t["patch"];
        strict_keys(
            patch,
            &["changed_files", "changed_lines", "replacement_sha256"],
            &[],
            "patch",
        )?;
        let mut files = strings(&patch["changed_files"])?;
        files.sort();
        if files.iter().collect::<BTreeSet<_>>().len() != files.len()
            || json!(files) != policy["allowed_changes"]
        {
            return Err("changed file scope does not match".into());
        }
        integer(
            &patch["changed_lines"],
            1,
            policy["max_changed_lines"]
                .as_i64()
                .ok_or("invalid line budget")?,
            "changed lines",
        )?;
        let replacements = patch["replacement_sha256"]
            .as_object()
            .ok_or("replacement digests missing")?;
        let mut replacement_files = replacements.keys().cloned().collect::<Vec<_>>();
        replacement_files.sort();
        if replacement_files != files {
            return Err("replacement digests incomplete".into());
        }
        for value in replacements.values() {
            hash(value, "replacement digest")?;
        }
        let image = &t["sandbox_image"];
        strict_keys(image, &["reference", "image_id"], &[], "image")?;
        if image["reference"] != image_ref || image["image_id"] != image_id {
            return Err("image not release pinned".into());
        }
        let tests = t["host_test_receipts"]
            .as_array()
            .filter(|a| a.len() == 3)
            .ok_or("exactly three host tests required")?;
        for (i, test) in tests.iter().enumerate() {
            strict_keys(
                test,
                &["command", "exit_code", "timed_out", "duration_ms", "passed"],
                &[],
                "host test",
            )?;
            if test["command"] != policy["test_commands"][i] {
                return Err("test command substituted".into());
            }
            if test["exit_code"].as_i64() != Some(0)
                || test["timed_out"] != false
                || test["passed"] != true
            {
                return Err("host test did not pass".into());
            }
            finite(&test["duration_ms"], 0., "host test duration")?;
        }
        evidence.insert(n,json!({"trial":n,"observation_sha256":t["observation_sha256"],"patch":patch,"patch_sha256":digest(patch),"sandbox_image":image,"sandbox_image_sha256":digest(image),"host_test_receipts":tests,"host_test_receipts_sha256":digest(&t["host_test_receipts"])}));
    }
    Ok(
        json!({"schema_version":"coding-host-test-evidence/v2","receipt_schema":RECEIPT_SCHEMA,"receipt_file_sha256":file_hash,"receipt_payload_sha256":digest(p),"host_attestation_key_id":&sha256(key.as_bytes())[..24],"suite_id":SUITE_ID,"scenario_id":SCENARIO_ID,"scenario_contract_sha256":e.scenario_contract_sha256,"observations_sha256":e.observations_sha256,"validator_policy":policy,"trials":evidence.into_values().collect::<Vec<_>>()}),
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    fn fixtures() -> (Value, Value, BTreeMap<i64, Value>) {
        let h = "a".repeat(64);
        let b = "b".repeat(64);
        let commands = json!([
            ["cargo", "test", "--test", "idempotency"],
            ["cargo", "test", "--test", "allocation"],
            ["cargo", "test", "--test", "service"]
        ]);
        let policy = json!({"validator_contract_sha256":h,"verifier_executable_sha256":h,"sandbox_image_reference":format!("rust@sha256:{h}"),"sandbox_image_config_digest":format!("sha256:{h}"),"sandbox_platform":"linux/amd64","test_commands":commands,"allowed_changes":["src/allocation.rs","src/idempotency.rs"],"max_changed_lines":48,"minimum_parallel_overlap_ms":25});
        let mut expected = BTreeMap::new();
        let trials=(1..=3).map(|n|{let checks=json!([{"check_id":"execution","passed":true,"observed_sha256":h}]);expected.insert(n,json!({"observation_sha256":h,"execution_checks":checks}));json!({"trial":n,"observation_sha256":h,"accepted":true,"execution_checks":checks,"timeline":{"overlap_ms":30,"spawn_call_id_sha256":h,"child_terminal_sha256s":[h,b],"last_child_terminal_event_index":4,"spawn_result_event_index":5,"final_json_event_index":6,"parent_terminal_event_index":7},"patch":{"changed_files":["src/allocation.rs","src/idempotency.rs"],"changed_lines":20,"replacement_sha256":{"src/allocation.rs":h,"src/idempotency.rs":b}},"sandbox_image":{"reference":policy["sandbox_image_reference"],"image_id":format!("sha256:{h}")},"host_test_receipts":commands.as_array().unwrap().iter().map(|command|json!({"command":command,"exit_code":0,"timed_out":false,"duration_ms":1,"passed":true})).collect::<Vec<_>>(),"errors":[]})}).collect::<Vec<_>>();
        let payload = json!({"schema_version":RECEIPT_SCHEMA,"suite_id":SUITE_ID,"scenario_id":SCENARIO_ID,"scenario_contract_sha256":h,"observations_sha256":h,"validator_contract_sha256":h,"verifier_executable_sha256":h,"source_artifacts":[],"plugin_definitions":[],"runtime_binding":{},"raw_sse_artifact":{},"collector_attestation_key_id":"collector","trials":trials,"three_run_policy_met":true,"validation_passed":true,"acceptance_eligible":true,"passed":true,"evidence_boundary":"isolated host tests"});
        (payload, policy, expected)
    }
    #[test]
    fn independent_receipt_checks_reject_partial_or_substituted_evidence() {
        let (payload, policy, trials) = fixtures();
        let h = "a".repeat(64);
        let expected = Expected {
            scenario_contract_sha256: &h,
            observations_sha256: &h,
            collector_attestation_key_id: "collector",
            trials: &trials,
            source_artifacts: None,
            plugin_definitions: None,
            runtime_binding: None,
            raw_sse_artifact: None,
        };
        assert!(verify_payload(&payload, &h, &"key".repeat(16), &policy, &expected).is_ok());
        for field in [
            "passed",
            "validation_passed",
            "acceptance_eligible",
            "three_run_policy_met",
        ] {
            let mut changed = payload.clone();
            changed[field] = json!(false);
            assert!(verify_payload(&changed, &h, &"key".repeat(16), &policy, &expected).is_err());
        }
        let mut changed = payload.clone();
        changed["trials"][0]["host_test_receipts"][0]["command"] = json!(["true"]);
        assert!(verify_payload(&changed, &h, &"key".repeat(16), &policy, &expected).is_err());
        let mut changed = payload;
        changed["trials"][1]["trial"] = json!(1);
        assert!(verify_payload(&changed, &h, &"key".repeat(16), &policy, &expected).is_err());
    }
    #[test]
    fn old_language_receipts_cannot_certify_native_fixture() {
        let (mut payload, policy, trials) = fixtures();
        payload["schema_version"] = json!("coding-agent-patch-validation/v1");
        let h = "a".repeat(64);
        let expected = Expected {
            scenario_contract_sha256: &h,
            observations_sha256: &h,
            collector_attestation_key_id: "collector",
            trials: &trials,
            source_artifacts: None,
            plugin_definitions: None,
            runtime_binding: None,
            raw_sse_artifact: None,
        };
        assert!(verify_payload(&payload, &h, &"key".repeat(16), &policy, &expected).is_err());
    }
}
