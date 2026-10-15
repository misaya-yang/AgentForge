//! Live candidate execution exercises the same public V2 Thread/Turn/Item boundary.
use super::{
    common::{CompatError, SseDecoder, object, text},
    eval_clients::{EvalJwtSigner, bounded_json},
    eval_observation::{self, RuntimeTraceInput},
    eval_queue::EvalOutbox,
};
use axum::http::{HeaderMap, HeaderValue};
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::Arc, time::Duration};
#[derive(Clone, Debug)]
pub struct EvalCandidateResult {
    pub trace_id: String,
    pub output: String,
    pub usage: Value,
    pub fingerprint: Value,
    pub error: Option<String>,
    pub trace_payload: Option<Value>,
}
pub struct EvalCandidateClient {
    base: reqwest::Url,
    http: reqwest::Client,
    pub allow_service_identity: bool,
    token: Option<String>,
    api_key: Option<String>,
    signer: Arc<EvalJwtSigner>,
    pub outbox: EvalOutbox,
}
fn error(code: &str) -> CompatError {
    CompatError::new(503, code)
}
impl EvalCandidateClient {
    pub fn new(
        base: &str,
        token: Option<String>,
        api_key: Option<String>,
        signer: Arc<EvalJwtSigner>,
        allow_service_identity: bool,
        outbox: EvalOutbox,
    ) -> Result<Self, CompatError> {
        let base =
            reqwest::Url::parse(base).map_err(|_| error("AGENT_EVAL_EVENTS_ORIGIN_INVALID"))?;
        if !matches!(base.scheme(), "http" | "https")
            || base.host_str().is_none()
            || !base.username().is_empty()
            || base.password().is_some()
        {
            return Err(error("AGENT_EVAL_EVENTS_ORIGIN_INVALID"));
        }
        Ok(Self {
            base,
            http: reqwest::Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(Duration::from_secs(5))
                .timeout(Duration::from_secs(300))
                .build()?,
            allow_service_identity,
            token,
            api_key,
            signer,
            outbox,
        })
    }
    fn endpoint(&self, path: &str) -> Result<reqwest::Url, CompatError> {
        self.base
            .join(path)
            .map_err(|_| error("AGENT_EVAL_EVENTS_ORIGIN_INVALID"))
    }
    fn headers(
        &self,
        tenant: &str,
        delegation: Option<&Value>,
        force: bool,
    ) -> Result<HeaderMap, CompatError> {
        let mut headers = HeaderMap::new();
        headers.insert("content-type", HeaderValue::from_static("application/json"));
        headers.insert("accept", HeaderValue::from_static("application/json"));
        let (name, value) = if force {
            if !self.allow_service_identity || delegation.is_none() {
                return Err(error("AGENT_EVAL_DELEGATION_REQUIRED"));
            }
            (
                "authorization",
                format!("Bearer {}", self.signer.token(tenant, delegation)?),
            )
        } else if let Some(token) = self.token.as_ref().filter(|v| !v.is_empty()) {
            ("authorization", format!("Bearer {token}"))
        } else if let Some(key) = self.api_key.as_ref().filter(|v| !v.is_empty()) {
            ("x-api-key", key.clone())
        } else if self.allow_service_identity {
            if tenant.is_empty() || tenant.trim() != tenant || tenant == "public" {
                return Err(error("AGENT_EVAL_IDENTITY_INVALID"));
            }
            if delegation.is_none() {
                return Err(error("AGENT_EVAL_DELEGATION_REQUIRED"));
            }
            (
                "authorization",
                format!("Bearer {}", self.signer.token(tenant, delegation)?),
            )
        } else {
            return Err(error("AGENT_EVAL_AUTH_REQUIRED"));
        };
        let mut value =
            HeaderValue::from_str(&value).map_err(|_| error("AGENT_EVAL_IDENTITY_INVALID"))?;
        value.set_sensitive(true);
        headers.insert(name, value);
        Ok(headers)
    }
    async fn identity(&self, tenant: &str, headers: &HeaderMap) -> Result<Value, CompatError> {
        if tenant.is_empty() || tenant.trim() != tenant || tenant == "public" {
            return Err(error("AGENT_EVAL_IDENTITY_INVALID"));
        }
        let response = self
            .http
            .get(self.endpoint("/api/v1/auth/me")?)
            .headers(headers.clone())
            .send()
            .await
            .map_err(|_| error("AGENT_EVAL_IDENTITY_UNAVAILABLE"))?;
        if response.status().as_u16() != 200 {
            return Err(error("AGENT_EVAL_IDENTITY_UNAVAILABLE"));
        }
        let identity = bounded_json(response, 1024 * 1024)
            .await
            .map_err(|_| error("AGENT_EVAL_IDENTITY_INVALID"))?;
        let actual = text(&identity, "tenant_id");
        if text(&identity, "user_id").trim().is_empty()
            || actual.is_empty()
            || actual.trim() != actual
            || actual == "public"
        {
            return Err(error("AGENT_EVAL_IDENTITY_INVALID"));
        }
        if actual != tenant {
            return Err(error("AGENT_EVAL_TENANT_MISMATCH"));
        }
        Ok(identity)
    }
    pub async fn persist_case(
        &self,
        job: &Value,
        case: &str,
        status: &str,
        trace: Option<&str>,
        metrics: Option<&Value>,
        error_message: Option<&str>,
        handle: Option<&Value>,
        dispatch: Option<&str>,
    ) -> Result<(), CompatError> {
        let mut tx = self.outbox.pool.begin().await?;
        self.outbox.fence(&mut tx, job).await?;
        sqlx::query("UPDATE eval_experiment_run_cases SET status=$3,candidate_trace_id=COALESCE($4::text::uuid,candidate_trace_id),observed_metrics=COALESCE($5::jsonb,observed_metrics),error_message=$6,runtime_handle=COALESCE($7::jsonb,runtime_handle),dispatch_state=COALESCE($8,dispatch_state),updated_at=NOW() WHERE tenant_id=$1 AND run_case_id=$2::text::uuid").bind(text(job,"tenant_id")).bind(case).bind(status).bind(trace).bind(metrics).bind(error_message).bind(handle).bind(dispatch).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(())
    }
    pub async fn reconcile_handle(
        &self,
        tenant: &str,
        case: &str,
        handle: &Value,
    ) -> Result<Option<Value>, CompatError> {
        let rows=sqlx::query_scalar::<_,Value>("SELECT jsonb_build_object('run_id',run_id,'runtime_thread_id',runtime_thread_id) FROM assistant_runtime_snapshots WHERE tenant_id=$1 AND session_id=$2 AND user_id=$3 AND runtime_thread_id=$4::text::uuid ORDER BY created_at LIMIT 2").bind(tenant).bind(case).bind(text(handle,"user_id")).bind(text(handle,"thread_id")).fetch_all(&self.outbox.pool).await?;
        if rows.len() != 1 {
            return Ok(None);
        }
        let mut recovered = handle.clone();
        recovered["turn_id"] = rows[0]["run_id"].clone();
        recovered["events_url"] = json!(format!(
            "/api/v2/agent/threads/{}/events?after_sequence=0&turn_id={}",
            text(&rows[0], "runtime_thread_id"),
            text(&rows[0], "run_id")
        ));
        Ok(Some(recovered))
    }
    pub async fn run(
        &self,
        job: &Value,
        run_case: &Value,
        config: &Value,
    ) -> Result<EvalCandidateResult, CompatError> {
        let tenant = text(job, "tenant_id");
        let case = text(run_case, "run_case_id");
        let message = text(&run_case["input"], "message").trim();
        let version = config["candidate_type"] == "agent_version";
        if config
            .get("system_prompt_override")
            .is_some_and(|v| !v.is_null() && v.as_str() != Some(""))
        {
            return Err(error(
                "V2 Agent Runtime does not support eval system_prompt_override; refusing to evaluate a different prompt",
            ));
        }
        if version
            && [
                "agent_id",
                "agent_version_id",
                "agent_spec_hash",
                "agent_runtime_snapshot_hash",
            ]
            .iter()
            .any(|key| text(config, key).is_empty())
        {
            return Err(error("AGENT_EVAL_VERSION_IDENTITY_INCOMPLETE"));
        }
        let delegation = if version
            || (self.allow_service_identity
                && self.token.as_ref().is_none_or(String::is_empty)
                && self.api_key.as_ref().is_none_or(String::is_empty))
        {
            Some(self.outbox.resolve_actor(job).await?)
        } else {
            None
        };
        let headers = self.headers(tenant, delegation.as_ref(), version)?;
        let identity = self.identity(tenant, &headers).await?;
        if delegation
            .as_ref()
            .is_some_and(|d| d["subject"] != identity["user_id"])
        {
            return Err(error("AGENT_EVAL_DELEGATION_SUBJECT_MISMATCH"));
        }
        let mut resume = run_case
            .get("runtime_handle")
            .filter(|v| v.is_object() && !object(v).is_empty())
            .cloned();
        let dispatch = text(run_case, "dispatch_state");
        if !matches!(dispatch, "" | "not_started")
            && resume
                .as_ref()
                .is_none_or(|v| text(v, "turn_id").is_empty())
        {
            let recovered = if let Some(handle) = &resume {
                if !text(handle, "thread_id").is_empty() {
                    self.reconcile_handle(tenant, case, handle).await?
                } else {
                    None
                }
            } else {
                None
            };
            resume = Some(
                recovered.ok_or_else(|| error("AGENT_EVAL_DISPATCH_RECONCILIATION_REQUIRED"))?,
            );
        }
        let model = text(config, "model_id").trim();
        let model = if matches!(model, "" | "current") {
            None
        } else {
            Some(model)
        };
        let mut thread_body = json!({"session_id":case,"expected_tenant_id":tenant});
        if version {
            thread_body["agent_id"] = config["agent_id"].clone();
            thread_body["agent_version_id"] = config["agent_version_id"].clone();
        } else if let Some(model) = model {
            thread_body["model_id"] = json!(model);
        }
        let started = chrono::Utc::now();
        let thread_response = if let Some(handle) = &resume {
            if handle["tenant_id"] != tenant
                || handle["user_id"] != identity["user_id"]
                || handle["run_case_id"] != case
            {
                return Err(error("AGENT_EVAL_HANDLE_IDENTITY_MISMATCH"));
            }
            let thread = uuid::Uuid::parse_str(text(handle, "thread_id"))
                .map_err(|_| error("AGENT_EVAL_RUNTIME_HANDLE_INVALID"))?;
            self.http
                .get(self.endpoint(&format!("/api/v2/agent/threads/{thread}"))?)
                .headers(headers.clone())
                .send()
                .await?
        } else {
            self.http
                .post(self.endpoint("/api/v2/agent/threads")?)
                .headers(headers.clone())
                .json(&thread_body)
                .send()
                .await?
        };
        if thread_response.status().as_u16() >= 400 {
            return Err(error(&format!(
                "Agent Runtime thread create failed with HTTP {}",
                thread_response.status().as_u16()
            )));
        }
        let thread_payload = bounded_json(thread_response, 4 * 1024 * 1024).await?;
        let thread = &thread_payload["thread"];
        if thread["runtime"]["owner"] != "agent_runtime" {
            return Err(error("Agent Eval candidate is not owned by agent_runtime"));
        }
        let mut fingerprint = json!({});
        if version {
            for (key, expected) in [
                ("agent_id", &config["agent_id"]),
                ("agent_version_id", &config["agent_version_id"]),
                ("agent_spec_hash", &config["agent_spec_hash"]),
                (
                    "runtime_snapshot_hash",
                    &config["agent_runtime_snapshot_hash"],
                ),
            ] {
                if thread["agent_version_target"][key] != *expected {
                    return Err(error("AGENT_EVAL_VERSION_PIN_MISMATCH"));
                }
            }
            fingerprint = json!({"candidate_type":"agent_version","agent_id":config["agent_id"],"agent_version_id":config["agent_version_id"],"agent_spec_hash":config["agent_spec_hash"],"agent_runtime_snapshot_hash":config["agent_runtime_snapshot_hash"]});
        }
        let thread_id = if text(thread, "thread_id").is_empty() {
            text(thread, "id")
        } else {
            text(thread, "thread_id")
        };
        uuid::Uuid::parse_str(thread_id)
            .map_err(|_| error("Agent Runtime thread response has no thread_id"))?;
        let mut turn_body = json!({"message":message});
        if !version {
            turn_body["model_id"] = json!(model);
            for key in [
                "provider_id",
                "reasoning_option",
                "thinking_level",
                "temperature",
                "max_tokens",
            ] {
                if let Some(value) = config.get(key).filter(|v| !v.is_null()) {
                    turn_body[key] = value.clone();
                }
            }
            if let Some(reference) = config.get("model_ref").filter(|v| !v.is_null()) {
                turn_body["expected_model_ref"] = reference.clone();
            }
            turn_body["kb_dataset_ids"] = config
                .get("kb_dataset_ids")
                .filter(|v| !v.is_null())
                .cloned()
                .unwrap_or(json!([]));
            turn_body["kb_mode"] = json!(if text(config, "kb_mode").is_empty() {
                "off"
            } else {
                text(config, "kb_mode")
            });
            turn_body["kb_top_k"] =
                json!(config["kb_top_k"].as_i64().filter(|v| *v != 0).unwrap_or(5));
            turn_body["web_search_enabled"] = json!(config["web_search_enabled"] == true);
            turn_body["web_search_max_results"] = json!(
                config["web_search_max_results"]
                    .as_i64()
                    .filter(|v| *v != 0)
                    .unwrap_or(5)
            );
            turn_body
                .as_object_mut()
                .unwrap()
                .retain(|_, v| !v.is_null());
        }
        let mut handle = json!({"tenant_id":tenant,"user_id":identity["user_id"],"run_case_id":case,"thread_id":thread_id});
        if let Some(delegation) = &delegation {
            handle["delegation"] = delegation.clone();
        }
        let (turn, events) = if let Some(resume) = &resume {
            (
                text(resume, "turn_id").to_owned(),
                text(resume, "events_url").to_owned(),
            )
        } else {
            self.persist_case(
                job,
                case,
                "running",
                None,
                None,
                None,
                Some(&handle),
                Some("dispatching"),
            )
            .await?;
            let response = self
                .http
                .post(self.endpoint(&format!("/api/v2/agent/threads/{thread_id}/turns"))?)
                .headers(headers.clone())
                .json(&turn_body)
                .send()
                .await?;
            if !response.status().is_success() {
                return Err(error(&format!(
                    "Agent Runtime turn create failed with HTTP {}",
                    response.status().as_u16()
                )));
            }
            let response = bounded_json(response, 4 * 1024 * 1024).await?;
            (
                text(&response["turn"], "id").to_owned(),
                text(&response["turn"], "events_url").to_owned(),
            )
        };
        if turn.is_empty() || events.is_empty() {
            return Err(error("AGENT_EVAL_RUNTIME_HANDLE_INVALID"));
        }
        let events_url = self.endpoint(&events)?;
        if events_url.origin() != self.base.origin()
            || events_url.path() != format!("/api/v2/agent/threads/{thread_id}/events")
        {
            return Err(error("AGENT_EVAL_EVENTS_ORIGIN_INVALID"));
        }
        let events_path = format!(
            "{}{}",
            events_url.path(),
            events_url
                .query()
                .map(|q| format!("?{q}"))
                .unwrap_or_default()
        );
        handle["turn_id"] = json!(turn);
        handle["events_url"] = json!(events_path);
        self.persist_case(
            job,
            case,
            "running",
            None,
            None,
            None,
            Some(&handle),
            Some("accepted"),
        )
        .await?;
        let mut response = self
            .http
            .get(events_url)
            .headers(headers)
            .header("accept", "text/event-stream")
            .send()
            .await?;
        if response.status().as_u16() >= 400 {
            return Err(error(&format!(
                "Agent Runtime event stream failed with HTTP {}",
                response.status().as_u16()
            )));
        }
        let (mut decoder, mut observed, mut counts, mut terminals) = (
            SseDecoder::default(),
            vec![],
            BTreeMap::<String, i64>::new(),
            vec![],
        );
        let (mut trace, mut output, mut usage, mut terminal_error, mut terminal_status, mut first) = (
            String::new(),
            String::new(),
            json!({}),
            None,
            "failed",
            None,
        );
        while let Some(chunk) = response.chunk().await? {
            for (_, frame) in decoder.push(&chunk) {
                let Ok(envelope) = serde_json::from_str::<Value>(&frame) else {
                    continue;
                };
                if envelope.is_object() {
                    observed.push(envelope.clone());
                    if envelope["sequence"].as_i64().is_some() {
                        handle["last_sequence"] = envelope["sequence"].clone();
                        if observed.len() % 50 == 0 {
                            self.persist_case(
                                job,
                                case,
                                "running",
                                None,
                                None,
                                None,
                                Some(&handle),
                                Some("accepted"),
                            )
                            .await?;
                        }
                    }
                }
                let raw = &envelope["event"]["payload"];
                if !raw.is_object() {
                    continue;
                }
                let kind = text(raw, "event_type");
                let data = &raw["data"];
                if !kind.is_empty() {
                    *counts.entry(kind.into()).or_default() += 1;
                }
                match kind {
                    "run_started" if data.is_object() => {
                        trace = if text(data, "run_id").is_empty() {
                            if trace.is_empty() {
                                turn.clone()
                            } else {
                                trace
                            }
                        } else {
                            text(data, "run_id").into()
                        };
                        if let Some(revision) = data.get("kernel_revision") {
                            fingerprint["runtime_revision"] = revision.clone();
                        }
                    }
                    "context_budget" if data.is_object() => {
                        fingerprint.as_object_mut().unwrap().extend(object(
                            &eval_observation::candidate_fingerprint_from_context(data),
                        ));
                    }
                    "text_delta" => {
                        first.get_or_insert_with(chrono::Utc::now);
                        output.push_str(data.as_str().unwrap_or_else(|| {
                            if text(data, "delta").is_empty() {
                                text(data, "content")
                            } else {
                                text(data, "delta")
                            }
                        }));
                    }
                    "usage" if data.is_object() => usage = data.clone(),
                    "run_finished" | "run_error" | "cancelled" => {
                        terminals.push(kind.to_owned());
                        let status = text(data, "status").to_lowercase();
                        terminal_status = if kind == "run_finished"
                            && matches!(status.as_str(), "" | "completed" | "succeeded")
                        {
                            "succeeded"
                        } else if kind == "cancelled" || status == "cancelled" {
                            "cancelled"
                        } else {
                            "failed"
                        };
                        if data["usage"].is_object() {
                            usage = data["usage"].clone();
                        }
                    }
                    _ => {}
                }
                if matches!(kind, "error" | "run_error" | "cancelled") {
                    let value = if data.is_object() {
                        if text(data, "message").is_empty() {
                            text(data, "error")
                        } else {
                            text(data, "message")
                        }
                    } else {
                        data.as_str().unwrap_or("")
                    };
                    terminal_error = Some(value.to_owned());
                }
            }
        }
        if terminals.len() != 1 {
            return Err(error(&format!(
                "Agent Runtime event stream must contain exactly one terminal event (run_finished, run_error, or cancelled); observed {:?}",
                terminals
            )));
        }
        let failures = eval_observation::validate_runtime_observation(thread, &observed);
        if !failures.is_empty() {
            return Err(error(&format!(
                "Agent Runtime observation contract failed: {}",
                failures.join("; ")
            )));
        }
        if trace.is_empty() {
            return Err(error(
                terminal_error
                    .as_deref()
                    .filter(|s| !s.is_empty())
                    .unwrap_or("Agent Runtime event stream returned no run_id"),
            ));
        }
        let snapshot = json!({"agent_id":if version{config["agent_id"].clone()}else{Value::Null},"agent_version_id":if version{config["agent_version_id"].clone()}else{Value::Null},"model":{"id":model.map(|v|json!(v)).unwrap_or(fingerprint["model_id"].clone()),"provider":fingerprint["provider"]},"publication":{"channel":if version{"preview"}else{"builtin"}}});
        let trace_payload = eval_observation::build_assistant_runtime_trace(RuntimeTraceInput {
            run: &trace,
            request: case,
            tenant,
            user: text(&identity, "user_id"),
            session: case,
            message,
            snapshot: &snapshot,
            status: terminal_status,
            started,
            ended: chrono::Utc::now(),
            first_token_ms: first.map(|v| (v - started).num_milliseconds()).unwrap_or(0),
            output: &output,
            event_counts: &json!(counts),
            usage: &usage,
            error: terminal_error
                .as_ref()
                .filter(|v| !v.is_empty())
                .map(|_| "runtime_error"),
        })
        .ok();
        Ok(EvalCandidateResult {
            trace_id: trace,
            output,
            usage,
            fingerprint,
            error: terminal_error.filter(|v| !v.is_empty()),
            trace_payload,
        })
    }
    pub async fn runtime_evidence(
        &self,
        tenant: &str,
        case: &str,
        run: &str,
    ) -> Result<Value, CompatError> {
        let mut result=sqlx::query_scalar::<_,Value>("SELECT jsonb_build_object('tenant_id',s.tenant_id,'session_id',s.session_id,'run_id',s.run_id,'provider_id',l.provider_id,'model_id',l.model_id,'capability_revision',l.capability_revision,'runtime_revision',s.snapshot->>'kernel_revision','pricing_snapshot',s.snapshot->'pricing'->'snapshot','parameters',s.snapshot->'parameters','limits',s.snapshot->'limits','eval_fingerprint',s.snapshot->'eval_fingerprint','agent_id',a.agent_id,'agent_version_id',a.agent_version_id,'agent_spec_hash',a.agent_spec_hash,'agent_runtime_snapshot_hash',a.runtime_fingerprint) FROM assistant_runtime_snapshots s LEFT JOIN assistant.sessions a ON a.session_id=s.session_id AND a.tenant_id=s.tenant_id AND a.user_id=s.user_id AND a.agent_id::text=s.snapshot->'agent_spec'->>'agentId' AND a.agent_version_id::text=s.snapshot->'agent_spec'->>'agentVersionId' JOIN assistant_runtime_model_leases l ON l.snapshot_id=s.snapshot_id AND l.run_id=s.run_id AND l.tenant_id=s.tenant_id AND l.session_id=s.session_id AND l.user_id=s.user_id WHERE s.tenant_id=$1 AND s.session_id=$2 AND s.run_id=$3::text::uuid").bind(tenant).bind(case).bind(run).fetch_optional(&self.outbox.pool).await?.unwrap_or(Value::Null);
        if result.is_null() {
            return Ok(result);
        }
        result["calls"]=json!(sqlx::query_scalar::<_,Value>("SELECT to_jsonb(c) FROM assistant_runtime_model_calls c WHERE tenant_id=$1 AND session_id=$2 AND run_id=$3::text::uuid ORDER BY reserved_at,call_id").bind(tenant).bind(case).bind(run).fetch_all(&self.outbox.pool).await?);
        result["tool_executions"]=json!(sqlx::query_scalar::<_,Value>("SELECT to_jsonb(e)||jsonb_build_object('quiz_id',result_summary->>'quiz_id') FROM assistant_capability_executions e WHERE tenant_id=$1 AND session_id=$2 AND run_id=$3::text::uuid ORDER BY created_at,execution_id").bind(tenant).bind(case).bind(run).fetch_all(&self.outbox.pool).await?);
        Ok(result)
    }
}
