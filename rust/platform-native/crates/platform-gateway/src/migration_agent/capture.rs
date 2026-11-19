//! Byte-preserving terminal trace capture around the public Runtime stream.
use super::{
    AgentError, Scope,
    routes::{AgentState, event_response},
    text,
};
use crate::migration_host::context as http_context;
use crate::migration_trace::capture::{
    CaptureContext, FrameReceiver, ReplaySource, capture_assistant_runtime_stream,
};
use axum::{
    body::{Body, Bytes},
    response::Response,
};
use futures_core::Stream;
use futures_util::StreamExt;
use serde_json::{Value, json};
use std::{
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};
fn frames(body: Body) -> FrameReceiver {
    let (sender, receiver) = tokio::sync::mpsc::channel(1);
    tokio::spawn(async move {
        let mut body = body.into_data_stream();
        loop {
            tokio::select! {_ = sender.closed()=>break,item=body.next()=>{let Some(item)=item else{break;};let item=item.map(|b|b.to_vec()).map_err(|_|"AI_PLATFORM_AGENT_RUNTIME_EVENT_STREAM_FAILED".into());let failed=item.is_err();if sender.send(item).await.is_err()||failed{break;}}}
        }
    });
    receiver
}
struct CapturedFrames(FrameReceiver);
impl Stream for CapturedFrames {
    type Item = Result<Bytes, AgentError>;
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.0.poll_recv(cx).map(|item| {
            item.map(|v| {
                v.map(Bytes::from).map_err(|_| {
                    AgentError::code(503, "AI_PLATFORM_AGENT_RUNTIME_EVENT_STREAM_FAILED")
                })
            })
        })
    }
}
#[allow(clippy::too_many_arguments)]
pub async fn capture_response(
    state: &AgentState,
    response: Response,
    scope: &Scope,
    thread: &str,
    turn: Option<&str>,
    after: i64,
    limit: i64,
    v2: bool,
    metadata: Option<Value>,
    request_id: &str,
    snapshot: Option<Value>,
) -> Response {
    let Some(turn) = turn else {
        return response;
    };
    let row=sqlx::query_scalar::<_,Value>("SELECT jsonb_build_object('snapshot',s.snapshot,'started_at',r.started_at) FROM assistant_runtime_snapshots s JOIN assistant_runs r ON r.run_id=s.run_id AND r.tenant_id=s.tenant_id AND r.user_id=s.user_id AND r.session_id=s.session_id WHERE s.run_id=$1::uuid AND s.runtime_thread_id=$2::uuid AND s.tenant_id=$3 AND s.user_id=$4 AND s.session_id=$5 AND r.engine='agent_runtime'").bind(turn).bind(thread).bind(&scope.tenant_id).bind(&scope.user_id).bind(&scope.session_id).fetch_optional(ai_platform_gateway_foundation::tenant_database::executor(&state.repository.pool, &scope.tenant_id)).await;
    let Ok(Some(row)) = row else {
        return response;
    };
    let native = &row["snapshot"];
    let snapshot=snapshot.unwrap_or_else(||{
        // Published callers supply their exact legacy snapshot. For builtin V2
        // streams the native snapshot owns the same model and launch identity.
        let agent=&native["agent_spec"];
        json!({"agent_id":agent["agentId"],"agent_version_id":agent["agentVersionId"],"publication":{"id":null,"channel":agent["channel"]},"model":{"id":native["model"]["id"],"provider":native["model"]["provider_id"]}})
    });
    let context = CaptureContext {
        run_id: turn.into(),
        request_id: request_id.into(),
        tenant_id: scope.tenant_id.clone(),
        user_id: scope.user_id.clone(),
        session_id: scope.session_id.clone(),
        message: text(&native["input"], "message").into(),
        snapshot,
        started_at: chrono::DateTime::parse_from_rfc3339(text(&row, "started_at"))
            .map(|v| v.with_timezone(&chrono::Utc))
            .unwrap_or_else(|_| chrono::Utc::now()),
        first_token_latency_ms: None,
    };
    let replay: ReplaySource = {
        let control = state.control.clone();
        let scope = scope.clone();
        let thread = thread.to_owned();
        let turn = turn.to_owned();
        let correlation = http_context::capture();
        Arc::new(move || {
            let control = control.clone();
            let scope = scope.clone();
            let thread = thread.clone();
            let turn = turn.clone();
            let metadata = metadata.clone();
            Box::pin(http_context::scoped(correlation.clone(), async move {
                let events = control
                    .stream_thread_events(scope, thread.clone(), after, limit, Some(turn.clone()))
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(frames(
                    event_response(events, thread, Some(turn), after, v2, metadata, None)
                        .into_body(),
                ))
            }))
        })
    };
    let (parts, body) = response.into_parts();
    let captured =
        capture_assistant_runtime_stream(frames(body), state.traces.clone(), context, Some(replay));
    Response::from_parts(parts, Body::from_stream(CapturedFrames(captured)))
}
