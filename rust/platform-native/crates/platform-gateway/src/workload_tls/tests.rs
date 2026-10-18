use super::*;
use axum::{body::Body, http::Request as HttpRequest, routing::post};
use tower::ServiceExt;
fn rules() -> PeerRules {
    PeerRules::new([
        ServiceIdentity::parse("spiffe://agentforge.local/ns/platform/sa/runtime").unwrap(),
    ])
    .unwrap()
}
fn admitted() -> Routes {
    Routes::new([("POST".into(), "/internal/v1/signed".into())]).unwrap()
}
#[tokio::test]
async fn required_blocks_all_cleartext_internal_copies_and_keeps_public_http() {
    let router = protect_cleartext(
        Router::new()
            .route("/internal/v1/signed", post(|| async { "business" }))
            .route("/internal/v1/future-route", post(|| async { "future" }))
            .route("/api/v1/public", post(|| async { "public" })),
        Arc::new(WorkloadAuth::new(Mode::Required, None, rules()).unwrap()),
    );
    for path in [
        "/internal/v1/signed",
        "/internal/v1/future-route",
        "/api/v1/public",
    ] {
        let response = router
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .method("POST")
                    .uri(path)
                    .header("authorization", "Bearer old-transport-secret")
                    .header("x-ai-platform-internal-token", "old-transport-secret")
                    .header(
                        "x-spiffe-id",
                        "spiffe://agentforge.local/ns/platform/sa/runtime",
                    )
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            if path.starts_with("/internal/") {
                StatusCode::UNAUTHORIZED
            } else {
                StatusCode::OK
            }
        );
    }
}

#[tokio::test]
async fn forged_http_identity_headers_and_public_routes_never_authenticate() {
    let router = restrict(
        Router::new()
            .route("/internal/v1/signed", post(|| async { "business" }))
            .route("/api/v1/public", post(|| async { "public" })),
        Arc::new(WorkloadAuth::new(Mode::Required, None, rules()).unwrap()),
        admitted(),
    );
    for header in [
        "x-spiffe-id",
        "x-workload-identity",
        "x-forwarded-client-cert",
    ] {
        let response = router
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .method("POST")
                    .uri("/internal/v1/signed")
                    .header(header, "spiffe://agentforge.local/ns/platform/sa/runtime")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }
    let response = router
        .clone()
        .oneshot(
            HttpRequest::builder()
                .method("POST")
                .uri("/internal/v1/signed")
                .header(
                    "authorization",
                    "Bearer synthetic-shared-token-cannot-authenticate-required",
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    for path in [
        "/api/v1/public",
        "/internal/v1/signed/extra",
        "/internal/v1/%73igned",
    ] {
        let response = router
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .method("POST")
                    .uri(path)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }
}
#[cfg(unix)]
#[tokio::test]
async fn dual_window_preserves_original_signature_tenant_and_authorization_headers() {
    use std::os::unix::fs::PermissionsExt;
    let root = std::env::temp_dir().join(format!("workload-facade-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let file = root.join("token");
    std::fs::write(
        &file,
        "synthetic-unit-shared-token-abcdefghijklmnopqrstuvwxyz",
    )
    .unwrap();
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
    let env = BTreeMap::from([
        ("AI_PLATFORM_WORKLOAD_MTLS_MODE".into(), "dual".into()),
        (
            "AI_PLATFORM_INTERNAL_TOKEN_FILE".into(),
            file.to_str().unwrap().into(),
        ),
    ]);
    let gate = Arc::new(WorkloadAuth::from_environment(&env, rules()).unwrap());
    let business = Router::new().route(
        "/internal/v1/signed",
        post(|request: Request| async move {
            if request.headers().get("x-tenant-signature").is_none() {
                StatusCode::FORBIDDEN
            } else if request.headers().get("authorization").is_none() {
                StatusCode::BAD_REQUEST
            } else {
                StatusCode::OK
            }
        }),
    );
    let router = restrict(business, gate, admitted());
    for (signature, status) in [(false, StatusCode::FORBIDDEN), (true, StatusCode::OK)] {
        let mut request = HttpRequest::builder()
            .method("POST")
            .uri("/internal/v1/signed")
            .header(
                "authorization",
                "Bearer synthetic-unit-shared-token-abcdefghijklmnopqrstuvwxyz",
            );
        if signature {
            request = request.header("x-tenant-signature", "synthetic-test-proof")
        }
        let response = router
            .clone()
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), status);
    }
    std::fs::remove_dir_all(root).unwrap();
}
#[test]
fn configuration_and_route_scope_fail_closed() {
    assert!(
        Configuration::from_environment(&BTreeMap::new())
            .unwrap()
            .is_none()
    );
    let mut env = BTreeMap::from([
        ("AI_PLATFORM_WORKLOAD_MTLS_MODE".into(), "required".into()),
        (
            "AI_PLATFORM_WORKLOAD_TLS_BIND".into(),
            "127.0.0.1:8443".into(),
        ),
        (
            "AI_PLATFORM_WORKLOAD_CREDENTIALS_DIR".into(),
            "/run/agentforge/workload".into(),
        ),
        (
            "AI_PLATFORM_WORKLOAD_SPIFFE_ID".into(),
            "spiffe://agentforge.local/ns/platform/sa/gateway".into(),
        ),
        (
            "AI_PLATFORM_WORKLOAD_ALLOWED_CLIENT_IDS".into(),
            "spiffe://agentforge.local/ns/platform/sa/runtime".into(),
        ),
    ]);
    assert!(Configuration::from_environment(&env).is_ok());
    for bind in [
        "0.0.0.0:8443",
        "[::]:8443",
        "192.0.2.1:8443",
        "public.example:8443",
    ] {
        env.insert("AI_PLATFORM_WORKLOAD_TLS_BIND".into(), bind.into());
        assert!(Configuration::from_environment(&env).is_err());
    }
    for path in [
        "/api/v1/public",
        "/internal/v1/../admin",
        "/internal/%2e%2e/admin",
        "/internal/v1/action?scope=all",
    ] {
        assert!(Routes::new([("POST".into(), path.into())]).is_err());
    }
}

#[tokio::test]
async fn dataset_pdp_facade_only_admits_the_two_exact_posts_and_retains_existing_inventory() {
    let routes = crate::migration_host::bootstrap::workload::routes().unwrap();
    for path in [
        "/internal/v2/agent-capabilities/knowledge-actor",
        "/internal/v2/knowledge/model-leases",
        "/internal/v1/sdk-tools/models/execute",
    ] {
        assert!(routes.0.contains(&("POST".into(), path.into())));
    }
    let root = ai_platform_service_primitives::object_authorization::DATASET_DECISION_PATH;
    let router = Router::new()
        .route(root, post(|| async { "signed-business" }))
        .route(
            &format!("{root}/batch"),
            post(|| async { "signed-business" }),
        );
    let gate=Arc::new(WorkloadAuth::new(Mode::Dual,Some(ai_platform_service_primitives::workload_identity::secrets::SecretBytes::from_configuration(vec![b'a';48])),rules()).unwrap());
    let router = restrict(router, gate, routes);
    for (method, path, expected) in [
        ("POST", root.to_string(), StatusCode::OK),
        ("POST", format!("{root}/batch"), StatusCode::OK),
        ("GET", root.to_string(), StatusCode::NOT_FOUND),
        ("POST", format!("{root}/batch/extra"), StatusCode::NOT_FOUND),
        (
            "POST",
            "/internal/v1/authorization/dataset-other".into(),
            StatusCode::NOT_FOUND,
        ),
        (
            "POST",
            "/internal/v1/authorization/%64ataset".into(),
            StatusCode::NOT_FOUND,
        ),
    ] {
        let response = router
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .method(method)
                    .uri(path)
                    .header("authorization", format!("Bearer {}", "a".repeat(48)))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), expected);
    }
}
