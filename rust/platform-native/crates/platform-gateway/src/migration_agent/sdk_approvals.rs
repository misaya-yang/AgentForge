//! Owner-authenticated SDK approval projection/decision, sharing the original
//! durable approval row and existing exact-action preview policy.
use super::{Actor,AgentError,approval::action_details};
use ai_platform_gateway_foundation::sdk_capabilities::canonical_hash;
use serde_json::{Value,json};
use sqlx::PgPool;
mod binding;
mod usage;
async fn row(pool:&PgPool,tenant:&str,user:&str,id:&str)->Result<Value,AgentError>{
    let id=super::uuid(id)?;
    sqlx::query_scalar::<_,Value>("SELECT jsonb_build_object('approval_id',a.approval_id,'run_id',a.run_id,'status',a.status,'tool_name',c.tool_name,'capability_id',a.tool_name,'arguments',a.arguments,'arguments_hash',c.arguments_sha256,'tool_call_id',a.tool_call_id,'expires_in_seconds',GREATEST(0,extract(epoch from a.expires_at-now())),'engine',s.engine,'active',r.status='running' AND s.revoked_at IS NULL AND s.expires_at>now() AND s.lease_until>now()) FROM assistant.assistant_tool_approvals a JOIN assistant.assistant_sdk_tool_calls c ON c.run_id=a.run_id AND c.approval_id=a.approval_id AND c.tool_call_id=a.tool_call_id AND c.arguments=a.arguments JOIN assistant.assistant_sdk_runs s ON s.run_id=a.run_id AND s.tenant_id=a.tenant_id AND s.user_id=a.user_id AND s.session_id=a.session_id JOIN assistant.assistant_runs r ON r.run_id=s.run_id AND r.engine=s.engine WHERE a.approval_id=$1::uuid AND a.tenant_id=$2 AND a.user_id=$3")
        .bind(id).bind(tenant).bind(user).fetch_optional(pool).await?.ok_or_else(||AgentError::code(404,"APPROVAL_NOT_FOUND"))
}
pub async fn preview(pool:&PgPool,tenant:&str,user:&str,id:&str)->Result<Value,AgentError>{
    let value=row(pool,tenant,user,id).await?;
    let unavailable=json!({"can_approve":false,"reason":"Action details could not be verified or the run is no longer active.","authorization_scope":"this action only"});
    let args=&value["arguments"];
    if value["active"]!=true || value["status"]!="pending" || value["expires_in_seconds"].as_f64().unwrap_or(0.0)<=0.0
        || canonical_hash(args)!=value["arguments_hash"].as_str().unwrap_or("").trim() {return Ok(unavailable);}
    let Some((target,parameters))=action_details(value["tool_name"].as_str().unwrap_or(""),args) else{return Ok(unavailable)};
    Ok(json!({"can_approve":true,"tool_name":value["tool_name"],"effect":"write","target":target,"parameters":parameters,"authorization_scope":"this action only","expires_in_seconds":value["expires_in_seconds"],"arguments_hash":canonical_hash(args)}))
}
pub async fn get(pool:&PgPool,actor:&Actor,id:&str)->Result<Value,AgentError>{
    let mut value=row(pool,&actor.tenant_id,&actor.user_id,id).await?;
    let preview=preview(pool,&actor.tenant_id,&actor.user_id,id).await?;
    if value["status"]=="pending" {
        if value["expires_in_seconds"].as_f64().unwrap_or(0.0)<=0.0 {value["status"]=json!("expired");}
        else if value["active"]!=true {value["status"]=json!("cancelled");}
    }
    value.as_object_mut().expect("projection").remove("arguments");
    Ok(json!({"approval":value,"preview":preview}))
}
pub async fn decide(pool:&PgPool,actor:&Actor,id:&str,approved:bool,reason:Option<&str>,payload:&Value)->Result<Value,AgentError>{
    let expected=binding::ExpectedBinding::parse(payload).map_err(|code|AgentError::code(422,code))?;
    let value=row(pool,&actor.tenant_id,&actor.user_id,id).await?;
    if expected.as_ref().is_some_and(|binding|!binding.matches(&value)){
        return Err(AgentError::code(409,"APPROVAL_BINDING_MISMATCH"));
    }
    if approved && preview(pool,&actor.tenant_id,&actor.user_id,id).await?["can_approve"]!=true {
        return Err(AgentError::code(409,"APPROVAL_ACTION_UNVERIFIED"));
    }
    let status:String=sqlx::query_scalar("SELECT assistant.decide_sdk_approval($1::uuid,$2,$3,$4,$5,$6)")
        .bind(super::uuid(id)?).bind(&actor.tenant_id).bind(&actor.user_id).bind(approved).bind(reason.filter(|r|r.len()<=1024))
        .bind(expected.as_ref().map(|binding|binding.arguments_hash).unwrap_or_else(||value["arguments_hash"].as_str().unwrap_or("").trim())).fetch_one(pool).await?;
    Ok(json!({"approval":{"approval_id":id,"run_id":value["run_id"],"status":status,"approved":approved}}))
}

/// Read/cancel genuine SDK runs through the existing authenticated run surface.
/// No Runtime thread recovery endpoint or fabricated thread identity is involved.
pub async fn run_status(pool:&PgPool,actor:&Actor,run:&str)->Result<Option<Value>,AgentError>{
    run_status_inner(pool,actor,run,true).await
}
async fn run_status_inner(pool:&PgPool,actor:&Actor,run:&str,include_usage:bool)->Result<Option<Value>,AgentError>{
    let Some(id)=uuid::Uuid::parse_str(run).ok() else{return Ok(None)};
    let row=sqlx::query_scalar::<_,Value>("SELECT jsonb_build_object('run_id',r.run_id,'session_id',r.session_id,'engine',r.engine,'status',CASE WHEN r.status='running' AND (s.revoked_at IS NOT NULL OR s.expires_at<=now() OR s.lease_until<=now()) THEN 'failed' ELSE r.status END,'terminal_reason',CASE WHEN r.status='running' AND (s.revoked_at IS NOT NULL OR s.expires_at<=now() OR s.lease_until<=now()) THEN 'sdk_authority_expired' ELSE NULL END,'started_at',r.started_at,'finished_at',r.finished_at,'updated_at',r.updated_at,'service_id',s.service_id,'request_id',s.request_id,'harness_thread_id',NULL,'harness_turn_id',NULL) FROM assistant.assistant_runs r JOIN assistant.assistant_sdk_runs s ON s.run_id=r.run_id AND s.engine=r.engine WHERE r.run_id=$1::uuid AND r.tenant_id=$2 AND r.user_id=$3 AND r.engine IN ('openai_agents','claude_agent_sdk')")
        .bind(id.to_string()).bind(&actor.tenant_id).bind(&actor.user_id).fetch_optional(pool).await?;
    let Some(mut row)=row else{return Ok(None)};
    let identity=json!({"tenant_id":actor.tenant_id,"user_id":actor.user_id,"service_id":row["service_id"],"request_id":row["request_id"],"session_id":row["session_id"],"framework":row["engine"]});
    if include_usage {row["usage"]=usage::read(pool,&identity,&id.to_string()).await;}
    // Request identity is internal lookup scope, not a second public task identity.
    row.as_object_mut().expect("projection").remove("request_id");
    row.as_object_mut().expect("projection").remove("service_id");
    Ok(Some(row))
}
pub async fn cancel_run(pool:&PgPool,actor:&Actor,run:&str)->Result<Option<Value>,AgentError>{
    let Some(id)=uuid::Uuid::parse_str(run).ok() else{return Ok(None)};
    let row=sqlx::query_scalar::<_,Value>("SELECT to_jsonb(s) FROM assistant.assistant_sdk_runs s WHERE s.run_id=$1::uuid AND s.tenant_id=$2 AND s.user_id=$3")
        .bind(id.to_string()).bind(&actor.tenant_id).bind(&actor.user_id).fetch_optional(pool).await?;
    let Some(row)=row else{return Ok(None)};
    let _:bool=sqlx::query_scalar("SELECT assistant.finish_sdk_execution($1::uuid,$2::uuid,$3,'cancelled')")
        .bind(id.to_string()).bind(row["owner_id"].as_str().ok_or_else(||AgentError::code(409,"SDK_OWNER_INVALID"))?)
        .bind(row["fence"].as_i64().ok_or_else(||AgentError::code(409,"SDK_OWNER_INVALID"))?).fetch_one(pool).await?;
    let status=run_status_inner(pool,actor,run,false).await?.ok_or_else(||AgentError::code(404,"RUN_NOT_FOUND"))?;
    Ok(Some(json!({"task_id":run,"session_id":status["session_id"],"cancelled":status["status"]=="cancelled","message":"SDK run cancellation recorded"})))
}
