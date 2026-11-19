//! Canonical structured GatewayException and legacy GatewayError projections.
use axum::{
    Json,
    http::{HeaderValue, StatusCode},
    response::{IntoResponse, Response},
};
use serde_json::{Value, json};
#[derive(Clone, Copy)]
pub struct ErrorDefinition {
    pub name: &'static str,
    pub code: i32,
    pub http_status: u16,
    pub message: &'static str,
    pub retryable: bool,
}
pub fn definition(code: i32) -> Option<&'static ErrorDefinition> {
    DEFINITIONS.iter().find(|d| d.code == code)
}
pub fn error_response(
    code: i32,
    message: &str,
    details: Option<Value>,
    trace_id: Option<&str>,
    retryable: bool,
    retry_after: Option<i64>,
) -> Value {
    let mut response = json!({"error":{"code":code,"message":message}});
    if let Some(details) = details.filter(truthy) {
        response["error"]["details"] = details;
    }
    if retryable {
        response["error"]["retryable"] = json!(true);
        if let Some(after) = retry_after.filter(|v| *v != 0) {
            response["error"]["retry_after"] = json!(after);
        }
    }
    if let Some(trace) = trace_id.filter(|s| !s.is_empty()) {
        response["error"]["trace_id"] = json!(trace);
    }
    response
}
fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(v) => *v,
        Value::String(v) => !v.is_empty(),
        Value::Array(v) => !v.is_empty(),
        Value::Object(v) => !v.is_empty(),
        Value::Number(v) => v.as_f64() != Some(0.),
    }
}
pub struct GatewayException {
    pub code: i32,
    pub message: String,
    pub details: Option<Value>,
    pub retryable: bool,
    pub retry_after: Option<i64>,
    pub trace_id: Option<String>,
}
impl GatewayException {
    pub fn new(
        code: i32,
        message: Option<String>,
        details: Option<Value>,
        retryable: Option<bool>,
        retry_after: Option<i64>,
    ) -> Self {
        let definition = definition(code);
        Self {
            code,
            message: message.filter(|s| !s.is_empty()).unwrap_or_else(|| {
                definition
                    .map(|d| d.message)
                    .unwrap_or("Unknown error")
                    .into()
            }),
            details,
            retryable: retryable.unwrap_or_else(|| definition.is_some_and(|d| d.retryable)),
            retry_after,
            trace_id: None,
        }
    }
    pub fn status(&self) -> u16 {
        definition(self.code).map(|d| d.http_status).unwrap_or(500)
    }
    pub fn category(&self) -> i32 {
        self.code / 1000
    }
    pub fn payload(&self, include_trace: bool) -> Value {
        error_response(
            self.code,
            &self.message,
            self.details.clone(),
            if include_trace {
                self.trace_id.as_deref()
            } else {
                None
            },
            self.retryable,
            self.retry_after,
        )
    }
    pub fn response(&self, include_trace: bool) -> Response {
        let mut response = (
            StatusCode::from_u16(self.status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            Json(self.payload(include_trace)),
        )
            .into_response();
        if self.retryable
            && let Some(after) = self.retry_after.filter(|v| *v != 0)
            && let Ok(value) = HeaderValue::from_str(&after.to_string())
        {
            response.headers_mut().insert("retry-after", value);
        }
        response
    }
}
#[derive(Clone, Copy)]
pub enum LegacyKind {
    Gateway,
    Auth,
    PermissionDenied,
    RateLimitExceeded,
    ServiceNotFound,
    CircuitBreakerOpen,
}
pub fn legacy_response(kind: LegacyKind, message: &str) -> Response {
    let status = match kind {
        LegacyKind::Gateway => 400,
        LegacyKind::Auth => 401,
        LegacyKind::PermissionDenied => 403,
        LegacyKind::RateLimitExceeded => 429,
        LegacyKind::ServiceNotFound => 404,
        LegacyKind::CircuitBreakerOpen => 503,
    };
    (
        StatusCode::from_u16(status).unwrap(),
        Json(json!({"error":message})),
    )
        .into_response()
}
pub fn generic_response(trace: Option<&str>) -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(error_response(
            2000,
            "Internal server error",
            None,
            trace,
            false,
            None,
        )),
    )
        .into_response()
}

pub const DEFINITIONS: &[ErrorDefinition] = &[
    ErrorDefinition {
        name: "INVALID_REQUEST",
        code: 1000,
        http_status: 400,
        message: "Invalid request",
        retryable: false,
    },
    ErrorDefinition {
        name: "INVALID_PARAMETER",
        code: 1001,
        http_status: 400,
        message: "Invalid parameter",
        retryable: false,
    },
    ErrorDefinition {
        name: "MISSING_PARAMETER",
        code: 1002,
        http_status: 400,
        message: "Missing required parameter",
        retryable: false,
    },
    ErrorDefinition {
        name: "INVALID_CONTENT_TYPE",
        code: 1003,
        http_status: 415,
        message: "Invalid content type",
        retryable: false,
    },
    ErrorDefinition {
        name: "INVALID_JSON",
        code: 1004,
        http_status: 400,
        message: "Invalid JSON payload",
        retryable: false,
    },
    ErrorDefinition {
        name: "PAYLOAD_TOO_LARGE",
        code: 1005,
        http_status: 413,
        message: "Request payload too large",
        retryable: false,
    },
    ErrorDefinition {
        name: "AUTHENTICATION_REQUIRED",
        code: 1100,
        http_status: 401,
        message: "Authentication required",
        retryable: false,
    },
    ErrorDefinition {
        name: "INVALID_CREDENTIALS",
        code: 1101,
        http_status: 401,
        message: "Invalid credentials",
        retryable: false,
    },
    ErrorDefinition {
        name: "TOKEN_EXPIRED",
        code: 1102,
        http_status: 401,
        message: "Token has expired",
        retryable: false,
    },
    ErrorDefinition {
        name: "TOKEN_INVALID",
        code: 1103,
        http_status: 401,
        message: "Invalid token",
        retryable: false,
    },
    ErrorDefinition {
        name: "API_KEY_INVALID",
        code: 1104,
        http_status: 401,
        message: "Invalid API key",
        retryable: false,
    },
    ErrorDefinition {
        name: "API_KEY_EXPIRED",
        code: 1105,
        http_status: 401,
        message: "API key has expired",
        retryable: false,
    },
    ErrorDefinition {
        name: "PERMISSION_DENIED",
        code: 1200,
        http_status: 403,
        message: "Permission denied",
        retryable: false,
    },
    ErrorDefinition {
        name: "INSUFFICIENT_SCOPE",
        code: 1201,
        http_status: 403,
        message: "Insufficient scope for this operation",
        retryable: false,
    },
    ErrorDefinition {
        name: "RESOURCE_ACCESS_DENIED",
        code: 1202,
        http_status: 403,
        message: "Access to resource denied",
        retryable: false,
    },
    ErrorDefinition {
        name: "OPERATION_NOT_ALLOWED",
        code: 1203,
        http_status: 403,
        message: "Operation not allowed",
        retryable: false,
    },
    ErrorDefinition {
        name: "RESOURCE_NOT_FOUND",
        code: 1300,
        http_status: 404,
        message: "Resource not found",
        retryable: false,
    },
    ErrorDefinition {
        name: "SERVICE_NOT_FOUND",
        code: 1301,
        http_status: 404,
        message: "Service not found",
        retryable: false,
    },
    ErrorDefinition {
        name: "ADAPTER_NOT_FOUND",
        code: 1302,
        http_status: 404,
        message: "Adapter not found",
        retryable: false,
    },
    ErrorDefinition {
        name: "SESSION_NOT_FOUND",
        code: 1303,
        http_status: 404,
        message: "Session not found",
        retryable: false,
    },
    ErrorDefinition {
        name: "TASK_NOT_FOUND",
        code: 1304,
        http_status: 404,
        message: "Task not found",
        retryable: false,
    },
    ErrorDefinition {
        name: "USER_NOT_FOUND",
        code: 1305,
        http_status: 404,
        message: "User not found",
        retryable: false,
    },
    ErrorDefinition {
        name: "THREAD_NOT_FOUND",
        code: 1306,
        http_status: 404,
        message: "Thread not found",
        retryable: false,
    },
    ErrorDefinition {
        name: "RATE_LIMIT_EXCEEDED",
        code: 1400,
        http_status: 429,
        message: "Rate limit exceeded",
        retryable: true,
    },
    ErrorDefinition {
        name: "GLOBAL_RATE_LIMIT",
        code: 1401,
        http_status: 429,
        message: "Global rate limit exceeded",
        retryable: true,
    },
    ErrorDefinition {
        name: "USER_RATE_LIMIT",
        code: 1402,
        http_status: 429,
        message: "User rate limit exceeded",
        retryable: true,
    },
    ErrorDefinition {
        name: "IP_RATE_LIMIT",
        code: 1403,
        http_status: 429,
        message: "IP rate limit exceeded",
        retryable: true,
    },
    ErrorDefinition {
        name: "SERVICE_RATE_LIMIT",
        code: 1404,
        http_status: 429,
        message: "Service rate limit exceeded",
        retryable: true,
    },
    ErrorDefinition {
        name: "TENANT_RATE_LIMIT",
        code: 1405,
        http_status: 429,
        message: "Tenant rate limit exceeded",
        retryable: true,
    },
    ErrorDefinition {
        name: "INTERNAL_ERROR",
        code: 2000,
        http_status: 500,
        message: "Internal server error",
        retryable: false,
    },
    ErrorDefinition {
        name: "CONFIGURATION_ERROR",
        code: 2001,
        http_status: 500,
        message: "Configuration error",
        retryable: false,
    },
    ErrorDefinition {
        name: "INITIALIZATION_ERROR",
        code: 2002,
        http_status: 500,
        message: "Initialization error",
        retryable: false,
    },
    ErrorDefinition {
        name: "SERVICE_UNAVAILABLE",
        code: 2100,
        http_status: 503,
        message: "Service unavailable",
        retryable: true,
    },
    ErrorDefinition {
        name: "SERVICE_DEGRADED",
        code: 2101,
        http_status: 503,
        message: "Service degraded",
        retryable: false,
    },
    ErrorDefinition {
        name: "NO_HEALTHY_INSTANCE",
        code: 2102,
        http_status: 503,
        message: "No healthy instance available",
        retryable: true,
    },
    ErrorDefinition {
        name: "CIRCUIT_BREAKER_OPEN",
        code: 2103,
        http_status: 503,
        message: "Circuit breaker is open",
        retryable: false,
    },
    ErrorDefinition {
        name: "TIMEOUT",
        code: 2200,
        http_status: 504,
        message: "Request timeout",
        retryable: true,
    },
    ErrorDefinition {
        name: "REQUEST_TIMEOUT",
        code: 2201,
        http_status: 504,
        message: "Request timeout",
        retryable: true,
    },
    ErrorDefinition {
        name: "CONNECTION_TIMEOUT",
        code: 2202,
        http_status: 504,
        message: "Connection timeout",
        retryable: true,
    },
    ErrorDefinition {
        name: "READ_TIMEOUT",
        code: 2203,
        http_status: 504,
        message: "Read timeout",
        retryable: true,
    },
    ErrorDefinition {
        name: "CONCURRENCY_LIMIT",
        code: 2300,
        http_status: 503,
        message: "Concurrency limit exceeded",
        retryable: true,
    },
    ErrorDefinition {
        name: "QUEUE_FULL",
        code: 2301,
        http_status: 503,
        message: "Request queue is full",
        retryable: true,
    },
    ErrorDefinition {
        name: "SESSION_ERROR",
        code: 3000,
        http_status: 400,
        message: "Session error",
        retryable: false,
    },
    ErrorDefinition {
        name: "SESSION_EXPIRED",
        code: 3001,
        http_status: 410,
        message: "Session has expired",
        retryable: false,
    },
    ErrorDefinition {
        name: "SESSION_INVALID_STATE",
        code: 3002,
        http_status: 409,
        message: "Invalid session state",
        retryable: false,
    },
    ErrorDefinition {
        name: "TASK_ERROR",
        code: 3100,
        http_status: 500,
        message: "Task error",
        retryable: false,
    },
    ErrorDefinition {
        name: "TASK_CANCELLED",
        code: 3101,
        http_status: 410,
        message: "Task was cancelled",
        retryable: false,
    },
    ErrorDefinition {
        name: "TASK_FAILED",
        code: 3102,
        http_status: 500,
        message: "Task failed",
        retryable: false,
    },
    ErrorDefinition {
        name: "TASK_ALREADY_EXISTS",
        code: 3103,
        http_status: 409,
        message: "Task already exists",
        retryable: false,
    },
    ErrorDefinition {
        name: "QUOTA_EXCEEDED",
        code: 3200,
        http_status: 402,
        message: "Quota exceeded",
        retryable: false,
    },
    ErrorDefinition {
        name: "USAGE_LIMIT_EXCEEDED",
        code: 3201,
        http_status: 402,
        message: "Usage limit exceeded",
        retryable: false,
    },
    ErrorDefinition {
        name: "CREDITS_EXHAUSTED",
        code: 3202,
        http_status: 402,
        message: "Credits exhausted",
        retryable: false,
    },
    ErrorDefinition {
        name: "ADAPTER_ERROR",
        code: 4000,
        http_status: 502,
        message: "Adapter error",
        retryable: false,
    },
    ErrorDefinition {
        name: "ADAPTER_CONNECTION_FAILED",
        code: 4001,
        http_status: 502,
        message: "Failed to connect to adapter",
        retryable: false,
    },
    ErrorDefinition {
        name: "ADAPTER_RESPONSE_INVALID",
        code: 4002,
        http_status: 502,
        message: "Invalid response from adapter",
        retryable: false,
    },
    ErrorDefinition {
        name: "ADAPTER_TIMEOUT",
        code: 4003,
        http_status: 504,
        message: "Adapter request timeout",
        retryable: true,
    },
    ErrorDefinition {
        name: "UPSTREAM_ERROR",
        code: 4100,
        http_status: 502,
        message: "Upstream service error",
        retryable: false,
    },
    ErrorDefinition {
        name: "UPSTREAM_UNAVAILABLE",
        code: 4101,
        http_status: 503,
        message: "Upstream service unavailable",
        retryable: true,
    },
    ErrorDefinition {
        name: "UPSTREAM_TIMEOUT",
        code: 4102,
        http_status: 504,
        message: "Upstream service timeout",
        retryable: true,
    },
    ErrorDefinition {
        name: "UPSTREAM_RESPONSE_INVALID",
        code: 4103,
        http_status: 502,
        message: "Invalid response from upstream",
        retryable: false,
    },
    ErrorDefinition {
        name: "DATABASE_ERROR",
        code: 4200,
        http_status: 503,
        message: "Database error",
        retryable: false,
    },
    ErrorDefinition {
        name: "DATABASE_CONNECTION_FAILED",
        code: 4201,
        http_status: 503,
        message: "Failed to connect to database",
        retryable: true,
    },
    ErrorDefinition {
        name: "DATABASE_QUERY_FAILED",
        code: 4202,
        http_status: 503,
        message: "Database query failed",
        retryable: false,
    },
    ErrorDefinition {
        name: "DATABASE_TRANSACTION_FAILED",
        code: 4203,
        http_status: 503,
        message: "Database transaction failed",
        retryable: false,
    },
    ErrorDefinition {
        name: "CACHE_ERROR",
        code: 4300,
        http_status: 503,
        message: "Cache error",
        retryable: false,
    },
    ErrorDefinition {
        name: "CACHE_CONNECTION_FAILED",
        code: 4301,
        http_status: 503,
        message: "Failed to connect to cache",
        retryable: true,
    },
    ErrorDefinition {
        name: "CACHE_OPERATION_FAILED",
        code: 4302,
        http_status: 503,
        message: "Cache operation failed",
        retryable: false,
    },
];
