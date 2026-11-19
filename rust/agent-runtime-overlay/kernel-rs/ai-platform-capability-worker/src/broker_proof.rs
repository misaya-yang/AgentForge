//! Worker is a verify-only consumer. The signed original call is durable and
//! scoped to the operation future; a caller cannot replace it through headers.
use crate::workload_identity::WorkloadSend;
pub use ai_platform_capability_contract::broker_proof as protocol;
pub use protocol::required;
use serde_json::{Value, json};
static GATEWAY: std::sync::OnceLock<String> = std::sync::OnceLock::new();
pub const BINDING_KEY: &str = "private_transport_broker_call_v3";
tokio::task_local! { static ORIGINAL: Value; }
pub async fn scope<F: std::future::Future>(binding: &Value, fence: &str, future: F) -> F::Output {
    let mut original = binding[BINDING_KEY].clone();
    if let Some(map) = original.as_object_mut() {
        map.insert("dispatch_fence".into(), json!(fence));
    }
    ORIGINAL.scope(original, future).await
}
pub fn validate_configuration(gateway: &str) -> Result<(), &'static str> {
    if std::env::var_os("AI_PLATFORM_CAPABILITY_PROOF_SECRET").is_some() {
        return Err("worker_broker_signing_secret_forbidden");
    }
    protocol::keys()?;
    endpoint(gateway)?;
    if GATEWAY.get().is_some_and(|current| current != gateway) {
        return Err("broker_issuer_configuration_changed");
    }
    let _ = GATEWAY.set(gateway.to_owned());
    Ok(())
}
fn endpoint(gateway: &str) -> Result<reqwest::Url, &'static str> {
    let mut url = reqwest::Url::parse(gateway).map_err(|_| "broker_issuer_invalid")?;
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err("broker_issuer_requires_workload_tls");
    }
    url.set_path(protocol::ISSUER_PATH);
    Ok(url)
}
pub async fn issue(path: &str, body: &Value) -> Result<String, &'static str> {
    let original = ORIGINAL
        .try_with(Clone::clone)
        .map_err(|_| "broker_original_call_required")?;
    if !original.is_object() {
        return Err("broker_original_call_required");
    }
    let gateway = GATEWAY
        .get()
        .cloned()
        .or_else(|| std::env::var("AI_PLATFORM_GATEWAY_INTERNAL_URL").ok())
        .ok_or("broker_issuer_required")?;
    let url = endpoint(&gateway)?;
    let client = crate::workload_identity::internal_client(
        &[url.as_str()],
        std::time::Duration::from_secs(2),
        std::time::Duration::from_secs(5),
    )?;
    let hash = protocol::body_hash(body)?;
    let token = std::env::var("AI_PLATFORM_INTERNAL_TOKEN").unwrap_or_default();
    let mut request = client
        .post(url)
        .header("x-ai-platform-internal-token", token);
    for (header, field) in [
        ("x-ai-tenant-id", "tenant_id"),
        ("x-ai-user-id", "user_id"),
        ("x-ai-session-id", "session_id"),
        ("x-ai-run-id", "run_id"),
        ("x-ai-tool-call-id", "tool_call_id"),
        ("x-ai-arguments-hash", "arguments_hash"),
    ] {
        request = request.header(
            header,
            original["lease"][field]
                .as_str()
                .ok_or("broker_original_scope_required")?,
        );
    }
    request = request.header(
        "x-ai-execution-id",
        original["execution_id"]
            .as_str()
            .ok_or("broker_original_scope_required")?,
    );
    let mut response=request
        .json(&json!({"original":original,"path":path,"body_sha256":hash.trim_start_matches("sha256:"),"body":body}))
        .workload_send().await.map_err(|_|"broker_issuer_unavailable")?;
    if !response.status().is_success() {
        return Err("broker_issuer_rejected");
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| "broker_issuer_unavailable")?
    {
        if bytes.len() + chunk.len() > 32768 {
            return Err("broker_proof_too_large");
        };
        bytes.extend_from_slice(&chunk);
    }
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| "broker_issuer_invalid")?;
    let proof = value["proof"].as_str().ok_or("broker_issuer_invalid")?;
    let mut lease = original["lease"].clone();
    lease["execution_id"] = original["execution_id"].clone();
    protocol::verify_with_keys(
        &protocol::keys()?,
        proof,
        "POST",
        path,
        hash.trim_start_matches("sha256:"),
        &lease,
        crate::now_epoch_ms(),
    )?;
    Ok(proof.into())
}
