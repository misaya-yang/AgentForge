//! Platform-owned local TLS bridge for the unmodified upstream model client.
//! Kernel requests keep their existing turn metadata and signed Gateway lease.
use super::{RuntimeHttpState, security::RuntimeError};
use crate::workload_identity::WorkloadSend;
use axum::{
    body::{Body, to_bytes},
    extract::{Request, State},
    response::{IntoResponse, Response},
};
use std::time::Duration;

pub(super) fn kernel_base_url(base: &str) -> Result<String, RuntimeError> {
    if !crate::workload_identity::tls_enabled() {
        return Ok(base.to_owned());
    }
    let target = std::env::var("AI_PLATFORM_AGENT_RUNTIME_MODEL_PLANE_RUNTIME_BASE_URL")
        .map_err(|_| RuntimeError::unavailable("runtime_model_plane_target_missing"))?;
    if base.trim_end_matches('/') != target.trim_end_matches('/') {
        return Err(RuntimeError::bad_request(
            "runtime_model_plane_target_mismatch",
        ));
    }
    let local = std::env::var("AI_PLATFORM_RUNTIME_MODEL_PROXY_BASE_URL")
        .map_err(|_| RuntimeError::unavailable("runtime_model_proxy_missing"))?;
    validate_local(&local)?;
    Ok(local)
}
fn validate_local(local: &str) -> Result<(), RuntimeError> {
    let url = reqwest::Url::parse(local)
        .map_err(|_| RuntimeError::bad_request("runtime_model_proxy_invalid"))?;
    if url.scheme() != "http"
        || !matches!(url.host_str(), Some("127.0.0.1" | "[::1]" | "::1"))
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != "/internal/v1/model-proxy"
    {
        return Err(RuntimeError::bad_request("runtime_model_proxy_invalid"));
    }
    Ok(())
}
pub(super) async fn responses(
    State(state): State<RuntimeHttpState>,
    request: Request,
) -> Result<Response, RuntimeError> {
    if !crate::workload_identity::tls_enabled() {
        return Err(RuntimeError::not_found("runtime_model_proxy_disabled"));
    }
    let expected = std::env::var("AI_PLATFORM_RUNTIME_MODEL_PROXY_TOKEN")
        .map_err(|_| RuntimeError::unavailable("runtime_model_plane_token_missing"))?;
    // Reuse the constant-time service authenticator without accepting proxy hints.
    let bearer = request
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("");
    if expected.len() < 32
        || !super::security::constant_time_eq(bearer.as_bytes(), expected.as_bytes())
    {
        return Err(RuntimeError::bad_request(
            "runtime_model_proxy_auth_invalid",
        ));
    }
    let base = std::env::var("AI_PLATFORM_AGENT_RUNTIME_MODEL_PLANE_RUNTIME_BASE_URL")
        .map_err(|_| RuntimeError::unavailable("runtime_model_plane_target_missing"))?;
    let identity = crate::workload_identity::WorkloadIdentity::from_environment()
        .map_err(RuntimeError::unavailable)?
        .ok_or_else(|| RuntimeError::unavailable("runtime_model_plane_identity_missing"))?;
    identity
        .validate_endpoint(&base)
        .map_err(RuntimeError::unavailable)?;
    let (parts, body) = request.into_parts();
    let bytes = to_bytes(body, 16 * 1024 * 1024)
        .await
        .map_err(|_| RuntimeError::bad_request("runtime_model_request_too_large"))?;
    let mut outbound = state
        .capability_client
        .post(format!("{}/responses", base.trim_end_matches('/')))
        .header("content-type", "application/json")
        .body(bytes)
        .timeout(Duration::from_secs(300));
    for (name, value) in &parts.headers {
        if (name.as_str().ends_with("-turn-metadata")
            || matches!(name.as_str(), "traceparent" | "tracestate" | "x-request-id"))
            && value.len() <= 16_384
        {
            outbound = outbound.header(name, value);
        }
    }
    let mut response = outbound
        .workload_send()
        .await
        .map_err(|_| RuntimeError::unavailable("runtime_model_plane_acknowledgement_unknown"))?;
    if response.status().is_redirection() {
        return Err(RuntimeError::unavailable(
            "runtime_model_plane_redirect_forbidden",
        ));
    }
    let status = response.status();
    let content_type = response.headers().get("content-type").cloned();
    let stream = async_stream::stream! {
        let mut total=0usize;
        loop {
            match response.chunk().await {
                Ok(Some(bytes))=>{
                    total=total.saturating_add(bytes.len());
                    if total>16*1024*1024 {yield Err(std::io::Error::other("runtime_model_response_limit"));break;}
                    yield Ok::<_,std::io::Error>(bytes);
                },
                Ok(None)=>break,
                Err(_)=>{yield Err(std::io::Error::other("runtime_model_plane_stream_unknown"));break;},
            }
        }
    };
    let mut response = (status, Body::from_stream(stream)).into_response();
    if let Some(value) = content_type {
        response.headers_mut().insert("content-type", value);
    }
    Ok(response)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn kernel_bridge_can_only_use_its_explicit_loopback_route() {
        assert!(validate_local("http://127.0.0.1:8094/internal/v1/model-proxy").is_ok());
        for url in [
            "http://runtime:8094/internal/v1/model-proxy",
            "http://127.0.0.1:8094/other",
            "http://user@127.0.0.1:8094/internal/v1/model-proxy",
            "http://127.0.0.1:8094/internal/v1/model-proxy?token=x",
        ] {
            assert!(validate_local(url).is_err());
        }
    }
}
