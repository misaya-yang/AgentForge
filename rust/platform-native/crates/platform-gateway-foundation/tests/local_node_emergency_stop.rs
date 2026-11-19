//! Offline HTTP contract tests; no DB or device execution is claimed.
use ai_platform_gateway_foundation::{
    local_nodes_api::{LocalNodesApi, LocalNodesIdentityResolver, local_nodes_router},
    local_nodes_types::{
        LocalNodesControlPlane, LocalNodesError, LocalNodesFuture, LocalNodesIdentity,
    },
};
use axum::{
    body::{Body, to_bytes},
    http::{HeaderMap, Request, StatusCode},
};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use tower::ServiceExt;
struct Owner;
impl LocalNodesIdentityResolver for Owner {
    fn resolve<'a>(
        &'a self,
        headers: &'a HeaderMap,
        _: &'a str,
    ) -> LocalNodesFuture<'a, LocalNodesIdentity> {
        Box::pin(async move {
            if headers.get("authorization").and_then(|h| h.to_str().ok())
                != Some("Bearer fixture-owner")
            {
                return Err(LocalNodesError::detail(401, "authentication required"));
            }
            Ok(LocalNodesIdentity {
                tenant_id: "tenant-a".into(),
                user_id: "owner-a".into(),
            })
        })
    }
}
#[derive(Default)]
struct Plane(Mutex<Vec<(String, LocalNodesIdentity, Value)>>);
impl LocalNodesControlPlane for Plane {
    fn call<'a>(
        &'a self,
        operation: &'a str,
        owner: &'a LocalNodesIdentity,
        args: Value,
    ) -> LocalNodesFuture<'a, Value> {
        Box::pin(async move {
            self.0
                .lock()
                .unwrap()
                .push((operation.into(), owner.clone(), args.clone()));
            if args["device_id"] != "device-owned" {
                return Err(LocalNodesError::repository("device is not owned"));
            }
            Ok(json!({"status":"pending"}))
        })
    }
}
fn request(path: &str, body: Value, authorized: bool) -> Request<Body> {
    let mut request = Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/json")
        .header("x-ai-tenant-id", "foreign-injected");
    if authorized {
        request = request.header("authorization", "Bearer fixture-owner");
    }
    request.body(Body::from(body.to_string())).unwrap()
}
#[tokio::test]
async fn confirmed_stop_aliases_forward_only_trusted_owner_and_device_scope() {
    let plane = Arc::new(Plane::default());
    let router = local_nodes_router(
        LocalNodesApi {
            control_plane: Some(plane.clone()),
            channel_verifier: None,
        },
        Arc::new(Owner),
    );
    for prefix in ["/api/v1/local-nodes", "/api/v1/assistant/local-nodes"] {
        let body = json!({"confirmation":"stop-all-local-actions","idempotency_key":uuid::Uuid::new_v4().to_string()});
        let response = router
            .clone()
            .oneshot(request(
                &format!("{prefix}/device-owned/emergency-stop"),
                body.clone(),
                true,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let call = plane.0.lock().unwrap().last().unwrap().clone();
        assert_eq!(call.0, "device.emergency_stop");
        assert_eq!(call.1.tenant_id, "tenant-a");
        assert_eq!(call.1.user_id, "owner-a");
        assert_eq!(call.2["device_id"], "device-owned");
        assert_eq!(call.2["idempotency_key"], body["idempotency_key"]);
    }
    let path = "/api/v1/local-nodes/device-owned/emergency-stop";
    for body in [
        json!({}),
        json!({"confirmation":true,"idempotency_key":uuid::Uuid::new_v4().to_string()}),
        json!({"confirmation":"stop-all-local-actions","idempotency_key":uuid::Uuid::new_v4().to_string(),"tenant_id":"foreign"}),
    ] {
        assert_eq!(
            router
                .clone()
                .oneshot(request(path, body, true))
                .await
                .unwrap()
                .status(),
            StatusCode::UNPROCESSABLE_ENTITY
        );
    }
    assert_eq!(plane.0.lock().unwrap().len(), 2);
    let body = json!({"confirmation":"stop-all-local-actions","idempotency_key":uuid::Uuid::new_v4().to_string()});
    assert_eq!(
        router
            .clone()
            .oneshot(request(path, body.clone(), false))
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(plane.0.lock().unwrap().len(), 2);
    let response = router
        .oneshot(request(
            "/api/v1/local-nodes/foreign-device/emergency-stop",
            body,
            true,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let bytes = to_bytes(response.into_body(), 4096).await.unwrap();
    assert!(!String::from_utf8_lossy(&bytes).contains("foreign-injected"));
}

#[tokio::test]
async fn stop_audit_status_uses_owner_resolver_and_path_command_id() {
    let plane = Arc::new(Plane::default());
    let router = local_nodes_router(
        LocalNodesApi {
            control_plane: Some(plane.clone()),
            channel_verifier: None,
        },
        Arc::new(Owner),
    );
    let id = uuid::Uuid::new_v4().to_string();
    let path = format!("/api/v1/local-nodes/device-owned/controls/{id}");
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .uri(path.as_str())
                .header("authorization", "Bearer fixture-owner")
                .header("x-ai-user-id", "foreign-owner")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let call = plane.0.lock().unwrap().last().unwrap().clone();
    assert_eq!(call.0, "controls.status");
    assert_eq!(call.1.user_id, "owner-a");
    assert_eq!(call.2["device_id"], "device-owned");
    assert_eq!(call.2["command_id"], id);
    assert_eq!(
        router
            .oneshot(
                Request::builder()
                    .uri(path.as_str())
                    .body(Body::empty())
                    .unwrap()
            )
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(plane.0.lock().unwrap().len(), 1);
}
