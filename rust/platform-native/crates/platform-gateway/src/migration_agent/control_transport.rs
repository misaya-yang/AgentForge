//! One-shot bounded Runtime command transport. No retries and no authority minting.
use super::AgentError;
use super::execution_transport::WorkloadSend;
use crate::migration_host::context;
use ai_platform_capability_contract::private_transport::Edge;
use reqwest::{RequestBuilder, StatusCode};
use serde_json::Value;
use std::time::{Duration, Instant};
const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
pub(super) struct JsonResponse {
    status: StatusCode,
    payload: Result<Value, ()>,
}
impl JsonResponse {
    pub(super) fn status(&self) -> StatusCode {
        self.status
    }
    pub(super) async fn json(self) -> Result<Value, ()> {
        self.payload
    }
}
/// The caller supplies only an existing local route cap. Model/SDK lease
/// validation stays in its owning authority; HTTP never supplies verified expiry.
pub(super) async fn send_json(
    request: RequestBuilder,
    cap: Duration,
    code: &'static str,
) -> Result<JsonResponse, AgentError> {
    let (client, request) = request.build_split();
    let mut request = request.map_err(|_| AgentError::code(503, code))?;
    let cap = request
        .timeout()
        .copied()
        .map(|existing| existing.min(cap))
        .unwrap_or(cap);
    let budget = context::command_budget(cap)
        .and_then(|budget| {
            budget
                .child(Edge::GatewayRuntimeCommand.policy(), Instant::now())
                .map_err(|_| "private_transport_budget_expired".into())
        })
        .map_err(|_| AgentError::code(503, code))?;
    let mut wire = vec![];
    budget
        .append(&mut wire, context::wall_ms(), Instant::now())
        .map_err(|_| AgentError::code(503, code))?;
    let timeout = budget
        .remaining(Instant::now())
        .map_err(|_| AgentError::code(503, code))?;
    for (name, value) in wire {
        request.headers_mut().insert(
            reqwest::header::HeaderName::from_bytes(name.as_bytes())
                .map_err(|_| AgentError::code(503, code))?,
            reqwest::header::HeaderValue::from_str(&value)
                .map_err(|_| AgentError::code(503, code))?,
        );
    }
    *request.timeout_mut() = Some(timeout);
    tokio::time::timeout(timeout, async move {
        // Underlying client also disables all transport retries and redirects.
        let mut response = reqwest::RequestBuilder::from_parts(client, request)
            .workload_send()
            .await
            .map_err(|_| AgentError::code(503, code))?;
        let status = response.status();
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| AgentError::code(503, code))?
        {
            if chunk.len() > MAX_RESPONSE_BYTES.saturating_sub(bytes.len()) {
                return Err(AgentError::code(503, code));
            }
            bytes.extend_from_slice(&chunk);
        }
        let payload = serde_json::from_slice(&bytes).map_err(|_| ());
        budget
            .remaining(Instant::now())
            .map_err(|_| AgentError::code(503, code))?;
        Ok(JsonResponse { status, payload })
    })
    .await
    .map_err(|_| AgentError::code(503, code))?
}
#[cfg(test)]
#[path = "control_transport_tests.rs"]
mod tests;
