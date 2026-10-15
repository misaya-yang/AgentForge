//! Real production framing and in-memory reqwest bodies. No listeners, providers or DB.
use super::{HttpResponse, LineBuffer, Lines};
use futures_util::FutureExt;
use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};
use tokio::sync::Semaphore;

fn decode(parts: &[&[u8]]) -> Vec<String> {
    let mut buffer = LineBuffer::default();
    let mut lines = Vec::new();
    for part in parts {
        buffer.push(part);
        while let Some(line) = buffer.next(false).unwrap() {
            lines.push(line);
        }
    }
    while let Some(line) = buffer.next(true).unwrap() {
        lines.push(line);
    }
    lines
}

#[test]
fn line_buffer_preserves_empty_lf_cr_crlf_and_final_unterminated_lines() {
    let wire = b"\nalpha\r\nbeta\rgamma\n\r\nlast";
    let expected = vec!["", "alpha", "beta", "gamma", "", "last"];
    assert_eq!(decode(&[wire]), expected);
    for width in 1..=wire.len() {
        assert_eq!(decode(&wire.chunks(width).collect::<Vec<_>>()), expected);
    }
    assert!(decode(&[]).is_empty());
    assert!(decode(&[b""]).is_empty());
    assert_eq!(decode(&[b"\r"]), vec![""]);
    assert_eq!(decode(&[b"\r", b"\n"]), vec![""]);
    assert_eq!(decode(&[b"a\n"]), vec!["a"]);
}

#[test]
fn line_buffer_preserves_utf8_at_every_split_position() {
    let wire = "你好\r\n🦀café\n終".as_bytes();
    for split in 0..=wire.len() {
        assert_eq!(decode(&[&wire[..split], &wire[split..]]), vec!["你好", "🦀café", "終"]);
    }
    assert_eq!(decode(&wire.chunks(1).collect::<Vec<_>>()), vec!["你好", "🦀café", "終"]);
}

#[test]
fn line_buffer_preserves_all_partitions_of_mixed_delimiters() {
    let wire = b"a\r\n\rb\nc\rd";
    let expected = decode(&[wire]);
    for cuts in 0..(1usize << (wire.len() - 1)) {
        let mut parts: Vec<&[u8]> = Vec::new();
        let mut start = 0;
        for end in 1..wire.len() {
            if cuts & (1 << (end - 1)) != 0 {
                parts.push(&wire[start..end]);
                start = end;
            }
        }
        parts.push(&wire[start..]);
        assert_eq!(decode(&parts), expected, "partition {cuts}");
    }
}

#[test]
fn line_buffer_does_not_move_coalesced_lines_until_next_chunk() {
    let mut buffer = LineBuffer::default();
    buffer.push(b"one\ntwo\npart");
    assert_eq!(buffer.next(false).unwrap().as_deref(), Some("one"));
    assert_eq!(buffer.next(false).unwrap().as_deref(), Some("two"));
    assert_eq!(buffer.bytes, b"one\ntwo\npart");
    assert_eq!(buffer.consumed, 8);
    assert!(buffer.next(false).unwrap().is_none());
    assert_eq!(buffer.scanned, buffer.bytes.len());
    buffer.push(b"ial\n");
    assert_eq!(buffer.bytes, b"partial\n");
    assert_eq!(buffer.scanned, 4);
    assert_eq!(buffer.next(false).unwrap().as_deref(), Some("partial"));
}

#[test]
fn line_buffer_retains_scan_cursor_for_long_partial_and_split_crlf() {
    let mut buffer = LineBuffer::default();
    for _ in 0..1024 {
        buffer.push(&[b'x'; 64]);
        assert!(buffer.next(false).unwrap().is_none());
        assert_eq!(buffer.scanned, buffer.bytes.len());
    }
    buffer.push(b"\r");
    assert!(buffer.next(false).unwrap().is_none());
    assert_eq!(buffer.scanned, 65536);
    // Waiting again does not cause a full prefix rescan.
    assert!(buffer.next(false).unwrap().is_none());
    assert_eq!(buffer.scanned, 65536);
    buffer.push(b"\n");
    assert_eq!(buffer.next(false).unwrap().unwrap(), "x".repeat(65536));
    assert!(buffer.next(true).unwrap().is_none());
}

#[test]
fn line_buffer_preserves_invalid_utf8_and_clear_semantics() {
    let mut buffer = LineBuffer::default();
    buffer.push(&[0xff, b'\n']);
    assert!(buffer.next(false).is_err());
    // A delimited invalid line remains invalid until the owner closes it.
    assert!(buffer.next(false).is_err());
    buffer.clear();
    assert!(buffer.next(true).unwrap().is_none());
    buffer.push(&[0xe2, 0x82]);
    assert!(buffer.next(false).unwrap().is_none());
    assert!(buffer.next(true).is_err());
    // Existing EOF behavior consumes an invalid unterminated tail.
    assert!(buffer.next(true).unwrap().is_none());
}

async fn fixture(body: reqwest::Body, slots: &Arc<Semaphore>) -> Lines {
    let response = axum::http::Response::builder().status(200).body(body).unwrap();
    Lines::new(HttpResponse {
        response: response.into(),
        permit: Some(slots.clone().acquire_owned().await.unwrap()),
    }).unwrap()
}

#[tokio::test]
async fn lines_remain_pull_based_and_release_permit_at_eof() {
    let reads = Arc::new(AtomicUsize::new(0));
    let observed = reads.clone();
    let mut chunks = vec![b"one\ntwo\n".to_vec(), "三\r".as_bytes().to_vec(), b"\nend".to_vec()].into_iter();
    let body = reqwest::Body::wrap_stream(futures_util::stream::poll_fn(move |_| {
        observed.fetch_add(1, Ordering::SeqCst);
        std::task::Poll::Ready(chunks.next().map(Ok::<_, std::io::Error>))
    }));
    let slots = Arc::new(Semaphore::new(1));
    let mut lines = fixture(body, &slots).await;
    assert_eq!(reads.load(Ordering::SeqCst), 0);
    assert_eq!(slots.available_permits(), 0);
    assert_eq!(lines.next().await.unwrap().as_deref(), Some("one"));
    assert_eq!(reads.load(Ordering::SeqCst), 1);
    assert_eq!(lines.next().await.unwrap().as_deref(), Some("two"));
    assert_eq!(reads.load(Ordering::SeqCst), 1);
    assert_eq!(lines.next().await.unwrap().as_deref(), Some("三"));
    assert_eq!(reads.load(Ordering::SeqCst), 3);
    assert_eq!(slots.available_permits(), 0);
    assert_eq!(lines.next().await.unwrap().as_deref(), Some("end"));
    assert_eq!(reads.load(Ordering::SeqCst), 4);
    assert_eq!(slots.available_permits(), 1);
    assert!(lines.next().await.unwrap().is_none());
    assert_eq!(reads.load(Ordering::SeqCst), 4);
}

struct DropProbe(Arc<AtomicUsize>);
impl Drop for DropProbe {
    fn drop(&mut self) { self.0.fetch_add(1, Ordering::SeqCst); }
}

#[tokio::test]
async fn closing_or_dropping_pending_lines_releases_response_and_permit() {
    for explicit_close in [true, false] {
        let dropped = Arc::new(AtomicUsize::new(0));
        let probe = DropProbe(dropped.clone());
        let body = reqwest::Body::wrap_stream(futures_util::stream::poll_fn(move |_| {
            let _keep_alive = &probe;
            std::task::Poll::<Option<Result<Vec<u8>, std::io::Error>>>::Pending
        }));
        let slots = Arc::new(Semaphore::new(1));
        let mut lines = fixture(body, &slots).await;
        assert!(lines.next().now_or_never().is_none());
        assert_eq!(slots.available_permits(), 0);
        assert_eq!(dropped.load(Ordering::SeqCst), 0);
        if explicit_close {
            lines.close();
            assert!(lines.next().await.unwrap().is_none());
        }
        drop(lines);
        assert_eq!(slots.available_permits(), 1);
        assert_eq!(dropped.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn cancelled_next_future_preserves_partial_line_for_resume() {
    let (send, mut receive) = tokio::sync::mpsc::channel(2);
    send.send(Ok::<_, std::io::Error>(b"one\npar".to_vec())).await.unwrap();
    let body = reqwest::Body::wrap_stream(futures_util::stream::poll_fn(move |context| receive.poll_recv(context)));
    let slots = Arc::new(Semaphore::new(1));
    let mut lines = fixture(body, &slots).await;
    assert_eq!(lines.next().await.unwrap().as_deref(), Some("one"));
    assert!(lines.next().now_or_never().is_none());
    send.send(Ok(b"tial\r\n".to_vec())).await.unwrap();
    assert_eq!(lines.next().await.unwrap().as_deref(), Some("partial"));
    lines.close();
    assert_eq!(slots.available_permits(), 1);
}

async fn limited_fixture(body: reqwest::Body, slots: &Arc<Semaphore>, limit: usize) -> Lines {
    let response = axum::http::Response::builder().status(200).body(body).unwrap();
    Lines::with_max_line_bytes(HttpResponse {
        response: response.into(),
        permit: Some(slots.clone().acquire_owned().await.unwrap()),
    }, limit).unwrap()
}

fn assert_line_limit(error: super::AdapterError) {
    assert!(matches!(super::dispatch(error),
        super::DispatchError::AdapterValidationFailed("provider_stream_line_too_large")));
}

#[test]
fn line_buffer_never_copies_an_entire_oversized_http_chunk() {
    let mut buffer = LineBuffer::with_limit(8);
    let upstream = vec![b'x'; 1024 * 1024];
    assert_eq!(buffer.push(&upstream), 10);
    assert_eq!(buffer.bytes.len(), 10);
    assert_eq!(buffer.next(false), Err("provider_stream_line_too_large"));
    buffer.clear();
    assert_eq!(buffer.bytes.capacity(), 0);
}

#[tokio::test]
async fn line_limit_counts_utf8_bytes_excludes_delimiters_and_accepts_exact_boundary() {
    for text in ["abcd", "é好", "🦀"] {
        for ending in ["", "\n", "\r", "\r\n"] {
            let wire = format!("{text}{ending}");
            for split in 0..=wire.len() {
                let chunks = [wire.as_bytes()[..split].to_vec(), wire.as_bytes()[split..].to_vec()];
                let body = reqwest::Body::wrap_stream(futures_util::stream::iter(
                    chunks.into_iter().map(Ok::<_, std::io::Error>)));
                let slots = Arc::new(Semaphore::new(1));
                let mut lines = limited_fixture(body, &slots, text.len()).await;
                assert_eq!(lines.next().await.unwrap().as_deref(), Some(text));
                assert!(lines.next().await.unwrap().is_none());
                assert_eq!(slots.available_permits(), 1);
            }
        }
    }
    let slots = Arc::new(Semaphore::new(1));
    let mut lines = limited_fixture("é好\n".into(), &slots, 4).await;
    assert_line_limit(lines.next().await.unwrap_err());
}

#[tokio::test]
async fn overlong_line_fails_before_eof_and_releases_every_owned_resource_once() {
    for suffix in ["", "\n", "\r", "\r\n"] {
        for fragmented in [false, true] {
            let dropped = Arc::new(AtomicUsize::new(0));
            let probe = DropProbe(dropped.clone());
            let polls = Arc::new(AtomicUsize::new(0));
            let observed = polls.clone();
            let wire = format!("abcde{suffix}").into_bytes();
            let chunks = if fragmented {
                wire.chunks(1).map(|chunk| chunk.to_vec()).collect::<Vec<_>>()
            } else {
                vec![wire]
            };
            let expected_reads = if fragmented { 5 } else { 1 };
            let mut chunks = chunks.into_iter();
            let body = reqwest::Body::wrap_stream(futures_util::stream::poll_fn(move |_| {
                let _keep_alive = &probe;
                observed.fetch_add(1, Ordering::SeqCst);
                match chunks.next() {
                    Some(bytes) => std::task::Poll::Ready(Some(Ok::<_, std::io::Error>(bytes))),
                    None => std::task::Poll::Pending,
                }
            }));
            let slots = Arc::new(Semaphore::new(1));
            let mut lines = limited_fixture(body, &slots, 4).await;
            let error = lines.next().now_or_never().expect("reject without waiting for EOF").unwrap_err();
            assert_line_limit(error);
            assert_eq!(polls.load(Ordering::SeqCst), expected_reads);
            assert_eq!(dropped.load(Ordering::SeqCst), 1);
            assert_eq!(slots.available_permits(), 1);
            assert!(lines.pending_chunk.is_none());
            assert_eq!(lines.buffer.bytes.capacity(), 0);
            assert!(lines.next().await.unwrap().is_none());
            lines.close();
            assert_eq!(polls.load(Ordering::SeqCst), expected_reads);
            assert_eq!(dropped.load(Ordering::SeqCst), 1);
        }
    }
}

#[tokio::test]
async fn coalesced_short_lines_can_exceed_the_limit_in_total_without_read_ahead() {
    let polls = Arc::new(AtomicUsize::new(0));
    let observed = polls.clone();
    let mut chunk = Some(b"abc\r\n".repeat(4096));
    let body = reqwest::Body::wrap_stream(futures_util::stream::poll_fn(move |_| {
        observed.fetch_add(1, Ordering::SeqCst);
        std::task::Poll::Ready(chunk.take().map(Ok::<_, std::io::Error>))
    }));
    let slots = Arc::new(Semaphore::new(1));
    let mut lines = limited_fixture(body, &slots, 3).await;
    for index in 0..4096 {
        assert_eq!(lines.next().await.unwrap().as_deref(), Some("abc"));
        assert!(lines.buffer.bytes.len() <= 5);
        assert_eq!(polls.load(Ordering::SeqCst), 1);
        if index == 0 {
            assert_eq!(lines.pending_chunk.as_ref().unwrap().len(), 4096 * 5 - 5);
        }
    }
    assert!(lines.next().await.unwrap().is_none());
    assert_eq!(polls.load(Ordering::SeqCst), 2);
    assert_eq!(slots.available_permits(), 1);
}

#[tokio::test]
async fn a_valid_line_precedes_a_coalesced_oversized_line_and_never_a_truncated_success() {
    let slots = Arc::new(Semaphore::new(1));
    let mut lines = limited_fixture("ok\nabcde\nlast\n".into(), &slots, 4).await;
    assert_eq!(lines.next().await.unwrap().as_deref(), Some("ok"));
    assert_line_limit(lines.next().await.unwrap_err());
    assert!(lines.next().await.unwrap().is_none());
    assert_eq!(slots.available_permits(), 1);
}

#[tokio::test]
async fn cancelled_read_at_limit_and_split_crlf_can_resume_without_growing_the_budget() {
    let (send, mut receive) = tokio::sync::mpsc::channel(2);
    send.send(Ok::<_, std::io::Error>(b"abcd".to_vec())).await.unwrap();
    let body = reqwest::Body::wrap_stream(futures_util::stream::poll_fn(move |context| receive.poll_recv(context)));
    let slots = Arc::new(Semaphore::new(1));
    let mut lines = limited_fixture(body, &slots, 4).await;
    assert!(lines.next().now_or_never().is_none());
    assert_eq!(lines.buffer.bytes.len(), 4);
    send.send(Ok(b"\r".to_vec())).await.unwrap();
    assert!(lines.next().now_or_never().is_none());
    assert_eq!(lines.buffer.bytes.len(), 5);
    send.send(Ok(b"\n".to_vec())).await.unwrap();
    assert_eq!(lines.next().await.unwrap().as_deref(), Some("abcd"));
    lines.close();
    assert_eq!(slots.available_permits(), 1);
    assert!(lines.pending_chunk.is_none());
    assert_eq!(lines.buffer.bytes.capacity(), 0);
}

#[tokio::test]
async fn invalid_utf8_keeps_its_error_code_and_releases_the_shared_framer() {
    for wire in [vec![0xff, b'\n'], vec![0xff]] {
        let slots = Arc::new(Semaphore::new(1));
        let mut lines = limited_fixture(wire.into(), &slots, 4).await;
        assert!(matches!(super::dispatch(lines.next().await.unwrap_err()),
            super::DispatchError::AdapterValidationFailed("provider_stream_utf8_invalid")));
        assert_eq!(slots.available_permits(), 1);
        assert!(lines.pending_chunk.is_none());
        assert_eq!(lines.buffer.bytes.capacity(), 0);
        assert!(lines.next().await.unwrap().is_none());
    }
}
