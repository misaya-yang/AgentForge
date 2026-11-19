//! Public transparent proxy adapter; all authority and execution remain shared.
use super::{common::{CompatError, CompatUser}, langgraph::{LangGraphClient}, langgraph_body, transparent_proxy::{ProxyRequest, ProxyServiceConfig, TransparentProxy}};
use crate::{capacity::{CapacityRequest, CapacityResolver}, migration_content::{ContentAccess, ContentIdentity}, migration_host::bootstrap::foundation_backends::{LoadedProxyConfiguration, PgProxyConfigLoader}, service_access_policy::{ServiceAccessConstraints, ServiceAccessLoadContext, service_visible_with_scopes}, service_access_resolver::{PgServiceAccessConstraintStore, ServiceAccessConstraintResolver, ServiceAccessRequestCache, ServiceAccessResolverContext}, service_registry::ServiceRegistry};
use ai_platform_auth::admission::AdmissionService;
use axum::{Router, Json, body::{Bytes, to_bytes}, extract::{Request, State}, http::{Method, StatusCode}, response::{IntoResponse, Response}, routing::{any, get}};
use serde_json::{Value, json};
use std::sync::Arc;

pub struct ProxyHttp {
    pub access: Arc<crate::migration_content::access::NativeContentAccess>,
    pub admission: Arc<AdmissionService>,
    pub registry: Arc<ServiceRegistry>,
    pub loader: Arc<PgProxyConfigLoader>,
    pub proxy: Arc<TransparentProxy>,
    pub capacity: Arc<CapacityResolver>,
    pub constraints: ServiceAccessConstraintResolver,
    pub rates: crate::rate_policy_store::RatePolicyResolver,
    pub security: Arc<crate::migration_host::bootstrap::foundation_backends::PgFoundationSecurityEvents>,
    pub constraint_store: Arc<PgServiceAccessConstraintStore>,
    pub langgraph: Arc<LangGraphClient>,
}
pub fn router(state: Arc<ProxyHttp>) -> Router {
    Router::new()
        .route(crate::migration_host::native_public_contract::PROXY_LIST, get(dispatch).head(|| async { StatusCode::METHOD_NOT_ALLOWED }))
        .route(crate::migration_host::native_public_contract::PROXY_ROOT, any(dispatch))
        .route(crate::migration_host::native_public_contract::PROXY_SLASH, any(dispatch))
        .route(crate::migration_host::native_public_contract::PROXY_WILDCARD, any(dispatch))
        .with_state(state)
}
async fn dispatch(State(state): State<Arc<ProxyHttp>>, request: Request) -> Response {
    let headers=request.headers().clone();
    let path=request.uri().path().to_owned();
    match handle(&state, request).await {
        Ok(response) => response,
        Err(error) => {
            if error.status==403 && error.detail.get("policy").is_none() && !path.ends_with("_selftest") && path!="/api/v1/proxy" {
                if let Ok(user)=state.access.resolve(&headers).await {
                    let service=path.strip_prefix("/api/v1/proxy/").unwrap_or("").split('/').next().unwrap_or("");
                    state.security.record_decision(&user.tenant_id,&user.user_id,service,"auth_failed").await;
                }
            }
            error.into_response()
        }
    }
}
fn content_error(e: crate::migration_content::ContentError) -> CompatError { let mut error=CompatError::value(e.status, e.detail); error.headers=e.headers; error }
fn visible(config: &ProxyServiceConfig, extra: Option<&str>, user: &ContentIdentity, constraints: &ServiceAccessConstraints) -> bool {
    service_visible_with_scopes(&[json!(config.service_id), json!(config.service_name), json!(extra.unwrap_or(""))], &constraints.allowed_sources, &constraints.user_policy, user.roles.iter().any(|r| r == "admin"))
}
async fn handle(state: &ProxyHttp, request: Request) -> Result<Response, CompatError> {
    if !matches!(request.method().as_str(), "GET"|"POST"|"PUT"|"PATCH"|"DELETE"|"HEAD"|"OPTIONS") { return Ok(StatusCode::METHOD_NOT_ALLOWED.into_response()); }
    let (parts, body) = request.into_parts();
    let user = state.access.resolve(&parts.headers).await.map_err(content_error)?;
    let tail = parts.uri.path().strip_prefix("/api/v1/proxy").unwrap_or("").trim_start_matches('/');
    let (service, path) = tail.split_once('/').unwrap_or((tail, ""));
    let admin = user.roles.iter().any(|r| r == "admin");
    let mut actor = CompatUser { user_id:user.user_id.clone(), tenant_id:user.tenant_id.clone(), tier:user.tier.clone(), authenticated:user.is_authenticated, roles:user.roles.clone(), permissions:user.permissions.clone(), ..Default::default() };
    actor.apply_request_context(&parts.extensions);
    // Canonical resolution above owns guest/static fallback; a second lookup only
    // extracts restrictions from a genuine current database-bound credential.
    let auth_request = state.admission.request(&parts.headers, parts.uri.path());
    let principal = auth_request.resolve().await.ok();
    let key = principal.as_ref().and_then(|p| p.api_key());
    let context = ServiceAccessResolverContext {
        load: ServiceAccessLoadContext { api_key_info:key.map_or(Value::Null, |k| json!({"allowed_services":k.allowed_services})), user_id:user.user_id.clone(), tenant_id:user.tenant_id.clone(), is_authenticated:user.is_authenticated },
        database_identity:Arc::as_ptr(&state.constraint_store) as usize, database_type_module:"native.postgres".into(), configured_ttl:json!(30), api_key_hash:key.map_or(Value::Null, |k| json!(k.key_hash)), roles:user.roles.iter().map(|r|json!(r)).collect(),
    };

    if path == "_selftest" {
        if !admin { return Err(CompatError::new(403,"Admin access required")); }

    } else {
        state.access.require_capability(&user,"AgentInvoke").await.map_err(content_error)?;
    }
    let constraints = if path == "_selftest" { None } else { Some(state.constraints.load(&mut ServiceAccessRequestCache::default(), &context, Some(state.constraint_store.as_ref())).await.map_err(|_| CompatError::new(503,"Service access policy unavailable"))?) };
    if service.is_empty() {
        let configs = state.loader.list().await.map_err(|_|CompatError::new(503,"Proxy configuration unavailable"))?;
        let mut services=Vec::new();
        for config in configs {
            if visible(&config.proxy,None,&user,constraints.as_deref().expect("non-selftest constraints")) {
                let availability=state.proxy.availability(&config.proxy).await.unwrap_or_else(|_|json!({"availability_status":"unknown"}));
                services.push(discovery(&config,admin,&availability));
            }
        }
        return Ok(Json(json!({"count":services.len(),"services":services})).into_response());
    }
    if path != "_selftest" {
        let mut aliases=vec![json!(service)];
        let mut handle=state.registry.get(service).await.map_err(|_|CompatError::new(503,"Service registry unavailable"))?;
        if handle.is_none() { handle=state.registry.list(None,&[],None).await.map_err(|_|CompatError::new(503,"Service registry unavailable"))?.into_iter().find(|s|s.snapshot().name==service); }
        if let Some(handle)=handle {
            let definition=handle.snapshot();
            aliases.extend([json!(definition.service_id),json!(definition.name)]);
            if let Some(config)=&definition.service_config {
                let auth=&config.auth;
                if auth.enabled && !auth.public {
                    if auth.require_auth && !user.is_authenticated { return Err(CompatError::new(403,format!("Permission denied: authentication required for {service}"))); }
                    if !auth.allowed_roles.is_empty() && !admin && !auth.allowed_roles.iter().any(|r|user.roles.contains(r)) { return Err(CompatError::new(403,format!("Permission denied: not authorized for service {service}"))); }
                    if !auth.allowed_api_keys.is_empty() {
                        let supplied=if state.access.identities.api_key_enabled { parts.headers.get(&state.access.identities.api_key_header).and_then(|v|v.to_str().ok()).unwrap_or("") } else { "" };
                        if supplied.is_empty() { return Err(CompatError::new(403,format!("Permission denied: API key required for service {service}"))); }
                        if !auth.allowed_api_keys.iter().any(|p|p=="*"||supplied.starts_with(p)) { return Err(CompatError::new(403,format!("Permission denied: API key not allowed for service {service}"))); }
                    }
                }
            }
        }
        enforce_constraints(service,&aliases,admin,constraints.as_deref().expect("non-selftest constraints"))?;
    }
    let loaded=state.loader.load(service).await.map_err(|_|CompatError::new(503,"Proxy configuration unavailable"))?;
    let loaded=match loaded {
        Some(loaded)=>loaded,
        None if path=="_health"=>return Err(CompatError::value(503,json!({"status":"unhealthy","service":service,"message":"UNAVAILABLE","availability_status":null,"last_health_check_at":null,"last_health_error":null}))),
        None if path=="_selftest"=>{
            let empty=LoadedProxyConfiguration{proxy:ProxyServiceConfig{service_id:service.into(),service_name:service.into(),..Default::default()},capacity_config:Value::Null,rate_config:Value::Null,model_override:Value::Null};
            if let Err(response)=rate_limit(state,&user,key,&empty,service,"proxy_selftest").await{return Ok(response);}
            let error=format!("Service not found: {service}");
            return Ok(Json(json!({"service":service,"auth_header_present":parts.headers.contains_key("authorization"),"assistant_list":{"status_code":404,"ok":false,"error":error,"body_preview":null},"stream":{"ok":false,"status_code":404,"error":error}})).into_response());
        }
        None=>return Err(CompatError::new(404,format!("Service not found: {service}"))),
    };
    if path=="_health" {
        let availability=state.proxy.availability(&loaded.proxy).await?;
        let healthy=availability["available_upstreams"].as_array().is_some_and(|v|!v.is_empty());
        let mut result=json!({"availability_status":availability["availability_status"],"last_health_check_at":availability["last_health_check_at"],"last_health_error":availability["last_health_error"]});
        result["status"]=json!(if healthy{"healthy"}else{"unhealthy"});
        result["service"]=json!(service);
        result["message"]=json!(if healthy{"HEALTHY"}else{"UNAVAILABLE"});
        return if healthy { Ok(Json(result).into_response()) } else { Err(CompatError::value(503,result)) };
    }
    if path=="_selftest" {
        if let Err(response)=rate_limit(state,&user,key,&loaded,service,"proxy_selftest").await {return Ok(response);}
        return selftest(state,loaded,&actor,key.map(|k|k.key_id.clone()),parts.headers).await; }
    let operation=operation(parts.method.as_str(),path);
    let rate_headers=match rate_limit(state,&user,key,&loaded,service,operation).await {Ok(headers)=>headers,Err(response)=>return Ok(response)};
    let mut bytes=if matches!(parts.method.as_str(),"POST"|"PUT"|"PATCH") { to_bytes(body,usize::MAX).await.map_err(|_|CompatError::new(400,"Invalid request body"))? } else { Bytes::new() };
    let mut payload=serde_json::from_slice::<Value>(&bytes).ok().filter(Value::is_object).unwrap_or_else(||json!({}));
    let run=langgraph_body::is_run_path(parts.method.as_str(),path) && (loaded.proxy.assistant_id.is_some() || loaded.proxy.graph_id.is_some() || loaded.proxy.metadata["adapter_type"]=="langgraph");
    let config=json!({"service_id":loaded.proxy.service_id,"service_name":loaded.proxy.service_name,"assistant_id":loaded.proxy.assistant_id,"graph_id":loaded.proxy.graph_id,"default_model":loaded.proxy.default_model,"default_provider":loaded.proxy.default_provider,"metadata":loaded.proxy.metadata,"model_override":if run{loaded.model_override.clone()}else{Value::Null}});
    let original=payload.clone();
    state.langgraph.governance.prepare_proxy_payload(&mut payload,path,&actor,&config,None,key.map_or(&[],|k|k.allowed_models.as_slice()),run,should_apply_quota(parts.method.as_str(),operation,path)).await?;
    if payload!=original { bytes=Bytes::from(serde_json::to_vec(&payload).map_err(|_|CompatError::new(500,"Body encoding failed"))?); }
    let query:Vec<(String,String)>=url::form_urlencoded::parse(parts.uri.query().unwrap_or("").as_bytes()).into_owned().collect();
    let stream=wants_stream(parts.headers.get("accept").and_then(|h|h.to_str().ok()).unwrap_or(""),path,&query);
    let effective_model=crate::usage_parser::extract_model(&payload).or(loaded.proxy.default_model.clone());
    let effective_provider=crate::usage_parser::extract_provider(&payload).or(loaded.proxy.default_provider.clone());
    let budgets=state.capacity.resolve(&CapacityRequest{tenant_id:user.tenant_id,service_id:loaded.proxy.service_id.clone(),request_class:capacity_class(stream,path).into(),upstream_group:Value::Null,provider_id:json!(effective_provider),is_admin_read:false,service_config:json!({"capacity":loaded.capacity_config}),resource_kind:"request".into()}).map_err(|_|CompatError::new(503,"Capacity configuration unavailable"))?;
    let mut response=state.proxy.proxy(ProxyRequest{config:loaded.proxy,user:actor,api_key_id:key.map(|k|k.key_id.clone()),method:parts.method,path:path.into(),body:bytes,headers:parts.headers,query,stream,langgraph_body_prepared:run,effective_model,effective_provider,capacity_budgets:budgets}).await?;
    response.headers_mut().extend(rate_headers);
    Ok(response)
}
fn wants_stream(accept:&str,path:&str,query:&[(String,String)])->bool {
    accept.contains("text/event-stream") || ["/stream","/runs/stream","/sse","stream=true"].iter().any(|p|path.to_lowercase().contains(p)) || query.iter().rev().find(|(k,_)|k=="stream").is_some_and(|(_,v)|matches!(v.as_str(),"true"|"1"|"yes"))
}
fn discovery(config:&LoadedProxyConfiguration,admin:bool,availability:&Value)->Value {
    let c=&config.proxy;
    let adapter=c.metadata["adapter_type"].as_str().unwrap_or("").trim().to_lowercase();
    let kind=c.metadata["service_type"].as_str().filter(|s|!s.trim().is_empty()).map(|s|s.trim().to_lowercase()).unwrap_or_else(||if adapter=="langgraph"||c.assistant_id.as_deref().is_some_and(|s|!s.trim().is_empty())||c.graph_id.as_deref().is_some_and(|s|!s.trim().is_empty()){"langgraph"}else{"proxy"}.into());
    let mut metadata=json!({"model_override":safe_override(&config.model_override)});
    if !adapter.is_empty() || kind=="langgraph" { metadata["adapter_type"]=json!(if adapter.is_empty(){"langgraph"}else{&adapter}); }
    let mode=c.metadata["proxy_mode"].as_str().unwrap_or(if kind=="langgraph"{"transparent"}else{""}).trim().to_lowercase();
    if !mode.is_empty(){metadata["proxy_mode"]=json!(mode);}
    if c.metadata["ui_preferences"].is_object(){metadata["ui_preferences"]=c.metadata["ui_preferences"].clone();}
    let domain=langgraph_body::normalize_domain_policy(&c.metadata["domain_policy"]);
    if domain!="none"{metadata["domain_policy"]=json!(domain);}
    json!({"service_id":c.service_id,"service_name":c.service_name,"service_type":kind,"metadata":metadata,"upstream_url":if admin{json!(c.upstream_url)}else{Value::Null},"graph_id":if admin{json!(c.graph_id)}else{Value::Null},"assistant_id":if admin{json!(c.assistant_id)}else{Value::Null},"enabled":c.enabled,"availability_status":availability.get("availability_status").cloned().unwrap_or(json!("unknown")),"last_health_check_at":availability["last_health_check_at"],"last_health_error":availability["last_health_error"]})
}
fn safe_override(v:&Value)->Value {
    fn id(v:&Value)->Value { v.as_str().map(str::trim).filter(|s|!s.is_empty()).map_or(Value::Null,|s|json!(s)) }
    let mut result=json!({"enabled":crate::service_registry::python_truthy(&v["enabled"]),"provider_id":id(&v["provider_id"]),"model_id":id(&v["model_id"]),"cache_epoch":if v["cache_epoch"].is_i64()||v["cache_epoch"].is_string(){v["cache_epoch"].clone()}else{Value::Null},"temperature":if v["temperature"].is_number(){v["temperature"].clone()}else{Value::Null}});
    if v["failover"].is_object(){let f=&v["failover"];let candidates:Vec<_>=f["candidates"].as_array().into_iter().flatten().filter(|v|v.is_object()).map(|v|json!({"provider_id":id(&v["provider_id"]),"model_id":id(&v["model_id"])})).collect();result["failover"]=json!({"enabled":crate::service_registry::python_truthy(&f["enabled"]),"max_attempts":if f["max_attempts"].is_i64(){f["max_attempts"].clone()}else{Value::Null},"candidate_count":candidates.len(),"candidates":candidates});}
    result
}

const OPERATIONS: &[(&str,&str)] = &[
    ("GET /assistants","assistant_list"),
    ("POST /assistants","assistant_create"),
    ("POST /assistants/search","assistant_list"),
    ("GET /assistants/","assistant_read"),
    ("PATCH /assistants/","assistant_update"),
    ("DELETE /assistants/","assistant_delete"),
    ("POST /threads","thread_create"),
    ("POST /threads/search","thread_search"),
    ("GET /threads/","thread_read"),
    ("PATCH /threads/","thread_update"),
    ("DELETE /threads/","thread_delete"),
    ("GET /threads/*/state","state_read"),
    ("POST /threads/*/state","state_update"),
    ("GET /threads/*/history","history_read"),
    ("POST /threads/*/runs","run_create"),
    ("POST /threads/*/runs/stream","run_stream"),
    ("POST /threads/*/runs/wait","run_wait"),
    ("GET /threads/*/runs","run_list"),
    ("GET /threads/*/runs/","run_read"),
    ("POST /threads/*/runs/*/cancel","run_cancel"),
    ("POST /threads/*/runs/*/join","run_join"),
    ("POST /runs","run_create"),
    ("POST /runs/stream","run_stream"),
    ("POST /runs/wait","run_wait"),
    ("GET /runs/","run_read"),
    ("GET /store/items","store_read"),
    ("POST /store/items","store_write"),
    ("DELETE /store/items","store_delete"),
    ("GET /store/namespaces","store_list"),
    ("POST /store/namespaces","store_list"),
    ("GET /crons","cron_list"),
    ("POST /crons","cron_create"),
    ("GET /crons/","cron_read"),
    ("PATCH /crons/","cron_update"),
    ("DELETE /crons/","cron_delete"),
    ("POST /crons/*/trigger","cron_trigger"),
];
pub(crate) fn operation(method:&str,path:&str)->&'static str {
    let normalized=format!("/{}",path.trim_matches('/').to_lowercase());
    let method=method.to_uppercase();
    let exact=format!("{method} {normalized}");
    if let Some((_,v))=OPERATIONS.iter().find(|(k,_)|*k==exact){return v;}
    for (pattern,value) in OPERATIONS {
        let (m,p)=pattern.split_once(' ').unwrap();
        if m!=method {continue;}
        if p.contains('*') {
            let ps:Vec<_>=p.trim_end_matches('/').split('/').collect();
            let ns:Vec<_>=normalized.split('/').collect();
            if (ps.len()==ns.len() || p.ends_with('/')&&ns.len()>=ps.len()) && ps.iter().zip(ns.iter()).all(|(a,b)|*a==*b||*a=="*"&&!b.is_empty()){return value;}
        } else if p.ends_with('/') && normalized.starts_with(p.trim_end_matches('/')){return value;}
    }
    "proxy"
}
async fn selftest(state:&ProxyHttp,loaded:LoadedProxyConfiguration,user:&CompatUser,key:Option<String>,headers:axum::http::HeaderMap)->Result<Response,CompatError>{
    use futures_util::StreamExt;
    let config=&loaded.proxy;
    let assistant=config.assistant_id.as_ref().map(|id|if id.chars().count()>8{format!("{}...",id.chars().take(8).collect::<String>())}else{id.clone()});
    let host=url::Url::parse(&config.upstream_url).ok().map(|url| match url.port(){Some(p)=>format!("{}:{p}",url.host_str().unwrap_or("")),None=>url.host_str().unwrap_or("").into()});
    let mut result=json!({"service":config.service_name,"auth_header_present":headers.contains_key("authorization"),"assistant_id":assistant,"upstream_host":host,"enabled":config.enabled});
    for (path,payload,stream) in [("assistants/search",json!({}),false),("runs/stream",json!({"input":{"messages":[{"role":"user","content":"ping"}]}}),true)] {
        let budgets=state.capacity.resolve(&CapacityRequest{tenant_id:user.tenant_id.clone(),service_id:config.service_id.clone(),request_class:capacity_class(stream,path).into(),upstream_group:Value::Null,provider_id:json!(config.default_provider),is_admin_read:false,service_config:json!({"capacity":loaded.capacity_config}),resource_kind:"request".into()}).map_err(|_|CompatError::new(503,"Capacity configuration unavailable"))?;
        let response=state.proxy.proxy(ProxyRequest{config:config.clone(),user:user.clone(),api_key_id:key.clone(),method:Method::POST,path:path.into(),body:Bytes::from(payload.to_string()),headers:headers.clone(),query:vec![],stream,langgraph_body_prepared:false,effective_model:None,effective_provider:None,capacity_budgets:budgets}).await;
        let section=if stream{"stream"}else{"assistant_list"};
        match response {
            Err(error)=>result[section]=json!({"ok":false,"status_code":error.status,"error":error.detail}),
            Ok(response)=>{
                let status=response.status().as_u16();
                if stream && response.headers().get("content-type").and_then(|v|v.to_str().ok()).is_some_and(|v|v.contains("text/event-stream")) {
                    let start=std::time::Instant::now();
                    let mut bytes=response.into_body().into_data_stream();
                    result[section]=match tokio::time::timeout(std::time::Duration::from_secs(5),bytes.next()).await {
                        Ok(Some(Ok(chunk)))=>json!({"ok":true,"first_chunk_ms":(start.elapsed().as_secs_f64()*100000.0).round()/100.0,"chunk_bytes":chunk.len()}),
                        Ok(None)=>json!({"ok":false,"error":"no chunks"}),
                        Err(_)=>json!({"ok":false,"error":"timeout waiting for first chunk"}),
                        _=>json!({"ok":false,"error":"upstream stream failed"}),
                    };
                    // Dropping the receiver releases native stream work and its lease.
                } else if stream { result[section]=json!({"ok":false,"status_code":status,"error":"not streaming"}); }
                else { let bytes=to_bytes(response.into_body(),usize::MAX).await.unwrap_or_default(); let preview=String::from_utf8_lossy(&bytes[..bytes.len().min(200)]); result[section]=json!({"status_code":status,"ok":status<500,"error":null,"body_preview":if bytes.is_empty(){Value::Null}else{json!(preview)}}); }
            }
        }
    }
    Ok(Json(result).into_response())
}
#[cfg(test)] mod tests {
    use super::*;
    #[test] fn source_operation_order_is_preserved(){assert_eq!(operation("POST","threads/t/runs/stream/"),"run_stream");assert_eq!(operation("GET","threads/t/state"),"thread_read");assert_eq!(operation("GET","assistants/search"),"assistant_read");assert_eq!(operation("DELETE","store/items"),"store_delete");assert_eq!(operation("PUT","other"),"proxy");}
    #[test] fn stream_detection_matches_source(){assert!(wants_stream("text/event-stream","",&[]));assert!(wants_stream("","runs/stream",&[]));assert!(wants_stream("","",&[("stream".into(),"yes".into())]));assert!(!wants_stream("","",&[("stream".into(),"TRUE".into())]));}
    #[test] fn discovery_never_exposes_provider_secrets(){let v=safe_override(&json!({"enabled":true,"api_key":"secret","provider_id":" p ","temperature":true,"failover":{"candidates":[{"provider_id":"p","model_id":"m","api_key":"secret"}],"token":"secret"}}));assert!(!v.to_string().contains("secret"));assert!(v["temperature"].is_null());assert_eq!(v["failover"]["candidate_count"],1);}
}
async fn rate_limit(state:&ProxyHttp,user:&ContentIdentity,key:Option<&ai_platform_auth::api_keys::ApiKeyAuthority>,loaded:&LoadedProxyConfiguration,service:&str,operation:&str)->Result<axum::http::HeaderMap,Response> {
    use crate::{rate_policy_store::{PolicyContext,ServiceRatePolicy},rate_limits::{RateLimitContext,RatePolicy}};
    fn headers(values:std::collections::BTreeMap<String,String>)->axum::http::HeaderMap {let mut out=axum::http::HeaderMap::new();for(k,v)in values{if let(Ok(k),Ok(v))=(axum::http::HeaderName::try_from(k),axum::http::HeaderValue::try_from(v)){out.insert(k,v);}}out}
    let exempt=||headers(std::collections::BTreeMap::from([("X-Gateway-Policy-Exempt".into(),"rate_limit".into())]));
    let Some(limiter)=&state.access.limiter else{return Ok(exempt());};
    let actor=RateLimitContext{ip:crate::migration_content::access::context().client_ip,user_id:(!user.user_id.is_empty()).then(||user.user_id.clone()),user_tier:user.tier.clone(),tenant_id:(!user.tenant_id.is_empty()).then(||user.tenant_id.clone()),assistant_id:Some(service.into()),operation:Some(operation.into())};
    let context=PolicyContext{actor:actor.clone(),service_name:service.into(),user_service_id:String::new(),api_key_hash:key.map_or_else(String::new,|k|k.key_hash.clone()),api_key_ids:key.map_or_else(Vec::new,|k|vec![k.id.to_string(),k.key_id.clone()])};
    let rate=&loaded.rate_config;
    let configured=ServiceRatePolicy{enabled:crate::service_registry::python_truthy(&rate["enabled"]),requests:rate["requests"].as_i64().unwrap_or(100),window:rate["window"].as_i64().unwrap_or(60),service_id:loaded.proxy.service_id.clone()};
    let failed=||CompatError::new(503,"Rate limiter unavailable").into_response();
    let policies=state.rates.resolve(&context,Some(&configured)).await.map_err(|_|failed())?;
    let mut last=None;
    let mut offset=0;
    while offset<policies.len(){let mut end=offset+1;while end<policies.len()&&policies[end].window==policies[offset].window{end+=1;}
        let batch:Vec<RatePolicy>=policies[offset..end].iter().map(|p|p.policy.clone()).collect();
        for result in limiter.check_custom_limits(&batch).await.map_err(|_|failed())? {
            if !result.allowed {state.security.record_decision(&user.tenant_id,&user.user_id,service,"rate_limited").await;let mut response=CompatError::value(429,result.exceeded_response()).into_response();response.headers_mut().extend(headers(result.headers()));return Err(response);}
            last=Some(result);
        }offset=end;
    }
    if let Some(last)=last{return Ok(headers(last.headers()));}
    let policies=state.access.rate_config.policies(&actor,&Default::default()).map_err(|_|failed())?;
    let now=std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_err(|_|failed())?.as_secs_f64();
    let result=limiter.check_dimensions_at(&policies,now).await.map_err(|_|failed())?;
    if !result.allowed{state.security.record_decision(&user.tenant_id,&user.user_id,service,"rate_limited").await;let mut response=CompatError::value(429,result.exceeded_response()).into_response();response.headers_mut().extend(headers(result.headers()));return Err(response);}
    Ok(if result.limit>0{headers(result.headers())}else{exempt()})
}

fn capacity_class(stream:bool,path:&str)->&'static str { let path=path.to_lowercase(); if stream||path.contains("runs/stream"){"stream"}else if path.contains("image"){"image"}else if path.contains("retrieve")||path.contains("knowledge"){"rag"}else if path.contains("tool"){"tool"}else{"sync"} }

fn should_apply_quota(method:&str,operation:&str,path:&str)->bool { matches!(method,"POST"|"PUT"|"PATCH") && (operation.starts_with("run_")||path.to_lowercase().contains("/runs")) }

fn enforce_constraints(service:&str,aliases:&[Value],admin:bool,constraints:&ServiceAccessConstraints)->Result<(),CompatError>{
    use crate::service_access_policy::{service_scope_matches,evaluate_service_access};
    for (_,allowed) in &constraints.allowed_sources {
        if !service_scope_matches(&json!(allowed),aliases){return Err(CompatError::new(403,format!("Permission denied: service {service} not in allowed services")));}
    }
    if !admin && !evaluate_service_access(&constraints.user_policy,aliases).0 {
        return Err(CompatError::new(403,format!("Permission denied: service {service} blocked by user policy")));
    }
    Ok(())
}

#[cfg(test)] mod source_authorization_detail_tests {
    use super::*;
    use crate::service_access_policy::service_access_policy_from_metadata;
    #[test] fn source_user_and_key_denial_details_are_distinct(){
        let mut c=ServiceAccessConstraints{allowed_sources:vec![],user_policy:service_access_policy_from_metadata(&json!({"service_access":{"mode":"allowlist","allowed_services":["flash"]}}))};
        let error=enforce_constraints("agent",&[json!("agent")],false,&c).unwrap_err();
        assert_eq!(error.status,403);assert_eq!(error.detail,"Permission denied: service agent blocked by user policy");
        c.allowed_sources.push(("api_key".into(),vec!["flash".into()]));
        let error=enforce_constraints("agent",&[json!("agent")],true,&c).unwrap_err();
        assert_eq!(error.status,403);assert_eq!(error.detail,"Permission denied: service agent not in allowed services");
    }
    #[test] fn quota_predicate_and_capacity_class_match_source(){
        assert!(!should_apply_quota("GET","run_read","threads/t/runs"));
        assert!(!should_apply_quota("POST","assistant_list","assistants/search"));
        assert!(should_apply_quota("POST","run_stream","runs/stream"));
        assert_eq!(capacity_class(true,"runs/stream"),"stream");assert_eq!(capacity_class(false,"images/generate"),"image");
        assert_eq!(capacity_class(false,"knowledge/retrieve"),"rag");assert_eq!(capacity_class(false,"tools/call"),"tool");
        assert_eq!(capacity_class(false,"assistants/search"),"sync");
    }
}
