//! Source channel-policy and browser-widget security contracts.
use ai_platform_gateway::migration_agent::{
    public::{normalized_origin, reject_forgery},
    release_contract::normalized_policy,
};
use axum::http::HeaderMap;
use serde_json::json;
#[test]
fn policy_rejects_nonexact_and_credentialed_origins() {
    for origin in [
        "https://*.example.com",
        "https://user:password@example.com",
        "https://example.com/path",
        "https://example.com?next=evil",
        "null",
        "javascript:alert(1)",
    ] {
        assert!(
            normalized_policy(&json!({"allowed_origins":[origin]})).is_err(),
            "{origin}"
        );
    }
}
#[test]
fn widget_validates_origin_source_version_without_storage_tokens() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(4)
        .unwrap();
    let iframe = std::fs::read_to_string(root.join("web/public/agent-embed.js")).unwrap();
    let loader = std::fs::read_to_string(root.join("web/public/agent-widget.js")).unwrap();
    let wildcard = regex::Regex::new(r#"postMessage\([^\n]+,\s*["']\*["']"#).unwrap();
    for source in [&iframe, &loader] {
        for required in [
            "event.origin",
            "event.source",
            "agent-embed/v1",
            "postMessage",
        ] {
            assert!(source.contains(required));
        }
        for forbidden in ["localStorage", "sessionStorage", "agt_"] {
            assert!(!source.contains(forbidden));
        }
        assert!(!wildcard.is_match(source));
    }
    assert!(loader.contains("iframe.sandbox = \"allow-scripts allow-forms allow-same-origin\""));
    assert!(loader.contains("launcher?.focus()"));
}
#[test]
fn origin_normalization_handles_referer_without_accepting_credentials() {
    assert_eq!(
        normalized_origin("https://allowed.example/page"),
        Some("https://allowed.example".into())
    );
    assert!(normalized_origin("https://user:password@example.com").is_none());
}
#[test]
fn forged_runtime_dimensions_fail_before_dispatch() {
    for key in ["agent_id", "agent_version_id", "publication_id"] {
        let body = json!({key:"forged"});
        assert!(
            reject_forgery(&HeaderMap::new(), Some(&body)).is_err(),
            "{key}"
        );
    }
}
mod agent_studio_support;
use agent_studio_support::{Result, graph};
use axum::{
    body::{Body, to_bytes},
    http::Request,
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use hmac::{Hmac, Mac};
use sha2::Sha256;
#[tokio::test]
#[ignore = "explicit isolated PostgreSQL and Redis browser contract"]
async fn embed_token_is_signed_short_lived_and_bound_to_publication_origin_and_cookie() -> Result<()>
{
    agent_studio_support::state::fixture(|_, g| async move {
        let public = g.state.public.as_ref().unwrap();
        let id = uuid::Uuid::new_v4().to_string();
        let mut headers = HeaderMap::new();
        headers.insert("x-agent-embed-origin", "https://allowed.example".parse()?);
        headers.insert("cookie", "ag_embed_session=nonce-1".parse()?);
        let before = chrono::Utc::now().timestamp();
        let token =
            public.issue_embed_token(&id, "https://allowed.example", "nonce-1", "192.0.2.1")?;
        let claim = public.verify_embed_token(&headers, &token, &id)?;
        assert_eq!(claim["origin"], "https://allowed.example");
        let exp = claim["exp"].as_i64().unwrap();
        assert!(exp >= before + 300 && exp <= chrono::Utc::now().timestamp() + 300);
        let renewed =
            public.issue_embed_token(&id, "https://allowed.example", "nonce-1", "192.0.2.1")?;
        assert_eq!(
            public.verify_embed_token(&headers, &renewed, &id)?["sub"],
            claim["sub"]
        );
        assert!(
            public
                .verify_embed_token(&headers, &token, &uuid::Uuid::new_v4().to_string())
                .is_err()
        );
        let parts = token.split('.').collect::<Vec<_>>();
        let tampered = format!("{}.{}x.{}", parts[0], parts[1], parts[2]);
        assert!(public.verify_embed_token(&headers, &tampered, &id).is_err());
        let mut expired = claim.clone();
        expired["exp"] = json!(chrono::Utc::now().timestamp() - 1000);
        let body = URL_SAFE_NO_PAD.encode(ai_platform_gateway::migration_agent::spec::canonical(
            &expired,
        ));
        let mut mac = Hmac::<Sha256>::new_from_slice(public.embed_secret.as_bytes())?;
        mac.update(body.as_bytes());
        let expired_token = format!(
            "e1.{body}.{}",
            URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes())
        );
        agent_studio_support::error(
            public.verify_embed_token(&headers, &expired_token, &id),
            401,
            "AGENT_EMBED_TOKEN_INVALID",
        );
        headers.insert("x-agent-embed-origin", "https://evil.example".parse()?);
        assert!(public.verify_embed_token(&headers, &token, &id).is_err());
        headers.insert("x-agent-embed-origin", "https://allowed.example".parse()?);
        headers.insert("cookie", "ag_embed_session=other-browser".parse()?);
        assert!(public.verify_embed_token(&headers, &token, &id).is_err());
        Ok(())
    })
    .await
}
#[tokio::test]
#[ignore = "explicit isolated PostgreSQL and Redis browser contract"]
async fn embed_document_enforces_exact_parent_origin_and_contains_no_internal_snapshot()
-> Result<()> {
    agent_studio_support::state::fixture(|p,g|async move {
 let seed=graph(&p,"embed-browser").await?;
 sqlx::query("UPDATE agent_publications SET status='active',channel='embed',auth_mode='public',policy=$1 WHERE publication_id=$2::uuid").bind(json!({"allowed_origins":["https://allowed.example"]})).bind(&seed.publication).execute(&p).await?;
 let public_id:String=sqlx::query_scalar("SELECT public_id::text FROM agent_publications WHERE publication_id=$1::uuid").bind(&seed.publication).fetch_one(&p).await?;
 let public=g.state.public.as_ref().unwrap();let path=format!("/embed/agents/{public_id}");
 for origin in [None,Some("https://evil.example")] {
 let mut request=Request::builder().uri(&path);if let Some(origin)=origin {request=request.header("origin",origin);}
 let response=public.dispatch(&g.state,Default::default(),request.body(Body::empty())?).await;assert_eq!(response.status(),403);
 }
 let response=public.dispatch(&g.state,Default::default(),Request::builder().uri(&path).header("referer","https://allowed.example/page").body(Body::empty())?).await;
 assert_eq!(response.status(),200);assert_eq!(response.headers()["cache-control"],"no-store");assert_eq!(response.headers()["referrer-policy"],"no-referrer");let csp=response.headers()["content-security-policy"].to_str()?;assert!(csp.contains("frame-ancestors https://allowed.example"));let body=String::from_utf8(to_bytes(response.into_body(),1024*1024).await?.to_vec())?;
 for forbidden in ["agt_","resolved_spec","instructions","GATEWAY_ASSISTANT_SHARED_SECRET"] {assert!(!body.contains(forbidden),"{forbidden}");}
 sqlx::query("UPDATE agent_publications SET policy=$1 WHERE publication_id=$2::uuid").bind(json!({"allowed_origins":["https://*.example.com"]})).bind(&seed.publication).execute(&p).await?;
 let response=public.dispatch(&g.state,Default::default(),Request::builder().uri(&path).header("origin","https://app.example.com").body(Body::empty())?).await;assert_eq!(response.status(),403);Ok(())}).await
}
