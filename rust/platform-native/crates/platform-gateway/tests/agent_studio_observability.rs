//! Pure source defaults/redaction; database/API observability matrix remains separate.
use ai_platform_gateway::{
    metrics_redaction::redact_sensitive_text, migration_agent::governance::default_policy,
    migration_compat::eval_observation::redact_trace_text,
};
use serde_json::json;
#[test]
fn governance_defaults_match_source_fixture() {
    assert_eq!(
        default_policy("tenant-a", "agent-a", "owner-user"),
        json!({"tenant_id":"tenant-a","agent_id":"agent-a","trace_retention_days":90,"runtime_retention_days":30,"attachment_retention_days":1,"legal_hold":false,"principal_requests_per_minute":30,"principal_requests_per_day":1000,"ip_requests_per_minute":60,"ip_requests_per_day":2000,"publication_requests_per_minute":300,"publication_requests_per_day":10000,"max_agents_per_tenant":100,"max_active_publications":10,"max_concurrent_runs":25,"max_daily_tokens":10000000,"max_daily_mcp_calls":100000,"max_storage_bytes":10737418240_i64,"alert_threshold_percent":90,"cache_epoch":0,"updated_by":"owner-user","created_at":null,"updated_at":null})
    );
}
#[test]
fn analytics_preview_redaction_removes_source_secret_examples() {
    let input = redact_sensitive_text(&redact_trace_text("authorization=secret-value", Some(500)));
    let output = redact_sensitive_text(&redact_trace_text("Bearer raw-token", Some(500)));
    assert!(!input.contains("secret-value"));
    assert!(!output.contains("raw-token"));
}
mod agent_studio_support;
use agent_studio_support::*;
use ai_platform_gateway::migration_agent::{governance::runtime_usage, studio::AgentRepository};
use futures_util::FutureExt;
use serde_json::Value;
use sqlx::{PgPool, Row};
async fn operations<F, Fut>(test: F) -> Result<()>
where
    F: FnOnce(PgPool, AgentRepository, String, String) -> Fut,
    Fut: std::future::Future<Output = Result<()>>,
{
    let f = Fixture::custom(
        include_str!("agent_studio_support/operations_base.sql"),
        &["081_agent_studio_operations_governance.sql"],
    )
    .await?;
    let pool = f.pool.clone();
    let repo = f.repo();
    let result=std::panic::AssertUnwindSafe(async move{let id=uuid::Uuid::new_v4().to_string();let publication=uuid::Uuid::new_v4().to_string();let mut tx=pool.begin().await?;sqlx::query("INSERT INTO users(tenant_id,user_id) VALUES('tenant-ops','owner-ops')").execute(&mut *tx).await?;sqlx::query("INSERT INTO agents(tenant_id,agent_id,owner_id) VALUES('tenant-ops',$1::uuid,'owner-ops')").bind(&id).execute(&mut *tx).await?;sqlx::query("INSERT INTO agent_members(tenant_id,agent_id,principal_type,principal_id,role) VALUES('tenant-ops',$1::uuid,'user','owner-ops','owner')").bind(&id).execute(&mut *tx).await?;sqlx::query("INSERT INTO agent_publications(tenant_id,publication_id,agent_id,channel) VALUES('tenant-ops',$1::uuid,$2::uuid,'api')").bind(&publication).bind(&id).execute(&mut *tx).await?;tx.commit().await?;test(pool,repo,id,publication).await}).catch_unwind().await;
    f.close().await?;
    match result {
        Ok(r) => r,
        Err(e) => std::panic::resume_unwind(e),
    }
}
#[tokio::test]
#[ignore = "explicit isolated PostgreSQL contract"]
async fn audit_dimensions_redaction_and_policy_bounds() -> Result<()> {
    operations(|p,_repo,id,publication|async move{let version=uuid::Uuid::new_v4().to_string();let r=sqlx::query("INSERT INTO audit_logs(event_type,user_id,tenant_id,resource_type,resource_id,action,request_summary,status) VALUES('agent_studio','owner-ops','tenant-ops','agent',$1,'publication_promote',jsonb_build_object('agent_version_id',$2::text,'publication_id',$3::text,'channel','api','authorization','synthetic-secret'),'success') RETURNING agent_id::text,agent_version_id::text,publication_id::text,channel,request_summary,redaction_state").bind(&id).bind(&version).bind(&publication).fetch_one(&p).await?;assert_eq!(r.get::<String,_>("agent_id"),id);assert_eq!(r.get::<String,_>("agent_version_id"),version);assert_eq!(r.get::<String,_>("publication_id"),publication);assert_eq!(r.get::<String,_>("channel"),"api");let summary:Value=r.get("request_summary");assert_eq!(summary["authorization"],"[REDACTED]");assert!(!summary.to_string().contains("synthetic-secret"));assert_eq!(r.get::<Value,_>("redaction_state")["sensitive_fields"],"removed");state(sqlx::query("INSERT INTO agent_governance_policies(tenant_id,agent_id,trace_retention_days,updated_by) VALUES('tenant-ops',$1::uuid,0,'owner-ops')").bind(&id).execute(&p).await,"23514","");Ok(())}).await
}
#[tokio::test]
#[ignore = "explicit isolated PostgreSQL contract"]
async fn runtime_usage_caps_emit_threshold_and_exceeded_alerts() -> Result<()> {
    operations(|p,repo,id,publication|async move{repo.update_governance(&actor("owner-ops","tenant-ops"),&id,&json!({"max_concurrent_runs":1,"max_daily_tokens":10,"max_daily_mcp_calls":1,"max_storage_bytes":5,"alert_threshold_percent":50})).await?;let trace=uuid::Uuid::new_v4().to_string();sqlx::query("INSERT INTO assistant_runs(run_id,tenant_id,user_id,agent_id) VALUES(gen_random_uuid(),'tenant-ops','owner-ops',$1::uuid)").bind(&id).execute(&p).await?;sqlx::query("INSERT INTO agent_traces(trace_id,tenant_id,user_id,agent_id,publication_id,channel,total_tokens) VALUES($1::uuid,'tenant-ops','owner-ops',$2::uuid,$3::uuid,'api',6)").bind(&trace).bind(&id).bind(&publication).execute(&p).await?;sqlx::query("INSERT INTO agent_trace_spans(span_id,trace_id,span_kind,name) VALUES(gen_random_uuid(),$1::uuid,'tool_execution','tool:mcp_docs_write')").bind(&trace).execute(&p).await?;sqlx::query("INSERT INTO agent_runtime_attachments(tenant_id,attachment_id,publication_id,principal_id,channel,storage_key,size_bytes,expires_at) VALUES('tenant-ops',gen_random_uuid(),$1::uuid,'owner-ops','api','tenant/quota-existing',3,NOW()+INTERVAL '1 day')").bind(&publication).execute(&p).await?;let result=runtime_usage(&repo,&json!({"tenant_id":"tenant-ops","agent_id":id,"publication_id":publication})).await?;assert_eq!(result["usage"],json!({"concurrent_runs":1,"daily_tokens":6,"daily_mcp_calls":1,"storage_bytes":3}));assert_eq!(result["exceeded"].as_array().unwrap().iter().map(|x|x.as_str().unwrap()).collect::<std::collections::BTreeSet<_>>(),["AGENT_RUNTIME_CONCURRENCY_QUOTA_EXCEEDED","AGENT_RUNTIME_MCP_QUOTA_EXCEEDED"].into_iter().collect());let count:i64=sqlx::query_scalar("SELECT COUNT(*) FROM audit_logs WHERE tenant_id='tenant-ops' AND agent_id=$1::uuid AND action IN ('quota_threshold_reached','quota_exceeded')").bind(&id).fetch_one(&p).await?;assert_eq!(count,4);Ok(())}).await
}
#[tokio::test]
#[ignore = "explicit isolated PostgreSQL contract"]
async fn legal_hold_blocks_cleanup_and_receipt_is_terminal() -> Result<()> {
    operations(|p,repo,id,_|async move{let owner=actor("owner-ops","tenant-ops");repo.update_governance(&owner,&id,&json!({"legal_hold":true})).await?;let result=repo.prepare_data_deletion(&owner,&id,"tenant",None,"tenant-delete-0001").await?;assert_eq!(result["status"],"blocked");assert_eq!(result["error_code"],"AGENT_LEGAL_HOLD_ACTIVE");state(sqlx::query("UPDATE agent_data_deletion_requests SET error_code=NULL WHERE deletion_id=$1::uuid").bind(result["deletion_id"].as_str().unwrap()).execute(&p).await,"55000","");let replay=repo.prepare_data_deletion(&owner,&id,"tenant",None,"tenant-delete-0001").await?;assert_eq!(replay["deletion_id"],result["deletion_id"]);Ok(())}).await
}
#[tokio::test]
#[ignore = "explicit isolated PostgreSQL and Redis atomic channel limiter contract"]
async fn channel_limiter_is_atomic_shared_and_ip_bound_across_rotated_principals() -> Result<()> {
    operations(|p,repo,id,publication|async move {
 let g=agent_studio_support::state::state(p.clone()).await?;let control=g.control_pool.clone();
 let result=std::panic::AssertUnwindSafe(async {
 let public=g.state.public.as_ref().unwrap();let owner=actor("owner-ops","tenant-ops");
 repo.update_governance(&owner,&id,&json!({"publication_requests_per_minute":1,"ip_requests_per_minute":100,"principal_requests_per_minute":100})).await?;
 let publication_value=json!({"tenant_id":"tenant-ops","agent_id":id,"publication_id":publication,"policy":{}});
 let outcomes=futures_util::future::join_all((0..8).map(|index|{let publication_value=&publication_value;let public=public.clone();let repo=&repo;async move{public.enforce_limits(repo,publication_value,&format!("principal-{index}"),&format!("192.0.2.{index}")).await}})).await;
 assert_eq!(outcomes.iter().filter(|r|r.is_ok()).count(),1);for result in outcomes.into_iter().filter(|r|r.is_err()){error(result,429,"AGENT_RUNTIME_RATE_LIMITED");}
 repo.update_governance(&owner,&id,&json!({"publication_requests_per_minute":100,"ip_requests_per_minute":1})).await?;
 let second=uuid::Uuid::new_v4().to_string();sqlx::query("INSERT INTO agent_publications(tenant_id,publication_id,agent_id,channel) VALUES('tenant-ops',$1::uuid,$2::uuid,'api')").bind(&second).bind(&id).execute(&p).await?;
 let publication_value=json!({"tenant_id":"tenant-ops","agent_id":id,"publication_id":second,"policy":{}});
 public.enforce_limits(&repo,&publication_value,"first-token","198.51.100.10").await?;
 error(public.enforce_limits(&repo,&publication_value,"rotated-token","198.51.100.10").await,429,"AGENT_RUNTIME_RATE_LIMITED");
 // The database policy is authoritative even when the caller supplies a looser limit.
 let mut loose=publication_value.clone();loose["policy"]=json!({"ip_requests_per_minute":1000});error(public.enforce_limits(&repo,&loose,"third-token","198.51.100.10").await,429,"AGENT_RUNTIME_RATE_LIMITED");
 Ok::<(),Box<dyn std::error::Error+Send+Sync>>(())
 }).catch_unwind().await;drop(g);control.close().await;match result{Ok(r)=>r,Err(e)=>std::panic::resume_unwind(e)}
 }).await
}
