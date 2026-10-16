//! Composition of the canonical gateway dispatcher, without an HTTP listener.
//!
//! Authentication must resolve a PreflightAuthority before entry. Session writes
//! retain Python ordering: user history precedes adapter lookup; assistant history
//! follows successful execution and precedes the best-effort accounting event.
//! The usage sink is mandatory and owns pricing/recording; it is not a fake meter.
//! Captured service handles survive awaits; session and accounting read the same
//! live object at their source stages. The executor still receives an owned
//! DispatchService: sync capacity is frozen through its internal circuit/admission
//! waits and retries, and timeout is not reread per attempt. Dynamic execution
//! configuration parity requires a separate handle-aware executor seam.
use crate::{
    capacity_admission::AdmissionCancellation,
    dispatch_preflight::{DispatchPreflight, PreflightAuthority, PreflightError, PreflightMode},
    dispatcher::{DispatchContext, DispatchError, DispatchExecutor, DispatchStream, StreamStatus},
    protocol::{ServiceDefinition, StreamChunk, UnifiedRequest, UnifiedResponse},
    service_ownership::ServiceHandle,
    service_registry::RegistryError,
    session_manager::{GetOrCreateSession, SessionError, SessionLifecycle},
    stream_statistics::{StreamStatistics, StreamStatisticsError},
    task_manager::{TaskError, TaskManager},
    usage_parser::{self, UsageParseError},
    usage_recorder::UsageRecord,
};
use chrono::Utc;
use serde_json::{Value, json};
use std::{fmt, future::Future, pin::Pin, sync::Arc, time::Instant};
use tokio::{runtime::Handle, task::JoinHandle};

pub type UsageFuture<'a> = Pin<Box<dyn Future<Output = Result<(), UsageSinkError>> + Send + 'a>>;
/// Deliberately contains no database error text, credentials, or provider payload.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UsageSinkError;
pub trait DispatcherUsageSink: Send + Sync {
    /// Receives normalized, unpriced usage. Implementations must resolve the
    /// original tenant/model price and freeze its receipt before persistence.
    fn record(&self, record: UsageRecord) -> UsageFuture<'_>;
}

#[derive(Debug)]
pub enum GatewayDispatchError {
    Preflight(PreflightError),
    Session(SessionError),
    Registry(RegistryError),
    Dispatch(DispatchError),
    Task(TaskError),
    Metadata(UsageParseError),
    Configuration(&'static str),
    StreamStatistics(StreamStatisticsError),
    Cleanup,
}
impl fmt::Display for GatewayDispatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Preflight(e) => e.fmt(f),
            Self::Session(e) => e.fmt(f),
            Self::Registry(e) => e.fmt(f),
            Self::Dispatch(e) => e.fmt(f),
            Self::Task(e) => e.fmt(f),
            Self::Metadata(e) => e.fmt(f),
            Self::StreamStatistics(e) => e.fmt(f),
            Self::Cleanup => f.write_str("gateway_stream_cleanup_failed"),
            Self::Configuration(_) => f.write_str("invalid_dispatch_configuration"),
        }
    }
}
impl std::error::Error for GatewayDispatchError {}

pub struct GatewayDispatcher {
    preflight: Arc<DispatchPreflight>,
    executor: Arc<DispatchExecutor>,
    sessions: Arc<dyn SessionLifecycle>,
    tasks: Arc<TaskManager>,
    usage: Arc<dyn DispatcherUsageSink>,
}
impl GatewayDispatcher {
    pub fn new(
        preflight: Arc<DispatchPreflight>,
        executor: Arc<DispatchExecutor>,
        sessions: Arc<dyn SessionLifecycle>,
        tasks: Arc<TaskManager>,
        usage: Arc<dyn DispatcherUsageSink>,
    ) -> Self {
        Self {
            preflight,
            executor,
            sessions,
            tasks,
            usage,
        }
    }

    /// Existing operator-facing registry lookup. Dispatcher-only managed model
    /// handles are resolved only inside preflight and are never returned here.
    pub async fn service(&self, service_id: &str) -> Result<Option<ServiceHandle>, RegistryError> {
        self.preflight.registry().get(service_id).await
    }

    /// Auth-filtered service catalogue for OpenAI-compatible `/v1/models`.
    /// Execution, quotas, and usage still flow through `invoke`/`stream`.
    pub(crate) async fn authorized_services(
        &self,
        authority: &PreflightAuthority<'_>,
    ) -> Result<Vec<ServiceHandle>, RegistryError> {
        self.preflight.authorized_services(authority).await
    }

    /// Replace dispatcher-only model projections for one tenant. They remain
    /// subject to the same preflight/auth/admission/usage path as stored services.
    pub(crate) fn replace_openai_model_services(
        &self,
        tenant_id: &str,
        services: Vec<ServiceHandle>,
    ) {
        self.preflight
            .registry()
            .replace_dispatcher_model_services(tenant_id, services);
    }

    pub async fn sdk_read_lease(
        &self,
        request: &UnifiedRequest,
        principal: &ai_platform_auth::service::Principal,
        headers: &axum::http::HeaderMap,
    ) -> Result<Option<crate::sdk_read_grants::SdkReadLease>, GatewayDispatchError> {
        let Some(service) = self
            .preflight
            .registry()
            .get(&request.service_id)
            .await
            .map_err(GatewayDispatchError::Registry)?
        else {
            return Ok(None);
        };
        self.preflight
            .registry()
            .sdk_read_grants()
            .issue(request, &service.snapshot(), principal, headers)
            .map_err(GatewayDispatchError::Configuration)
    }
    async fn prepare_session(
        &self,
        request: &mut UnifiedRequest,
        service: &ServiceHandle,
    ) -> Result<Option<String>, GatewayDispatchError> {
        let service = service.snapshot();
        if !service.session_enabled {
            return Ok(None);
        }
        let session = self
            .sessions
            .get_or_create(GetOrCreateSession {
                session_id: request.session_id.clone(),
                user_id: fallback(&request.user_id, "local").into(),
                tenant_id: fallback(&request.tenant_id, "local").into(),
                service_id: Some(service.service_id.clone()),
                ttl: None,
            })
            .await
            .map_err(GatewayDispatchError::Session)?;
        request.session_id = Some(session.session_id.clone());
        Ok(Some(session.session_id))
    }

    async fn save_user(
        &self,
        request: &UnifiedRequest,
        session_id: Option<&str>,
    ) -> Result<(), GatewayDispatchError> {
        if let Some(session_id) = session_id {
            let text = usage_parser::inputs_to_text(request);
            if !text.is_empty() {
                self.sessions
                    .add_message(session_id, "user", text.into(), None)
                    .await
                    .map_err(GatewayDispatchError::Session)?;
            }
        }
        Ok(())
    }

    /// Preserve the complete synchronous Python dispatch ordering. No production
    /// route is mounted by this constructor or method.
    pub async fn invoke(
        &self,
        request: UnifiedRequest,
        authority: &PreflightAuthority<'_>,
        client_ip: Option<&str>,
        cancellation: AdmissionCancellation,
    ) -> Result<UnifiedResponse, GatewayDispatchError> {
        let start = Instant::now();
        let ready = self
            .preflight
            .prepare(request, authority, client_ip, PreflightMode::Invoke)
            .await
            .map_err(GatewayDispatchError::Preflight)?;
        let mut request = ready.request;
        let service = ready.service;
        let session_id = self.prepare_session(&mut request, &service).await?;
        self.save_user(&request, session_id.as_deref()).await?;
        let adapter = self
            .preflight
            .registry()
            .get_adapter(service.clone())
            .map_err(GatewayDispatchError::Registry)?;
        // The executor still owns a per-execution policy/capacity projection;
        // it does not yet reread live capacity between retry attempts.
        let dispatch_service = service
            .snapshot()
            .dispatch_view()
            .map_err(GatewayDispatchError::Configuration)?;
        let context = dispatch_context(&request);
        // Python catches BaseException around execution, including cancellation.
        // Own only that scope so dropping the caller cancels provider I/O while
        // preserving lease release followed by its one best-effort error record.
        let execution_cancellation = cancellation.clone();
        let executor = self.executor.clone();
        let usage = self.usage.clone();
        let execution_request = request.clone();
        let execution_service = service.clone();
        let execution = tokio::spawn(async move {
            let outcome = executor
                .invoke_adapter_with_cancel(
                    &context,
                    &dispatch_service,
                    adapter,
                    execution_request.clone(),
                    execution_cancellation,
                )
                .await;
            let duration_ms = elapsed_ms(start);
            if let Err(error) = &outcome {
                let status = if matches!(error, DispatchError::UpstreamOutcomeUnknown) {
                    "unknown"
                } else {
                    "error"
                };
                if let Ok(record) = usage_event(
                    &execution_request,
                    &execution_service.snapshot(),
                    &json!({}),
                    duration_ms,
                    "invoke",
                    status,
                    "",
                ) {
                    let _ = usage.record(record).await;
                }
            }
            (outcome, duration_ms)
        });
        let mut cancellation_guard = InvokeCancellationGuard {
            execution: Some(execution),
            cancellation,
            runtime: Handle::current(),
            usage: self.usage.clone(),
            request: request.clone(),
            service: service.clone(),
            start,
        };
        let joined = cancellation_guard
            .execution
            .as_mut()
            .expect("invoke owns its execution until joined")
            .await;
        // This synchronous handoff ends Python's BaseException scope. No await
        // may separate receiving the outcome from disarming cancellation.
        cancellation_guard.execution.take();
        let (outcome, duration_ms) =
            joined.map_err(|_| GatewayDispatchError::Dispatch(DispatchError::CleanupFailed))?;
        let mut response = outcome.map_err(GatewayDispatchError::Dispatch)?;
        if let Some(session_id) = session_id.filter(|_| service.snapshot().session_enabled) {
            if response.session_id.as_deref().is_none_or(str::is_empty) {
                response.session_id = Some(session_id.clone());
            }
            let output = usage_parser::response_to_text(&response);
            if !output.is_empty() {
                let stats = usage_parser::sync_response_stats(&request, &response, duration_ms)
                    .map_err(GatewayDispatchError::Metadata)?;
                self.sessions
                    .add_message(
                        &session_id,
                        "assistant",
                        output.into(),
                        Some(serde_json::Map::from_iter([(
                            "stats".into(),
                            Value::Object(stats),
                        )])),
                    )
                    .await
                    .map_err(GatewayDispatchError::Session)?;
            }
        }
        let payload = json!({"usage":response.usage.clone().unwrap_or_default(),"metadata":response.metadata.clone().unwrap_or_default()});
        self.record_usage(
            &request,
            &service,
            &payload,
            duration_ms,
            "invoke",
            fallback(&response.status, "success"),
            &usage_parser::response_to_text(&response),
        )
        .await;
        Ok(response)
    }

    /// Opens a single owned stream. Poll to None or call close() to join history,
    /// accounting, producer closure and capacity release. Drop starts the same
    /// owned cleanup; abrupt runtime/process shutdown cannot guarantee completion.
    pub async fn stream(
        &self,
        request: UnifiedRequest,
        authority: &PreflightAuthority<'_>,
        client_ip: Option<&str>,
        cancellation: AdmissionCancellation,
    ) -> Result<GatewayStream, GatewayDispatchError> {
        let start = Instant::now();
        let ready = self
            .preflight
            .prepare(request, authority, client_ip, PreflightMode::Stream)
            .await
            .map_err(GatewayDispatchError::Preflight)?;
        let mut request = ready.request;
        let service = ready.service;
        let session_id = self.prepare_session(&mut request, &service).await?;
        let dispatch_service = service.snapshot().stream_dispatch_view();
        self.executor
            .check_stream_circuit(&dispatch_service)
            .await
            .map_err(GatewayDispatchError::Dispatch)?;
        // Python stream rereads session_enabled after circuit admission and
        // intentionally tolerates user-history errors; invoke/submit do not.
        if service.snapshot().session_enabled {
            let _ = self.save_user(&request, session_id.as_deref()).await;
        }
        let adapter = self
            .preflight
            .registry()
            .get_adapter(service.clone())
            .map_err(GatewayDispatchError::Registry)?;
        // Admission is later than circuit selection and user-history I/O.
        let dispatch_service = service.snapshot().stream_dispatch_view();
        let mut inner = self
            .executor
            .open_adapter_stream_after_circuit(
                &dispatch_context(&request),
                &dispatch_service,
                adapter,
                request.clone(),
                cancellation,
            )
            .await
            .map_err(GatewayDispatchError::Dispatch)?;
        inner.defer_success_cleanup_until_close();
        Ok(GatewayStream {
            state: Some(GatewayStreamState {
                inner,
                stats: if sdk_framework(&service.snapshot()).is_some() {
                    StreamStatistics::sdk()
                } else {
                    StreamStatistics::default()
                },
                request,
                service,
                session_id,
                sessions: self.sessions.clone(),
                usage: self.usage.clone(),
                start,
                end_sent: false,
                status: StreamStatus::InProgress,
            }),
            finalization: None,
            pending_error: None,
            runtime: tokio::runtime::Handle::current(),
        })
    }

    /// HTTP stream header preparation mirrors api/v1/stream.py before its lazy
    /// event generator. Authentication must already have resolved authority.
    /// This deliberately precedes validation/rate charging as in the source route.
    pub async fn prepare_stream_session(
        &self,
        request: &mut UnifiedRequest,
        authority: &PreflightAuthority<'_>,
    ) -> Result<Option<String>, GatewayDispatchError> {
        request.user_id = authority.user_id().into();
        request.tenant_id = authority.tenant_id().into();
        let Some(service) = self
            .preflight
            .registry()
            .get(&request.service_id)
            .await
            .map_err(GatewayDispatchError::Registry)?
        else {
            return Ok(None);
        };
        let service = service.snapshot();
        if !service.session_enabled {
            return Ok(None);
        }
        let session = self
            .sessions
            .get_or_create(GetOrCreateSession {
                session_id: request.session_id.clone(),
                user_id: request.user_id.clone(),
                tenant_id: request.tenant_id.clone(),
                service_id: Some(service.service_id.clone()),
                ttl: None,
            })
            .await
            .map_err(GatewayDispatchError::Session)?;
        request.session_id = Some(session.session_id.clone());
        Ok(Some(session.session_id))
    }

    pub async fn submit(
        &self,
        request: UnifiedRequest,
        authority: &PreflightAuthority<'_>,
        client_ip: Option<&str>,
    ) -> Result<String, GatewayDispatchError> {
        self.submit_with_session(request, authority, client_ip)
            .await
            .map(|(task_id, _)| task_id)
    }

    pub async fn submit_with_session(
        &self,
        request: UnifiedRequest,
        authority: &PreflightAuthority<'_>,
        client_ip: Option<&str>,
    ) -> Result<(String, Option<String>), GatewayDispatchError> {
        if self.tasks.requires_durable_actor() && authority.task_actor().is_none() {
            return Err(GatewayDispatchError::Task(
                TaskError::DurableAuthorityRequired,
            ));
        }
        let ready = self
            .preflight
            .prepare(request, authority, client_ip, PreflightMode::Submit)
            .await
            .map_err(GatewayDispatchError::Preflight)?;
        let mut request = ready.request;
        let session_id = self.prepare_session(&mut request, &ready.service).await?;
        self.save_user(&request, session_id.as_deref()).await?;
        let effective_session = request.session_id.clone();
        self.tasks
            .create_task_with_actor(
                request,
                authority.roles().to_vec(),
                client_ip.map(str::to_owned),
                authority.task_actor(),
            )
            .await
            .map(|task| (task.task_id, effective_session))
            .map_err(GatewayDispatchError::Task)
    }

    /// Caller must already have enforced ownership. This mirrors the internal
    /// dispatcher method and intentionally is not exposed as an HTTP endpoint.
    pub async fn get_task_result(
        &self,
        task_id: &str,
    ) -> Result<UnifiedResponse, GatewayDispatchError> {
        self.tasks
            .get_result(task_id)
            .await
            .map_err(GatewayDispatchError::Task)
    }

    #[allow(clippy::too_many_arguments)]
    async fn record_usage(
        &self,
        request: &UnifiedRequest,
        service: &ServiceHandle,
        payload: &Value,
        duration_ms: i64,
        request_type: &str,
        status: &str,
        output_text: &str,
    ) {
        // The canonical helper catches all ordinary parsing/recording exceptions.
        if let Ok(record) = usage_event(
            request,
            &service.snapshot(),
            payload,
            duration_ms,
            request_type,
            status,
            output_text,
        ) {
            let _ = self.usage.record(record).await;
        }
    }
}
/// Retain the execution until the caller receives its outcome. A child may
/// finish successfully before the caller is polled again; dropping that caller
/// is still cancellation inside Python's execution/accounting boundary.
/// Success history/accounting remain on the caller after this guard is disarmed.
struct InvokeCancellationGuard {
    execution: Option<JoinHandle<(Result<UnifiedResponse, DispatchError>, i64)>>,
    cancellation: AdmissionCancellation,
    runtime: Handle,
    usage: Arc<dyn DispatcherUsageSink>,
    request: UnifiedRequest,
    service: ServiceHandle,
    start: Instant,
}
impl Drop for InvokeCancellationGuard {
    fn drop(&mut self) {
        let Some(execution) = self.execution.take() else {
            return;
        };
        self.cancellation.cancel();
        let usage = self.usage.clone();
        let request = self.request.clone();
        let service = self.service.clone();
        let start = self.start;
        self.runtime.spawn(async move {
            // The child already records every execution error after release.
            // Only an unreceived success needs cancellation accounting here.
            if let Ok((Ok(response), _)) = execution.await
                && let Ok(record) = usage_event(
                    &request,
                    &service.snapshot(),
                    &if sdk_framework(&service.snapshot()).is_some() {
                        json!({"usage": response.usage.unwrap_or_default()})
                    } else {
                        json!({})
                    },
                    elapsed_ms(start),
                    "invoke",
                    "error",
                    "",
                )
            {
                let _ = usage.record(record).await;
            }
        });
    }
}
fn dispatch_context(request: &UnifiedRequest) -> DispatchContext {
    DispatchContext {
        request_id: request.request_id.clone(),
        tenant_id: request.tenant_id.clone(),
        user_id: request.user_id.clone(),
    }
}
fn fallback<'a>(value: &'a str, alternative: &'a str) -> &'a str {
    if value.is_empty() { alternative } else { value }
}
fn elapsed_ms(start: Instant) -> i64 {
    i64::try_from(start.elapsed().as_millis()).unwrap_or(i64::MAX)
}
fn sdk_framework(service: &ServiceDefinition) -> Option<&str> {
    service
        .metadata
        .get("adapter_type")
        .and_then(Value::as_str)
        .filter(|value| matches!(*value, "openai_agents" | "claude_agent_sdk"))
}

#[allow(clippy::too_many_arguments)]
pub fn usage_event(
    request: &UnifiedRequest,
    service: &ServiceDefinition,
    payload: &Value,
    duration_ms: i64,
    request_type: &str,
    status: &str,
    output_text: &str,
) -> Result<UsageRecord, UsageParseError> {
    // Official SDKs report whole-loop usage. Never substitute text estimates.
    let sdk = sdk_framework(service);
    if sdk.is_some() {
        let usage = &payload["usage"];
        let input = usage["input_tokens"].as_i64().filter(|n| *n >= 0);
        let output = usage["output_tokens"].as_i64().filter(|n| *n >= 0);
        if usage["sdk_usage"]["state"] != "reported"
            || usage["sdk_usage"]["platform_pricing_state"] != "two_rate_compatible"
            || input
                .zip(output)
                .and_then(|(a, b)| a.checked_add(b))
                .is_none()
        {
            return Err(UsageParseError::SdkUsageUnavailable);
        }
    }
    let tokens = usage_parser::resolve_usage_tokens(
        payload,
        &usage_parser::inputs_to_text(request),
        output_text,
    )?;
    let mut record = UsageRecord::new(
        fallback(&request.tenant_id, "default"),
        fallback(&request.user_id, "anonymous"),
        usage_parser::resolve_model_name(request, service, payload)?,
        Utc::now(),
    );
    record.request_id = request.request_id.clone();
    record.service_id = service.service_id.clone();
    record.assistant_id = usage_parser::resolve_assistant_id(request, service, payload)?;
    record.provider = usage_parser::resolve_provider_name(service, payload)?;
    if let Some(framework) = sdk {
        // Identity is pinned in server configuration, never request context or SDK output.
        record.model = service.metadata["sdk_agent"]["model"]
            .as_str()
            .filter(|v| !v.is_empty())
            .ok_or(UsageParseError::InvalidIdentityMapping)?
            .into();
        record.provider = match framework {
            "openai_agents" => "openai",
            _ => "anthropic",
        }
        .into();
        let mut observation = payload["usage"]["sdk_usage"]
            .as_object()
            .cloned()
            .ok_or(UsageParseError::SdkUsageUnavailable)?;
        observation.insert("execution_status".into(), json!(status));
        record
            .metadata
            .insert("sdk_usage".into(), Value::Object(observation));
        // Provider-reported cost is audit evidence only. Existing pricing receipt,
        // deduplication, aggregate and quota accounting remain the sole charge path.
    }
    record.input_tokens = tokens.input_tokens;
    record.output_tokens = tokens.output_tokens;
    record.latency_ms = duration_ms.max(0);
    record.status = status.into();
    record.request_type = request_type.into();
    record.metadata.insert("source".into(), json!("dispatcher"));
    record
        .metadata
        .insert("token_source".into(), json!(tokens.token_source.as_str()));
    Ok(record)
}

struct GatewayStreamState {
    inner: DispatchStream<StreamChunk>,
    stats: StreamStatistics,
    request: UnifiedRequest,
    service: ServiceHandle,
    session_id: Option<String>,
    sessions: Arc<dyn SessionLifecycle>,
    usage: Arc<dyn DispatcherUsageSink>,
    start: Instant,
    end_sent: bool,
    status: StreamStatus,
}
/// Single-consumer pull stream; no hidden prefetch or detached provider task.
pub struct GatewayStream {
    state: Option<GatewayStreamState>,
    finalization: Option<tokio::task::JoinHandle<Result<(), GatewayDispatchError>>>,
    // Keep the terminal result while history/accounting finalization yields.
    // A cancelled next() waiter must not consume that result before delivery.
    pending_error: Option<GatewayDispatchError>,
    runtime: tokio::runtime::Handle,
}
impl GatewayStream {
    pub fn cancellation(&self) -> Option<AdmissionCancellation> {
        self.state.as_ref().map(|state| state.inner.cancellation())
    }
    fn begin_finish(&mut self, closing: bool) {
        if let Some(mut state) = self.state.take() {
            if closing
                && matches!(
                    state.status,
                    StreamStatus::InProgress | StreamStatus::Success
                )
            {
                // GeneratorExit after yielding stream_end is still an unknown outcome.
                state.status = StreamStatus::Unknown;
                state.stats.mark_sdk_cancelled();
            }
            self.finalization = Some(self.runtime.spawn(finish_stream(state)));
        }
    }
    async fn join_finish(&mut self) -> Result<(), GatewayDispatchError> {
        let result = match self.finalization.as_mut() {
            Some(task) => task.await.unwrap_or(Err(GatewayDispatchError::Cleanup)),
            None => return Ok(()),
        };
        self.finalization = None;
        if result.is_err() {
            // As with Python finally, cleanup failure supersedes the original.
            self.pending_error = None;
        }
        result
    }
    async fn finish_next(&mut self) -> Result<Option<StreamChunk>, GatewayDispatchError> {
        self.join_finish().await?;
        match self.pending_error.take() {
            Some(error) => Err(error),
            None => Ok(None),
        }
    }
    pub async fn next(&mut self) -> Result<Option<StreamChunk>, GatewayDispatchError> {
        let Some(state) = self.state.as_mut() else {
            return self.finish_next().await;
        };
        if state.end_sent {
            let cancelled = state.inner.cancellation().is_cancelled();
            if cancelled {
                state.status = StreamStatus::Unknown;
                state.stats.mark_sdk_cancelled();
                self.pending_error = Some(GatewayDispatchError::Dispatch(DispatchError::Cancelled));
            }
            self.begin_finish(false);
            return self.finish_next().await;
        }
        match state.inner.next().await {
            Ok(Some(chunk)) => Ok(Some(state.stats.observe(chunk, elapsed_ms(state.start)))),
            Ok(None) => {
                let result = state.stats.stream_end(
                    &state.request.request_id,
                    &usage_parser::inputs_to_text(&state.request),
                    elapsed_ms(state.start),
                );
                match result {
                    Ok(chunk) => {
                        state.end_sent = true;
                        state.status = StreamStatus::Success;
                        Ok(Some(chunk))
                    }
                    Err(error) => {
                        state.status = StreamStatus::Error;
                        self.pending_error = Some(GatewayDispatchError::StreamStatistics(error));
                        self.begin_finish(false);
                        self.finish_next().await
                    }
                }
            }
            Err(error) => {
                state.status = state.inner.status();
                self.pending_error = Some(GatewayDispatchError::Dispatch(error));
                self.begin_finish(false);
                self.finish_next().await
            }
        }
    }
    /// Idempotent joined cleanup. Cancelling this await does not discard the
    /// owned finalization task; a subsequent close() can join it again.
    pub async fn close(&mut self) -> Result<(), GatewayDispatchError> {
        self.begin_finish(true);
        self.join_finish().await
    }
}
impl Drop for GatewayStream {
    fn drop(&mut self) {
        self.begin_finish(true);
    }
}
async fn finish_stream(mut state: GatewayStreamState) -> Result<(), GatewayDispatchError> {
    // Cleanup failure supersedes history/accounting exactly as Python finally.
    state
        .inner
        .close()
        .await
        .map_err(GatewayDispatchError::Dispatch)?;
    let finalization = state
        .stats
        .finalize(
            state.status,
            &usage_parser::inputs_to_text(&state.request),
            elapsed_ms(state.start),
            state.session_id.as_ref().map(|_| elapsed_ms(state.start)),
        )
        .map_err(GatewayDispatchError::StreamStatistics)?;
    if let (Some(session_id), Some(metadata)) = (&state.session_id, finalization.session_metadata)
        && state.service.snapshot().session_enabled
    {
        let _ = state
            .sessions
            .add_message(
                session_id,
                "assistant",
                finalization.output_text.clone().into(),
                Some(metadata),
            )
            .await;
    }
    let duration = finalization
        .record_stats
        .get("duration_ms")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    if let Ok(record) = usage_event(
        &state.request,
        &state.service.snapshot(),
        &json!({"usage":finalization.record_stats}),
        duration,
        "stream",
        state.status.usage_status(),
        &finalization.output_text,
    ) {
        let _ = state.usage.record(record).await;
    }
    Ok(())
}

#[cfg(test)]
mod sdk_usage_accounting_tests {
    use super::*;
    fn fixture() -> (UnifiedRequest, ServiceDefinition) {
        let mut request = UnifiedRequest::new(
            "request",
            "service",
            vec![crate::protocol::ContentItem::text(
                "long input must not become estimated usage",
            )],
        );
        request.tenant_id = "tenant".into();
        request.user_id = "user".into();
        let mut service = ServiceDefinition::new("service", "SDK");
        service.metadata =
            json!({"adapter_type":"openai_agents", "sdk_agent":{"model":"configured-model"}});
        (request, service)
    }
    #[test]
    fn sdk_missing_usage_is_not_estimated_or_zero_charged() {
        let (request, service) = fixture();
        for payload in [
            json!({}),
            json!({"usage":{"sdk_usage":{"state":"unavailable"},"output_tokens":2}}),
        ] {
            assert_eq!(
                usage_event(&request, &service, &payload, 1, "stream", "unknown", "text"),
                Err(UsageParseError::SdkUsageUnavailable)
            );
        }
    }
    #[test]
    fn sdk_reported_observation_preserves_run_cost_and_identity() {
        let (request, service) = fixture();
        let payload = json!({"usage":{"input_tokens":14,"output_tokens":6,"total_tokens":20,"sdk_usage":{"state":"reported","platform_pricing_state":"two_rate_compatible","assistant_run_id":"genuine-adapter-run","provider_cost_usd":0.25}}});
        let record =
            usage_event(&request, &service, &payload, 1, "stream", "success", "text").unwrap();
        assert_eq!((record.input_tokens, record.output_tokens), (14, 6));
        assert_eq!(record.provider, "openai");
        assert_eq!(record.model, "configured-model");
        assert_eq!(
            record.metadata["sdk_usage"]["assistant_run_id"],
            "genuine-adapter-run"
        );
        assert_eq!(record.metadata["sdk_usage"]["provider_cost_usd"], 0.25);
        assert_eq!(record.input_cost_cents, 0); // Pricing is exclusively the existing sink's job.
        assert_eq!(record.request_id, "request");
    }
    #[test]
    fn sdk_cached_usage_requires_reconciliation_not_full_rate_charge() {
        let (request, service) = fixture();
        let payload = json!({"usage":{"input_tokens":9,"output_tokens":1,"sdk_usage":{"state":"reported","platform_pricing_state":"reconciliation_required"}}});
        assert_eq!(
            usage_event(&request, &service, &payload, 1, "stream", "success", "text"),
            Err(UsageParseError::SdkUsageUnavailable)
        );
    }
    #[test]
    fn sdk_explicit_zero_is_upstream_usage() {
        let (request, service) = fixture();
        let record=usage_event(&request,&service,&json!({"usage":{"input_tokens":0,"output_tokens":0,"sdk_usage":{"state":"reported","platform_pricing_state":"two_rate_compatible"}}}),1,"stream","success","text").unwrap();
        assert_eq!(record.input_tokens, 0);
        assert_eq!(record.output_tokens, 0);
        assert_eq!(record.metadata["token_source"], "upstream");
    }
    #[test]
    fn sdk_invalid_reported_counts_are_unavailable() {
        let (request, service) = fixture();
        for input in [
            json!(-1),
            json!(true),
            json!(1.5),
            json!("3"),
            json!(i64::MAX),
        ] {
            assert_eq!(
                usage_event(
                    &request,
                    &service,
                    &json!({"usage":{"input_tokens":input,"output_tokens":1,"sdk_usage":{"state":"reported","platform_pricing_state":"two_rate_compatible"}}}),
                    1,
                    "stream",
                    "error",
                    "text"
                ),
                Err(UsageParseError::SdkUsageUnavailable)
            );
        }
    }
}
