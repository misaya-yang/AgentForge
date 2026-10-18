//! Gateway-only broker proof issuer. A proof requires the original sealed call
//! and a still-live dispatch fence; the Worker never receives a signing key.
use super::{ContentCapabilityBroker, ContentError, broker_proof_v3 as protocol};
use axum::{Json, extract::State, http::HeaderMap};
use protocol::wire;
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PrivateKeys {
    active_key_id: String,
    signing_seed_hex: String,
    verification: wire::PublicKeyring,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Request {
    original: Value,
    path: String,
    body_sha256: String,
    body: Value,
}
fn rejected() -> ContentError {
    ContentError::new(403, "broker original authority not active")
}
fn keys() -> Result<PrivateKeys, ContentError> {
    let path =
        protocol::setting("AI_PLATFORM_GATEWAY_ED25519_KEYRING_FILE").map_err(|_| rejected())?;
    let bytes = std::fs::read(path).map_err(|_| rejected())?;
    if bytes.len() > 16384 {
        return Err(rejected());
    }
    let c: PrivateKeys = serde_json::from_slice(&bytes).map_err(|_| rejected())?;
    c.verification.validate().map_err(|_| rejected())?;
    Ok(c)
}
fn route(capability: &str, path: &str) -> bool {
    match path {
        "/internal/v2/agent-capabilities/office/artifacts" => matches!(
            capability,
            "mcp_docgen__generate_document" | "mcp_docgen__modify_document"
        ),
        "/internal/v2/agent-capabilities/python/artifacts" => capability == "execute_python_code",
        "/internal/v2/agent-capabilities/image/generate" => capability == "generate_image",
        "/internal/v2/agent-capabilities/attachments/read" => capability == "read_attachment",
        "/internal/v2/agent-capabilities/confluence/write" => capability == "confluence_write",
        "/internal/v2/agent-capabilities/confluence/read" => capability == "confluence_read",
        "/internal/v2/agent-capabilities/web-search" => capability == "search_web",
        "/internal/v2/agent-capabilities/mcp/read" => {
            !capability.is_empty() && capability.len() <= 160
        }
        "/internal/v2/agent-capabilities/local-node/describe" => matches!(
            capability,
            "local_node_catalog" | "local_node_describe" | "local_node_action"
        ),
        "/internal/v2/agent-capabilities/local-node/dispatch"
        | "/internal/v2/agent-capabilities/local-node/status"
        | "/internal/v2/agent-capabilities/local-node/cancel" => {
            matches!(capability, "local_node_action")
        }
        _ => {
            (capability == "search_knowledge_base"
                && exact_segment(path, "/internal/v2/capabilities/knowledge/", "/retrieve"))
                || (capability == "read_tool_artifact"
                    && exact_segment(path, "/internal/v2/agent-capabilities/artifacts/", "/read"))
        }
    }
}
fn exact_segment(path: &str, prefix: &str, suffix: &str) -> bool {
    path.strip_prefix(prefix)
        .and_then(|v| v.strip_suffix(suffix))
        .is_some_and(|v| {
            !v.is_empty()
                && v.len() <= 255
                && v.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
        })
}
// Consumers do not possess a signing key: Gateway validates the original
// sealed argument hash and the allowed request transformation before signing.
fn authorized_body(row: &Value, call: &Value, path: &str, body: &Value) -> bool {
    let args = &row["arguments"];
    if ai_platform_capability_contract::canonical_json_hash(args)
        .ok()
        .as_deref()
        != call["arguments_hash"].as_str()
    {
        return false;
    }
    let binding = &row["resource_binding"]["connector_binding"];
    let value = |key: &str, default: Value| args.get(key).cloned().unwrap_or(default);
    let expected = match path {
        "/internal/v2/agent-capabilities/web-search" => {
            json!({"queries":args["queries"],"max_results":value("max_results",json!(5))})
        }
        "/internal/v2/agent-capabilities/confluence/read" => {
            json!({"arguments":args,"binding":binding})
        }
        "/internal/v2/agent-capabilities/confluence/write" => {
            json!({"arguments":args,"arguments_hash":call["arguments_hash"],"binding":binding})
        }
        "/internal/v2/agent-capabilities/image/generate" => {
            json!({"arguments":args,"arguments_hash":call["arguments_hash"]})
        }
        "/internal/v2/agent-capabilities/attachments/read" => {
            let mut expected = args.clone();
            if expected.get("max_chars").is_none() {
                expected["max_chars"] = json!(100000)
            }
            expected
        }
        "/internal/v2/agent-capabilities/mcp/read" => {
            json!({"connection_id":binding["connection_id"],"principal_type":binding["principal_type"],"channel":binding["channel"],"runtime_name":call["capability_id"],"schema_hash":binding["schema_hash"],"risk_level":binding.get("risk_level").filter(|v|!v.is_null()).cloned().unwrap_or(json!("medium")),"arguments":args,"arguments_hash":call["arguments_hash"]})
        }
        "/internal/v2/agent-capabilities/python/artifacts"
        | "/internal/v2/agent-capabilities/office/artifacts" => {
            // Generated output bytes cannot be known at turn admission. The
            // existing artifact broker rechecks the active write row, format,
            // exact size/content hash, idempotency and tenant storage prefix.
            return body["tool_call_id"] == call["tool_call_id"]
                && body["arguments_hash"] == call["arguments_hash"];
        }
        p if p.starts_with("/internal/v2/capabilities/knowledge/") => {
            let Some(id) = p
                .strip_prefix("/internal/v2/capabilities/knowledge/")
                .and_then(|s| s.strip_suffix("/retrieve"))
            else {
                return false;
            };
            if !row["resource_binding"]["bound_dataset_ids"]
                .as_array()
                .is_some_and(|ids| ids.iter().any(|v| v.as_str() == Some(id)))
            {
                return false;
            }
            if args.get("dataset_ids").is_some_and(|ids| {
                !ids.as_array()
                    .is_some_and(|ids| ids.iter().any(|v| v.as_str() == Some(id)))
            }) {
                return false;
            }
            // Worker intentionally normalizes accepted JSON integer thresholds
            // through f64 before sending its retrieval request.
            let threshold = match args.get("score_threshold") {
                None => 0.0,
                Some(v) => match v
                    .as_f64()
                    .filter(|v| v.is_finite() && (0.0..=1.0).contains(v))
                {
                    Some(v) => v,
                    None => return false,
                },
            };
            json!({"query":args["query"],"top_k":value("top_k",json!(5)),"threshold":threshold})
        }
        p if p.starts_with("/internal/v2/agent-capabilities/artifacts/") => {
            if args["artifact_id"].as_str().is_none_or(|id| {
                p != format!("/internal/v2/agent-capabilities/artifacts/{id}/read")
            }) {
                return false;
            }
            json!({"offset":value("offset",json!(0)),"limit":value("limit",json!(4000))})
        }
        p if p.starts_with("/internal/v2/agent-capabilities/local-node/") => {
            let mut context = json!({"execution_id":row["execution_id"]});
            for key in [
                "tenant_id",
                "user_id",
                "session_id",
                "run_id",
                "tool_call_id",
                "lease_id",
                "attempt_id",
                "capability_id",
                "capability_revision",
                "effect",
            ] {
                context[key] = call[key].clone();
            }
            let device = row["resource_binding"]
                .get("device_id")
                .filter(|v| v.is_string())
                .or_else(|| binding.get("device_id"))
                .or_else(|| binding.get("connection_id"))
                .cloned()
                .unwrap_or(Value::Null);
            let mut e = json!({"schema_version":"ai-platform.local-node.worker.v1","context":context,"device_id":device});
            if p.ends_with("/dispatch") {
                e["operation"] = args["operation"].clone();
                e["arguments"] = args["arguments"].clone();
                e["arguments_hash"] = call["arguments_hash"].clone();
                e["idempotency_key"] = row["idempotency_key"].clone();
                if !row["approval_id"].is_null() {
                    e["approval_id"] = row["approval_id"].clone();
                }
            } else if p.ends_with("/status") {
                if body["after_sequence"].as_u64().is_none() {
                    return false;
                };
                e["after_sequence"] = body["after_sequence"].clone();
            } else if p.ends_with("/cancel") {
                e["action_id"] = row["execution_id"].clone();
            }
            e
        }
        _ => return false,
    };
    expected == *body
}
pub(super) async fn issue(
    State(b): State<Arc<ContentCapabilityBroker>>,
    headers: HeaderMap,
    Json(r): Json<Request>,
) -> Result<Json<Value>, ContentError> {
    b.authorization.peer(&headers)?;
    let now = chrono::Utc::now().timestamp_millis().max(0) as u64;
    if r.body_sha256.len() != 64
        || !r
            .body_sha256
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        return Err(rejected());
    }
    if protocol::body_hash(&r.body).map_err(|_| rejected())? != r.body_sha256 {
        return Err(rejected());
    }
    let c = keys()?;
    let mut call = r.original["lease"].clone();
    let signature = call["signature"].as_str().ok_or_else(rejected)?.to_owned();
    call["signature"] = json!("");
    let token: wire::CapabilityToken =
        wire::decode(wire::CAPABILITY_PREFIX, &signature).map_err(|_| rejected())?;
    token
        .verify_call(
            &c.verification,
            &call,
            r.original["schema_hash"].as_str().ok_or_else(rejected)?,
            now,
        )
        .map_err(|_| rejected())?;
    if !route(
        call["capability_id"].as_str().ok_or_else(rejected)?,
        &r.path,
    ) {
        return Err(rejected());
    }
    for (header, field) in [
        ("x-ai-tenant-id", "tenant_id"),
        ("x-ai-user-id", "user_id"),
        ("x-ai-session-id", "session_id"),
        ("x-ai-run-id", "run_id"),
        ("x-ai-tool-call-id", "tool_call_id"),
        ("x-ai-arguments-hash", "arguments_hash"),
    ] {
        if headers.get(header).and_then(|v| v.to_str().ok()) != call[field].as_str() {
            return Err(rejected());
        }
    }

    let execution = r.original["execution_id"]
        .as_str()
        .ok_or_else(rejected)?
        .to_owned();
    let fence = r.original["dispatch_fence"]
        .as_str()
        .ok_or_else(rejected)?
        .to_owned();
    for id in [&execution, &fence] {
        uuid::Uuid::parse_str(id).map_err(|_| rejected())?;
    }
    let mut expected = call.clone();
    expected["__snapshot_id"] = token.root.claims["snapshot_id"].clone();
    let original = r.original.clone();
    let row=b.database.run::<Option<Value>,_>(move|db|Box::pin(async move{
        sqlx::query_scalar::<_,Value>("SELECT to_jsonb(e) FROM assistant_capability_executions e JOIN assistant_runs r ON r.run_id=e.run_id AND r.tenant_id=e.tenant_id AND r.user_id=e.user_id AND r.session_id=e.session_id WHERE e.execution_id=$1::text::uuid AND e.dispatch_fence=$2::text::uuid AND e.lease_id=$3::text::uuid AND e.tenant_id=$4 AND e.user_id=$5 AND e.session_id=$6 AND e.run_id=$7::text::uuid AND e.status IN ('dispatched','running') AND e.worker_lease_until>clock_timestamp() AND r.status='running' AND e.resource_binding->'private_transport_broker_call_v3'=$8 AND NOT EXISTS (SELECT 1 FROM assistant_runtime_snapshot_revocations rev WHERE rev.snapshot_id=$9::text::uuid AND rev.tenant_id=e.tenant_id AND rev.user_id=e.user_id AND rev.session_id=e.session_id) AND NOT EXISTS (SELECT 1 FROM assistant.assistant_sdk_runs sdk WHERE sdk.run_id=e.run_id AND (sdk.revoked_at IS NOT NULL OR sdk.expires_at<=clock_timestamp() OR sdk.lease_until<=clock_timestamp()))")
            .bind(execution).bind(fence).bind(expected["lease_id"].as_str().unwrap_or("")).bind(expected["tenant_id"].as_str().unwrap_or("")).bind(expected["user_id"].as_str().unwrap_or("")).bind(expected["session_id"].as_str().unwrap_or("")).bind(expected["run_id"].as_str().unwrap_or(""))
            .bind({let mut v=original;v.as_object_mut().map(|m|m.remove("dispatch_fence"));v}).bind(expected["__snapshot_id"].as_str().unwrap_or("")).fetch_optional(db).await
    })).await?.ok_or_else(rejected)?;
    if row["capability_id"] != call["capability_id"]
        || row["tool_call_id"] != call["tool_call_id"]
        || row["arguments_sha256"].as_str().unwrap_or("").trim()
            != call["arguments_hash"]
                .as_str()
                .unwrap_or("")
                .trim_start_matches("sha256:")
        || (call["effect"] != "read"
            && (row["approval_status"] != "consumed" || row["approval_id"] != call["approval_id"]))
    {
        return Err(rejected());
    }
    if r.path == "/internal/v2/agent-capabilities/mcp/read"
        && (row["resource_binding"]["connector_binding"]["provider"] != "mcp"
            || row["resource_binding"]["connector_binding"]["tool_name"] != call["capability_id"])
    {
        return Err(rejected());
    }
    if !authorized_body(&row, &call, &r.path, &r.body) {
        return Err(rejected());
    }
    let expires = now
        .saturating_add(30_000)
        .min(call["expires_at_epoch_ms"].as_u64().ok_or_else(rejected)?);
    if expires <= now + 1000 {
        return Err(rejected());
    }
    let mut claims = json!({"schema_version":protocol::SCHEMA,"method":"POST","path":r.path,"body_sha256":r.body_sha256,"issued_at_ms":now,"expires_at_ms":expires,"execution_id":row["execution_id"],"nonce":uuid::Uuid::now_v7().to_string()});
    for field in [
        "tenant_id",
        "user_id",
        "session_id",
        "run_id",
        "tool_call_id",
        "arguments_hash",
        "capability_id",
    ] {
        claims[field] = call[field].clone();
    }
    let envelope = wire::SignedEnvelope::issue(
        &wire::unhex::<32>(&c.signing_seed_hex).map_err(|_| rejected())?,
        &c.active_key_id,
        protocol::PURPOSE,
        claims,
        now,
        expires,
        &c.verification,
    )
    .map_err(|_| rejected())?;
    Ok(Json(
        json!({"proof":wire::encode(wire::ENVELOPE_PREFIX,&envelope).map_err(|_|rejected())?}),
    ))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn retrieval_normalizes_integer_threshold_and_preserves_requested_dataset_subset() {
        let args = json!({"query":"q","dataset_ids":["chosen"],"score_threshold":0});
        let row =
            json!({"arguments":args,"resource_binding":{"bound_dataset_ids":["chosen","other"]}});
        let call = json!({"arguments_hash":ai_platform_capability_contract::canonical_json_hash(&args).unwrap()});
        let body = json!({"query":"q","top_k":5,"threshold":0.0});
        assert!(authorized_body(
            &row,
            &call,
            "/internal/v2/capabilities/knowledge/chosen/retrieve",
            &body
        ));
        assert!(!authorized_body(
            &row,
            &call,
            "/internal/v2/capabilities/knowledge/other/retrieve",
            &body
        ));
    }
    #[test]
    fn signed_call_cannot_be_reused_for_different_query_dataset_or_body() {
        let args = json!({"query":"original","dataset_ids":["bound"]});
        let row = json!({"arguments":args,"resource_binding":{"bound_dataset_ids":["bound"]}});
        let call = json!({"arguments_hash":ai_platform_capability_contract::canonical_json_hash(&args).unwrap()});
        let body = json!({"query":"original","top_k":5,"threshold":0.0});
        assert!(authorized_body(
            &row,
            &call,
            "/internal/v2/capabilities/knowledge/bound/retrieve",
            &body
        ));
        assert!(!authorized_body(
            &row,
            &call,
            "/internal/v2/capabilities/knowledge/foreign/retrieve",
            &body
        ));
        let mut changed = body;
        changed["query"] = json!("widened");
        assert!(!authorized_body(
            &row,
            &call,
            "/internal/v2/capabilities/knowledge/bound/retrieve",
            &changed
        ));
        let mut changed = row;
        changed["arguments"]["query"] = json!("changed stored arguments");
        assert!(!authorized_body(
            &changed,
            &call,
            "/internal/v2/capabilities/knowledge/bound/retrieve",
            &json!({"query":"changed stored arguments","top_k":5,"threshold":0.0})
        ));
    }
    #[test]
    fn routes_never_expand_capability_or_allow_path_ambiguity() {
        assert!(route(
            "search_knowledge_base",
            "/internal/v2/capabilities/knowledge/docs/retrieve"
        ));
        assert!(!route(
            "search_web",
            "/internal/v2/capabilities/knowledge/docs/retrieve"
        ));
        for p in [
            "/internal/v2/capabilities/knowledge/../retrieve",
            "/internal/v2/capabilities/knowledge/a/b/retrieve",
            "/internal/v2/capabilities/knowledge/a%2fb/retrieve",
            "/internal/v2/agent-capabilities/office/artifacts?x=1",
        ] {
            assert!(!route("search_knowledge_base", p));
        }
        assert!(!route(
            "read_attachment",
            "/internal/v2/agent-capabilities/python/artifacts"
        ));
    }
}
