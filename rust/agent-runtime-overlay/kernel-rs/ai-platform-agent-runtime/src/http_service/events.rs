//! SSE event stream for one thread: durable backlog replay followed by the
//! live broadcast channel, with lag recovery from the store.

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::Path;
use axum::extract::Query;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::http::HeaderValue;
use axum::response::IntoResponse;
use axum::response::Response;
use axum::response::sse::Event;
use axum::response::sse::KeepAlive;
use axum::response::sse::Sse;
use codex_protocol::ThreadId;
use serde::Deserialize;
use tokio::sync::broadcast;

use super::RuntimeHttpState;
use super::security::RuntimeError;
use super::security::SESSION_HEADER;
use super::security::TENANT_HEADER;
use super::security::USER_HEADER;
use super::security::authorize;
use super::security::required_header;
use crate::PlatformThreadIdentity;
use crate::SequencedAssistantTurnEventV1;

#[derive(Deserialize)]
pub(super) struct EventsQuery {
    #[serde(default)]
    after_sequence: i64,
    #[serde(default = "default_event_limit")]
    limit: i64,
}

fn default_event_limit() -> i64 {
    200
}

pub(super) async fn events(
    State(state): State<RuntimeHttpState>,
    Path(thread_id): Path<String>,
    Query(query): Query<EventsQuery>,
    headers: HeaderMap,
) -> Result<Response, RuntimeError> {
    authorize(&headers, &state.internal_token)?;
    let root_thread_id = ThreadId::from_string(&thread_id)
        .map_err(|_| RuntimeError::bad_request("invalid_thread_id"))?;
    let identity = PlatformThreadIdentity::new(
        root_thread_id,
        required_header(&headers, TENANT_HEADER)?,
        required_header(&headers, USER_HEADER)?,
        required_header(&headers, SESSION_HEADER)?,
    );
    state
        .store
        .verify_root_identity(&identity)
        .await
        .map_err(|_| RuntimeError::not_found("thread_not_found"))?;
    let mut receiver = state.events.subscribe();
    let initial = state
        .store
        .read_v1_events_after(root_thread_id, query.after_sequence, query.limit)
        .await
        .map_err(RuntimeError::from_store)?;
    let store = Arc::clone(&state.store);
    let shutdown = state.recovery_shutdown.clone();
    let stream = async_stream::stream! {
        let mut cursor = query.after_sequence;
        for event in initial {
            cursor = event.sequence;
            if let Some(sse) = sse_event(&event) {
                yield Ok::<Event, Infallible>(sse);
            }
        }
        // Broadcast is a latency optimization; the append-only store remains
        // authoritative. Periodic replay closes the gap when a notification is
        // committed while the subscriber is being attached or a sender has no
        // active receiver. Without this, an approval can exist durably while
        // the UI waits forever with no actionable card.
        let mut durable_poll = tokio::time::interval(Duration::from_millis(500));
        durable_poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        durable_poll.tick().await;
        loop {
            tokio::select! {
                () = shutdown.cancelled() => break,
                message = receiver.recv() => match message {
                    Ok(message) if message.root_thread_id == root_thread_id => {
                        if message.event.sequence > cursor {
                            cursor = message.event.sequence;
                            if let Some(sse) = sse_event(&message.event) {
                                yield Ok(sse);
                            }
                        }
                    }
                    Ok(_) => {}
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        match store.read_v1_events_after(root_thread_id, cursor, 1_000).await {
                            Ok(backlog) => {
                                for event in backlog {
                                    cursor = event.sequence;
                                    if let Some(sse) = sse_event(&event) {
                                        yield Ok(sse);
                                    }
                                }
                            }
                            Err(_) => break,
                        }
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                },
                _ = durable_poll.tick() => {
                    match store.read_v1_events_after(root_thread_id, cursor, 1_000).await {
                        Ok(backlog) => {
                            for event in backlog {
                                cursor = event.sequence;
                                if let Some(sse) = sse_event(&event) {
                                    yield Ok(sse);
                                }
                            }
                        }
                        Err(_) => break,
                    }
                }
            }
        }
    };
    let mut response = Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response();
    response
        .headers_mut()
        .insert("cache-control", HeaderValue::from_static("no-cache"));
    response
        .headers_mut()
        .insert("x-accel-buffering", HeaderValue::from_static("no"));
    Ok(response)
}

fn sse_event(event: &SequencedAssistantTurnEventV1) -> Option<Event> {
    let data = serde_json::to_string(event).ok()?;
    Some(
        Event::default()
            .id(event.sequence.to_string())
            .event(&event.event.event_type)
            .data(data),
    )
}
