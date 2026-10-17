//! Per-thread SSE: subscribe before replay; all durable progress comes from the
//! ordered Item Store. Local broadcast and PostgreSQL NOTIFY are wake hints only.
use std::{convert::Infallible, sync::Arc, time::Duration};
use axum::{extract::{Path, Query, State}, http::{HeaderMap, HeaderValue},
    response::{IntoResponse, Response, sse::{Event, KeepAlive, Sse}}};
use codex_protocol::ThreadId;
use codex_thread_store::ThreadStoreResult;
use serde::Deserialize;
use tokio::sync::broadcast;
use super::{RuntimeHttpState, event_hub::HubSignal};
use super::security::{RuntimeError, SESSION_HEADER, TENANT_HEADER, USER_HEADER, authorize, required_header};
use crate::{PlatformThreadIdentity, PostgresThreadStore, SequencedAssistantTurnEventV1};
const DURABLE_BACKSTOP: Duration = Duration::from_secs(7);
const REPLAY_PAGE_SIZE: i64 = 1_000;

#[derive(Deserialize)]
pub(super) struct EventsQuery {
    #[serde(default)]
    after_sequence: Option<i64>,
    #[serde(default = "default_event_limit")]
    limit: i64,
}
fn default_event_limit() -> i64 { 200 }

fn resume_cursor(query: &EventsQuery, headers: &HeaderMap) -> Result<i64, RuntimeError> {
    if !(1..=REPLAY_PAGE_SIZE).contains(&query.limit) {
        return Err(RuntimeError::bad_request("invalid_event_limit"));
    }
    // An explicit query is the existing API and overrides a browser's retained
    // Last-Event-ID. No query means the standard SSE header, then zero.
    let cursor = if let Some(cursor) = query.after_sequence { cursor } else {
        let mut values = headers.get_all("last-event-id").iter();
        let header = values.next();
        if values.next().is_some() { return Err(RuntimeError::bad_request("invalid_last_event_id")); }
        match header {
            None => 0,
            Some(header) => {
                let text = header.to_str().map_err(|_| RuntimeError::bad_request("invalid_last_event_id"))?;
                if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
                    return Err(RuntimeError::bad_request("invalid_last_event_id"));
                }
                text.parse::<i64>().map_err(|_| RuntimeError::bad_request("invalid_last_event_id"))?
            }
        }
    };
    if cursor < 0 { return Err(RuntimeError::bad_request("invalid_event_cursor")); }
    Ok(cursor)
}

async fn read_page(store: &PostgresThreadStore, identity: &PlatformThreadIdentity, cursor: i64, limit: i64)
    -> ThreadStoreResult<Vec<SequencedAssistantTurnEventV1>> {
    // Recheck scope/visibility at replay time, including after a NOTIFY from a
    // different process. A hint never grants membership or bypasses a tombstone.
    store.verify_root_identity(identity).await?;
    store.read_v1_events_after(identity.runtime_thread_id, cursor, limit).await
}

pub(super) async fn events(
    State(state): State<RuntimeHttpState>, Path(thread_id): Path<String>,
    Query(query): Query<EventsQuery>, headers: HeaderMap,
) -> Result<Response, RuntimeError> {
    authorize(&headers, &state.internal_token)?;
    let after_sequence = resume_cursor(&query, &headers)?;
    let root_thread_id = ThreadId::from_string(&thread_id).map_err(|_| RuntimeError::bad_request("invalid_thread_id"))?;
    let identity = PlatformThreadIdentity::new(root_thread_id,
        required_header(&headers, TENANT_HEADER)?, required_header(&headers, USER_HEADER)?, required_header(&headers, SESSION_HEADER)?);
    state.store.verify_root_identity(&identity).await.map_err(|_| RuntimeError::not_found("thread_not_found"))?;
    // The guard creates one channel only for an authenticated active subscriber.
    // Its Drop removes the channel after the last subscriber disconnects.
    let mut receiver = state.events.subscribe(root_thread_id);
    let initial = read_page(&state.store, &identity, after_sequence, query.limit).await.map_err(RuntimeError::from_store)?;
    let store = Arc::clone(&state.store);
    let shutdown = state.recovery_shutdown.clone();
    let stream = async_stream::stream! {
        let mut cursor = after_sequence;
        let mut backlog = initial;
        let mut page_limit = query.limit;
        let mut durable_poll = tokio::time::interval(DURABLE_BACKSTOP);
        durable_poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        durable_poll.tick().await;
        loop {
            let full_page = backlog.len() == page_limit as usize;
            for event in backlog.drain(..) {
                if shutdown.is_cancelled() { return; }
                if event.sequence > cursor {
                    cursor = event.sequence;
                    if let Some(sse) = sse_event(&event) { yield Ok::<Event, Infallible>(sse); }
                }
            }
            if !full_page {
                tokio::select! {
                    () = shutdown.cancelled() => break,
                    message = receiver.recv() => match message {
                        // Preserve the established fail-closed non-durable
                        // terminal path during storage failure. Never turn its
                        // sentinel into a durable cursor or a synthetic reset.
                        Ok(HubSignal::Committed(message)) if message.event.sequence == i64::MAX
                            && message.event.event.data.get("durable").and_then(serde_json::Value::as_bool) == Some(false) => {
                            if let Some(sse) = sse_event(&message.event) { yield Ok(sse); }
                            break;
                        }
                        Ok(HubSignal::Committed(message)) if message.event.sequence <= cursor => continue,
                        Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {},
                        Err(broadcast::error::RecvError::Closed) => break,
                    },
                    _ = durable_poll.tick() => {},
                }
            }
            // Never advance to a broadcast/notification sequence. Root cursors
            // contain non-V1 items, so gaps are legal; only persisted V1 order
            // matters. Drain every full page immediately, including at startup.
            page_limit = REPLAY_PAGE_SIZE;
            match read_page(&store, &identity, cursor, page_limit).await {
                Ok(page) => backlog = page,
                Err(_) => break,
            }
        }
    };
    let mut response = Sse::new(stream).keep_alive(KeepAlive::default()).into_response();
    response.headers_mut().insert("cache-control", HeaderValue::from_static("no-cache"));
    response.headers_mut().insert("x-accel-buffering", HeaderValue::from_static("no"));
    Ok(response)
}
fn sse_event(event: &SequencedAssistantTurnEventV1) -> Option<Event> {
    let data = serde_json::to_string(event).ok()?;
    Some(Event::default().id(event.sequence.to_string()).event(&event.event.event_type).data(data))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn query(cursor: Option<i64>) -> EventsQuery { EventsQuery { after_sequence: cursor, limit: 200 } }
    #[test]
    fn resume_uses_header_only_when_query_is_absent() {
        let mut headers = HeaderMap::new();
        assert_eq!(resume_cursor(&query(None), &headers).ok(), Some(0));
        headers.insert("last-event-id", HeaderValue::from_static("42"));
        assert_eq!(resume_cursor(&query(None), &headers).ok(), Some(42));
        assert_eq!(resume_cursor(&query(Some(0)), &headers).ok(), Some(0));
        assert_eq!(resume_cursor(&query(Some(17)), &headers).ok(), Some(17));
        headers.insert("last-event-id", HeaderValue::from_static("malformed"));
        assert_eq!(resume_cursor(&query(Some(17)), &headers).ok(), Some(17));
    }
    #[test]
    fn invalid_or_duplicate_resume_headers_and_bad_query_are_rejected() {
        for value in ["", "-1", "+1", "1.2", "18446744073709551615", " 42"] {
            let mut headers = HeaderMap::new(); headers.insert("last-event-id", HeaderValue::from_str(value).unwrap());
            assert!(resume_cursor(&query(None), &headers).is_err());
        }
        let mut headers = HeaderMap::new();
        headers.append("last-event-id", HeaderValue::from_static("1"));
        headers.append("last-event-id", HeaderValue::from_static("2"));
        assert!(resume_cursor(&query(None), &headers).is_err());
        assert!(resume_cursor(&query(Some(-1)), &HeaderMap::new()).is_err());
        assert!(resume_cursor(&EventsQuery { after_sequence: None, limit: 1001 }, &HeaderMap::new()).is_err());
    }
}

#[cfg(test)]
#[path = "event_fanout_live_tests.rs"]
mod live_tests;
