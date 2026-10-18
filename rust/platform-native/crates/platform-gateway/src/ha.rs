//! Explicit routing of live SDK provenance to its owning process incarnation.
//! Shared hints never grant identity; the owner rechecks the full grant, current
//! JWT/session/service and all existing domain fences. Owner death is terminal
//! for the ephemeral grant and never replays a provider/tool operation elsewhere.
use ai_platform_gateway_foundation::multi_instance::affinity::Directory;
use ai_platform_service_primitives::workload_identity::http::WorkloadHttp;
use axum::{
    Router,
    body::{Body, to_bytes},
    extract::{Request, State},
    http::{HeaderMap, HeaderName, HeaderValue, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
};
use std::{collections::BTreeSet, sync::Arc};
#[derive(Clone)]
pub struct ProvenanceRouting {
    directory: Directory,
    http: WorkloadHttp,
    paths: Arc<BTreeSet<String>>,
    body_limit: usize,
}
impl ProvenanceRouting {
    pub fn new(
        directory: Directory,
        http: WorkloadHttp,
        paths: impl IntoIterator<Item = String>,
        body_limit: usize,
    ) -> Result<Self, &'static str> {
        let paths = paths.into_iter().collect::<BTreeSet<_>>();
        if paths.is_empty()
            || paths.iter().any(|p| {
                !p.starts_with("/internal/v1/sdk-tools/")
                    || p.contains(['%', '?', '#', '\\'])
                    || p.split('/').any(|part| part == "." || part == "..")
            })
            || body_limit == 0
            || body_limit > 16 * 1024 * 1024
        {
            return Err("ha_provenance_routing_invalid");
        }
        Ok(Self {
            directory,
            http,
            paths: Arc::new(paths),
            body_limit,
        })
    }
    pub fn protect(self, router: Router) -> Router {
        router.layer(middleware::from_fn_with_state(self, route))
    }
}
fn fail(status: StatusCode, code: &'static str) -> Response {
    (
        status,
        axum::Json(serde_json::json!({"detail":code,"retry_unsafe_operation":false})),
    )
        .into_response()
}
fn clean(headers: &HeaderMap) -> HeaderMap {
    let mut result = headers.clone();
    let connection_names = headers
        .get_all("connection")
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .map(|value| value.trim().to_owned())
        .collect::<Vec<_>>();
    for name in connection_names {
        result.remove(name);
    }

    for name in [
        "host",
        "connection",
        "transfer-encoding",
        "upgrade",
        "te",
        "trailer",
        "keep-alive",
        "proxy-authorization",
        "proxy-authenticate",
        "cookie",
        "x-forwarded-client-cert",
        "x-spiffe-id",
        "x-workload-identity",
        "x-client-cert",
        "x-ssl-client-cert",
    ] {
        result.remove(name);
    }
    result
}
async fn route(State(state): State<ProvenanceRouting>, request: Request, next: Next) -> Response {
    if !state.paths.contains(request.uri().path()) {
        return next.run(request).await;
    }
    if request.method() != axum::http::Method::POST {
        return fail(StatusCode::METHOD_NOT_ALLOWED, "ha_callback_method");
    }
    if request.headers().get_all("authorization").iter().count() != 1 {
        return fail(StatusCode::FORBIDDEN, "sdk_grant_invalid");
    }
    let Some(token) = request
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
    else {
        return fail(StatusCode::FORBIDDEN, "sdk_grant_invalid");
    };
    let owner = match state.directory.resolve(token).await {
        Ok(owner) => owner,
        Err(_) => return fail(StatusCode::SERVICE_UNAVAILABLE, "sdk_provenance_owner_lost"),
    };
    if owner.prefix == state.directory.local_prefix() {
        return next.run(request).await;
    }
    if request.headers().contains_key("x-agentforge-affinity-hop") {
        return fail(StatusCode::SERVICE_UNAVAILABLE, "sdk_provenance_route_loop");
    }
    let endpoint = format!(
        "{}{}",
        owner.origin,
        request
            .uri()
            .path_and_query()
            .map(|p| p.as_str())
            .unwrap_or(request.uri().path())
    );
    if state.http.validate_endpoint(&endpoint).is_err() {
        return fail(
            StatusCode::SERVICE_UNAVAILABLE,
            "sdk_provenance_peer_not_admitted",
        );
    }
    let (parts, body) = request.into_parts();
    let bytes = match to_bytes(body, state.body_limit).await {
        Ok(bytes) => bytes,
        Err(_) => return fail(StatusCode::PAYLOAD_TOO_LARGE, "sdk_provenance_body_limit"),
    };
    let mut headers = clean(&parts.headers);
    headers.insert(
        HeaderName::from_static("x-agentforge-affinity-hop"),
        HeaderValue::from_static("1"),
    );
    let response = match state
        .http
        .send(parts.method, &endpoint, headers, Some(bytes.to_vec()))
        .await
    {
        Ok(response) => response,
        Err(_) => {
            return fail(
                StatusCode::SERVICE_UNAVAILABLE,
                "sdk_provenance_forward_outcome_unknown",
            );
        }
    };
    let status = response.status();
    let headers = clean(response.headers());
    let mut outgoing = Response::new(Body::from_stream(response.bytes_stream()));
    *outgoing.status_mut() = status;
    *outgoing.headers_mut() = headers;
    outgoing
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn proxy_never_forwards_ambient_identity_or_transport_headers() {
        let mut headers = HeaderMap::new();
        for key in [
            "authorization",
            "x-tenant-signature",
            "x-forwarded-client-cert",
            "cookie",
            "host",
            "connection",
        ] {
            headers.insert(key, HeaderValue::from_static("synthetic-test"));
        }
        let output = clean(&headers);
        assert!(output.contains_key("authorization"));
        assert!(output.contains_key("x-tenant-signature"));
        assert!(!output.contains_key("x-forwarded-client-cert"));
        assert!(!output.contains_key("cookie"));
        assert!(!output.contains_key("host"));
    }
}
