//! Durable, non-monetary SDK observations in the existing Gateway audit log.
//! No prompts, provider credentials, tool arguments or invented token zeros.
use super::*;
use ai_platform_gateway_foundation::sdk_capabilities::canonical_hash;
use sqlx::Row;

const EVENT: &str = "sdk_usage_observation";
const RESOURCE: &str = "sdk_request";
// Existing Gateway role has SELECT/INSERT and audit_logs sequence USAGE. No DDL.
const INDEX_READY: &str = "SELECT EXISTS (SELECT 1 FROM pg_index WHERE indexrelid=to_regclass('gateway.sdk_usage_audit_lookup') AND indrelid='gateway.audit_logs'::regclass AND indisvalid AND indisready)";
const LOCK: &str = "SELECT pg_advisory_xact_lock(hashtextextended($1,0))";
const SELECT: &str = "SELECT request_summary,response_summary FROM gateway.audit_logs WHERE event_type='sdk_usage_observation' AND resource_type='sdk_request' AND resource_id=$1 AND tenant_id=$2 AND user_id=$3 AND action=$4 ORDER BY id LIMIT 2";
const INSERT: &str = "INSERT INTO gateway.audit_logs (event_type,user_id,tenant_id,resource_type,resource_id,action,request_summary,response_summary,status) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,'reconciliation_required')";

#[derive(Clone)]
struct Scope {
    tenant: String,
    user: String,
    resource: String,
    request: Value,
}
impl Scope {
    fn new(request: &UnifiedRequest, framework: &str, agent: &Value, selected_tools: &Value, stream: bool, run: Option<&Value>) -> Result<Self, &'static str> {
        let session = request.session_id.as_deref().filter(|s| !s.is_empty()).ok_or("sdk_audit_scope_invalid")?;
        if [&request.tenant_id,&request.user_id,&request.service_id,&request.request_id].iter().any(|s|s.is_empty() || s.len()>255)
            || session.len()>255 || !matches!(framework,"openai_agents"|"claude_agent_sdk") {
            return Err("sdk_audit_scope_invalid");
        }
        let run = run.map(|v| v.as_str().and_then(|v|uuid::Uuid::parse_str(v).ok()).map(|v|v.to_string()).ok_or("sdk_audit_run_invalid")).transpose()?;
        let model = agent["model"].as_str().filter(|s| !s.is_empty() && s.len()<=160).ok_or("sdk_audit_model_invalid")?;
        let identity=json!({"tenant_id":request.tenant_id,"user_id":request.user_id,"service_id":request.service_id,"request_id":request.request_id,"session_id":session,"framework":framework});
        let mut summary=identity.clone();
        summary["schema"] = json!("sdk-usage-audit/v1");
        summary["model"] = json!(model);
        summary["provider"] = json!(if framework=="openai_agents"{"openai"}else{"anthropic"});
        summary["assistant_run_id"] = json!(run);
        // Include configuration and input in the digest only, never in persisted JSON.
        summary["request_sha256"] = json!(canonical_hash(&json!({"scope":identity,"inputs":request.inputs,"agent":agent,"tools":selected_tools,"stream":stream})));
        Ok(Self{tenant:request.tenant_id.clone(),user:request.user_id.clone(),resource:canonical_hash(&identity),request:summary})
    }
}

/// Copy an explicit numeric allowlist, excluding arbitrary provider metadata.
fn token_fields(value:&Value)->Value {
    let mut result=Map::new();
    for name in ["input_tokens","output_tokens","total_tokens","cache_read_input_tokens","cache_creation_input_tokens","cached_tokens","cache_write_tokens"] {
        if let Some(value)=value.get(name).filter(|v|v.as_i64().is_some_and(|n|n>=0)) {result.insert(name.into(),value.clone());}
    }
    if let Some(details)=value.get("input_tokens_details").filter(|v|v.is_object()) {
        let mut fields=Map::new();
        for key in ["cached_tokens","cache_write_tokens"] {
            if let Some(value)=details.get(key).filter(|v|v.as_i64().is_some_and(|n|n>=0)){fields.insert(key.into(),value.clone());}
        }
        result.insert("input_tokens_details".into(),Value::Object(fields));
    }
    Value::Object(result)
}
fn completed(usage:&Map<String,Value>)->Value {
    let value=Value::Object(usage.clone());let audit=&value["sdk_usage"];
    let mut observation=token_fields(&value);
    observation["local_execution_status"]=json!("completed");
    observation["provider_outcome"]=json!("completed");
    observation["usage_state"]=json!(if audit["state"]=="reported"{"reported"}else{"unavailable"});
    observation["platform_pricing_state"]=json!(if audit["platform_pricing_state"]=="two_rate_compatible"{"two_rate_compatible"}else{"reconciliation_required"});
    observation["cost_state"]=json!("unavailable");
    if let Some(cost)=audit.get("provider_cost_usd").filter(|v|v.as_f64().is_some_and(|v|v.is_finite() && v>=0.0)) {
        observation["provider_cost_usd"]=cost.clone();observation["cost_state"]=json!("reported");
    }
    if let Some(raw)=audit.get("provider_usage") {
        if let Some(items)=raw.as_array().filter(|v|v.len()<=32) {
            observation["provider_usage"]=Value::Array(items.iter().map(token_fields).collect());
        }else if raw.is_object(){observation["provider_usage"]=token_fields(raw);}
    }
    // This is an observation, never a statement that the priced sink committed.
    observation["accounting_status"]=json!("not_asserted");
    observation
}
fn unresolved(status:&str)->Value {
    json!({"local_execution_status":status,"provider_outcome":"unknown","usage_state":"unavailable","cost_state":"unavailable","platform_pricing_state":"reconciliation_required","accounting_status":"not_asserted"})
}

fn existing_matches(rows:&[(Value,Value)],scope:&Scope,observation:&Value)->Result<bool,&'static str>{
    match rows {
        []=>Ok(false),
        [(request,response)] if *request==scope.request && *response==*observation=>Ok(true),
        _=>Err("sdk_audit_identity_conflict"),
    }
}

async fn append(pool: &sqlx::PgPool, scope: &Scope, phase: &str, observation: &Value) -> Result<(), &'static str> {
    if !matches!(phase,"admitted"|"completed"|"cancelled"|"failed") || observation.to_string().len()>32768 {return Err("sdk_audit_observation_invalid");}
    let mut tx=pool.begin().await.map_err(|_|"sdk_audit_unavailable")?;
    // The SELECT after waiting for the lock must see the previous writer's commit,
    // even when the connection's default isolation has been raised by an operator.
    sqlx::query("SET TRANSACTION ISOLATION LEVEL READ COMMITTED").execute(&mut *tx).await.map_err(|_|"sdk_audit_unavailable")?;
    // Fixed server-side timeout prevents stalled audit locks from hanging SDK cleanup.
    sqlx::query("SET LOCAL lock_timeout='5s'").execute(&mut *tx).await.map_err(|_|"sdk_audit_unavailable")?;
    sqlx::query("SET LOCAL statement_timeout='10s'").execute(&mut *tx).await.map_err(|_|"sdk_audit_unavailable")?;
    // Missing/invalid paired epoch16 must fail before a growing-history scan.
    if !sqlx::query_scalar::<_,bool>(INDEX_READY).fetch_one(&mut *tx).await.map_err(|_|"sdk_audit_unavailable")? {
        return Err("sdk_audit_index_required");
    }
    sqlx::query(LOCK).bind(format!("sdk-usage-audit/v1:{}",scope.resource)).execute(&mut *tx).await.map_err(|_|"sdk_audit_unavailable")?;
    let prior=sqlx::query(SELECT).bind(&scope.resource).bind(&scope.tenant).bind(&scope.user).bind(phase)
        .fetch_all(&mut *tx).await.map_err(|_|"sdk_audit_unavailable")?;
    let prior=prior.iter().map(|row|Ok((row.try_get::<Value,_>("request_summary")?,row.try_get::<Value,_>("response_summary")?)))
        .collect::<Result<Vec<_>,sqlx::Error>>().map_err(|_|"sdk_audit_unavailable")?;
    if !existing_matches(&prior,scope,observation)? {
        // Every terminal write requires the matching pre-dispatch admission.
        if phase!="admitted" {
            let admission=sqlx::query(SELECT).bind(&scope.resource).bind(&scope.tenant).bind(&scope.user).bind("admitted")
                .fetch_all(&mut *tx).await.map_err(|_|"sdk_audit_unavailable")?;
            if admission.len()!=1 || admission[0].try_get::<Value,_>("request_summary").map_err(|_|"sdk_audit_unavailable")?!=scope.request {
                return Err("sdk_audit_admission_missing");
            }
        }
        sqlx::query(INSERT).bind(EVENT).bind(&scope.user).bind(&scope.tenant).bind(RESOURCE).bind(&scope.resource).bind(phase).bind(&scope.request).bind(observation)
            .execute(&mut *tx).await.map_err(|_|"sdk_audit_unavailable")?;
    }
    // An uncertain commit is not retried. Admission already supplies recovery scope.
    tx.commit().await.map_err(|_|"sdk_audit_commit_unknown")
}

/// Admission is durable before the worker HTTP request. Terminal writes are owned
/// by a task once started; dropping its waiter does not cancel persistence.
/// A hard crash leaves the original admission, never an assertion of zero cost.
pub(super) struct UsageAudit {
    pool:sqlx::PgPool,
    scope:Scope,
    terminal:bool,
}
impl UsageAudit {
    /// Closed, lazy pool for adapter stream tests: no connection or audit I/O.
    #[cfg(test)]
    pub(super) async fn for_stream_test()->Self {
        let pool=sqlx::postgres::PgPoolOptions::new().connect_lazy_with(sqlx::postgres::PgConnectOptions::new());
        pool.close().await;
        let mut request=UnifiedRequest::new("request","service",vec![ContentItem::text("test")]);
        request.tenant_id="tenant".into();request.user_id="user".into();request.session_id=Some("session".into());
        let scope=Scope::new(&request,"openai_agents",&json!({"model":"model"}),&Value::Null,true,None).unwrap();
        Self{pool,scope,terminal:false}
    }
    pub async fn admit(pool:&sqlx::PgPool,request:&UnifiedRequest,framework:&str,agent:&Value,tools:&Value,stream:bool,run:Option<&Value>)->Result<Self,AdapterError>{
        let scope=Scope::new(request,framework,agent,tools,stream,run).map_err(validation)?;
        append(pool,&scope,"admitted",&unresolved("admitted")).await.map_err(validation)?;
        Ok(Self{pool:pool.clone(),scope,terminal:false})
    }
    pub async fn complete(&mut self,usage:&mut Map<String,Value>) {
        self.terminal=true;
        let pool=self.pool.clone();let scope=self.scope.clone();let observation=completed(usage);
        let written=tokio::spawn(async move {append(&pool,&scope,"completed",&observation).await}).await;
        if let Some(audit)=usage.get_mut("sdk_usage").and_then(Value::as_object_mut){
            audit.insert("audit_resource_id".into(),json!(self.scope.resource));
            audit.insert("audit_persistence".into(),json!(if matches!(written,Ok(Ok(()))){"durable"}else{"unresolved_admission"}));
        }
    }
    pub fn fail(&mut self){self.end_unresolved("failed");}
    pub fn cancel(&mut self){self.end_unresolved("cancelled");}
    fn end_unresolved(&mut self,phase:&'static str){
        if self.terminal{return;}self.terminal=true;
        let pool=self.pool.clone();let scope=self.scope.clone();
        if let Ok(runtime)=tokio::runtime::Handle::try_current(){runtime.spawn(async move {let _=append(&pool,&scope,phase,&unresolved(phase)).await;});}
    }
}
impl Drop for UsageAudit {fn drop(&mut self){self.cancel();}}

#[cfg(test)]
mod tests {
    use super::*;
    fn request()->UnifiedRequest {
        let mut r=UnifiedRequest::new("request","service",vec![ContentItem::text("PRIVATE PROMPT")]);
        r.tenant_id="tenant".into();r.user_id="user".into();r.session_id=Some("session".into());r
    }
    fn scope(r:&UnifiedRequest)->Scope{Scope::new(r,"openai_agents",&json!({"model":"model","instructions":"PRIVATE INSTRUCTIONS"}),&Value::Null,true,None).unwrap()}
    #[test]
    fn scope_is_stable_across_restart_and_contains_no_prompt(){
        let first=scope(&request());let second=scope(&request());
        assert_eq!(first.resource,second.resource);assert_eq!(first.request,second.request);
        let stored=first.request.to_string();assert!(!stored.contains("PRIVATE"));assert!(stored.contains("request_sha256"));
    }
    #[test]
    fn all_account_scope_fields_partition_identity(){
        let base=request();let id=scope(&base).resource;
        for field in ["tenant","user","service","session","request"] {
            let mut other=base.clone();
            match field {"tenant"=>other.tenant_id="other".into(),"user"=>other.user_id="other".into(),"service"=>other.service_id="other".into(),"session"=>other.session_id=Some("other".into()),_=>other.request_id="other".into()}
            assert_ne!(scope(&other).resource,id);
        }
        assert_ne!(Scope::new(&base,"claude_agent_sdk",&json!({"model":"model"}),&Value::Null,true,None).unwrap().resource,id);
    }
    #[test]
    fn changed_request_has_same_identity_but_conflicts(){
        let a=scope(&request());let mut changed=request();changed.inputs=vec![ContentItem::text("CHANGED")];let b=scope(&changed);
        assert_eq!(a.resource,b.resource);assert_ne!(a.request,b.request);
        assert_eq!(existing_matches(&[(a.request.clone(),unresolved("admitted"))],&b,&unresolved("admitted")),Err("sdk_audit_identity_conflict"));
    }
    #[test]
    fn exact_duplicate_does_not_insert_but_conflicting_phase_payload_rejects(){
        let scope=scope(&request());let row=(scope.request.clone(),unresolved("admitted"));
        assert_eq!(existing_matches(&[row.clone()],&scope,&unresolved("admitted")),Ok(true));
        assert_eq!(existing_matches(&[row.clone()],&scope,&unresolved("cancelled")),Err("sdk_audit_identity_conflict"));
        assert_eq!(existing_matches(&[row.clone(),row],&scope,&unresolved("admitted")),Err("sdk_audit_identity_conflict"));
    }
    #[test]
    fn cancellation_has_unknown_provider_outcome_and_no_zero_tokens(){
        let value=unresolved("cancelled");assert_eq!(value["provider_outcome"],"unknown");
        assert!(value.get("input_tokens").is_none());assert!(value.get("provider_cost_usd").is_none());
    }
    #[test]
    fn partial_usage_and_cache_cost_survive_without_arbitrary_metadata(){
        let usage=json!({"output_tokens":2,"secret":"SECRET","sdk_usage":{"state":"unavailable","provider_cost_usd":0.25,"provider_usage":{"input_tokens":4,"cache_read_input_tokens":3,"tool_arguments":"SECRET"}}}).as_object().unwrap().clone();
        let value=completed(&usage);assert_eq!(value["output_tokens"],2);assert!(value.get("input_tokens").is_none());
        assert_eq!(value["provider_usage"]["cache_read_input_tokens"],3);assert_eq!(value["provider_cost_usd"],0.25);
        assert!(!value.to_string().contains("SECRET"));assert_eq!(value["accounting_status"],"not_asserted");
    }
    #[test]
    fn governed_run_is_validated_not_fabricated(){
        let r=request();let agent=json!({"model":"m"});
        assert!(Scope::new(&r,"openai_agents",&agent,&Value::Null,true,Some(&json!("not-a-run"))).is_err());
        let id="00000000-0000-4000-8000-000000000001";
        assert_eq!(Scope::new(&r,"openai_agents",&agent,&Value::Null,true,Some(&json!(id))).unwrap().request["assistant_run_id"],id);
        assert!(scope(&r).request["assistant_run_id"].is_null());
    }
    #[test]
    fn sql_is_scoped_insert_only_and_does_not_charge(){
        assert!(SELECT.contains("tenant_id=$2 AND user_id=$3 AND action=$4"));
        assert!(LOCK.contains("pg_advisory_xact_lock"));
        assert!(INSERT.contains("gateway.audit_logs"));
        for forbidden in ["usage_records","billing_events","user_quotas","UPDATE","DELETE"] {assert!(!INSERT.contains(forbidden));}
    }
}
