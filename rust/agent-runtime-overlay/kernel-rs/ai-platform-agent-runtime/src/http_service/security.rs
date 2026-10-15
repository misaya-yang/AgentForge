use axum::Json;
use axum::http::HeaderMap;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::response::Response;
use serde::Serialize;

const INTERNAL_TOKEN_HEADER: &str = "x-ai-platform-internal-token";
pub(super) const TENANT_HEADER: &str = "x-ai-tenant-id";
pub(super) const USER_HEADER: &str = "x-ai-user-id";
pub(super) const SESSION_HEADER: &str = "x-ai-session-id";

pub(super) fn authorize(headers: &HeaderMap, expected: &str) -> Result<(), RuntimeError> {
    let provided = headers
        .get(INTERNAL_TOKEN_HEADER)
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| RuntimeError::unauthorized("missing_internal_token"))?;
    if constant_time_eq(provided.as_bytes(), expected.as_bytes()) {
        Ok(())
    } else {
        Err(RuntimeError::unauthorized("invalid_internal_token"))
    }
}

pub(super) fn required_header(
    headers: &HeaderMap,
    name: &'static str,
) -> Result<String, RuntimeError> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| RuntimeError::bad_request("missing_runtime_scope_header"))
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    let mut difference = left.len() ^ right.len();
    for index in 0..left.len().max(right.len()) {
        difference |= usize::from(*left.get(index).unwrap_or(&0) ^ *right.get(index).unwrap_or(&0));
    }
    difference == 0
}

#[derive(Serialize)]
struct ErrorResponse {
    error: &'static str,
}

pub(super) struct RuntimeError {
    status: StatusCode,
    code: &'static str,
}

impl RuntimeError {
    pub(super) fn bad_request(code: &'static str) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code,
        }
    }

    fn unauthorized(code: &'static str) -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            code,
        }
    }

    pub(super) fn conflict(code: &'static str) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            code,
        }
    }

    pub(super) fn not_found(code: &'static str) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            code,
        }
    }

    pub(super) fn unavailable(code: &'static str) -> Self {
        Self {
            status: StatusCode::SERVICE_UNAVAILABLE,
            code,
        }
    }

    pub(super) fn internal(code: &'static str) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code,
        }
    }

    pub(super) fn from_store(error: codex_thread_store::ThreadStoreError) -> Self {
        match error {
            codex_thread_store::ThreadStoreError::ThreadNotFound { .. } => {
                Self::not_found("thread_not_found")
            }
            codex_thread_store::ThreadStoreError::InvalidRequest { .. } => {
                Self::bad_request("invalid_thread_store_request")
            }
            codex_thread_store::ThreadStoreError::Conflict { .. } => Self {
                status: StatusCode::CONFLICT,
                code: "thread_store_conflict",
            },
            codex_thread_store::ThreadStoreError::Unsupported { .. } => {
                Self::bad_request("unsupported_thread_store_operation")
            }
            codex_thread_store::ThreadStoreError::Internal { .. } => {
                Self::internal("thread_store_failure")
            }
        }
    }
}

impl IntoResponse for RuntimeError {
    fn into_response(self) -> Response {
        (self.status, Json(ErrorResponse { error: self.code })).into_response()
    }
}
