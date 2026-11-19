//! Original model-plane stream with tenant-scoped guardrail boundaries.
use super::*;
use crate::llm_api::guardrails::{Context as GuardContext, GuardrailService};
impl AgentModelPlane {
    pub async fn stream(
        &self,
        body: Value,
        call: AuthorizedCall,
    ) -> Result<ModelStream, AgentError> {
        let mut timing = crate::migration_agent::model_timing::TimingEvidence::new(&call);
        let mut cleanup = CallCleanup {
            plane: self.clone(),
            id: call.call_id.clone(),
            armed: true,
            lease: None,
        };
        cleanup.lease = match self.acquire_provider(&call).await {
            Ok(lease) => lease,
            Err(error) => {
                self.fail_call(&call.call_id, "capacity_rejected", false)
                    .await?;
                return Err(error);
            }
        };
        let preflight = self.prepare(&body, &call).await;
        let prepared = match preflight {
            Ok(v) => v,
            Err(e) => {
                self.fail_call(&call.call_id, &e.to_string(), false).await?;
                return Err(e);
            }
        };
        let guard = match GuardrailService::new(self.pool.clone())
            .inspect_input(
                &call.scope.tenant_id,
                &call.scope.user_id,
                &call.call_id,
                &serde_json::to_vec(&prepared.body)
                    .map_err(|_| AgentError::code(503, "GUARDRAILS_INPUT_INVALID"))?,
            )
            .await
        {
            Ok(g) => g,
            Err(e) => {
                self.fail_call(&call.call_id, e.code, false).await?;
                return Err(guard_error(e));
            }
        };
        let mut guarded = match GuardedFrames::new(guard).await {
            Ok(g) => g,
            Err(e) => {
                self.fail_call(&call.call_id, "GUARDRAILS_BUFFER_BUSY", false)
                    .await?;
                return Err(e);
            }
        };
        // Actual final context, after provider translation and input guardrails.
        // Write before claiming dispatch or posting any provider side effect.
        if let Some(metrics) = &self.context_metrics {
            if let Err(error) = metrics.observe_prepared(&call, &prepared.body).await {
                self.fail_call(&call.call_id, "context_metrics_unavailable", false)
                    .await?;
                return Err(error);
            }
        }
        let claimed=sqlx::query("UPDATE assistant_runtime_model_calls SET status='dispatched',dispatched_at=NOW(),updated_at=NOW() WHERE call_id=$1::uuid AND status='reserved'").bind(&call.call_id).execute(ai_platform_gateway_foundation::tenant_database::executor(&self.pool, &call.scope.tenant_id)).await?.rows_affected();
        if claimed != 1 {
            return Err(AgentError::code(
                409,
                "RUNTIME_MODEL_CALL_ALREADY_DISPATCHED",
            ));
        }
        let capacity = cleanup
            .lease
            .as_ref()
            .ok_or_else(|| AgentError::code(503, "MODEL_ADMISSION_UNAVAILABLE"))?;
        capacity
            .before_post()
            .await
            .map_err(|_| AgentError::code(503, "MODEL_ADMISSION_LOST"))?;
        timing.wire = if prepared.native {
            "responses_v1"
        } else {
            "chat_completions"
        };
        timing.dispatch();
        let send = self
            .client
            .post(prepared.url)
            .headers(prepared.headers)
            .json(&prepared.body)
            .send();
        let response = tokio::select! {biased;
            _=capacity.wait_lost()=>Err(AgentError::code(503,"MODEL_ADMISSION_LOST")),
            response=send=>response.map_err(|_|AgentError::code(502,"RUNTIME_PROVIDER_TRANSPORT_ERROR")),
        };
        let mut response = match response {
            Ok(response) => response,
            Err(error) => {
                self.fail_call(&call.call_id, "provider_connection_interrupted", true)
                    .await?;
                return Err(error);
            }
        };
        if !response.status().is_success() {
            self.fail_call(
                &call.call_id,
                &format!("provider_http_{}", response.status().as_u16()),
                true,
            )
            .await?;
            return Err(AgentError::code(502, "RUNTIME_PROVIDER_REJECTED"));
        }
        let provider_request = response
            .headers()
            .get("x-request-id")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let (tx, rx) = mpsc::channel(16);
        let plane = self.clone();
        let task = tokio::spawn(async move {
            let capacity = cleanup.lease.clone();
            let mut pending = Vec::new();
            let mut native = crate::migration_agent::model_projection::NativeValidator::new(
                prepared.aliases.clone(),
                prepared.allowed.clone(),
                prepared.native_search,
                text(&call.snapshot["capabilities"]["reasoning"], "visibility") == "stream",
            );
            let mut chat = crate::migration_agent::model_projection::ChatProjector::new(
                &call.model_id,
                call.estimated_input_tokens,
                prepared.aliases.clone(),
                prepared.allowed.clone(),
            );
            let mut observed = ProviderFrames::default();
            let work = async {
                if !prepared.native {
                    let frame = chat.created();
                    observed.observe(&frame);
                    if !guarded.send(&tx, frame).await? {
                        return Ok(());
                    }
                }
                let mut chat_terminal = false;
                while let Some(chunk) = response
                    .chunk()
                    .await
                    .map_err(|_| AgentError::code(502, "RUNTIME_PROVIDER_TRANSPORT_ERROR"))?
                {
                    for part in chunk.chunks(64 * 1024) {
                        pending.extend_from_slice(part);
                        while let Some(pos) = pending.iter().position(|b| *b == b'\n') {
                            let line: Vec<_> = pending.drain(..=pos).collect();
                            if line.len() > 2 * 1024 * 1024 {
                                return Err(AgentError::code(502, "RUNTIME_PROVIDER_FRAME_LIMIT"));
                            }
                            let line = std::str::from_utf8(&line)
                                .map_err(|_| {
                                    AgentError::code(502, "RUNTIME_PROVIDER_STREAM_INVALID")
                                })?
                                .trim_end_matches(['\r', '\n']);
                            let Some(payload) = line
                                .strip_prefix("data:")
                                .map(str::trim)
                                .filter(|s| !s.is_empty())
                            else {
                                continue;
                            };
                            if prepared.native {
                                if let Some(frame) = native.consume(payload)? {
                                    timing.frame();
                                    timing.projected(&frame);
                                    observed.observe(&frame);
                                    if !guarded.send(&tx, frame).await? {
                                        return Ok(());
                                    }
                                }
                            } else if payload == "[DONE]" {
                                chat_terminal = true;
                            } else {
                                let p: Value = serde_json::from_str(payload).map_err(|_| {
                                    AgentError::code(502, "RUNTIME_PROVIDER_STREAM_INVALID")
                                })?;
                                if p.is_object() {
                                    timing.frame();
                                }
                                for frame in chat.consume(&p)? {
                                    timing.projected(&frame);
                                    observed.observe(&frame);
                                    if !guarded.send(&tx, frame).await? {
                                        return Ok(());
                                    }
                                }
                                if p["choices"].as_array().is_some_and(|a| {
                                    a.iter().any(|v| !v["finish_reason"].is_null())
                                }) {
                                    chat_terminal = true;
                                }
                            }
                        }
                        if pending.len() > 2 * 1024 * 1024 {
                            return Err(AgentError::code(502, "RUNTIME_PROVIDER_FRAME_LIMIT"));
                        }
                    }
                }
                if !pending.is_empty() {
                    return Err(AgentError::code(502, "RUNTIME_PROVIDER_STREAM_INCOMPLETE"));
                }
                if prepared.native {
                    let terminal = native.finish()?;
                    let screened = guarded
                        .screen(&[
                            terminal.frame.clone(),
                            Bytes::from_static(b"data: [DONE]\n\n"),
                        ])
                        .await;
                    plane
                        .complete_call_versioned(
                            &call,
                            terminal.input,
                            terminal.output,
                            terminal
                                .provider_request_id
                                .as_deref()
                                .or(provider_request.as_deref()),
                            terminal.raw_usage,
                        )
                        .await?;
                    cleanup.armed = false;
                    screened?;
                    guarded.flush(&tx).await;
                    let _ = tx.send(Ok(terminal.frame)).await;
                    let _ = tx.send(Ok(Bytes::from_static(b"data: [DONE]\n\n"))).await;
                } else {
                    if !chat_terminal {
                        return Err(AgentError::code(502, "RUNTIME_PROVIDER_STREAM_INCOMPLETE"));
                    }
                    let (frames, input, output) = chat.complete()?;
                    let screened = guarded.screen(&frames).await;
                    plane
                        .complete_call_versioned(
                            &call,
                            input,
                            output,
                            provider_request.as_deref(),
                            chat.raw_usage(),
                        )
                        .await?;
                    cleanup.armed = false;
                    screened?;
                    guarded.flush(&tx).await;
                    for frame in frames {
                        if tx.send(Ok(frame)).await.is_err() {
                            break;
                        }
                    }
                }
                Ok(())
            };
            let result: Result<(), AgentError> = if let Some(capacity) = capacity {
                tokio::select! {result=work=>result,_=capacity.wait_lost()=>Err(AgentError::code(503,"CAPACITY_LEASE_LOST"))}
            } else {
                work.await
            };
            if let Err(error) = result {
                let _ = plane
                    .fail_call(&call.call_id, &error.to_string(), true)
                    .await;
                cleanup.armed = false;
                if error.detail["code"] == "RUNTIME_PROVIDER_TRANSPORT_ERROR" && observed.next > 0 {
                    let _ = tx.send(Ok(observed.failed())).await;
                } else {
                    let _ = tx.send(Err(error)).await;
                }
            }
        });
        let mut stream = ModelStream {
            receiver: rx,
            task,
            first: None,
        };
        match stream.receiver.recv().await {
            Some(Ok(first)) => {
                stream.first = Some(first);
                Ok(stream)
            }
            Some(Err(error)) => Err(error),
            None => Err(AgentError::code(502, "RUNTIME_PROVIDER_STREAM_EMPTY")),
        }
    }
}

fn guard_error(e: crate::llm_api::LlmError) -> AgentError {
    AgentError::code(e.status, e.code)
}
struct GuardedFrames {
    context: Option<GuardContext>,
    frames: Vec<Bytes>,
    bytes: usize,
    started: std::time::Instant,
    _permit: Option<tokio::sync::OwnedSemaphorePermit>,
}
impl GuardedFrames {
    async fn new(context: Option<GuardContext>) -> Result<Self, AgentError> {
        let context = context.filter(|g| g.output_enabled());
        // A separate bounded inner-model lane prevents nesting a public
        // Responses holdback and its model call on the same semaphore.
        static S: std::sync::OnceLock<Arc<tokio::sync::Semaphore>> = std::sync::OnceLock::new();
        let permit = if context.is_some() {
            Some(
                tokio::time::timeout(
                    std::time::Duration::from_millis(500),
                    S.get_or_init(|| Arc::new(tokio::sync::Semaphore::new(4)))
                        .clone()
                        .acquire_owned(),
                )
                .await
                .map_err(|_| AgentError::code(503, "GUARDRAILS_BUFFER_BUSY"))?
                .map_err(|_| AgentError::code(503, "GUARDRAILS_UNAVAILABLE"))?,
            )
        } else {
            None
        };
        Ok(Self {
            context,
            frames: vec![],
            bytes: 0,
            started: std::time::Instant::now(),
            _permit: permit,
        })
    }
    async fn send(
        &mut self,
        tx: &mpsc::Sender<Result<Bytes, AgentError>>,
        frame: Bytes,
    ) -> Result<bool, AgentError> {
        if let Some(g) = &self.context {
            self.bytes = self.bytes.saturating_add(frame.len());
            if self.bytes > g.output_limit() {
                g.reject_output_limit(self.holdback())
                    .await
                    .map_err(guard_error)?;
                return Err(AgentError::code(502, "GUARDRAILS_OUTPUT_LIMIT"));
            }
            self.frames.push(frame);
            Ok(true)
        } else {
            Ok(tx.send(Ok(frame)).await.is_ok())
        }
    }
    fn holdback(&self) -> u64 {
        u64::try_from(self.started.elapsed().as_micros()).unwrap_or(u64::MAX)
    }
    async fn screen(&self, terminal: &[Bytes]) -> Result<(), AgentError> {
        if let Some(g) = &self.context {
            let total = terminal
                .iter()
                .fold(self.bytes, |n, b| n.saturating_add(b.len()));
            if total > g.output_limit() {
                g.reject_output_limit(self.holdback())
                    .await
                    .map_err(guard_error)?;
                return Err(AgentError::code(502, "GUARDRAILS_OUTPUT_LIMIT"));
            }
            let bytes = self
                .frames
                .iter()
                .chain(terminal)
                .flat_map(|b| b.iter().copied())
                .collect::<Vec<_>>();
            g.inspect_output(&bytes, true, self.holdback())
                .await
                .map_err(guard_error)?;
        }
        Ok(())
    }
    async fn flush(&mut self, tx: &mpsc::Sender<Result<Bytes, AgentError>>) {
        for frame in self.frames.drain(..) {
            if tx.send(Ok(frame)).await.is_err() {
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn absent_policy_keeps_existing_frame_delivery_immediate() {
        let (tx, mut rx) = mpsc::channel(2);
        let mut gate = GuardedFrames::new(None).await.unwrap();
        let frame = Bytes::from_static(b"unchanged frame");
        assert!(gate.send(&tx, frame.clone()).await.unwrap());
        assert_eq!(rx.try_recv().unwrap().unwrap(), frame);
        assert!(gate.frames.is_empty());
    }
    #[tokio::test]
    async fn enforced_model_output_never_releases_a_partial_secret() {
        let context = crate::llm_api::guardrails::test_context();
        let (tx, mut rx) = mpsc::channel(2);
        let mut gate = GuardedFrames::new(Some(context.clone())).await.unwrap();
        assert!(
            gate.send(
                &tx,
                Bytes::from_static(
                    b"data: {\"type\":\"response.output_text.delta\",\"delta\":\"person@\"}\n\n"
                )
            )
            .await
            .unwrap()
        );
        assert!(rx.try_recv().is_err());
        let final_frame=Bytes::from_static(b"data: {\"type\":\"response.completed\",\"response\":{\"output\":[{\"content\":[{\"text\":\"person@example.test\"}]}]}}\n\n");
        let rejected = gate.screen(&[final_frame]).await.unwrap_err();
        assert_eq!(rejected.status, 403);
        assert!(rx.try_recv().is_err());
        assert_eq!(
            context.output_report().unwrap().decision,
            crate::llm_api::guardrails::Decision::Block
        );
    }
}
