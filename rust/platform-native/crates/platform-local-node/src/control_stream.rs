//! Signed, bounded NDJSON on the outbound control endpoint. Stream cursors order
//! frames; durable command IDs/nonces and receipts remain the execution authority
//! across reconnections (including a crash before a cursor checkpoint).
use crate::{Error, Result, envelope::PlatformVerifier};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{future::Future, pin::Pin, time::Duration};

pub const FRAME_TIMEOUT: Duration = Duration::from_secs(3);
pub const STREAM_RETRY: Duration = Duration::from_secs(30);
pub const FALLBACK_POLL: Duration = Duration::from_millis(250);
const MAX_FRAME_BYTES: usize = 1024 * 1024;

pub(crate) fn resume_sequence(cursor: Option<&str>) -> Result<u64> {
    let Some(cursor) = cursor else {
        return Ok(0);
    };
    let (id, sequence) = cursor.rsplit_once(':').ok_or(Error::Protocol)?;
    uuid::Uuid::parse_str(id).map_err(|_| Error::Protocol)?;
    let sequence = sequence.parse::<u64>().map_err(|_| Error::Protocol)?;
    if sequence == 0 || sequence == u64::MAX {
        return Err(Error::Protocol);
    }
    Ok(sequence)
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ControlFrame {
    pub protocol_version: String,
    pub device_id: String,
    pub stream_id: String,
    pub sequence: u64,
    pub cursor: String,
    pub issued_at: f64,
    pub expires_at: f64,
    pub commands: Vec<Value>,
    pub platform_key_id: String,
    pub platform_signature: String,
}
impl ControlFrame {
    pub fn verify(&self, verifier: &dyn PlatformVerifier) -> Result<()> {
        let now = crate::now();
        if self.protocol_version != crate::transport::PROTOCOL_VERSION
            || self.sequence == 0
            || uuid::Uuid::parse_str(&self.stream_id).is_err()
            || self.cursor != format!("{}:{}", self.stream_id, self.sequence)
            || self.commands.len() > 100
            || self
                .commands
                .iter()
                .any(|c| !matches!(c["kind"].as_str(), Some("cancel" | "emergency_stop")))
            || !self.issued_at.is_finite()
            || !self.expires_at.is_finite()
            || self.issued_at > now + 30.0
            || self.expires_at <= now
            || self.expires_at <= self.issued_at
            || self.expires_at - self.issued_at > 10.0
        {
            return Err(Error::Protocol);
        }
        let mut payload = serde_json::to_value(self).map_err(|_| Error::Protocol)?;
        payload
            .as_object_mut()
            .unwrap()
            .remove("platform_signature");
        if !verifier.verify(
            &self.platform_key_id,
            crate::canonical(&payload).as_bytes(),
            &self.platform_signature,
        ) {
            return Err(Error::Denied("control stream signature"));
        }
        Ok(())
    }
}

pub trait ControlStream: Send {
    fn next(&mut self) -> Pin<Box<dyn Future<Output = Result<ControlFrame>> + Send + '_>>;
}

pub(crate) struct HttpControlStream {
    response: reqwest::Response,
    pending: Vec<u8>,
    stream_id: String,
    device_id: String,
    sequence: u64,
}
impl HttpControlStream {
    pub(crate) fn new(
        response: reqwest::Response,
        stream_id: String,
        device_id: String,
        sequence: u64,
    ) -> Self {
        Self {
            response,
            pending: Vec::new(),
            stream_id,
            device_id,
            sequence,
        }
    }
    async fn read_frame(&mut self) -> Result<ControlFrame> {
        loop {
            if let Some(end) = self.pending.iter().position(|b| *b == b'\n') {
                if end > MAX_FRAME_BYTES {
                    return Err(Error::Protocol);
                }
                let frame: ControlFrame =
                    serde_json::from_slice(&self.pending[..end]).map_err(|_| Error::Protocol)?;
                self.pending.drain(..=end);
                if frame.stream_id != self.stream_id
                    || frame.device_id != self.device_id
                    || self.sequence.checked_add(1) != Some(frame.sequence)
                {
                    return Err(Error::Denied("control stream binding or sequence"));
                }
                self.sequence = frame.sequence;
                return Ok(frame);
            }
            if self.pending.len() > MAX_FRAME_BYTES {
                return Err(Error::Protocol);
            }
            let chunk = self
                .response
                .chunk()
                .await
                .map_err(|_| Error::Transport)?
                .ok_or(Error::Transport)?;
            // A response chunk may contain several frames, but never retain an
            // unbounded proxy response before looking for a newline.
            if self.pending.len() + chunk.len() > 2 * MAX_FRAME_BYTES {
                return Err(Error::Protocol);
            }
            self.pending.extend_from_slice(&chunk);
        }
    }
}
impl ControlStream for HttpControlStream {
    fn next(&mut self) -> Pin<Box<dyn Future<Output = Result<ControlFrame>> + Send + '_>> {
        Box::pin(async move {
            tokio::time::timeout(FRAME_TIMEOUT, self.read_frame())
                .await
                .map_err(|_| Error::Transport)?
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        signing::{Ed25519PairingSigner, PinnedEd25519Verifier},
        transport::PairingSigner,
    };
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn frame(stream: &str, sequence: u64) -> ControlFrame {
        let signer = Ed25519PairingSigner::from_identity_secret(&[17; 32]).unwrap();
        let mut frame = ControlFrame {
            protocol_version: crate::transport::PROTOCOL_VERSION.into(),
            device_id: "dev_fixture".into(),
            stream_id: stream.into(),
            sequence,
            cursor: format!("{stream}:{sequence}"),
            issued_at: crate::now(),
            expires_at: crate::now() + 5.0,
            commands: Vec::new(),
            platform_key_id: "fixture-key".into(),
            platform_signature: String::new(),
        };
        let mut value = serde_json::to_value(&frame).unwrap();
        value.as_object_mut().unwrap().remove("platform_signature");
        frame.platform_signature = signer.sign(crate::canonical(&value).as_bytes()).unwrap();
        frame
    }
    fn line(frame: &ControlFrame) -> Vec<u8> {
        let mut line = serde_json::to_vec(frame).unwrap();
        line.push(b'\n');
        line
    }
    struct FixtureServer {
        origin: tokio::task::JoinHandle<()>,
        proxy: Option<tokio::task::JoinHandle<()>>,
    }
    impl FixtureServer {
        fn abort(&self) {
            self.origin.abort();
            if let Some(proxy) = &self.proxy {
                proxy.abort();
            }
        }
    }
    impl Drop for FixtureServer {
        fn drop(&mut self) {
            self.abort();
        }
    }
    // A local HTTP fixture isolates framing/proxy behaviour. Production endpoint
    // construction still requires HTTPS and verifies CA/hostname and credentials.
    async fn fixture(
        stream: &str,
    ) -> (
        HttpControlStream,
        tokio::sync::mpsc::Sender<Vec<u8>>,
        FixtureServer,
    ) {
        fixture_inner(stream, false).await
    }
    async fn fixture_inner(
        stream: &str,
        buffering: bool,
    ) -> (
        HttpControlStream,
        tokio::sync::mpsc::Sender<Vec<u8>>,
        FixtureServer,
    ) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::channel::<Vec<u8>>(4);
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            loop {
                let mut b = [0; 1024];
                let count = socket.read(&mut b).await.unwrap();
                if count == 0 {
                    return;
                }
                request.extend_from_slice(&b[..count]);
                if request.windows(4).any(|b| b == b"\r\n\r\n") {
                    break;
                }
            }
            socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/x-ndjson\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n").await.unwrap();
            while let Some(bytes) = rx.recv().await {
                let header = format!("{:x}\r\n", bytes.len());
                if socket.write_all(header.as_bytes()).await.is_err() {
                    return;
                }
                if socket.write_all(&bytes).await.is_err() {
                    return;
                }
                if socket.write_all(b"\r\n").await.is_err() {
                    return;
                }
            }
            let _ = socket.write_all(b"0\r\n\r\n").await;
        });
        let (address, proxy) = if buffering {
            let proxy_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let proxy_address = proxy_listener.local_addr().unwrap();
            let proxy = tokio::spawn(async move {
                let (mut downstream, _) = proxy_listener.accept().await.unwrap();
                let mut request = Vec::new();
                loop {
                    let mut bytes = [0u8; 1024];
                    let n = downstream.read(&mut bytes).await.unwrap();
                    if n == 0 {
                        return;
                    }
                    request.extend_from_slice(&bytes[..n]);
                    if request.windows(4).any(|v| v == b"\r\n\r\n") {
                        break;
                    }
                }
                let mut upstream = tokio::net::TcpStream::connect(address).await.unwrap();
                upstream.write_all(&request).await.unwrap();
                let mut buffered = Vec::new();
                let mut headers_sent = false;
                loop {
                    let mut bytes = [0u8; 4096];
                    let n = upstream.read(&mut bytes).await.unwrap();
                    if n == 0 {
                        break;
                    }
                    buffered.extend_from_slice(&bytes[..n]);
                    if !headers_sent {
                        if let Some(end) = buffered.windows(4).position(|v| v == b"\r\n\r\n") {
                            downstream.write_all(&buffered[..end + 4]).await.unwrap();
                            buffered.drain(..end + 4);
                            headers_sent = true;
                        }
                    }
                    assert!(buffered.len() < 2 * MAX_FRAME_BYTES);
                }
                // Typical response-buffering proxy: flush only at origin EOF.
                let _ = downstream.write_all(&buffered).await;
            });
            (proxy_address, Some(proxy))
        } else {
            (address, None)
        };
        let response = reqwest::Client::builder()
            .no_proxy()
            .build()
            .unwrap()
            .get(format!("http://{address}"))
            .send()
            .await
            .unwrap();
        (
            HttpControlStream::new(response, stream.into(), "dev_fixture".into(), 0),
            tx,
            FixtureServer {
                origin: server,
                proxy,
            },
        )
    }
    #[test]
    fn signed_heartbeats_reject_tampering_expiry_and_unapproved_command_kinds() {
        let signer = Ed25519PairingSigner::from_identity_secret(&[17; 32]).unwrap();
        let verifier =
            PinnedEd25519Verifier::new([("fixture-key".into(), signer.public_key().into())])
                .unwrap();
        let original = frame(&uuid::Uuid::new_v4().to_string(), 1);
        original.verify(&verifier).unwrap();
        let mut changed = original.clone();
        changed.device_id = "dev_other".into();
        assert!(changed.verify(&verifier).is_err());
        let mut changed = original.clone();
        changed.expires_at = crate::now() - 1.0;
        assert!(changed.verify(&verifier).is_err());
        let mut changed = original;
        changed.commands.push(serde_json::json!({"kind":"claim"}));
        assert!(changed.verify(&verifier).is_err());
    }
    #[tokio::test]
    async fn split_frames_and_multiple_frames_in_one_chunk_preserve_order_and_reject_replay() {
        let id = uuid::Uuid::new_v4().to_string();
        let (mut stream, tx, server) = fixture(&id).await;
        let first = line(&frame(&id, 1));
        tx.send(first[..10].to_vec()).await.unwrap();
        let mut remainder = first[10..].to_vec();
        remainder.extend(line(&frame(&id, 2)));
        remainder.extend(line(&frame(&id, 2)));
        tx.send(remainder).await.unwrap();
        assert_eq!(stream.next().await.unwrap().sequence, 1);
        assert_eq!(stream.next().await.unwrap().sequence, 2);
        assert!(matches!(stream.next().await, Err(Error::Denied(_))));
        server.abort();
    }
    #[tokio::test]
    async fn stream_rejects_other_connection_device_sequence_gap_and_oversized_frames() {
        for case in 0..4 {
            let id = uuid::Uuid::new_v4().to_string();
            let (mut stream, tx, server) = fixture(&id).await;
            let mut f = frame(&id, 1);
            match case {
                0 => f.stream_id = uuid::Uuid::new_v4().to_string(),
                1 => f.device_id = "dev_other".into(),
                2 => f.sequence = 2,
                _ => {}
            }
            tx.send(if case == 3 {
                vec![b'x'; MAX_FRAME_BYTES + 1]
            } else {
                line(&f)
            })
            .await
            .unwrap();
            assert!(stream.next().await.is_err());
            server.abort();
        }
    }
    #[tokio::test]
    async fn buffering_proxy_and_partial_trickle_hit_complete_frame_deadline() {
        let id = uuid::Uuid::new_v4().to_string();
        let (mut stream, tx, server) = fixture(&id).await;
        tx.send(b"{".to_vec()).await.unwrap();
        let start = tokio::time::Instant::now();
        assert!(matches!(stream.next().await, Err(Error::Transport)));
        assert!(start.elapsed() >= FRAME_TIMEOUT);
        assert!(start.elapsed() < FRAME_TIMEOUT + Duration::from_secs(1));
        server.abort();
    }
    #[tokio::test]
    async fn buffering_reverse_proxy_holds_valid_frames_and_triggers_fallback_deadline() {
        let id = uuid::Uuid::new_v4().to_string();
        let (mut stream, tx, server) = fixture_inner(&id, true).await;
        tx.send(line(&frame(&id, 1))).await.unwrap();
        let measured = tokio::time::Instant::now();
        assert!(matches!(stream.next().await, Err(Error::Transport)));
        assert!(measured.elapsed() >= FRAME_TIMEOUT);
        assert!(measured.elapsed() < FRAME_TIMEOUT + Duration::from_secs(1));
        server.abort();
    }
    #[tokio::test]
    async fn durable_resume_cursor_continues_sequence_on_a_fresh_connection() {
        let previous = uuid::Uuid::new_v4().to_string();
        let current = uuid::Uuid::new_v4().to_string();
        let cursor = format!("{previous}:23");
        let (mut stream, tx, server) = fixture(&current).await;
        stream.sequence = resume_sequence(Some(&cursor)).unwrap();
        tx.send(line(&frame(&current, 24))).await.unwrap();
        assert_eq!(stream.next().await.unwrap().sequence, 24);
        assert_eq!(resume_sequence(None).unwrap(), 0);
        for bad in [
            String::new(),
            "bogus:23".to_owned(),
            format!("{previous}:0"),
            format!("{previous}:{}", u64::MAX),
        ] {
            assert!(resume_sequence(Some(&bad)).is_err());
        }
        server.abort();
    }
}
