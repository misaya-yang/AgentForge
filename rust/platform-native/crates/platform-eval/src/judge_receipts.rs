//! Sealed real-agent release judging. Deterministic preflight precedes credentials/network.
use crate::{
    Result,
    canonical::{canonical, digest, sha256},
    seals::{constant_eq, hash, integer, strict_keys},
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};
mod lifecycle;
mod preflight;
mod provider;
mod release;
pub use preflight::{PrepareOptions, prepare_input};
pub use provider::{
    DeepSeekDualJudge, DeepSeekSettings, build_judge_messages, validate_judge_result,
};
pub use release::{evaluate_release, validate_release_inputs};
pub const REPORT_SCHEMA: &str = "real-agent-release-judge-report/v1";
pub const JUDGE_SCHEMA: &str = "real-agent-deepseek-judge/v1";
pub const PROMPT_VERSION: &str = "real-agent-release-judge-prompt/v1";
pub const MANIFEST_DIGEST: &str =
    "52341a05477c37ffe93fa4a77f2343cbd4691745bd7b14a5bde642005c0d6f16";
pub const DIMENSIONS: [(&str, i64, i64); 6] = [
    ("task_outcome", 35, 90),
    ("delegation_quality", 20, 90),
    ("synthesis_grounding", 15, 85),
    ("plugin_use", 10, 90),
    ("safety_recovery", 15, 85),
    ("efficiency", 5, 85),
];
const CHECKS: &[&str] = &[
    "terminal.current-attempt-success",
    "prompt.digest-bound",
    "delegation.exact-profile-lifecycle",
    "delegation.parallel-overlap",
    "delegation.task-prompts",
    "delegation.canonical-task-object",
    "tools.start-result-paired",
    "tools.delegation-call-observed",
    "tools.spawn-aggregate-success",
    "tools.no-extra-parent-side-effects",
    "lifecycle.delegation-synthesis-order",
];
#[derive(Clone)]
pub struct PreparedTrial {
    pub suite_id: String,
    pub scenario: Value,
    pub observation: Value,
    pub golden_validation: Value,
    pub provenance: Value,
    pub merged_receipt_sha256: String,
    pub deterministic_summary: Value,
    pub evidence_index: Vec<Value>,
    pub source_contents: Vec<Value>,
}
impl PreparedTrial {
    pub fn scenario_id(&self) -> &str {
        text(&self.scenario, "scenario_id")
    }
    pub fn domain(&self) -> &str {
        text(&self.scenario, "domain")
    }
    pub fn trial(&self) -> i64 {
        self.observation["trial"].as_i64().unwrap_or(0)
    }
}
#[derive(Clone)]
pub struct PreparedInput {
    pub suite_id: String,
    pub scenario_contract_sha256: String,
    pub merged_receipt_sha256: String,
    pub trials: Vec<PreparedTrial>,
    pub coding_host_test_evidence: Option<Value>,
}
#[derive(Clone)]
pub struct JudgeCall {
    pub audit_lane: String,
    pub model_requested: String,
    pub model_returned: String,
    pub request_sha256: String,
    pub response_sha256: String,
    pub content_sha256: String,
    pub usage: Value,
    pub result: Value,
}
pub trait DualJudge {
    fn metadata(&self) -> Value;
    fn judge<'a>(
        &'a self,
        trial: &'a PreparedTrial,
        lane: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<JudgeCall>> + Send + 'a>>;
}
fn require(ok: bool, message: impl Into<String>) -> Result<()> {
    if ok {
        Ok(())
    } else {
        Err(message.into().into())
    }
}
fn arr(v: &Value) -> &[Value] {
    v.as_array().map(Vec::as_slice).unwrap_or(&[])
}
fn text<'a>(v: &'a Value, key: &str) -> &'a str {
    v[key].as_str().unwrap_or("")
}
fn identifier(v: &Value, label: &str) -> Result<String> {
    let s = v
        .as_str()
        .ok_or_else(|| format!("{label} must be an identifier"))?;
    require(
        regex::Regex::new(r"^[A-Za-z0-9][A-Za-z0-9._:/-]{0,127}$")?.is_match(s),
        format!("invalid {label}"),
    )?;
    Ok(s.into())
}
fn bounded<'a>(v: &'a Value, max: usize, label: &str) -> Result<&'a str> {
    v.as_str()
        .filter(|s| !s.trim().is_empty() && s.chars().count() <= max)
        .ok_or_else(|| format!("{label} must be nonempty text up to {max} characters").into())
}
fn secret_patterns() -> &'static Vec<regex::Regex> {
    static P: std::sync::LazyLock<Vec<regex::Regex>> = std::sync::LazyLock::new(|| {
        [r"\bsk-[A-Za-z0-9_-]{12,}\b",r"(?i)\bbearer\s+[A-Za-z0-9._~+/=-]{12,}",r#"(?i)\b(?:api[_ -]?key|access[_ -]?token|password|secret)\s*[:=]\s*['"]?[A-Za-z0-9._~+/=-]{8,}"#].into_iter().map(|p|regex::Regex::new(p).unwrap()).collect()
    });
    &P
}
pub fn redact_text(value: &str) -> String {
    secret_patterns().iter().fold(value.to_owned(), |s, p| {
        p.replace_all(&s, "[REDACTED]").into_owned()
    })
}
fn redact(value: &Value) -> Value {
    match value {
        Value::String(s) => json!(redact_text(s)),
        Value::Array(a) => json!(a.iter().map(redact).collect::<Vec<_>>()),
        Value::Object(m) => json!(
            m.iter()
                .map(|(k, v)| (k.clone(), redact(v)))
                .collect::<BTreeMap<_, _>>()
        ),
        _ => value.clone(),
    }
}
fn has_secret(value: &Value) -> bool {
    let rendered = canonical(value);
    rendered.contains("[REDACTED]") || secret_patterns().iter().any(|p| p.is_match(&rendered))
}
fn unsigned(value: &Value, field: &str) -> Value {
    let mut v = value.clone();
    if let Some(m) = v.as_object_mut() {
        m.remove(field);
    }
    v
}
pub fn load_json(path: &Path) -> Result<Value> {
    let v = crate::strict_json::parse(&std::fs::read(path)?)?;
    require(v.is_object(), "input must be a JSON object")?;
    Ok(v)
}
pub fn write_report(path: &Path, value: &Value) -> Result<()> {
    use std::io::Write;
    require(!path.is_symlink(), "refusing judge report symlink")?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut opts = std::fs::OpenOptions::new();
    opts.create(true).truncate(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let mut f = opts.open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    let sorted: Value = serde_json::from_str(&canonical(value))?;
    f.write_all(format!("{}\n", serde_json::to_string_pretty(&sorted)?).as_bytes())?;
    f.sync_all()?;
    Ok(())
}
pub fn failure_report(error: &str, prepared: Option<&[PreparedInput]>) -> Value {
    let preflight = match prepared {
        None => json!({"passed":false}),
        Some(items) => {
            json!({"passed":true,"suite_count":items.len(),"trial_count":items.iter().map(|i|i.trials.len()).sum::<usize>(),"scenario_contract_sha256":items.iter().map(|i|&i.scenario_contract_sha256).collect::<Vec<_>>(),"merged_receipt_sha256":items.iter().map(|i|&i.merged_receipt_sha256).collect::<Vec<_>>()})
        }
    };
    let prefix = if prepared.is_some() {
        "judge setup/execution failed closed"
    } else {
        "deterministic release preflight failed closed"
    };
    json!({"schema_version":REPORT_SCHEMA,"generated_at":chrono::Utc::now().to_rfc3339(),"status":"fail","passed":false,"raw_score":0.0,"score":0.0,"threshold":92.0,"deterministic_preflight":preflight,"failures":[format!("{prefix}: {}",redact_text(error))],"domains":[],"trials":[]})
}

#[cfg(test)]
mod tests {
    use super::*;
    fn oracle() -> (Value, PreparedTrial) {
        let v: Value =
            serde_json::from_str(include_str!("judge_receipts/oracle_fixture.json")).unwrap();
        let t = PreparedTrial {
            suite_id: text(&v, "suite_id").into(),
            scenario: v["scenario"].clone(),
            observation: v["observation"].clone(),
            golden_validation: v["golden_validation"].clone(),
            provenance: v["provenance"].clone(),
            merged_receipt_sha256: text(&v, "merged_receipt_sha256").into(),
            deterministic_summary: v["deterministic_summary"].clone(),
            evidence_index: arr(&v["evidence_index"]).to_vec(),
            source_contents: arr(&v["source_contents"]).to_vec(),
        };
        (v, t)
    }
    #[test]
    fn exact_source_prompt_and_lifecycle_oracle() {
        let (v, t) = oracle();
        for lane in ["A", "B"] {
            assert_eq!(build_judge_messages(&t, lane).unwrap(), v["messages"][lane]);
        }
        let (failures, summary) =
            lifecycle::validate(&t.scenario, &t.observation, &BTreeMap::new());
        assert_eq!(json!([failures, summary]), v["lifecycle"]);
    }
    #[test]
    fn schema_and_evidence_fail_closed() {
        let (v, t) = oracle();
        let valid = &v["valid_judge_result"];
        validate_judge_result(valid, &t, "A").unwrap();
        for mutation in [
            "extra",
            "unknown_evidence",
            "candidate_only",
            "wrong_identity",
            "score_overflow",
            "bool_score",
        ] {
            let mut r = valid.clone();
            match mutation {
                "extra" => r["unexpected"] = json!(true),
                "unknown_evidence" => {
                    r["dimensions"]["efficiency"]["evidence_refs"] = json!(["missing"])
                }
                "candidate_only" => {
                    r["dimensions"]["efficiency"]["evidence_refs"] = json!(["candidate:output"])
                }
                "wrong_identity" => r["audit_lane"] = json!("B"),
                "score_overflow" => r["dimensions"]["efficiency"]["score"] = json!(101),
                _ => r["dimensions"]["efficiency"]["score"] = json!(true),
            }
            assert!(validate_judge_result(&r, &t, "A").is_err(), "{mutation}");
        }
    }
    #[test]
    fn sealed_preflight_source_fixture_and_tamper() {
        let v: Value =
            serde_json::from_str(include_str!("judge_receipts/preflight_fixture.json")).unwrap();
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("agent-plugins")).unwrap();
        std::fs::write(tmp.path().join("source.txt"), text(&v, "source_content")).unwrap();
        let collector = "c".repeat(32);
        let golden = "g".repeat(32);
        let nonce = "9".repeat(64);
        let options = PrepareOptions {
            repository: tmp.path(),
            scenario_directory: tmp.path(),
            expected_receipt_sha256: text(&v["receipt"], "merged_receipt_sha256"),
            collector_hmac_key: Some(&collector),
            golden_hmac_key: Some(&golden),
            coding_host_test_hmac_key: None,
            coding_host_test_receipt_path: None,
            expected_suite_nonce: Some(&nonce),
        };
        let prepared = prepare_input(&v["contract"], &v["receipt"], &options).unwrap();
        assert_eq!(prepared.trials.len(), 3);
        assert_eq!(
            prepared.trials[0].deterministic_summary,
            v["expected_summary"]
        );
        let mut forged = v["receipt"].clone();
        forged["trials"][0]["observation"]["candidate_output"] = json!("forged");
        assert!(prepare_input(&v["contract"], &forged, &options).is_err());
        std::fs::write(tmp.path().join("source.txt"), "altered source").unwrap();
        assert!(prepare_input(&v["contract"], &v["receipt"], &options).is_err());
    }
    #[test]
    fn provider_settings_are_pinned_and_secret_redaction_is_recursive() {
        let mut settings = DeepSeekSettings {
            api_key: "fixture".into(),
            base_url: "https://api.deepseek.com".into(),
            model: "deepseek-chat".into(),
            timeout_seconds: 90.,
            max_attempts: 3,
        };
        settings.validate().unwrap();
        settings.base_url = "https://example.invalid".into();
        assert!(settings.validate().is_err());
        let value = json!({"message":"Bearer abcdefghijklmnopqrstuvwxyz"});
        assert!(has_secret(&value));
        assert_eq!(redact(&value), json!({"message":"[REDACTED]"}));
    }
}
