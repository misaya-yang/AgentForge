//! Authenticated owner affinity for commands. Forward at most once; never retry
//! a request whose acknowledgement may have been lost. SSE stays instance-neutral.
use super::{
    RuntimeHttpState,
    security::{
        RuntimeError, SESSION_HEADER, TENANT_HEADER, USER_HEADER, authorize, required_header,
    },
    thread_lifecycle::authorize_thread_scope,
};
use crate::postgres_store::thread_owner::ThreadOwner;
use crate::workload_identity::WorkloadSend;
use axum::{
    Json,
    body::{Body, to_bytes},
    extract::{Path, Request, State},
    http::{HeaderMap, Method, Uri},
    middleware::Next,
    response::Response,
};
use codex_protocol::ThreadId;
use uuid::Uuid;
const FORWARDED: &str = "x-ai-runtime-owner-forwarded";
const BODY_LIMIT: usize = 16 * 1024 * 1024;

fn command_thread(path: &str) -> Option<&str> {
    let tail = path.strip_prefix("/internal/v1/threads/")?;
    let (thread, operation) = tail.split_once('/')?;
    if matches!(operation, "turns" | "resume" | "archive" | "unarchive")
        || operation
            .strip_prefix("turns/")
            .is_some_and(|tail| tail.ends_with("/recover"))
    {
        Some(thread)
    } else {
        None
    }
}
async fn target_thread(
    state: &RuntimeHttpState,
    method: &Method,
    uri: &Uri,
    headers: &HeaderMap,
) -> Result<Option<ThreadId>, RuntimeError> {
    if method != Method::POST {
        return Ok(None);
    }
    if let Some(thread) = command_thread(uri.path()) {
        return authorize_thread_scope(state, headers, thread)
            .await
            .map(Some);
    }
    let Some(approval) = uri
        .path()
        .strip_prefix("/internal/v1/approvals/")
        .and_then(|s| s.strip_suffix("/decision"))
    else {
        return Ok(None);
    };
    authorize(headers, &state.internal_token)?;
    let approval =
        Uuid::parse_str(approval).map_err(|_| RuntimeError::bad_request("invalid_approval_id"))?;
    let thread: Option<Uuid> = sqlx::query_scalar("SELECT r.harness_thread_id FROM assistant_tool_approvals a JOIN assistant_runs r ON r.run_id=a.run_id WHERE a.approval_id=$1 AND a.tenant_id=$2 AND a.user_id=$3 AND a.session_id=$4 AND r.engine='agent_runtime'")
        .bind(approval).bind(required_header(headers, TENANT_HEADER)?)
        .bind(required_header(headers, USER_HEADER)?).bind(required_header(headers, SESSION_HEADER)?)
        .fetch_optional(&state.store.pool).await.map_err(|_| RuntimeError::unavailable("runtime_owner_store_unavailable"))?;
    let thread = thread.ok_or_else(|| RuntimeError::not_found("approval_not_found"))?;
    authorize_thread_scope(state, headers, &thread.to_string())
        .await
        .map(Some)
}

pub(super) async fn route(
    State(state): State<RuntimeHttpState>,
    request: Request,
    next: Next,
) -> Result<Response, RuntimeError> {
    let Some(_routing) = state.store.owner_routing() else {
        return Ok(next.run(request).await);
    };
    // Borrow only Sync request metadata across authorization/DB awaits.
    // axum::Body is Send but not Sync; retaining &Request made this middleware
    // future non-Send even though the body itself was never examined here.
    let Some(thread) =
        target_thread(&state, request.method(), request.uri(), request.headers()).await?
    else {
        return Ok(next.run(request).await);
    };
    let owner = state
        .store
        .route_thread_owner(thread)
        .await
        .map_err(RuntimeError::from_store)?
        .ok_or_else(|| RuntimeError::unavailable("runtime_owner_unavailable"))?;
    if owner.owner_id == state.store.instance_id {
        let mut response = next.run(request).await;
        response.headers_mut().insert(
            "x-ai-runtime-owner",
            owner
                .owner_id
                .to_string()
                .parse()
                .map_err(|_| RuntimeError::internal("runtime_owner_invalid"))?,
        );
        response.headers_mut().insert(
            "x-ai-runtime-owner-fence",
            owner
                .fence
                .to_string()
                .parse()
                .map_err(|_| RuntimeError::internal("runtime_owner_invalid"))?,
        );
        return Ok(response);
    }
    if request.headers().contains_key(FORWARDED) {
        return Err(RuntimeError::conflict("runtime_owner_changed"));
    }
    let (parts, body) = request.into_parts();
    let body = to_bytes(body, BODY_LIMIT)
        .await
        .map_err(|_| RuntimeError::bad_request("runtime_command_body_too_large"))?;
    let budget = super::transport_budget::admit_authenticated(
        &parts.headers,
        std::time::Duration::from_secs(30),
        None,
    )
    .map_err(|_| RuntimeError::bad_request("runtime_transport_budget_invalid"))?;
    let child = budget
        .child(
            ai_platform_capability_contract::private_transport::Edge::GatewayRuntimeCommand
                .policy(),
            std::time::Instant::now(),
        )
        .map_err(|_| RuntimeError::bad_request("runtime_transport_budget_invalid"))?;
    let mut wire = vec![];
    child
        .append(
            &mut wire,
            super::transport_budget::wall_ms(),
            std::time::Instant::now(),
        )
        .map_err(|_| RuntimeError::bad_request("runtime_transport_budget_invalid"))?;
    let uri = parts
        .uri
        .path_and_query()
        .map(|s| s.as_str())
        .unwrap_or(parts.uri.path());
    let mut outbound = state
        .capability_client
        .request(parts.method.clone(), format!("{}{uri}", owner.address))
        .body(body.clone())
        .header(FORWARDED, state.store.instance_id.to_string());
    // Do not relay arbitrary host/proxy/cookie/auth headers to another workload.
    for name in [
        "content-type",
        "x-ai-platform-internal-token",
        TENANT_HEADER,
        USER_HEADER,
        SESSION_HEADER,
        "traceparent",
        "tracestate",
        "x-request-id",
    ] {
        if let Some(value) = parts.headers.get(name) {
            outbound = outbound.header(name, value);
        }
    }
    for (name, value) in wire {
        outbound = outbound.header(name, value);
    }
    let timeout = child
        .remaining(std::time::Instant::now())
        .map_err(|_| RuntimeError::unavailable("runtime_owner_timeout"))?;
    let delivery = tokio::time::timeout(timeout, async {
        let mut received = outbound
            .workload_send()
            .await
            .map_err(|_| RuntimeError::unavailable("runtime_owner_acknowledgement_unknown"))?;
        // Redirects are never followed; an unexpected 3xx cannot redirect authority.
        if received.status().is_redirection() {
            return Err(RuntimeError::unavailable("runtime_owner_redirect_rejected"));
        }
        let status = received.status();
        let mut response = Response::builder().status(status);
        for name in [
            "content-type",
            "x-ai-runtime-owner",
            "x-ai-runtime-owner-fence",
        ] {
            if let Some(value) = received.headers().get(name) {
                response = response.header(name, value);
            }
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = received
            .chunk()
            .await
            .map_err(|_| RuntimeError::unavailable("runtime_owner_acknowledgement_unknown"))?
        {
            if bytes.len().saturating_add(chunk.len()) > BODY_LIMIT {
                return Err(RuntimeError::unavailable(
                    "runtime_owner_response_too_large",
                ));
            }
            bytes.extend_from_slice(&chunk);
        }
        response
            .body(Body::from(bytes))
            .map_err(|_| RuntimeError::internal("runtime_owner_response_invalid"))
    })
    .await
    .map_err(|_| RuntimeError::unavailable("runtime_owner_acknowledgement_unknown"))
    .and_then(|result| result);
    if delivery.is_err()
        && parts.uri.path().starts_with("/internal/v1/approvals/")
        && parts.uri.path().ends_with("/decision")
    {
        // Dynamic decisions/receipt-only reads are idempotent durable actions.
        // A lost owner's acknowledgement cannot hide an existing DB receipt.
        // Native destinations still fail their normal destination-kind check.
        return Ok(next.run(Request::from_parts(parts, Body::from(body))).await);
    }
    delivery
}

pub(super) async fn owner(
    State(state): State<RuntimeHttpState>,
    Path(thread): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Option<ThreadOwner>>, RuntimeError> {
    let thread = authorize_thread_scope(&state, &headers, &thread).await?;
    // Gateway can resolve an owner without creating/renewing execution authority.
    if state.store.owner_routing().is_none() {
        return Ok(Json(None));
    }
    let row: Option<(Uuid,String,i64)> = sqlx::query_as("SELECT owner_id,address,fence FROM assistant_runtime_thread_owners WHERE runtime_thread_id=$1 AND lease_until>clock_timestamp()")
        .bind(Uuid::parse_str(&thread.to_string()).map_err(|_|RuntimeError::bad_request("invalid_thread_id"))?)
        .fetch_optional(&state.store.pool).await.map_err(|_|RuntimeError::unavailable("runtime_owner_store_unavailable"))?;
    let config = state
        .store
        .owner_routing()
        .ok_or_else(|| RuntimeError::unavailable("runtime_owner_unavailable"))?;
    match row {
        Some((owner_id, address, fence)) if config.admits(&address) && fence > 0 => {
            Ok(Json(Some(ThreadOwner {
                owner_id,
                address,
                fence,
            })))
        }
        Some(_) => Err(RuntimeError::unavailable(
            "runtime_owner_address_not_admitted",
        )),
        None => Ok(Json(None)),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_owner_affine_commands_are_forwarded() {
        for path in [
            "/internal/v1/threads/a/turns",
            "/internal/v1/threads/a/resume",
        ] {
            assert_eq!(command_thread(path), Some("a"));
        }
        for path in [
            "/internal/v1/threads/a/turns/b/interrupt",
            "/internal/v1/threads/a/events",
            "/health/ready",
            "/internal/v1/threads/a/owner",
            "/internal/v1/sdk-runs/a/capabilities",
        ] {
            assert!(command_thread(path).is_none());
        }
    }
}
