# 错误处理模块
from .base import (
    ClientError,
    ExternalDependencyError,
    GatewayException,
    ServerError,
)
from .codes import ErrorCategory, ErrorCode
from .exceptions import (
    # 外部依赖错误 (4xxx)
    AdapterError,
    AuthenticationError,
    AuthorizationError,
    CacheError,
    CircuitBreakerError,
    ConcurrencyLimitError,
    DatabaseError,
    # 服务端错误 (2xxx)
    InternalError,
    QuotaExceededError,
    RateLimitError,
    ResourceNotFoundError,
    ServiceUnavailableError,
    # 业务逻辑错误 (3xxx)
    SessionError,
    TaskError,
    TimeoutError,
    UpstreamServiceError,
    # 客户端错误 (1xxx)
    ValidationError,
)
from .handler import (
    ExceptionHandler,
    error_response,
    setup_exception_handlers,
)

__all__ = [
    # 错误码
    "ErrorCode",
    "ErrorCategory",
    # 基类
    "GatewayException",
    "ClientError",
    "ServerError",
    "ExternalDependencyError",
    # 客户端错误
    "ValidationError",
    "AuthenticationError",
    "AuthorizationError",
    "ResourceNotFoundError",
    "RateLimitError",
    # 服务端错误
    "InternalError",
    "ServiceUnavailableError",
    "CircuitBreakerError",
    "TimeoutError",
    "ConcurrencyLimitError",
    # 业务逻辑错误
    "SessionError",
    "TaskError",
    "QuotaExceededError",
    # 外部依赖错误
    "AdapterError",
    "UpstreamServiceError",
    "DatabaseError",
    "CacheError",
    # 处理器
    "ExceptionHandler",
    "error_response",
    "setup_exception_handlers",
]
