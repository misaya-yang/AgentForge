//! Source proxy heartbeat matrix. Rust Stream drop replaces Python aclose; destructors cannot return a cleanup error.
use ai_platform_gateway::migration_compat::sse_heartbeat::*;
use axum::body::Bytes;
use futures_util::{StreamExt, stream};
use std::{
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    task::{Context, Poll},
    time::Duration,
};
fn bytes(s: &'static str) -> Result<Bytes, &'static str> {
    Ok(Bytes::from_static(s.as_bytes()))
}
#[test]
fn default_interval_is_fifteen_seconds() {
    assert_eq!(DEFAULT_HEARTBEAT_INTERVAL, Duration::from_secs(15));
}
#[tokio::test]
async fn fast_producer_passes_through_without_heartbeats() {
    let output = with_sse_heartbeat(
        stream::iter([
            bytes("data: 0\n\n"),
            bytes("data: 1\n\n"),
            bytes("data: 2\n\n"),
        ]),
        Duration::from_secs(1),
    )
    .collect::<Vec<_>>()
    .await;
    assert_eq!(
        output,
        vec![
            bytes("data: 0\n\n"),
            bytes("data: 1\n\n"),
            bytes("data: 2\n\n")
        ]
    );
}
#[tokio::test]
async fn idle_producer_emits_heartbeat_lines() {
    let upstream = stream::once(async {
        tokio::time::sleep(Duration::from_millis(180)).await;
        bytes("data: ok\n\n")
    });
    let output = with_sse_heartbeat(upstream, Duration::from_millis(50))
        .collect::<Vec<_>>()
        .await;
    assert!(
        output
            .iter()
            .filter(|c| **c == bytes(": heartbeat\n\n"))
            .count()
            >= 2
    );
    assert_eq!(
        output
            .iter()
            .filter(|c| **c == bytes("data: ok\n\n"))
            .count(),
        1
    );
}
#[tokio::test]
async fn heartbeat_waits_for_sse_frame_boundary() {
    let upstream = stream::once(async { bytes("event: update\n") }).chain(stream::once(async {
        tokio::time::sleep(Duration::from_millis(140)).await;
        bytes("data: {\"ok\": true}\n\n")
    }));
    let output = with_sse_heartbeat(upstream, Duration::from_millis(40))
        .collect::<Vec<_>>()
        .await;
    assert_eq!(
        output,
        vec![bytes("event: update\n"), bytes("data: {\"ok\": true}\n\n")]
    );
}
#[tokio::test]
async fn heartbeat_resumes_after_complete_sse_frame() {
    let upstream = stream::once(async { bytes("data: first\n\n") }).chain(stream::once(async {
        tokio::time::sleep(Duration::from_millis(120)).await;
        bytes("data: second\n\n")
    }));
    let output = with_sse_heartbeat(upstream, Duration::from_millis(40))
        .collect::<Vec<_>>()
        .await;
    assert_eq!(output.first(), Some(&bytes("data: first\n\n")));
    assert!(output[1..output.len() - 1].contains(&bytes(": heartbeat\n\n")));
    assert_eq!(output.last(), Some(&bytes("data: second\n\n")));
}
#[tokio::test]
async fn heartbeat_payload_matches_string_stream_type() {
    let upstream = stream::once(async {
        tokio::time::sleep(Duration::from_millis(100)).await;
        Ok::<String, &str>("data: hello\n\n".into())
    });
    let output: Vec<Result<String, &str>> = with_sse_heartbeat(upstream, Duration::from_millis(40))
        .collect()
        .await;
    assert!(output.contains(&Ok(": heartbeat\n\n".into())));
}
#[tokio::test]
async fn default_byte_stream_heartbeats_before_empty_producer_finishes() {
    let upstream = stream::unfold(false, |done| async move {
        if done {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
        None::<(Result<Bytes, &str>, bool)>
    });
    let output = with_sse_heartbeat(upstream, Duration::from_millis(40))
        .collect::<Vec<_>>()
        .await;
    assert!(!output.is_empty());
    assert!(output.contains(&bytes(": heartbeat\n\n")));
}
#[tokio::test]
async fn producer_exception_propagates_after_prior_payload() {
    let upstream = stream::iter([bytes("data: a\n\n"), Err("upstream fell over")]);
    let output = with_sse_heartbeat(upstream, Duration::from_secs(1))
        .collect::<Vec<_>>()
        .await;
    assert_eq!(
        output,
        vec![bytes("data: a\n\n"), Err("upstream fell over")]
    );
}
struct OwnedStream<S> {
    inner: Pin<Box<S>>,
    drops: Arc<AtomicUsize>,
}
impl<S: futures_core::Stream> futures_core::Stream for OwnedStream<S> {
    type Item = S::Item;
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.inner.as_mut().poll_next(cx)
    }
}
impl<S> Drop for OwnedStream<S> {
    fn drop(&mut self) {
        self.drops.fetch_add(1, Ordering::SeqCst);
    }
}
#[tokio::test]
async fn upstream_primary_failure_survives_single_owner_cleanup() {
    let drops = Arc::new(AtomicUsize::new(0));
    let upstream = OwnedStream {
        inner: Box::pin(stream::iter([Err::<Bytes, _>("primary provider failure")])),
        drops: drops.clone(),
    };
    let output = with_sse_heartbeat(upstream, Duration::from_secs(1))
        .collect::<Vec<_>>()
        .await;
    assert_eq!(output, vec![Err("primary provider failure")]);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn consumer_disconnect_closes_upstream_once() {
    let drops = Arc::new(AtomicUsize::new(0));
    let upstream = OwnedStream {
        inner: Box::pin(stream::repeat(bytes("data: x\n\n"))),
        drops: drops.clone(),
    };
    let mut wrapped = with_sse_heartbeat(upstream, Duration::from_secs(10));
    assert_eq!(wrapped.next().await, Some(bytes("data: x\n\n")));
    drop(wrapped);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn slow_consumer_never_allows_upstream_more_than_one_chunk_ahead() {
    let produced = Arc::new(AtomicUsize::new(0));
    let counter = produced.clone();
    let upstream = stream::iter(0..5000).map(move |i| {
        counter.fetch_add(1, Ordering::SeqCst);
        Ok::<_, &str>(Bytes::from(format!("data: {i}\n\n")))
    });
    let mut wrapped = with_sse_heartbeat(upstream, Duration::from_secs(10));
    for consumed in 1..=5 {
        wrapped.next().await.unwrap().unwrap();
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert_eq!(produced.load(Ordering::SeqCst), consumed);
    }
    drop(wrapped);
    assert_eq!(produced.load(Ordering::SeqCst), 5);
}
