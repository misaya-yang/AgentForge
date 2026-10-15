use axum::http::HeaderMap;
use reqwest::RequestBuilder;

const MAX_TRACE_STATE: usize = 512;
const MAX_REQUEST_ID: usize = 64;

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

    pub(crate) fn apply(
        &self,
        mut request: RequestBuilder,
        run_id: &str,
        turn_id: &str,
        execution_id: Option<&str>,
    ) -> RequestBuilder {
        if let Some(value) = &self.traceparent {
            request = request.header("traceparent", value);
        }
        if let Some(value) = &self.tracestate {
            request = request.header("tracestate", value);
        }
        request = request
            .header(
                "x-request-id",
                self.request_id
                    .as_deref()
                    .unwrap_or_else(|| fallback_request_id(run_id)),
            )
            .header("x-ai-run-id", run_id)
            .header("x-ai-turn-id", turn_id);
        if let Some(value) = execution_id.filter(|value| valid_correlation_id(value)) {
            request = request.header("x-ai-execution-id", value);
        }
        request
    }

    pub(crate) fn extend_model_metadata(
        &self,
        metadata: &mut std::collections::HashMap<String, String>,
        run_id: &str,
    ) {
        if let Some(value) = &self.traceparent {
            metadata.insert("traceparent".into(), value.clone());
        }
        if let Some(value) = &self.tracestate {
            metadata.insert("tracestate".into(), value.clone());
        }
        metadata.insert(
            "x-request-id".into(),
            self.request_id
                .clone()
                .unwrap_or_else(|| fallback_request_id(run_id).to_string()),
        );
        metadata.insert("x-ai-run-id".into(), run_id.to_string());
        metadata.insert("x-ai-turn-id".into(), run_id.to_string());
    }
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

fn valid_correlation_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
}

fn fallback_request_id(run_id: &str) -> &str {
    if valid_request_id(run_id) {
        run_id
    } else {
        "runtime-internal"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arc05_filters_untrusted_headers_and_adds_correlations() {
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
        headers.insert(
            "authorization",
            "Bearer must-not-propagate"
                .parse()
                .expect("valid authorization fixture"),
        );
        let context = InternalTraceContext::from_headers(&headers);
        let request = context
            .apply(
                reqwest::Client::new().get("http://worker.test/"),
                "run-a",
                "turn-a",
                Some("execution-a"),
            )
            .build()
            .expect("request should build");
        assert_eq!(request.headers()["x-request-id"], "request-a");
        assert_eq!(request.headers()["x-ai-run-id"], "run-a");
        assert_eq!(request.headers()["x-ai-turn-id"], "turn-a");
        assert_eq!(request.headers()["x-ai-execution-id"], "execution-a");
        assert!(request.headers().get("authorization").is_none());
        let mut metadata = std::collections::HashMap::new();
        context.extend_model_metadata(&mut metadata, "run-a");
        assert_eq!(metadata["x-request-id"], "request-a");
        assert_eq!(metadata["x-ai-turn-id"], "run-a");
        assert!(
            !metadata
                .values()
                .any(|value| value.contains("must-not-propagate"))
        );

        headers.insert(
            "x-request-id",
            "bad value"
                .parse()
                .expect("HTTP-safe but policy-invalid id"),
        );
        let filtered = InternalTraceContext::from_headers(&headers);
        assert_eq!(filtered.request_id, None);
    }
}
