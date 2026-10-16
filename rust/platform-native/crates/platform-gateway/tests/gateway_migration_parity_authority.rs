//! Original proxy header/client-IP/LangGraph mutation assertions against production helpers.
//! No provider, database or external service calls; synthetic credentials only.
use ai_platform_gateway::{
    client_ip::{TrustedProxies, get_client_ip},
    migration_compat::{
        common::CompatUser,
        langgraph_body as body,
        transparent_proxy::{ProxyServiceConfig, forward_headers, forward_headers_with_client_ip},
    },
};
use axum::http::{HeaderMap, HeaderName, HeaderValue};
use serde_json::json;
fn headers(values: &[(&str, &str)]) -> HeaderMap {
    values
        .iter()
        .map(|(k, v)| {
            (
                HeaderName::from_bytes(k.as_bytes()).unwrap(),
                HeaderValue::from_str(v).unwrap(),
            )
        })
        .collect()
}
fn user() -> CompatUser {
    CompatUser {
        user_id: "user-auth".into(),
        tenant_id: "tenant-auth".into(),
        tier: "premium".into(),
        authenticated: true,
        roles: vec!["user".into()],
        ..Default::default()
    }
}
#[test]
fn client_user_and_tenant_headers_are_overwritten_by_verified_context() {
    let h = forward_headers(
        &headers(&[
            ("X-User-Id", "admin"),
            ("X-GW-User-ID", "root"),
            ("X-Tenant-Id", "tenant-other"),
            ("X-GW-Tenant-ID", "tenant-root"),
        ]),
        &user(),
        &ProxyServiceConfig::default(),
    )
    .unwrap();
    for key in ["x-user-id", "x-gw-user-id"] {
        assert_eq!(h[key], "user-auth");
    }
    for key in ["x-tenant-id", "x-gw-tenant-id"] {
        assert_eq!(h[key], "tenant-auth");
    }
    for forbidden in ["admin", "root", "tenant-other", "tenant-root"] {
        assert!(!h.values().any(|v| v == forbidden));
    }
}
#[test]
fn client_authenticated_header_cannot_upgrade_anonymous_context() {
    let h = forward_headers(
        &headers(&[
            ("X-GW-Authenticated", "true"),
            ("X-User-Type", "user"),
            ("X-User-Permissions", "read,write,admin,delete"),
        ]),
        &CompatUser::default(),
        &ProxyServiceConfig::default(),
    )
    .unwrap();
    assert_eq!(h["x-gw-authenticated"], "false");
    assert_eq!(h["x-user-type"], "anonymous");
    assert_eq!(h["x-user-permissions"], "read");
}
#[test]
fn transparent_forwarding_strips_gateway_prefixed_identity_headers() {
    let h = forward_headers(
        &headers(&[
            ("X-GW-User-Type", "admin"),
            ("X-GW-User-Name", "Root"),
            ("X-GW-User-Permissions", "admin,delete"),
            ("X-GW-User-Tier", "admin"),
            ("X-Custom-Trace", "kept"),
        ]),
        &user(),
        &ProxyServiceConfig::default(),
    )
    .unwrap();
    assert_eq!(h["x-custom-trace"], "kept");
    for key in ["x-gw-user-type", "x-gw-user-name", "x-gw-user-permissions"] {
        assert!(!h.contains_key(key));
    }
    assert_eq!(h["x-gw-user-tier"], "premium");
}
#[test]
fn langgraph_and_gateway_identity_headers_are_both_authoritative() {
    let h = forward_headers(
        &headers(&[
            ("X-User-Id", "spoofed-user"),
            ("X-GW-User-ID", "spoofed-gateway-user"),
            ("X-Tenant-Id", "spoofed-tenant"),
            ("X-GW-Tenant-ID", "spoofed-gateway-tenant"),
            ("X-User-Tier", "admin"),
            ("X-GW-User-Tier", "admin"),
            ("X-GW-Authenticated", "false"),
        ]),
        &user(),
        &ProxyServiceConfig::default(),
    )
    .unwrap();
    for key in ["x-user-id", "x-gw-user-id"] {
        assert_eq!(h[key], "user-auth");
    }
    for key in ["x-tenant-id", "x-gw-tenant-id"] {
        assert_eq!(h[key], "tenant-auth");
    }
    for key in ["x-user-tier", "x-gw-user-tier"] {
        assert_eq!(h[key], "premium");
    }
    assert_eq!(h["x-gw-authenticated"], "true");
}
#[test]
fn service_auth_token_rejects_arbitrary_and_crlf_injection() {
    for token in ["X-Evil: injected", "Bearer safe\r\nX-Evil: injected"] {
        let c = ProxyServiceConfig {
            forward_auth: false,
            auth_token: Some(token.into()),
            ..Default::default()
        };
        let e = forward_headers(&HeaderMap::new(), &user(), &c).unwrap_err();
        assert!(e.detail.as_str().unwrap().contains("service_auth_token"));
    }
}
#[test]
fn service_auth_token_uses_its_source_header_kind() {
    for (token, key) in [
        ("Bearer safe", "authorization"),
        ("opaque-safe", "x-api-key"),
    ] {
        let c = ProxyServiceConfig {
            forward_auth: false,
            auth_token: Some(token.into()),
            ..Default::default()
        };
        let h = forward_headers(
            &headers(&[
                ("authorization", "Bearer browser"),
                ("x-api-key", "browser-key"),
            ]),
            &user(),
            &c,
        )
        .unwrap();
        assert_eq!(h[key], token);
        assert!(
            !h.values()
                .any(|v| v == "Bearer browser" || v == "browser-key")
        );
    }
}
#[test]
fn client_forwarded_ip_headers_are_rebuilt_from_context() {
    let h = forward_headers_with_client_ip(
        &headers(&[
            ("X-Forwarded-For", "198.51.100.1"),
            ("X-Real-IP", "198.51.100.2"),
            ("X-Custom-Trace", "kept"),
        ]),
        &user(),
        &ProxyServiceConfig::default(),
        "203.0.113.9",
    )
    .unwrap();
    assert_eq!(h["x-forwarded-for"], "203.0.113.9");
    assert!(!h.contains_key("x-real-ip"));
    assert_eq!(h["x-custom-trace"], "kept");
}
#[test]
fn source_client_ip_trust_matrix() {
    let trusted = TrustedProxies::from_cidrs(["127.0.0.0/8", "::1/128"]);
    for (values, peer, expected) in [
        (
            vec![("X-Forwarded-For", "10.0.0.1, 10.0.0.2")],
            Some("192.168.1.1"),
            "192.168.1.1",
        ),
        (
            vec![("X-Real-IP", "10.0.0.5")],
            Some("192.168.1.1"),
            "192.168.1.1",
        ),
        (
            vec![("X-Forwarded-For", "10.0.0.1, 10.0.0.2")],
            Some("127.0.0.1"),
            "10.0.0.2",
        ),
        (
            vec![("X-Real-IP", "10.0.0.5")],
            Some("127.0.0.1"),
            "10.0.0.5",
        ),
        (vec![], Some("192.168.1.1"), "192.168.1.1"),
        (vec![], None, "unknown"),
    ] {
        assert_eq!(
            get_client_ip(&headers(&values), peer, Some(&trusted)),
            expected
        );
    }
}
#[test]
fn should_prepare_skips_run_control_endpoints() {
    for path in [
        "runs",
        "runs/wait",
        "threads/t1/runs",
        "threads/t1/runs/stream",
    ] {
        assert!(body::is_run_path("POST", path));
    }
    for path in ["threads/t1/runs/run-9/cancel", "threads/t1/runs/run-9/join"] {
        assert!(!body::is_run_path("POST", path));
    }
    assert!(!body::is_run_path("GET", "threads/t1/runs"));
}
#[test]
fn prepare_applies_authoritative_sync_mutations_and_is_idempotent() {
    let u = CompatUser {
        user_id: "gateway-user".into(),
        tenant_id: "tenant-a".into(),
        request_id: "req-1".into(),
        trace_id: "trace-1".into(),
        ..Default::default()
    };
    let mut p = json!({"assistant_id":"attacker-assistant","input":{"messages":[{"role":"user","content":"hello"}]},"config":{"configurable":{"gateway_model":{"_api_key":"browser-secret"},"user_id":"attacker"}}});
    let service = json!({"assistant_id":"assistant-1","metadata":{"adapter_type":"langgraph","domain_policy":"agent"}});
    body::prepare_run_payload(&mut p, "threads/thread-9/runs/stream", &u, &service, None);
    assert_eq!(p["assistant_id"], "assistant-1");
    assert_eq!(p["stream_mode"], json!(["messages", "updates", "custom"]));
    assert_eq!(p["stream_subgraphs"], true);
    assert_eq!(p["metadata"]["gateway"]["domain_policy"], "agent");
    let c = &p["config"]["configurable"];
    assert_eq!(c["user_id"], "gateway-user");
    assert_eq!(c["thread_id"], "thread-9");
    assert_eq!(c["tenant_id"], "tenant-a");
    assert!(!c.as_object().unwrap().contains_key("gateway_model"));
    let first = p.clone();
    body::prepare_run_payload(&mut p, "threads/thread-9/runs/stream", &u, &service, None);
    assert_eq!(p, first);
}
#[test]
fn billing_request_snapshot_strips_secrets_without_mutating_original() {
    let p = json!({"config":{"configurable":{"gateway_model":{"_api_key":"secret","model_id":"qwen-max","provider_id":"dashscope"},"user_id":"gateway-user"}}});
    let s = body::billing_snapshot(&p);
    let hints = &s["config"]["configurable"]["gateway_model"];
    assert_eq!(hints["model_id"], "qwen-max");
    assert_eq!(hints["provider_id"], "dashscope");
    assert!(hints.get("_api_key").is_none());
    assert_eq!(
        p["config"]["configurable"]["gateway_model"]["_api_key"],
        "secret"
    );
}
#[test]
fn quota_model_downgrade_preserves_resolved_provider() {
    let mut p = json!({"config":{"configurable":{"model":"downgraded-model","gateway_model":{"provider_id":"dashscope","model_id":"qwen-max","_api_key":"secret"}}}});
    assert!(body::apply_model_downgrade(
        &mut p,
        "downgraded-model",
        "premium-model"
    ));
    assert_eq!(p["config"]["configurable"]["model"], "downgraded-model");
    assert_eq!(
        p["config"]["configurable"]["gateway_model"]["model_id"],
        "downgraded-model"
    );
    assert_eq!(
        p["config"]["configurable"]["gateway_model"]["provider_id"],
        "dashscope"
    );
}
