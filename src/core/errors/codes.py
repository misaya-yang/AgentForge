"""
错误码定义

错误码规范：
- 1000-1999: 客户端错误（请求参数、认证失败）
- 2000-2999: 服务端错误（服务不可用、超时）
- 3000-3999: 业务逻辑错误（限流、余额不足）
- 4000-4999: 外部依赖错误（AI 服务异常）
"""

from enum import IntEnum


class ErrorCategory(IntEnum):
    """错误类别"""

    CLIENT = 1  # 客户端错误
    SERVER = 2  # 服务端错误
    BUSINESS = 3  # 业务逻辑错误
    EXTERNAL = 4  # 外部依赖错误


class ErrorCode(IntEnum):
    """错误码枚举"""

    # ============ 客户端错误 (1000-1999) ============

    # 请求参数错误 (1000-1099)
    INVALID_REQUEST = 1000
    INVALID_PARAMETER = 1001
    MISSING_PARAMETER = 1002
    INVALID_CONTENT_TYPE = 1003
    INVALID_JSON = 1004
    PAYLOAD_TOO_LARGE = 1005

    # 认证错误 (1100-1199)
    AUTHENTICATION_REQUIRED = 1100
    INVALID_CREDENTIALS = 1101
    TOKEN_EXPIRED = 1102
    TOKEN_INVALID = 1103
    API_KEY_INVALID = 1104
    API_KEY_EXPIRED = 1105

    # 授权错误 (1200-1299)
    PERMISSION_DENIED = 1200
    INSUFFICIENT_SCOPE = 1201
    RESOURCE_ACCESS_DENIED = 1202
    OPERATION_NOT_ALLOWED = 1203

    # 资源错误 (1300-1399)
    RESOURCE_NOT_FOUND = 1300
    SERVICE_NOT_FOUND = 1301
    ADAPTER_NOT_FOUND = 1302
    SESSION_NOT_FOUND = 1303
    TASK_NOT_FOUND = 1304
    USER_NOT_FOUND = 1305
    THREAD_NOT_FOUND = 1306

    # 限流错误 (1400-1499)
    RATE_LIMIT_EXCEEDED = 1400
    GLOBAL_RATE_LIMIT = 1401
    USER_RATE_LIMIT = 1402
    IP_RATE_LIMIT = 1403
    SERVICE_RATE_LIMIT = 1404
    TENANT_RATE_LIMIT = 1405

    # ============ 服务端错误 (2000-2999) ============

    # 内部错误 (2000-2099)
    INTERNAL_ERROR = 2000
    CONFIGURATION_ERROR = 2001
    INITIALIZATION_ERROR = 2002

    # 服务可用性错误 (2100-2199)
    SERVICE_UNAVAILABLE = 2100
    SERVICE_DEGRADED = 2101
    NO_HEALTHY_INSTANCE = 2102
    CIRCUIT_BREAKER_OPEN = 2103

    # 超时错误 (2200-2299)
    TIMEOUT = 2200
    REQUEST_TIMEOUT = 2201
    CONNECTION_TIMEOUT = 2202
    READ_TIMEOUT = 2203

    # 并发控制错误 (2300-2399)
    CONCURRENCY_LIMIT = 2300
    QUEUE_FULL = 2301

    # ============ 业务逻辑错误 (3000-3999) ============

    # 会话错误 (3000-3099)
    SESSION_ERROR = 3000
    SESSION_EXPIRED = 3001
    SESSION_INVALID_STATE = 3002

    # 任务错误 (3100-3199)
    TASK_ERROR = 3100
    TASK_CANCELLED = 3101
    TASK_FAILED = 3102
    TASK_ALREADY_EXISTS = 3103

    # 配额错误 (3200-3299)
    QUOTA_EXCEEDED = 3200
    USAGE_LIMIT_EXCEEDED = 3201
    CREDITS_EXHAUSTED = 3202

    # ============ 外部依赖错误 (4000-4999) ============

    # 适配器错误 (4000-4099)
    ADAPTER_ERROR = 4000
    ADAPTER_CONNECTION_FAILED = 4001
    ADAPTER_RESPONSE_INVALID = 4002
    ADAPTER_TIMEOUT = 4003

    # 上游服务错误 (4100-4199)
    UPSTREAM_ERROR = 4100
    UPSTREAM_UNAVAILABLE = 4101
    UPSTREAM_TIMEOUT = 4102
    UPSTREAM_RESPONSE_INVALID = 4103

    # 数据库错误 (4200-4299)
    DATABASE_ERROR = 4200
    DATABASE_CONNECTION_FAILED = 4201
    DATABASE_QUERY_FAILED = 4202
    DATABASE_TRANSACTION_FAILED = 4203

    # 缓存错误 (4300-4399)
    CACHE_ERROR = 4300
    CACHE_CONNECTION_FAILED = 4301
    CACHE_OPERATION_FAILED = 4302

    @property
    def category(self) -> ErrorCategory:
        """获取错误类别"""
        return ErrorCategory(self.value // 1000)

    @property
    def http_status(self) -> int:
        """获取对应的 HTTP 状态码"""
        return _ERROR_HTTP_STATUS.get(self, 500)

    @property
    def retryable(self) -> bool:
        """是否可重试"""
        return self in _RETRYABLE_ERRORS

    @property
    def message(self) -> str:
        """获取默认错误消息"""
        return _ERROR_MESSAGES.get(self, "Unknown error")


# 错误码到 HTTP 状态码映射
_ERROR_HTTP_STATUS = {
    # 客户端错误 -> 4xx
    ErrorCode.INVALID_REQUEST: 400,
    ErrorCode.INVALID_PARAMETER: 400,
    ErrorCode.MISSING_PARAMETER: 400,
    ErrorCode.INVALID_CONTENT_TYPE: 415,
    ErrorCode.INVALID_JSON: 400,
    ErrorCode.PAYLOAD_TOO_LARGE: 413,
    ErrorCode.AUTHENTICATION_REQUIRED: 401,
    ErrorCode.INVALID_CREDENTIALS: 401,
    ErrorCode.TOKEN_EXPIRED: 401,
    ErrorCode.TOKEN_INVALID: 401,
    ErrorCode.API_KEY_INVALID: 401,
    ErrorCode.API_KEY_EXPIRED: 401,
    ErrorCode.PERMISSION_DENIED: 403,
    ErrorCode.INSUFFICIENT_SCOPE: 403,
    ErrorCode.RESOURCE_ACCESS_DENIED: 403,
    ErrorCode.OPERATION_NOT_ALLOWED: 403,
    ErrorCode.RESOURCE_NOT_FOUND: 404,
    ErrorCode.SERVICE_NOT_FOUND: 404,
    ErrorCode.ADAPTER_NOT_FOUND: 404,
    ErrorCode.SESSION_NOT_FOUND: 404,
    ErrorCode.TASK_NOT_FOUND: 404,
    ErrorCode.USER_NOT_FOUND: 404,
    ErrorCode.THREAD_NOT_FOUND: 404,
    ErrorCode.RATE_LIMIT_EXCEEDED: 429,
    ErrorCode.GLOBAL_RATE_LIMIT: 429,
    ErrorCode.USER_RATE_LIMIT: 429,
    ErrorCode.IP_RATE_LIMIT: 429,
    ErrorCode.SERVICE_RATE_LIMIT: 429,
    ErrorCode.TENANT_RATE_LIMIT: 429,
    # 服务端错误 -> 5xx
    ErrorCode.INTERNAL_ERROR: 500,
    ErrorCode.CONFIGURATION_ERROR: 500,
    ErrorCode.INITIALIZATION_ERROR: 500,
    ErrorCode.SERVICE_UNAVAILABLE: 503,
    ErrorCode.SERVICE_DEGRADED: 503,
    ErrorCode.NO_HEALTHY_INSTANCE: 503,
    ErrorCode.CIRCUIT_BREAKER_OPEN: 503,
    ErrorCode.TIMEOUT: 504,
    ErrorCode.REQUEST_TIMEOUT: 504,
    ErrorCode.CONNECTION_TIMEOUT: 504,
    ErrorCode.READ_TIMEOUT: 504,
    ErrorCode.CONCURRENCY_LIMIT: 503,
    ErrorCode.QUEUE_FULL: 503,
    # 业务逻辑错误 -> 4xx/5xx
    ErrorCode.SESSION_ERROR: 400,
    ErrorCode.SESSION_EXPIRED: 410,
    ErrorCode.SESSION_INVALID_STATE: 409,
    ErrorCode.TASK_ERROR: 500,
    ErrorCode.TASK_CANCELLED: 410,
    ErrorCode.TASK_FAILED: 500,
    ErrorCode.TASK_ALREADY_EXISTS: 409,
    ErrorCode.QUOTA_EXCEEDED: 402,
    ErrorCode.USAGE_LIMIT_EXCEEDED: 402,
    ErrorCode.CREDITS_EXHAUSTED: 402,
    # 外部依赖错误 -> 502/503
    ErrorCode.ADAPTER_ERROR: 502,
    ErrorCode.ADAPTER_CONNECTION_FAILED: 502,
    ErrorCode.ADAPTER_RESPONSE_INVALID: 502,
    ErrorCode.ADAPTER_TIMEOUT: 504,
    ErrorCode.UPSTREAM_ERROR: 502,
    ErrorCode.UPSTREAM_UNAVAILABLE: 503,
    ErrorCode.UPSTREAM_TIMEOUT: 504,
    ErrorCode.UPSTREAM_RESPONSE_INVALID: 502,
    ErrorCode.DATABASE_ERROR: 503,
    ErrorCode.DATABASE_CONNECTION_FAILED: 503,
    ErrorCode.DATABASE_QUERY_FAILED: 503,
    ErrorCode.DATABASE_TRANSACTION_FAILED: 503,
    ErrorCode.CACHE_ERROR: 503,
    ErrorCode.CACHE_CONNECTION_FAILED: 503,
    ErrorCode.CACHE_OPERATION_FAILED: 503,
}

# 可重试的错误
_RETRYABLE_ERRORS = {
    ErrorCode.TIMEOUT,
    ErrorCode.REQUEST_TIMEOUT,
    ErrorCode.CONNECTION_TIMEOUT,
    ErrorCode.READ_TIMEOUT,
    ErrorCode.SERVICE_UNAVAILABLE,
    ErrorCode.NO_HEALTHY_INSTANCE,
    ErrorCode.CONCURRENCY_LIMIT,
    ErrorCode.QUEUE_FULL,
    ErrorCode.RATE_LIMIT_EXCEEDED,
    ErrorCode.GLOBAL_RATE_LIMIT,
    ErrorCode.USER_RATE_LIMIT,
    ErrorCode.IP_RATE_LIMIT,
    ErrorCode.SERVICE_RATE_LIMIT,
    ErrorCode.TENANT_RATE_LIMIT,
    ErrorCode.ADAPTER_TIMEOUT,
    ErrorCode.UPSTREAM_UNAVAILABLE,
    ErrorCode.UPSTREAM_TIMEOUT,
    ErrorCode.DATABASE_CONNECTION_FAILED,
    ErrorCode.CACHE_CONNECTION_FAILED,
}

# 错误码默认消息
_ERROR_MESSAGES = {
    ErrorCode.INVALID_REQUEST: "Invalid request",
    ErrorCode.INVALID_PARAMETER: "Invalid parameter",
    ErrorCode.MISSING_PARAMETER: "Missing required parameter",
    ErrorCode.INVALID_CONTENT_TYPE: "Invalid content type",
    ErrorCode.INVALID_JSON: "Invalid JSON payload",
    ErrorCode.PAYLOAD_TOO_LARGE: "Request payload too large",
    ErrorCode.AUTHENTICATION_REQUIRED: "Authentication required",
    ErrorCode.INVALID_CREDENTIALS: "Invalid credentials",
    ErrorCode.TOKEN_EXPIRED: "Token has expired",
    ErrorCode.TOKEN_INVALID: "Invalid token",
    ErrorCode.API_KEY_INVALID: "Invalid API key",
    ErrorCode.API_KEY_EXPIRED: "API key has expired",
    ErrorCode.PERMISSION_DENIED: "Permission denied",
    ErrorCode.INSUFFICIENT_SCOPE: "Insufficient scope for this operation",
    ErrorCode.RESOURCE_ACCESS_DENIED: "Access to resource denied",
    ErrorCode.OPERATION_NOT_ALLOWED: "Operation not allowed",
    ErrorCode.RESOURCE_NOT_FOUND: "Resource not found",
    ErrorCode.SERVICE_NOT_FOUND: "Service not found",
    ErrorCode.ADAPTER_NOT_FOUND: "Adapter not found",
    ErrorCode.SESSION_NOT_FOUND: "Session not found",
    ErrorCode.TASK_NOT_FOUND: "Task not found",
    ErrorCode.USER_NOT_FOUND: "User not found",
    ErrorCode.THREAD_NOT_FOUND: "Thread not found",
    ErrorCode.RATE_LIMIT_EXCEEDED: "Rate limit exceeded",
    ErrorCode.GLOBAL_RATE_LIMIT: "Global rate limit exceeded",
    ErrorCode.USER_RATE_LIMIT: "User rate limit exceeded",
    ErrorCode.IP_RATE_LIMIT: "IP rate limit exceeded",
    ErrorCode.SERVICE_RATE_LIMIT: "Service rate limit exceeded",
    ErrorCode.TENANT_RATE_LIMIT: "Tenant rate limit exceeded",
    ErrorCode.INTERNAL_ERROR: "Internal server error",
    ErrorCode.CONFIGURATION_ERROR: "Configuration error",
    ErrorCode.INITIALIZATION_ERROR: "Initialization error",
    ErrorCode.SERVICE_UNAVAILABLE: "Service unavailable",
    ErrorCode.SERVICE_DEGRADED: "Service degraded",
    ErrorCode.NO_HEALTHY_INSTANCE: "No healthy instance available",
    ErrorCode.CIRCUIT_BREAKER_OPEN: "Circuit breaker is open",
    ErrorCode.TIMEOUT: "Request timeout",
    ErrorCode.REQUEST_TIMEOUT: "Request timeout",
    ErrorCode.CONNECTION_TIMEOUT: "Connection timeout",
    ErrorCode.READ_TIMEOUT: "Read timeout",
    ErrorCode.CONCURRENCY_LIMIT: "Concurrency limit exceeded",
    ErrorCode.QUEUE_FULL: "Request queue is full",
    ErrorCode.SESSION_ERROR: "Session error",
    ErrorCode.SESSION_EXPIRED: "Session has expired",
    ErrorCode.SESSION_INVALID_STATE: "Invalid session state",
    ErrorCode.TASK_ERROR: "Task error",
    ErrorCode.TASK_CANCELLED: "Task was cancelled",
    ErrorCode.TASK_FAILED: "Task failed",
    ErrorCode.TASK_ALREADY_EXISTS: "Task already exists",
    ErrorCode.QUOTA_EXCEEDED: "Quota exceeded",
    ErrorCode.USAGE_LIMIT_EXCEEDED: "Usage limit exceeded",
    ErrorCode.CREDITS_EXHAUSTED: "Credits exhausted",
    ErrorCode.ADAPTER_ERROR: "Adapter error",
    ErrorCode.ADAPTER_CONNECTION_FAILED: "Failed to connect to adapter",
    ErrorCode.ADAPTER_RESPONSE_INVALID: "Invalid response from adapter",
    ErrorCode.ADAPTER_TIMEOUT: "Adapter request timeout",
    ErrorCode.UPSTREAM_ERROR: "Upstream service error",
    ErrorCode.UPSTREAM_UNAVAILABLE: "Upstream service unavailable",
    ErrorCode.UPSTREAM_TIMEOUT: "Upstream service timeout",
    ErrorCode.UPSTREAM_RESPONSE_INVALID: "Invalid response from upstream",
    ErrorCode.DATABASE_ERROR: "Database error",
    ErrorCode.DATABASE_CONNECTION_FAILED: "Failed to connect to database",
    ErrorCode.DATABASE_QUERY_FAILED: "Database query failed",
    ErrorCode.DATABASE_TRANSACTION_FAILED: "Database transaction failed",
    ErrorCode.CACHE_ERROR: "Cache error",
    ErrorCode.CACHE_CONNECTION_FAILED: "Failed to connect to cache",
    ErrorCode.CACHE_OPERATION_FAILED: "Cache operation failed",
}
