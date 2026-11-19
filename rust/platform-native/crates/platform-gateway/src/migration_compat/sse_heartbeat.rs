//! Frame-safe, demand-driven source SSE heartbeat contract. No task or read-ahead queue.
use axum::body::Bytes;
use futures_core::Stream;
use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};
pub const DEFAULT_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(15);
pub trait HeartbeatChunk: AsRef<[u8]> {
    fn heartbeat() -> Self;
}
impl HeartbeatChunk for Bytes {
    fn heartbeat() -> Self {
        Bytes::from_static(b": heartbeat\n\n")
    }
}
impl HeartbeatChunk for String {
    fn heartbeat() -> Self {
        ": heartbeat\n\n".into()
    }
}
pub struct SseHeartbeat<S> {
    upstream: Pin<Box<S>>,
    timer: Pin<Box<tokio::time::Sleep>>,
    interval: Duration,
    tail: Vec<u8>,
    boundary: bool,
    done: bool,
}
pub fn with_sse_heartbeat<S>(upstream: S, interval: Duration) -> SseHeartbeat<S> {
    SseHeartbeat {
        upstream: Box::pin(upstream),
        timer: Box::pin(tokio::time::sleep(interval)),
        interval,
        tail: Vec::with_capacity(4),
        boundary: true,
        done: false,
    }
}
impl<S, C, E> Stream for SseHeartbeat<S>
where
    S: Stream<Item = Result<C, E>>,
    C: HeartbeatChunk,
{
    type Item = Result<C, E>;
    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        if this.done {
            return Poll::Ready(None);
        }
        match this.upstream.as_mut().poll_next(cx) {
            Poll::Ready(Some(Ok(chunk))) => {
                for &byte in chunk.as_ref() {
                    if this.tail.len() == 4 {
                        this.tail.remove(0);
                    }
                    this.tail.push(byte);
                }
                this.boundary = this.tail.ends_with(b"\n\n") || this.tail.ends_with(b"\r\n\r\n");
                this.timer
                    .as_mut()
                    .reset(tokio::time::Instant::now() + this.interval);
                return Poll::Ready(Some(Ok(chunk)));
            }
            Poll::Ready(Some(Err(error))) => {
                this.done = true;
                return Poll::Ready(Some(Err(error)));
            }
            Poll::Ready(None) => {
                this.done = true;
                return Poll::Ready(None);
            }
            Poll::Pending => {}
        }
        if this.timer.as_mut().poll(cx).is_ready() {
            this.timer
                .as_mut()
                .reset(tokio::time::Instant::now() + this.interval);
            if this.boundary {
                return Poll::Ready(Some(Ok(C::heartbeat())));
            } // Register the new deadline without cancelling the pending upstream read.
            let _ = this.timer.as_mut().poll(cx);
        }
        Poll::Pending
    }
}
