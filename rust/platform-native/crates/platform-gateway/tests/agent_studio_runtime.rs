//! Public runtime receipt/forgery contracts from the original runtime API suite.
use ai_platform_gateway::migration_agent::{public::reject_forgery, routes::public_capabilities};
use axum::http::{HeaderMap, HeaderValue};
use serde_json::json;
#[test]
fn effective_native_capabilities_defaults_high_risk_confirmation() {
    let snapshot = json!({"capabilities":[{"type":"platform","id":"dangerous-native","schema_hash":format!("sha256:{}","a".repeat(64)),"risk":"high","config":{}},{"type":"platform","id":"safe-native","schema_hash":format!("sha256:{}","b".repeat(64)),"risk":"low","config":{}}]});
    assert_eq!(
        public_capabilities(&snapshot),
        vec![
            json!({"name":"dangerous-native","schema_hash":format!("sha256:{}","a".repeat(64)),"risk":"high","requires_confirmation":true})
        ]
    );
}
#[test]
fn public_projection_preserves_explicit_false_confirmation_and_rejects_unsealed_definitions() {
    let hash = format!("sha256:{}", "a".repeat(64));
    let snapshot = json!({"capabilities":[{"type":"platform","id":"pinned","schema_hash":hash,"risk":"high","config":{"requires_confirmation":false}},{"type":"platform","id":"unsealed","risk":"high","config":{}},{"type":"mcp","id":"foreign","schema_hash":hash,"risk":"high","config":{}}]});
    assert_eq!(
        public_capabilities(&snapshot),
        vec![
            json!({"name":"pinned","schema_hash":hash,"risk":"high","requires_confirmation":false})
        ]
    );
}
#[test]
fn client_runtime_dimensions_are_rejected_and_browser_proof_headers_are_allowed() {
    for field in [
        "agent_id",
        "agent_version_id",
        "draft_revision",
        "publication_id",
        "channel",
        "resolved_snapshot",
        "runtime_envelope",
        "snapshot_hash",
        "spec_hash",
        "runtime_fingerprint",
    ] {
        let e = reject_forgery(&HeaderMap::new(), Some(&json!({field:"forged"}))).unwrap_err();
        assert_eq!(e.status, 422);
        assert_eq!(e.detail["code"], "AGENT_RUNTIME_FIELD_FORBIDDEN");
        assert_eq!(e.detail["fields"], json!([field]));
    }
    for header in [
        "x-agent-runtime-envelope",
        "x-agent-tenant-id",
        "x-agent-snapshot-hash",
    ] {
        let mut h = HeaderMap::new();
        h.insert(header, HeaderValue::from_static("forged"));
        assert_eq!(reject_forgery(&h, None).unwrap_err().status, 400);
    }
    let mut h = HeaderMap::new();
    h.insert("x-agent-embed-token", HeaderValue::from_static("opaque"));
    h.insert(
        "x-agent-embed-origin",
        HeaderValue::from_static("https://allowed.example"),
    );
    assert!(reject_forgery(&h, None).is_ok());
}
mod agent_studio_support;
use agent_studio_support::{Result, error, graph};
use ai_platform_gateway::migration_agent::Actor;
use sha2::{Digest, Sha256};
#[tokio::test]
#[ignore = "explicit isolated PostgreSQL and Redis runtime contract"]
async fn runtime_api_token_is_scoped_publication_bound_revocable_and_expiring() -> Result<()> {
    agent_studio_support::state::fixture(|p,g|async move {
 sqlx::query("CREATE TABLE agent_version_revocations(tenant_id text NOT NULL,agent_version_id uuid NOT NULL,PRIMARY KEY(tenant_id,agent_version_id))").execute(&p).await?;
 let seed=graph(&p,"runtime-token").await?;let other=graph(&p,"foreign-token").await?;
 sqlx::raw_sql(&agent_studio_support::sql("079_agent_channel_runtime.sql")).execute(&p).await?;
 sqlx::query("UPDATE agent_publications SET status='active' WHERE publication_id=$1::uuid").bind(&seed.publication).execute(&p).await?;
 let raw=format!("agt_{}",uuid::Uuid::new_v4().simple());let hash=ai_platform_gateway::migration_agent::spec::hex(&Sha256::digest(raw.as_bytes()));
 let tid:String=sqlx::query_scalar("INSERT INTO agent_api_tokens(tenant_id,publication_id,token_hash,name,created_by,scopes) VALUES($1,$2::uuid,$3,'scoped',$4,ARRAY['chat:write']) RETURNING token_id::text").bind(&seed.tenant).bind(&seed.publication).bind(&hash).bind(&seed.owner).fetch_one(&p).await?;
 let public=g.state.public.as_ref().unwrap();let repo=&g.state.repository;
 error(public.resolve_api(repo,"bad-token",&seed.publication,"chat:write",None).await,401,"AGENT_RUNTIME_TOKEN_INVALID");
 error(public.resolve_api(repo,&raw,&other.publication,"chat:write",None).await,401,"AGENT_RUNTIME_TOKEN_INVALID");
 error(public.resolve_api(repo,&raw,&seed.publication,"feedback:write",None).await,403,"AGENT_RUNTIME_TOKEN_SCOPE_FORBIDDEN");
 let(resolution,principal)=public.resolve_api(repo,&raw,&seed.publication,"chat:write",None).await?;
 assert_eq!(principal.tenant_id,seed.tenant);assert_eq!(principal.user_id,format!("agent-token:{tid}"));assert_eq!(resolution["version"]["agent_version_id"],seed.version);assert_eq!(resolution["api_token"]["token_id"],tid);
 let used:bool=sqlx::query_scalar("SELECT last_used_at IS NOT NULL FROM agent_api_tokens WHERE token_id=$1::uuid").bind(&tid).fetch_one(&p).await?;assert!(used);
 assert!(public.resolve_api(repo,&raw,&seed.publication,"chat:write",Some(&other.version)).await.is_err());
 sqlx::query("UPDATE agent_api_tokens SET expires_at=NOW()-INTERVAL '1 second' WHERE token_id=$1::uuid").bind(&tid).execute(&p).await?;
 error(public.resolve_api(repo,&raw,&seed.publication,"chat:write",None).await,401,"AGENT_RUNTIME_TOKEN_INVALID");
 sqlx::query("UPDATE agent_api_tokens SET expires_at=NULL,revoked_at=NOW() WHERE token_id=$1::uuid").bind(&tid).execute(&p).await?;
 error(public.resolve_api(repo,&raw,&seed.publication,"chat:write",None).await,401,"AGENT_RUNTIME_TOKEN_INVALID");
 Ok(())}).await
}
#[tokio::test]
#[ignore = "explicit isolated PostgreSQL and Redis browser runtime contract"]
async fn hosted_private_requires_authentication_and_tenant_mode_isolated() -> Result<()> {
    agent_studio_support::state::fixture(|p,g|async move {
 sqlx::query("CREATE TABLE agent_version_revocations(tenant_id text NOT NULL,agent_version_id uuid NOT NULL,PRIMARY KEY(tenant_id,agent_version_id))").execute(&p).await?;
 let seed=graph(&p,"runtime-hosted").await?;
 let public_id:String=sqlx::query_scalar("UPDATE agent_publications SET status='active',channel='hosted',auth_mode='private' WHERE publication_id=$1::uuid RETURNING public_id::text").bind(&seed.publication).fetch_one(&p).await?;
 let public=g.state.public.as_ref().unwrap();let repo=&g.state.repository;let guest=Actor{user_id:"guest".into(),tenant_id:"public".into(),tier:"anonymous".into(),roles:vec!["guest".into()]};let owner=agent_studio_support::actor(&seed.owner,&seed.tenant);let foreign=agent_studio_support::actor("foreign-user","other-tenant");
 error(public.resolve_browser(repo,guest.clone(),&HeaderMap::new(),&public_id,"hosted",None,None).await,401,"PUBLICATION_AUTHENTICATION_REQUIRED");
 assert!(public.resolve_browser(repo,owner.clone(),&HeaderMap::new(),&public_id,"hosted",None,None).await.is_ok());
 error(public.resolve_browser(repo,foreign.clone(),&HeaderMap::new(),&public_id,"hosted",None,None).await,403,"PUBLICATION_ACCESS_DENIED");
 sqlx::query("UPDATE agent_publications SET auth_mode='tenant' WHERE publication_id=$1::uuid").bind(&seed.publication).execute(&p).await?;
 error(public.resolve_browser(repo,foreign,&HeaderMap::new(),&public_id,"hosted",None,None).await,403,"PUBLICATION_ACCESS_DENIED");
 let tenant_member=agent_studio_support::actor("member-without-agent-acl",&seed.tenant);assert!(public.resolve_browser(repo,tenant_member,&HeaderMap::new(),&public_id,"hosted",None,None).await.is_ok());
 sqlx::query("UPDATE agent_publications SET status='disabled' WHERE publication_id=$1::uuid").bind(&seed.publication).execute(&p).await?;
 error(public.resolve_browser(repo,owner,&HeaderMap::new(),&public_id,"hosted",None,None).await,409,"PUBLICATION_DISABLED");Ok(())}).await
}
