//! Public V2 SSE resume cursor. Authorization remains in the owning route;
//! this parser only selects observation position and never grants membership.
use std::collections::HashMap;
use axum::http::HeaderMap;
use super::AgentError;

pub(super) fn after_sequence(query: &HashMap<String, String>, headers: &HeaderMap) -> Result<i64, AgentError> {
    // Preserve the existing explicit query contract, even when a browser sends
    // a stale/malformed Last-Event-ID alongside a new explicit cursor.
    let cursor = if let Some(value) = query.get("after_sequence") {
        value.parse::<i64>().map_err(|_| invalid_cursor())?
    } else {
        let mut values = headers.get_all("last-event-id").iter();
        let value = values.next();
        if values.next().is_some() { return Err(invalid_cursor()); }
        match value {
            None => 0,
            Some(value) => {
                let text = value.to_str().map_err(|_| invalid_cursor())?;
                if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
                    return Err(invalid_cursor());
                }
                text.parse::<i64>().map_err(|_| invalid_cursor())?
            }
        }
    };
    if cursor < 0 { return Err(invalid_cursor()); }
    // Root cursors include non-V1 records: a gap is legal, and no local
    // sequence lookup, clamping, reset or successor assertion is appropriate.
    Ok(cursor)
}
fn invalid_cursor() -> AgentError { AgentError::code(422, "INVALID_CURSOR") }

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;
    #[test]
    fn standard_header_resumes_without_explicit_query() {
        let query = HashMap::new(); let mut headers = HeaderMap::new();
        assert_eq!(after_sequence(&query, &headers).unwrap(), 0);
        for cursor in [0, 17, 432, i64::MAX] {
            headers.insert("last-event-id", cursor.to_string().parse().unwrap());
            assert_eq!(after_sequence(&query, &headers).unwrap(), cursor);
        }
    }
    #[test]
    fn explicit_cursor_including_zero_overrides_even_invalid_or_duplicate_header() {
        let mut headers = HeaderMap::new();
        headers.append("last-event-id", HeaderValue::from_static("malformed"));
        headers.append("last-event-id", HeaderValue::from_static("99"));
        for cursor in [0, 7, 999] {
            let query = HashMap::from([("after_sequence".to_owned(), cursor.to_string())]);
            assert_eq!(after_sequence(&query, &headers).unwrap(), cursor);
        }
    }
    #[test]
    fn malformed_negative_overflow_or_duplicate_header_uses_existing_error_contract() {
        for value in ["", "-1", "+1", "1.5", "18446744073709551615", " 42", "1,2"] {
            let mut headers = HeaderMap::new(); headers.insert("last-event-id", HeaderValue::from_str(value).unwrap());
            let error = after_sequence(&HashMap::new(), &headers).unwrap_err();
            assert_eq!(error.status,422); assert_eq!(error.detail["code"],"INVALID_CURSOR");
        }
        let mut headers = HeaderMap::new();
        headers.append("last-event-id",HeaderValue::from_static("1"));
        headers.append("last-event-id",HeaderValue::from_static("2"));
        assert!(after_sequence(&HashMap::new(),&headers).is_err());
        for value in ["", "-1", "invalid", "18446744073709551615"] {
            assert!(after_sequence(&HashMap::from([("after_sequence".to_owned(),value.to_owned())]),&HeaderMap::new()).is_err());
        }
    }
    #[tokio::test]
    async fn header_selected_gap_cursor_reaches_runtime_with_admitted_scope_unchanged() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/events",listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut stream,_) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            while !bytes.windows(4).any(|w|w==b"\r\n\r\n") {
                let mut buffer = [0;1024]; let n = stream.read(&mut buffer).await.unwrap();
                assert!(n>0 && bytes.len()<65_536); bytes.extend_from_slice(&buffer[..n]);
            }
            stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await.unwrap();
            String::from_utf8(bytes).unwrap()
        });
        let mut public_headers = HeaderMap::new();
        public_headers.insert("last-event-id",HeaderValue::from_static("432"));
        let cursor = after_sequence(&HashMap::new(),&public_headers).unwrap();
        let mut admitted = reqwest::header::HeaderMap::new();
        for (key,value) in [("x-ai-platform-internal-token","fixture-internal-token"),
            ("x-ai-tenant-id","fixture-tenant"),("x-ai-user-id","fixture-user"),
            ("x-ai-session-id","fixture-session")] { admitted.insert(key,value.parse().unwrap()); }
        let response = super::super::event_transport::EventTransport::new().unwrap()
            .open(url,admitted,cursor,7).await.unwrap();
        assert!(response.status().is_success());
        let request = tokio::time::timeout(std::time::Duration::from_secs(2),server).await.unwrap().unwrap().to_lowercase();
        assert!(request.starts_with("get /events?after_sequence=432&limit=7 "));
        for expected in ["x-ai-tenant-id: fixture-tenant","x-ai-user-id: fixture-user",
            "x-ai-session-id: fixture-session","x-ai-platform-internal-token: fixture-internal-token"] {
            assert!(request.contains(expected));
        }
    }
}
