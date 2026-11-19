//! Provider-free, server-owned release integrity candidate and gate contracts.
use super::{AgentError, object, spec, text};
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use std::collections::BTreeSet;
const FINGERPRINT_KEYS: &[&str] = &[
    "spec_hash",
    "model_id",
    "provider_id",
    "model_authorization_hash",
    "prompt_hash",
    "tool_schema_hash",
    "skill_manifest_hash",
    "knowledge_revision",
    "eval_dataset_manifest_hash",
    "runtime_version",
    "snapshot_hash",
    "channel_policy_hash",
];
fn hash(v: &Value) -> String {
    format!("sha256:{}", spec::hash(v))
}
fn plain(s: &str) -> &str {
    s.strip_prefix("sha256:").unwrap_or(s)
}
fn error(code: &str) -> AgentError {
    AgentError::code(409, code)
}
pub fn profile(configured: &str) -> Value {
    let configured = configured.trim();
    let available = matches!(configured, "" | "offline_v1");
    json!({"profile_id":if configured.is_empty(){"offline_v1"}else{configured},"profile_version":if available{"2026-07-19"}else{"unconfigured"},"available":available,"execution_scope":if available{"provider_free_release_integrity"}else{"production_unavailable"},"model_quality_evaluated":false,"blocking_evaluators":if available{vec!["candidate_integrity","resource_readiness","secret_safety"]}else{vec![]},"critical_pass_rate":1.0})
}
pub fn normalized_policy(raw: &Value) -> Result<Value, AgentError> {
    if !raw.is_null() {
        super::routes::closed(
            raw,
            &[
                "attachments",
                "high_risk_tools",
                "allowed_origins",
                "expires_at",
                "requests_per_minute",
                "requests_per_day",
                "ip_requests_per_minute",
                "ip_requests_per_day",
                "publication_requests_per_minute",
                "publication_requests_per_day",
            ],
        )?;
    }
    for key in ["attachments", "high_risk_tools"] {
        if raw.get(key).is_some_and(|v| !v.is_boolean()) {
            return Err(AgentError::code(422, "AGENT_RELEASE_POLICY_INVALID"));
        }
    }
    for (key, max) in [
        ("requests_per_minute", 10000),
        ("requests_per_day", 10000000),
        ("ip_requests_per_minute", 10000),
        ("ip_requests_per_day", 10000000),
        ("publication_requests_per_minute", 100000),
        ("publication_requests_per_day", 100000000),
    ] {
        if raw
            .get(key)
            .is_some_and(|v| v.as_i64().is_none_or(|n| n < 1 || n > max))
        {
            return Err(AgentError::code(422, "AGENT_RELEASE_POLICY_INVALID"));
        }
    }
    if let Some(origins) = raw.get("allowed_origins") {
        let origins = origins
            .as_array()
            .filter(|a| a.len() <= 64)
            .ok_or_else(|| AgentError::code(422, "AGENT_RELEASE_POLICY_INVALID"))?;
        for origin in origins {
            let origin = origin
                .as_str()
                .ok_or_else(|| AgentError::code(422, "AGENT_RELEASE_POLICY_INVALID"))?
                .trim()
                .trim_end_matches('/');
            let url = reqwest::Url::parse(origin)
                .map_err(|_| AgentError::code(422, "AGENT_RELEASE_POLICY_INVALID"))?;
            if origin.contains('*')
                || !matches!(url.scheme(), "http" | "https")
                || url.host_str().is_none()
                || !url.username().is_empty()
                || url.password().is_some()
                || !matches!(url.path(), "" | "/")
                || url.query().is_some()
                || url.fragment().is_some()
            {
                return Err(AgentError::code(422, "AGENT_RELEASE_POLICY_INVALID"));
            }
        }
    }
    let origins: BTreeSet<_> = raw["allowed_origins"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(|s| s.trim().trim_end_matches('/'))
        .filter(|s| !s.is_empty())
        .collect();
    let mut p = json!({"attachments":raw["attachments"].as_bool().unwrap_or(false),"high_risk_tools":raw["high_risk_tools"].as_bool().unwrap_or(false),"allowed_origins":origins});
    if !raw["expires_at"].is_null() {
        let expiry = DateTime::parse_from_rfc3339(text(raw, "expires_at"))
            .map_err(|_| error("AGENT_RELEASE_PUBLIC_EXPIRY_INVALID"))?
            .with_timezone(&Utc);
        p["expires_at"] = expiry.to_rfc3339().into();
    }
    Ok(p)
}
pub fn public_expiry(auth: &str, p: &Value) -> Result<Option<DateTime<Utc>>, AgentError> {
    let expiry = p["expires_at"]
        .as_str()
        .map(DateTime::parse_from_rfc3339)
        .transpose()
        .map_err(|_| error("AGENT_PUBLIC_EXPIRY_INVALID"))?
        .map(|t| t.with_timezone(&Utc));
    if auth == "public" && expiry.is_none() {
        return Err(error("AGENT_PUBLIC_EXPIRY_REQUIRED"));
    }
    if expiry.is_some_and(|e| e <= Utc::now()) {
        return Err(error("AGENT_PUBLIC_EXPIRY_EXPIRED"));
    }
    Ok(expiry)
}
pub fn model_evidence(source: &str, model: &Value, provider: &Value, configured: bool) -> Value {
    let state = json!({"model_id":model["model_id"],"provider_id":model["provider_id"],"access_level":model["access_level"].as_str().unwrap_or("public"),"model_enabled":model["is_enabled"].as_bool().unwrap_or(false),"provider_enabled":provider["is_enabled"].as_bool().unwrap_or(false),"runtime_provider_configured":configured,"model_updated_at":model["updated_at"],"provider_updated_at":provider["updated_at"]});
    json!({"source":source,"model_id":state["model_id"],"provider_id":state["provider_id"],"access_level":state["access_level"],"catalog_state_hash":spec::hash(&state),"runtime_provider_configured":configured})
}
fn capability_key(v: &Value) -> Option<(String, String, String, Option<String>)> {
    let t = v["capability_type"]
        .as_str()
        .or_else(|| v["type"].as_str())
        .unwrap_or("");
    let t = if matches!(t, "native" | "model_native") {
        "platform"
    } else {
        t
    };
    let id = v["resource_id"]
        .as_str()
        .or_else(|| v["id"].as_str())
        .unwrap_or("");
    if !matches!(t, "platform" | "mcp" | "skill" | "connector") || id.is_empty() {
        return None;
    }
    let version = v["resource_version"]
        .as_str()
        .or_else(|| v["version"].as_str())
        .unwrap_or("");
    let h = v["schema_hash"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(|s| format!("sha256:{}", plain(s)));
    Some((t.into(), id.into(), version.into(), h))
}
pub fn candidate(
    resolution: &Value,
    snapshot: &Value,
    channel: &str,
    auth: &str,
    policy: &Value,
    dataset: Option<&Value>,
    authorization: &Value,
) -> Result<Value, AgentError> {
    let agent = &resolution["agent"];
    let draft = &resolution["draft"];
    let fingerprints = &snapshot["fingerprints"];
    let model = &snapshot["model"];
    let p = normalized_policy(policy)?;
    let h = plain(text(draft, "spec_hash"));
    if text(draft, "draft_id").is_empty()
        || draft["revision"].as_i64().unwrap_or(0) <= 0
        || h.is_empty()
    {
        return Err(error("AGENT_RELEASE_DRAFT_IDENTITY_INCOMPLETE"));
    }
    for (valid, code) in [
        (
            plain(text(fingerprints, "spec")) == h,
            "AGENT_RELEASE_SPEC_HASH_MISMATCH",
        ),
        (
            snapshot["agent_id"] == agent["agent_id"],
            "AGENT_RELEASE_AGENT_ID_MISMATCH",
        ),
        (
            snapshot["tenant_id"] == agent["tenant_id"],
            "AGENT_RELEASE_TENANT_ID_MISMATCH",
        ),
        (
            snapshot["schema_version"] == "agent-runtime/v1",
            "AGENT_RELEASE_RUNTIME_VERSION_UNSUPPORTED",
        ),
        (
            snapshot["publication"]["channel"] == channel,
            "AGENT_RELEASE_CHANNEL_MISMATCH",
        ),
        (
            snapshot["publication"]["auth_mode"] == auth,
            "AGENT_RELEASE_AUTH_MODE_MISMATCH",
        ),
        (
            normalized_policy(&snapshot["channel_policy"])? == p,
            "AGENT_RELEASE_CHANNEL_POLICY_MISMATCH",
        ),
    ] {
        if !valid {
            return Err(error(code));
        }
    }
    let mut expected = BTreeSet::new();
    for raw in resolution["capabilities"].as_array().into_iter().flatten() {
        if raw["capability_type"] == "knowledge" {
            continue;
        }
        let t = raw["capability_type"]
            .as_str()
            .or_else(|| raw["type"].as_str())
            .unwrap_or("");
        let risk = raw["risk"]
            .as_str()
            .or_else(|| raw["config"]["risk"].as_str())
            .unwrap_or(if matches!(t, "native" | "model_native" | "platform") {
                "low"
            } else {
                "high"
            });
        if !matches!(risk, "low" | "medium" | "high" | "critical") {
            return Err(error("AGENT_RELEASE_CAPABILITY_RISK_INVALID"));
        }
        if matches!(risk, "high" | "critical") && p["high_risk_tools"] != true {
            continue;
        }
        expected.insert(
            capability_key(raw)
                .ok_or_else(|| error("AGENT_RELEASE_CAPABILITY_IDENTITY_INVALID"))?,
        );
    }
    let actual: BTreeSet<_> = snapshot["capabilities"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(capability_key)
        .collect();
    if actual != expected {
        return Err(error("AGENT_RELEASE_CAPABILITY_SET_MISMATCH"));
    }
    let expected: BTreeSet<_> = resolution["knowledge"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| v["dataset_id"].as_str())
        .collect();
    let actual: BTreeSet<_> = snapshot["knowledge"]["datasets"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    if actual != expected {
        return Err(error("AGENT_RELEASE_KNOWLEDGE_SET_MISMATCH"));
    }
    let (id, version, manifest) = if let Some(d) = dataset {
        if d["tenant_id"] != agent["tenant_id"] {
            return Err(error("AGENT_RELEASE_EVAL_DATASET_TENANT_MISMATCH"));
        }
        let mh = plain(text(d, "manifest_hash"));
        if text(d, "dataset_id").is_empty()
            || text(d, "version").trim().is_empty()
            || mh.len() != 64
            || !mh.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err(error("AGENT_RELEASE_EVAL_DATASET_SNAPSHOT_INVALID"));
        }
        (d["dataset_id"].clone(), d["version"].clone(), json!(mh))
    } else {
        (Value::Null, Value::Null, Value::Null)
    };
    if authorization["model_id"] != model["id"]
        || authorization["provider_id"] != model["provider"]
        || text(authorization, "catalog_state_hash").len() != 64
    {
        return Err(error("AGENT_RELEASE_MODEL_AUTHORIZATION_INVALID"));
    }
    let fingerprint = json!({"spec_hash":h,"model_id":model["id"],"provider_id":model["provider"],"model_authorization_hash":spec::hash(authorization),"prompt_hash":snapshot["instructions"]["prompt_hash"],"tool_schema_hash":fingerprints["tool_schema"],"skill_manifest_hash":fingerprints["skills"],"knowledge_revision":fingerprints["knowledge_revision"],"eval_dataset_manifest_hash":manifest.as_str().map(str::to_owned).unwrap_or_else(||spec::hash(&json!({"dataset_id":null}))),"runtime_version":snapshot["schema_version"],"snapshot_hash":hash(snapshot),"channel_policy_hash":hash(&p)});
    let mut missing: Vec<_> = FINGERPRINT_KEYS
        .iter()
        .filter(|k| text(&fingerprint, k).is_empty())
        .copied()
        .collect();
    missing.sort();
    if !missing.is_empty() {
        return Err(error(&format!(
            "AGENT_RELEASE_FINGERPRINT_INCOMPLETE:{}",
            missing.join(",")
        )));
    }
    let identity = json!({"agent_id":agent["agent_id"],"draft_id":draft["draft_id"],"draft_revision":draft["revision"],"spec_hash":h,"model_id":fingerprint["model_id"],"provider_id":fingerprint["provider_id"],"model_authorization_hash":fingerprint["model_authorization_hash"],"prompt_hash":fingerprint["prompt_hash"],"tool_schema_hash":fingerprint["tool_schema_hash"],"skill_manifest_hash":fingerprint["skill_manifest_hash"],"knowledge_revision":fingerprint["knowledge_revision"],"eval_dataset_id":id,"eval_dataset_version":version,"eval_dataset_manifest_hash":manifest,"runtime_version":fingerprint["runtime_version"]});
    let mut evaluation = identity.clone();
    evaluation["channel"] = channel.into();
    evaluation["auth_mode"] = auth.into();
    evaluation["channel_policy_hash"] = spec::hash(&p).into();
    evaluation["runtime_fingerprint_hash"] = spec::hash(&fingerprint).into();
    Ok(
        json!({"schema_version":"agent-release-candidate/v1","tenant_id":agent["tenant_id"],"agent_id":agent["agent_id"],"draft_id":draft["draft_id"],"draft_revision":draft["revision"],"spec_hash":h,"channel":channel,"auth_mode":auth,"channel_policy":p,"channel_policy_hash":spec::hash(&p),"dataset_id":id,"dataset_version":version,"dataset_manifest_hash":manifest,"model_authorization":authorization,"runtime_fingerprint":fingerprint,"runtime_fingerprint_hash":spec::hash(&fingerprint),"release_identity_hash":spec::hash(&identity),"evaluation_identity_hash":spec::hash(&evaluation)}),
    )
}
pub fn evaluate(candidate: &Value, profile: &Value, duration_ms: f64) -> Result<Value, AgentError> {
    if profile["available"] != true {
        return Err(AgentError::code(503, "AGENT_RELEASE_PROFILE_UNAVAILABLE"));
    }
    let mut blocking = Vec::new();
    let mut add = |code: &str, field: String, message: &str| {
        blocking.push(json!({"code":code,"field":field,"message":message}))
    };
    let f = object(&candidate["runtime_fingerprint"]);
    for key in FINGERPRINT_KEYS {
        if text(&f, key).is_empty() {
            add(
                "AGENT_EVAL_FINGERPRINT_INCOMPLETE",
                format!("runtime_fingerprint.{key}"),
                "A required server runtime fingerprint is missing",
            );
        }
    }
    for (invalid, code, field, message) in [
        (
            candidate["runtime_fingerprint_hash"] != spec::hash(&f),
            "AGENT_EVAL_FINGERPRINT_HASH_MISMATCH",
            "runtime_fingerprint_hash",
            "Runtime fingerprint evidence is inconsistent",
        ),
        (
            candidate["channel_policy_hash"] != spec::hash(&object(&candidate["channel_policy"])),
            "AGENT_EVAL_CHANNEL_POLICY_MISMATCH",
            "channel_policy_hash",
            "Channel policy evidence is inconsistent",
        ),
        (
            f["model_authorization_hash"] != spec::hash(&object(&candidate["model_authorization"])),
            "AGENT_EVAL_MODEL_AUTHORIZATION_MISMATCH",
            "runtime_fingerprint.model_authorization_hash",
            "Model authorization evidence is inconsistent",
        ),
        (
            !matches!(text(candidate, "channel"), "hosted" | "embed" | "api"),
            "AGENT_EVAL_CHANNEL_INVALID",
            "channel",
            "Release channel is invalid",
        ),
    ] {
        if invalid {
            add(code, field.into(), message);
        }
    }
    if !candidate["dataset_id"].is_null() {
        let h = plain(text(candidate, "dataset_manifest_hash"));
        if text(candidate, "dataset_version").is_empty()
            || h.len() != 64
            || !h.bytes().all(|b| b.is_ascii_hexdigit())
            || f["eval_dataset_manifest_hash"] != h
        {
            add(
                "AGENT_EVAL_DATASET_FINGERPRINT_MISMATCH",
                "dataset_manifest_hash".into(),
                "Selected Eval Dataset evidence is inconsistent",
            );
        }
    }
    let mut non_blocking = vec![
        json!({"code":"AGENT_EVAL_PROVIDER_FREE_SCOPE","field":"profile","message":"This open-source gate verifies release integrity and resource readiness; it does not claim live-model quality."}),
    ];
    if candidate["dataset_id"].is_null() {
        non_blocking.push(json!({"code":"AGENT_EVAL_DATASET_NOT_SELECTED","field":"dataset_id","message":"No tenant Eval Dataset is attached to this provider-free run"}));
    }
    let status = if blocking.is_empty() {
        "passed"
    } else {
        "failed"
    };
    Ok(
        json!({"schema_version":"agent-release-gate/v1","status":status,"profile_id":profile["profile_id"],"profile_version":profile["profile_version"],"execution_scope":profile["execution_scope"],"model_quality_evaluated":false,"blocking_findings":blocking,"non_blocking_findings":non_blocking,"metrics":{"critical_pass_rate":if status=="passed"{1.0}else{0.0},"configured_critical_pass_rate":profile["critical_pass_rate"],"validation_duration_ms":(duration_ms.max(0.0)*1000.0).round()/1000.0,"provider_cost_cents":0.0,"evaluator_results":profile["blocking_evaluators"].as_array().into_iter().flatten().map(|v|json!({"evaluator":v,"blocking":true,"status":status})).collect::<Vec<_>>()}}),
    )
}
fn binding_summary(v: &Value, knowledge: bool) -> Value {
    let mut rows = Vec::new();
    for raw in v.as_array().into_iter().flatten() {
        let id = text(
            raw,
            if knowledge {
                "dataset_id"
            } else {
                "resource_id"
            },
        );
        if id.is_empty() {
            continue;
        }
        rows.push(json!({"type":if knowledge{"knowledge"}else{text(raw,"type")},"resource_id":id,"resource_version":if knowledge{Value::Null}else{raw["resource_version"].clone()},"schema_hash":if knowledge{Value::Null}else{raw["schema_hash"].clone()},"config_hash":hash(&object(&raw[if knowledge{"retrieval_config"}else{"config"}]))}));
    }
    rows.sort_by_key(|v| {
        (
            text(v, "type").to_owned(),
            text(v, "resource_id").to_owned(),
        )
    });
    json!(rows)
}
fn section(a: &Value, b: &Value, paths: Vec<String>) -> Value {
    json!({"changed":a!=b,"before_hash":hash(a),"after_hash":hash(b),"changed_paths":if a!=b{paths}else{vec![]}})
}
pub fn structured_diff(before: &Value, after: &Value) -> Value {
    let mut sections = json!({});
    for name in ["identity", "outcome", "model", "memory"] {
        let a = object(&before[name]);
        let b = object(&after[name]);
        let paths = if name == "identity" {
            let keys: BTreeSet<_> = a
                .as_object()
                .expect("object")
                .keys()
                .chain(b.as_object().expect("object").keys())
                .cloned()
                .collect();
            keys.into_iter()
                .filter(|k| a[k] != b[k])
                .map(|k| format!("identity.{k}"))
                .collect()
        } else {
            vec![name.into()]
        };
        sections[name] = section(&a, &b, paths);
        if name == "model" {
            sections[name]["before"] =
                json!({"model_id":a["model_id"],"provider_id":a["provider_id"]});
            sections[name]["after"] =
                json!({"model_id":b["model_id"],"provider_id":b["provider_id"]});
        }
    }
    let a = json!(text(before, "instructions"));
    let b = json!(text(after, "instructions"));
    sections["prompt"] = section(&a, &b, vec!["instructions".into()]);
    sections["prompt"]["before_length"] = text(before, "instructions").chars().count().into();
    sections["prompt"]["after_length"] = text(after, "instructions").chars().count().into();
    for name in ["capabilities", "knowledge"] {
        let a = binding_summary(&before[name], name == "knowledge");
        let b = binding_summary(&after[name], name == "knowledge");
        sections[name] = section(&a, &b, vec![name.into()]);
        sections[name]["before"] = a.clone();
        sections[name]["after"] = b.clone();
        if name == "capabilities" {
            let skills = |v: &Value| {
                json!(
                    v.as_array()
                        .into_iter()
                        .flatten()
                        .filter(|v| v["type"] == "skill")
                        .collect::<Vec<_>>()
                )
            };
            sections["skills"] =
                section(&skills(&a), &skills(&b), vec!["capabilities.skills".into()]);
        }
    }
    let changed: Vec<_> = [
        "identity",
        "outcome",
        "prompt",
        "model",
        "capabilities",
        "skills",
        "knowledge",
        "memory",
    ]
    .into_iter()
    .filter(|k| sections[*k]["changed"] == true)
    .collect();
    json!({"schema_version":"agent-release-diff/v1","changed_sections":changed,"sections":sections})
}
