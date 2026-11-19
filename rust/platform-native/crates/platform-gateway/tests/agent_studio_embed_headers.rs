//! Native counterpart of the five deployment framing source cases.
use std::path::PathBuf;
fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(4)
        .unwrap()
        .to_path_buf()
}
fn read(p: &str) -> String {
    std::fs::read_to_string(root().join(p)).unwrap()
}
fn block<'a>(s: &'a str, marker: &str) -> &'a str {
    let start = s.find(marker).unwrap();
    let opening = start + s[start..].find('{').unwrap();
    let mut depth = 0;
    for (i, c) in s[opening..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return &s[start..opening + i + 1];
                }
            }
            _ => (),
        }
    }
    panic!("unterminated block")
}
fn static_framing_headers_are_included(s: &str) {
    assert!(s.contains("include /tmp/frontend-security-headers.conf;"));
    let headers = read("web/docker-entrypoint.d/45-security-headers.sh");
    assert!(headers.contains("add_header X-Frame-Options \"SAMEORIGIN\" always;"));
    assert!(headers.contains("frame-ancestors 'self'"));
    assert!(headers.contains("add_header Content-Security-Policy \"$POLICY\" always;"));
}
#[test]
fn web_preserves_hosted_framing_and_dynamic_embed_headers() {
    let s = read("web/nginx.conf");
    let b = block(&s, "location /embed/agents/");
    static_framing_headers_are_included(&s);
    assert!(b.contains("proxy_pass http://$backend"));
    assert!(!b.contains("X-Frame-Options"));
    assert!(!b.contains("Content-Security-Policy"));
    assert!(b.contains("add_header X-Content-Type-Options \"nosniff\" always;"));
}
#[test]
fn helm_routes_embed_to_gateway_without_static_framing() {
    let s = read("deploy/helm/ai-gateway/files/nginx.conf");
    let config = read("deploy/helm/ai-gateway/templates/config.yaml");
    let frontend = read("deploy/helm/ai-gateway/templates/frontend.yaml");
    let routing = read("deploy/helm/ai-gateway/templates/routing.yaml");
    assert_eq!(
        s,
        read("web/nginx.conf"),
        "Helm must package the canonical proxy config"
    );
    assert!(config.contains(".Files.Get \"files/nginx.conf\""));
    assert!(config.contains("replace \"gateway:8080\" (printf \"%s-gateway:8080\""));
    assert!(
        frontend.contains("mountPath: /etc/nginx/nginx.conf, subPath: nginx.conf, readOnly: true")
    );
    let public_route = routing
        .split_once("kind: HTTPRoute")
        .unwrap()
        .1
        .split_once("{{- if .Values.routing.certificate.enabled }}")
        .unwrap()
        .0;
    assert!(public_route.contains("path: {type: PathPrefix, value: /}"));
    assert!(public_route.contains("}}-frontend, port: 8080}"));
    let b = block(&s, "location /embed/agents/");
    static_framing_headers_are_included(&s);
    assert!(b.contains("proxy_pass http://$backend"));
    assert!(!b.contains("X-Frame-Options"));
    assert!(!b.contains("Content-Security-Policy"));
    assert!(b.contains("add_header X-Content-Type-Options \"nosniff\" always;"));
}
#[tokio::test]
async fn middleware_exempts_only_dedicated_embed_document() {
    use axum::{Router, body::Body, http::Request, middleware, routing::get};
    use tower::ServiceExt;
    let app = Router::new()
        .route("/{*path}", get(|| async { "ok" }))
        .layer(middleware::from_fn(
            ai_platform_gateway::migration_host::middleware::security_headers,
        ));
    for (path, allow) in [
        ("/embed/agents/public", true),
        ("/api/v1/agents", false),
        ("/embed/agents", false),
        ("/embed/agents-evil/1", false),
    ] {
        let response = app
            .clone()
            .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(
            response
                .headers()
                .get("x-frame-options")
                .map(|x| x.to_str().unwrap()),
            if allow { None } else { Some("DENY") }
        );
        assert_eq!(response.headers()["x-content-type-options"], "nosniff");
    }
}
#[test]
fn header_script_preserves_ownership_and_built_image_mode() {
    let s = read("scripts/new/test-agent-embed-headers.sh");
    assert!(s.starts_with("#!/bin/bash\nset -euo pipefail"));
    for x in [
        "assert_compose_owner \"$PROJECT_ROOT\"",
        "\"--config-only\"",
        "\"--built-image\"",
        "docker build --load",
        "docker rm -f",
    ] {
        assert!(s.contains(x), "{x}");
    }
}
#[test]
fn embed_build_assets_are_credential_free() {
    for name in ["agent-embed.js", "agent-widget.js", "agent-embed.css"] {
        assert!(root().join("web/public").join(name).is_file());
    }
    let s = read("web/public/agent-embed.js") + &read("web/public/agent-widget.js");
    for key in [
        "AGENT_RUNTIME_TOKEN",
        "GATEWAY_ASSISTANT_SHARED_SECRET",
        "agt_",
    ] {
        assert!(!s.contains(key));
    }
}
