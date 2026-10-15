use std::future::Future;

use axum::http::HeaderMap;
use reqwest::RequestBuilder;

const MAX_TRACE_STATE: usize = 512;
const MAX_REQUEST_ID: usize = 64;

tokio::task_local! {
    static CURRENT: InternalTraceContext;
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct InternalTraceContext {
    traceparent: Option<String>,
    tracestate: Option<String>,
    request_id: Option<String>,
}

impl InternalTraceContext {
    pub(crate) fn from_headers(headers: &HeaderMap) -> Self {
        Self {
            traceparent: header(headers, "traceparent").filter(|value| valid_traceparent(value)),
            tracestate: header(headers, "tracestate").filter(|value| valid_tracestate(value)),
            request_id: header(headers, "x-request-id").filter(|value| valid_request_id(value)),
        }
    }

    pub(crate) async fn scope<F: Future>(self, future: F) -> F::Output {
        CURRENT.scope(self, future).await
    }
}

pub(crate) fn apply(
    mut request: RequestBuilder,
    run_id: &str,
    turn_id: &str,
    execution_id: &str,
) -> RequestBuilder {
    let context = CURRENT.try_with(Clone::clone).unwrap_or_default();
    if let Some(value) = context.traceparent {
        request = request.header("traceparent", value);
    }
    if let Some(value) = context.tracestate {
        request = request.header("tracestate", value);
    }
    request
        .header(
            "x-request-id",
            context
                .request_id
                .as_deref()
                .unwrap_or_else(|| fallback_request_id(execution_id)),
        )
        .header("x-ai-run-id", safe_correlation(run_id))
        .header("x-ai-turn-id", safe_correlation(turn_id))
        .header("x-ai-execution-id", safe_correlation(execution_id))
}

pub(crate) fn header_pairs(
    run_id: &str,
    turn_id: &str,
    execution_id: &str,
) -> Vec<(String, String)> {
    let context = CURRENT.try_with(Clone::clone).unwrap_or_default();
    let mut headers = Vec::with_capacity(6);
    if let Some(value) = context.traceparent {
        headers.push(("traceparent".into(), value));
    }
    if let Some(value) = context.tracestate {
        headers.push(("tracestate".into(), value));
    }
    headers.extend([
        (
            "x-request-id".into(),
            context
                .request_id
                .unwrap_or_else(|| fallback_request_id(execution_id).to_string()),
        ),
        ("x-ai-run-id".into(), safe_correlation(run_id).to_string()),
        ("x-ai-turn-id".into(), safe_correlation(turn_id).to_string()),
        (
            "x-ai-execution-id".into(),
            safe_correlation(execution_id).to_string(),
        ),
    ]);
    headers
}

fn header(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn valid_traceparent(value: &str) -> bool {
    value.is_ascii()
        && value.len() == 55
        && value.as_bytes()[2] == b'-'
        && value.as_bytes()[35] == b'-'
        && value.as_bytes()[52] == b'-'
        && value
            .bytes()
            .enumerate()
            .all(|(index, byte)| matches!(index, 2 | 35 | 52) || byte.is_ascii_hexdigit())
        && !value[..2].eq_ignore_ascii_case("ff")
        && value[3..35].bytes().any(|byte| byte != b'0')
        && value[36..52].bytes().any(|byte| byte != b'0')
}

fn valid_tracestate(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_TRACE_STATE
        && value
            .bytes()
            .all(|byte| byte == b' ' || (0x21..=0x7e).contains(&byte))
}

fn valid_request_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_REQUEST_ID
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn valid_correlation(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
}

fn safe_correlation(value: &str) -> &str {
    if valid_correlation(value) {
        value
    } else {
        "invalid-correlation"
    }
}

fn fallback_request_id(execution_id: &str) -> &str {
    if valid_request_id(execution_id) {
        execution_id
    } else {
        "worker-internal"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn arc05_task_scope_propagates_only_valid_headers() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "traceparent",
            "00-0123456789abcdef0123456789abcdef-0123456789abcdef-01"
                .parse()
                .expect("valid traceparent"),
        );
        headers.insert(
            "tracestate",
            "vendor=value".parse().expect("valid tracestate"),
        );
        headers.insert(
            "x-request-id",
            "request-a".parse().expect("valid request id"),
        );
        let context = InternalTraceContext::from_headers(&headers);
        context
            .scope(async {
                let request = apply(
                    reqwest::Client::new().get("http://gateway.test/"),
                    "run-a",
                    "turn-a",
                    "execution-a",
                )
                .build()
                .expect("request should build");
                assert_eq!(request.headers()["x-request-id"], "request-a");
                assert_eq!(request.headers()["x-ai-execution-id"], "execution-a");
                assert_eq!(request.headers()["tracestate"], "vendor=value");
            })
            .await;

        headers.insert(
            "x-request-id",
            "bad value"
                .parse()
                .expect("HTTP-safe but policy-invalid id"),
        );
        assert_eq!(
            InternalTraceContext::from_headers(&headers).request_id,
            None
        );
    }
}
