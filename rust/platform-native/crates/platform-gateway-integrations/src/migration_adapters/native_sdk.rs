//! Opt-in official Agent SDK process adapters. Gateway remains Rust and owns admission.
use super::*;
use reqwest::header::{HeaderMap, HeaderValue};

pub struct NativeSdkFactory {
    pub session_pool: Option<sqlx::PgPool>,
    pub capabilities:Arc<std::sync::OnceLock<Arc<dyn ai_platform_gateway_foundation::sdk_capabilities::SdkCapabilityAuthority>>>,
    pub read_grants: Arc<ai_platform_gateway_foundation::sdk_read_grants::SdkReadGrants>,
    pub encryption_key: Arc<std::sync::OnceLock<Arc<str>>>,
    pub framework: &'static str,
    pub(super) worker_config: Option<super::sdk_session_cleanup::SdkAgentWorkerConfig>,
}
impl AdapterFactory for NativeSdkFactory {
    fn create(&self, service: ServiceHandle) -> Result<Arc<dyn ProtocolAdapter>, RegistryError> {
        let bad = || RegistryError::AdapterConstructionFailed;
        if !matches!(self.framework, "openai_agents" | "claude_agent_sdk") { return Err(bad()); }
        let session_pool = self.session_pool.clone().ok_or_else(bad)?;
        let worker_config = self.worker_config.as_ref().ok_or_else(bad)?;
        let snapshot = service.snapshot();
        if snapshot.max_retries != 0 || !snapshot.session_enabled { return Err(bad()); }
        let mut agent = snapshot.metadata.get("sdk_agent").cloned().ok_or_else(bad)?;
        let object = agent.as_object().ok_or_else(bad)?;
        if object.len() != 3 || !object.keys().all(|v| matches!(v.as_str(), "model" | "instructions" | "max_turns"))
            || !agent["model"].as_str().is_some_and(|v| !v.trim().is_empty() && v.len() <= 160)
            || !agent["instructions"].as_str().is_some_and(|v| v.len() <= 32768)
            || !agent["max_turns"].as_u64().is_some_and(|v| (1..=32).contains(&v)) { return Err(bad()); }
        if agent["instructions"].as_str().is_some_and(|v|v.trim().is_empty()) {
            agent["instructions"] = json!("You are a helpful assistant.");
        }
        let key = ai_platform_gateway_foundation::sdk_agent_config::decrypt_key(&snapshot,
            self.encryption_key.get().map(|v|v.as_ref()).unwrap_or("")).map_err(|_|bad())?;
        // Captured secret headers are sent only to the private operator origin; no redirect/retry.
        let mut headers = HeaderMap::new();
        headers.insert("authorization", worker_config.authorization());
        let mut provider_key = HeaderValue::from_str(&key).map_err(|_| bad())?;
        provider_key.set_sensitive(true);
        headers.insert("x-sdk-provider-key", provider_key);
        let timeout=if ai_platform_gateway_foundation::sdk_capabilities::selected(&snapshot).map_err(|_|bad())?.is_empty(){310}else{1830};
        let mut private = snapshot;
        private.connector_config = json!({"base_url":worker_config.base_url(),"max_connections":16,"stream_timeout":timeout});
        Ok(Arc::new(NativeSdkAdapter { http: HttpTransport::new(ServiceHandle::new(private))?,
            framework:self.framework, agent, headers, service, capabilities:self.capabilities.clone(), read_grants:self.read_grants.clone(), session_pool }))
    }
}
struct NativeSdkAdapter { capabilities:Arc<std::sync::OnceLock<Arc<dyn ai_platform_gateway_foundation::sdk_capabilities::SdkCapabilityAuthority>>>, session_pool:sqlx::PgPool, service:ServiceHandle, read_grants:Arc<ai_platform_gateway_foundation::sdk_read_grants::SdkReadGrants>, http: HttpTransport, framework: &'static str, agent: Value, headers: HeaderMap }
impl NativeSdkAdapter {
    async fn admit(&self, r: &UnifiedRequest) -> Result<(), AdapterError> {
        sdk_session_cleanup::admit(&self.session_pool, &sdk_session_cleanup::SdkScope {
            tenant_id:r.tenant_id.clone(),user_id:r.user_id.clone(),service_id:r.service_id.clone(),
            session_id:r.session_id.clone().ok_or_else(||validation("sdk_authenticated_session_required"))?,
            framework:self.framework.into(),
        }).await.map_err(|_| validation("sdk_session_unavailable"))
    }
    async fn prepare(&self,r:&UnifiedRequest,stream:bool)->Result<(Value,Option<ai_platform_gateway_foundation::sdk_capabilities::SdkCapabilityLease>),AdapterError>{
        let service=self.service.snapshot();
        let selected=ai_platform_gateway_foundation::sdk_capabilities::selected(&service).map_err(validation)?;
        if !selected.is_empty() && !stream{return Err(validation("sdk_capabilities_require_stream"));}
        self.admit(r).await?;
        let mut body=self.payload(r,stream)?;
        // All SDK executions share the durable run authority. An empty tool list
        // grants lifecycle observation only, never catalog expansion or dispatch.
        let grant=body["read_bridge"]["grant"].as_str().ok_or_else(||validation("sdk_authenticated_grant_required"))?;
        let port=self.capabilities.get().ok_or_else(||validation("sdk_capability_authority_unavailable"))?;
        let capability=Some(port.admit(r,&service,grant).await.map_err(validation)?);
        if let Some(capability)=&capability{body["capability_bridge"]=capability.bridge.clone();}
        if body["read_bridge"]["dataset_ids"].as_array().is_some_and(|v|v.is_empty()){body.as_object_mut().unwrap().remove("read_bridge");}
        Ok((body,capability))
    }
    fn payload(&self, r: &UnifiedRequest, stream: bool) -> Result<Value, AdapterError> {
        if [&r.tenant_id, &r.user_id, &r.service_id, &r.request_id].iter().any(|s| s.trim().is_empty())
            || r.session_id.as_deref().is_none_or(|s| s.trim().is_empty()) { return Err(validation("sdk_authenticated_session_required")); }
        if r.parameters.as_ref().is_some_and(|v| !v.is_empty()) { return Err(validation("sdk_parameters_unsupported")); }
        let text = r.inputs.iter().map(|v| match (&v.content_type, &v.data) {
            (ContentType::Text, Some(ContentData::Text(s))) => Ok(s.as_str()),
            _ => Err(validation("sdk_text_inputs_required")),
        }).collect::<Result<Vec<_>, _>>()?.join("\n");
        if text.trim().is_empty() || text.len() > 262144 { return Err(validation("sdk_input_invalid")); }
        let bridge=self.read_grants.activate(r,&self.service.snapshot()).map_err(validation)?;
        let mut body=json!({"protocol":"ai-platform/sdk-agent/v1","framework":self.framework,"service_id":r.service_id,
            "tenant_id":r.tenant_id,"user_id":r.user_id,"request_id":r.request_id,"session_id":r.session_id,
            "agent":self.agent,"input":text,"stream":stream});
        if let Some(bridge)=bridge {body["read_bridge"]=bridge;}
        Ok(body)
    }
}
fn metadata(framework: &str, knowledge:bool, capability:Option<&ai_platform_gateway_foundation::sdk_capabilities::SdkCapabilityLease>) -> Map<String, Value> {
    let mut meta=json!({"sdk_framework":framework,"sdk_tools_profile":if knowledge {"scoped_knowledge_read_v1"} else {"pure_readonly_v1"},"sdk_session_continuity":"worker_durable"}).as_object().unwrap().clone();
    if let Some(lease)=capability {
        meta.insert("assistant_run_id".into(),lease.bridge["run_id"].clone());
        if lease.bridge["tools"].as_array().is_some_and(|tools|!tools.is_empty()){meta.insert("sdk_tools_profile".into(),json!("governed_capabilities_v1"));}
    }
    meta
}
fn sdk_usage(value: &Value, framework: &str, model: &str, capability: Option<&ai_platform_gateway_foundation::sdk_capabilities::SdkCapabilityLease>) -> Result<Map<String, Value>, AdapterError> {
    let mut usage = value.as_object().cloned().unwrap_or_default();
    if !usage.get("sdk_usage").is_some_and(Value::is_object) {
        usage.insert("sdk_usage".into(), json!({"state":"unavailable", "cost_state":"unavailable"}));
    }
    let audit = usage.get_mut("sdk_usage").and_then(Value::as_object_mut).expect("observation object");
    if !matches!(audit.get("state").and_then(Value::as_str), Some("reported" | "unavailable")) {
        audit.insert("state".into(), json!("unavailable"));
    }
    audit.insert("framework".into(), json!(framework));
    audit.insert("model".into(), json!(model));
    audit.remove("assistant_run_id");
    if let Some(lease) = capability {
        audit.insert("assistant_run_id".into(), lease.bridge["run_id"].clone());
    }
    Ok(usage)
}
impl ProtocolAdapter for NativeSdkAdapter {
    fn invoke<'a>(&'a self, request: &'a UnifiedRequest) -> AdapterFuture<'a, UnifiedResponse> {
        Box::pin(async move {
            let (payload,mut capability) = self.prepare(request, false).await?;
            let tools=self.service.snapshot().metadata.get("sdk_tools").cloned().unwrap_or(Value::Null);
            let mut audit=sdk_usage_audit::UsageAudit::admit(&self.session_pool,request,self.framework,&self.agent,&tools,false,capability.as_ref().map(|v|&v.bridge["run_id"])).await?;
            let outcome=async {
            let result = self.http.post_json("v1/runs", &payload, self.headers.clone()).await?.json().await?;
            if result["event"] != "completed" { return Err(validation("sdk_run_incomplete")); }
            let text = result["text"].as_str().ok_or_else(||validation("sdk_terminal_invalid"))?;
            let mut usage = sdk_usage(&result["usage"], self.framework, self.agent["model"].as_str().unwrap_or(""), capability.as_ref())?;
            audit.complete(&mut usage).await;
            let mut response = UnifiedResponse::new(&request.request_id,"success",vec![content(ContentType::Text,json!(text))]);
            if let Some(capability)=capability.as_mut(){capability.complete();}
            response.session_id = request.session_id.clone(); response.usage = Some(usage);
            response.metadata = Some(metadata(self.framework, !ai_platform_gateway_foundation::sdk_read_grants::datasets(&self.service.snapshot()).map_err(validation)?.is_empty(), capability.as_ref())); Ok(response)
            }.await;
            if outcome.is_err(){audit.fail();if let Some(lease)=capability.as_mut(){lease.fail();}}
            outcome
        })
    }
    fn stream(&self, request: UnifiedRequest) -> AdapterFuture<'_, Box<dyn DispatchProducer<StreamChunk>>> {
        Box::pin(async move {
            let (payload,capability) = self.prepare(&request,true).await?;
            let tools=self.service.snapshot().metadata.get("sdk_tools").cloned().unwrap_or(Value::Null);
            let mut audit=sdk_usage_audit::UsageAudit::admit(&self.session_pool,&request,self.framework,&self.agent,&tools,true,capability.as_ref().map(|v|&v.bridge["run_id"])).await?;
            let response = match self.http.post_json("v1/runs", &payload, self.headers.clone()).await {
                Ok(response)=>response,Err(error)=>{audit.fail();return Err(error);}
            };
            let lines=match Lines::new(response){Ok(lines)=>lines,Err(error)=>{audit.fail();return Err(error);}};
            Ok(Box::new(NativeSdkStream { audit, capability, lines, request_id:request.request_id,
                model:self.agent["model"].as_str().unwrap_or("").to_owned(), index:0, done:false, size:0, data:String::new(), text:String::new(), framework:self.framework, knowledge:!ai_platform_gateway_foundation::sdk_read_grants::datasets(&self.service.snapshot()).map_err(validation)?.is_empty() }) as Box<dyn DispatchProducer<StreamChunk>>)
        })
    }
    fn health_check(&self) -> AdapterFuture<'_, bool> {
        Box::pin(async move {
            let Ok(url) = self.http.url("health/ready") else { return Ok(false); };
            let mut headers=self.headers.clone(); headers.remove("x-sdk-provider-key");
            let Ok(response) = self.http.send(self.http.client.get(url).headers(headers)).await else { return Ok(false); };
            Ok(response.json().await.is_ok_and(|v|v["ready"] == true && v["protocol"] == "ai-platform/sdk-agent/v1"))
        })
    }
}
struct NativeSdkStream { audit:sdk_usage_audit::UsageAudit, model:String, capability:Option<ai_platform_gateway_foundation::sdk_capabilities::SdkCapabilityLease>, lines:Lines, request_id:String, index:i64, done:bool, size:usize, data:String, text:String, framework:&'static str, knowledge:bool }
impl NativeSdkStream {
    async fn read(&mut self) -> Result<Option<StreamChunk>, AdapterError> {
        if self.done { return Ok(None); }
        // Admission and worker response headers already succeeded. Publish the
        // genuine run identity once, without waiting for the first provider byte.
        if self.index==0 && self.capability.is_some() {
            let mut part=chunk(&self.request_id,self.index,json!(""),StreamEventType::TextDelta,false);
            part.metadata=Some(metadata(self.framework,self.knowledge,self.capability.as_ref()));
            self.index+=1;return Ok(Some(part));
        }
        loop {
            let Some(line) = self.lines.next().await? else { return Err(validation("sdk_stream_incomplete")); };
            self.size += line.len();
            if self.size > 4*1024*1024 { return Err(validation("sdk_stream_too_large")); }
            if let Some(data) = line.strip_prefix("data:") { self.data.push_str(data.trim_start()); }
            if !line.is_empty() || self.data.is_empty() { continue; }
            let value:Value = serde_json::from_str(&std::mem::take(&mut self.data)).map_err(|_|validation("sdk_event_invalid"))?;
            let event = value["event"].as_str().ok_or_else(||validation("sdk_event_invalid"))?;
            if event == "error" { return Err(validation("sdk_execution_failed")); }
            if matches!(event,"approval_required"|"approval_result"|"tool_result") {
                if self.capability.as_ref().is_none_or(|capability|value["run_id"]!=capability.bridge["run_id"] || capability.bridge["tools"].as_array().is_none_or(|tools|tools.is_empty()))
                    || !value["tool_call_id"].is_string(){return Err(validation("sdk_event_invalid"));}
                let kind=match event{"approval_required"=>StreamEventType::ApprovalRequired,"approval_result"=>StreamEventType::ApprovalResult,_=>StreamEventType::ToolResult};
                let mut part=chunk(&self.request_id,self.index,json!(""),kind,false);
                let mut meta=metadata(self.framework,self.knowledge,self.capability.as_ref());
                meta.insert("sdk_lifecycle".into(),value);part.metadata=Some(meta);self.index+=1;return Ok(Some(part));
            }

            let text = if event == "completed" { value["streamed_text"].as_str().or_else(||value["text"].as_str()) } else {value["text"].as_str()}.ok_or_else(||validation("sdk_event_invalid"))?;
            let mut terminal_usage=None;
            let (delta, final_chunk) = match event {
                "delta" => { self.text.push_str(text); (text.to_owned(),false) },
                "completed" => {
                    if !text.starts_with(&self.text) { return Err(validation("sdk_terminal_invalid")); }
                    let mut usage=sdk_usage(&value["usage"], self.framework, &self.model, self.capability.as_ref())?;
                    self.audit.complete(&mut usage).await;
                    terminal_usage=Some(usage);
                    if let Some(capability)=self.capability.as_mut(){capability.complete();}
                    self.done=true; self.lines.close(); (text[self.text.len()..].to_owned(),true)
                },
                "error" => return Err(validation("sdk_execution_failed")),
                _ => return Err(validation("sdk_event_unsupported")),
            };
            let mut chunk = chunk(&self.request_id,self.index,json!(delta),if final_chunk {StreamEventType::Final} else {StreamEventType::TextDelta},final_chunk);
            let mut meta=metadata(self.framework,self.knowledge,self.capability.as_ref());
            if let Some(usage)=terminal_usage { meta.insert("usage".into(), Value::Object(usage)); }
            chunk.metadata=Some(meta); self.index+=1; return Ok(Some(chunk));
        }
    }
}
impl DispatchProducer<StreamChunk> for NativeSdkStream {
    fn next(&mut self)->DispatchFuture<'_,Option<StreamChunk>> { Box::pin(async move {
        let result=self.read().await;
        if result.is_err(){self.audit.fail();if let Some(lease)=self.capability.as_mut(){lease.fail();}self.capability.take();self.done=true;self.lines.close();}
        result.map_err(dispatch)
    }) }
    fn close(mut self:Box<Self>)->DispatchFuture<'static,()> { Box::pin(async move {self.audit.cancel();self.capability.take();self.lines.close();self.done=true;Ok(())}) }
}

#[cfg(test)]
#[path = "native_sdk_tests.rs"]
mod tests;
