use super::*;
use codex_thread_store::ThreadStoreError;

async fn assert_response(error: ThreadStoreError, status: StatusCode, code: &str) {
    let response = RuntimeError::from_store(error).into_response();
    assert_eq!(response.status(), status);
    // Neither classification schedules an HTTP retry. In particular, an
    // uncertain write must never advertise a blind replay via Retry-After.
    assert!(response.headers().get("retry-after").is_none());
    let body = axum::body::to_bytes(response.into_body(), 1024)
        .await
        .expect("bounded error response");
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&body).expect("error JSON"),
        serde_json::json!({"error": code}),
    );
}

#[tokio::test]
async fn transient_failure_and_unknown_write_outcome_remain_distinct() {
    assert_response(
        ThreadStoreError::Retryable {
            message: "synthetic backend diagnostics, not for HTTP disclosure".into(),
        },
        StatusCode::SERVICE_UNAVAILABLE,
        "thread_store_retryable",
    )
    .await;
    assert_response(
        ThreadStoreError::OutcomeUnknown {
            message: "synthetic backend diagnostics, not for HTTP disclosure".into(),
        },
        StatusCode::CONFLICT,
        "thread_store_outcome_unknown",
    )
    .await;
}

#[tokio::test]
async fn unclassified_failure_does_not_infer_retryability_from_its_message() {
    assert_response(
        ThreadStoreError::Internal {
            message: "retryable transient conflict; outcome unknown".into(),
        },
        StatusCode::INTERNAL_SERVER_ERROR,
        "thread_store_failure",
    )
    .await;
}

#[tokio::test]
async fn existing_thread_store_errors_keep_their_http_contract() {
    for (error, status, code) in [
        (
            ThreadStoreError::ThreadNotFound {
                thread_id: codex_protocol::ThreadId::new(),
            },
            StatusCode::NOT_FOUND,
            "thread_not_found",
        ),
        (
            ThreadStoreError::InvalidRequest {
                message: "fixture".into(),
            },
            StatusCode::BAD_REQUEST,
            "invalid_thread_store_request",
        ),
        (
            ThreadStoreError::Conflict {
                message: "fixture".into(),
            },
            StatusCode::CONFLICT,
            "thread_store_conflict",
        ),
        (
            ThreadStoreError::Unsupported {
                operation: "fixture",
            },
            StatusCode::BAD_REQUEST,
            "unsupported_thread_store_operation",
        ),
    ] {
        assert_response(error, status, code).await;
    }
}
