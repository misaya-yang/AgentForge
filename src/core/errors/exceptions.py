"""
具体异常类定义

按错误类别组织，每个异常类对应一个或多个错误码。
"""

from __future__ import annotations

from typing import Any

from .base import BusinessError, ClientError, ExternalDependencyError, ServerError
from .codes import ErrorCode

# ============ 客户端错误 (1xxx) ============


class ValidationError(ClientError):
    """请求参数验证错误"""

    def __init__(
        self,
        message: str = "Validation failed",
        field: str | None = None,
        errors: list[dict[str, Any]] | None = None,
        **kwargs,
    ):
        details = kwargs.pop("details", {})
        if field:
            details["field"] = field
        if errors:
            details["errors"] = errors

        super().__init__(
            code=ErrorCode.INVALID_PARAMETER,
            message=message,
            details=details,
            **kwargs,
        )


class AuthenticationError(ClientError):
    """认证错误"""

    def __init__(
        self,
        code: ErrorCode = ErrorCode.AUTHENTICATION_REQUIRED,
        message: str | None = None,
        **kwargs,
    ):
        super().__init__(code=code, message=message, **kwargs)

    @classmethod
    def token_expired(cls, **kwargs) -> AuthenticationError:
        return cls(code=ErrorCode.TOKEN_EXPIRED, **kwargs)

    @classmethod
    def token_invalid(cls, **kwargs) -> AuthenticationError:
        return cls(code=ErrorCode.TOKEN_INVALID, **kwargs)

    @classmethod
    def api_key_invalid(cls, **kwargs) -> AuthenticationError:
        return cls(code=ErrorCode.API_KEY_INVALID, **kwargs)


class AuthorizationError(ClientError):
    """授权错误"""

    def __init__(
        self,
        message: str = "Permission denied",
        resource: str | None = None,
        action: str | None = None,
        **kwargs,
    ):
        details = kwargs.pop("details", {})
        if resource:
            details["resource"] = resource
        if action:
            details["action"] = action

        super().__init__(
            code=ErrorCode.PERMISSION_DENIED,
            message=message,
            details=details,
            **kwargs,
        )


class ResourceNotFoundError(ClientError):
    """资源不存在错误"""

    def __init__(
        self,
        resource_type: str,
        resource_id: str,
        code: ErrorCode | None = None,
        **kwargs,
    ):
        # 根据资源类型选择合适的错误码
        if code is None:
            code = _RESOURCE_TYPE_CODES.get(resource_type, ErrorCode.RESOURCE_NOT_FOUND)

        super().__init__(
            code=code,
            message=f"{resource_type} not found: {resource_id}",
            details={"resource_type": resource_type, "resource_id": resource_id},
            **kwargs,
        )


_RESOURCE_TYPE_CODES = {
    "service": ErrorCode.SERVICE_NOT_FOUND,
    "adapter": ErrorCode.ADAPTER_NOT_FOUND,
    "session": ErrorCode.SESSION_NOT_FOUND,
    "task": ErrorCode.TASK_NOT_FOUND,
    "user": ErrorCode.USER_NOT_FOUND,
    "thread": ErrorCode.THREAD_NOT_FOUND,
}


class RateLimitError(ClientError):
    """限流错误"""

    def __init__(
        self,
        dimension: str = "global",
        limit: int = 0,
        remaining: int = 0,
        reset_at: int = 0,
        retry_after: int = 0,
        **kwargs,
    ):
        # 根据维度选择错误码
        code = _RATE_LIMIT_DIMENSION_CODES.get(dimension, ErrorCode.RATE_LIMIT_EXCEEDED)

        super().__init__(
            code=code,
            message=f"Rate limit exceeded ({dimension})",
            details={
                "dimension": dimension,
                "limit": limit,
                "remaining": remaining,
                "reset_at": reset_at,
            },
            retryable=True,
            retry_after=retry_after if retry_after > 0 else None,
            **kwargs,
        )


_RATE_LIMIT_DIMENSION_CODES = {
    "global": ErrorCode.GLOBAL_RATE_LIMIT,
    "user": ErrorCode.USER_RATE_LIMIT,
    "ip": ErrorCode.IP_RATE_LIMIT,
    "service": ErrorCode.SERVICE_RATE_LIMIT,
    "tenant": ErrorCode.TENANT_RATE_LIMIT,
}


# ============ 服务端错误 (2xxx) ============


class InternalError(ServerError):
    """内部错误"""

    def __init__(
        self,
        message: str = "Internal server error",
        cause: str | None = None,
        **kwargs,
    ):
        details = kwargs.pop("details", {})
        if cause:
            details["cause"] = cause

        super().__init__(
            code=ErrorCode.INTERNAL_ERROR,
            message=message,
            details=details,
            **kwargs,
        )


class ServiceUnavailableError(ServerError):
    """服务不可用错误"""

    def __init__(
        self,
        service_id: str | None = None,
        reason: str | None = None,
        **kwargs,
    ):
        details = kwargs.pop("details", {})
        if service_id:
            details["service_id"] = service_id
        if reason:
            details["reason"] = reason

        super().__init__(
            code=ErrorCode.SERVICE_UNAVAILABLE,
            message=f"Service unavailable: {service_id}" if service_id else "Service unavailable",
            details=details,
            retryable=True,
            **kwargs,
        )


class CircuitBreakerError(ServerError):
    """熔断器打开错误"""

    def __init__(
        self,
        service_id: str,
        recovery_time: int | None = None,
        **kwargs,
    ):
        details = {"service_id": service_id}
        if recovery_time:
            details["recovery_time"] = recovery_time

        super().__init__(
            code=ErrorCode.CIRCUIT_BREAKER_OPEN,
            message=f"Circuit breaker open for service: {service_id}",
            details=details,
            retryable=True,
            retry_after=recovery_time,
            **kwargs,
        )


class TimeoutError(ServerError):
    """超时错误"""

    def __init__(
        self,
        operation: str = "request",
        timeout: float | None = None,
        code: ErrorCode = ErrorCode.TIMEOUT,
        **kwargs,
    ):
        details = kwargs.pop("details", {})
        details["operation"] = operation
        if timeout:
            details["timeout"] = timeout

        super().__init__(
            code=code,
            message=f"{operation.capitalize()} timeout",
            details=details,
            retryable=True,
            **kwargs,
        )


class ConcurrencyLimitError(ServerError):
    """并发限制错误"""

    def __init__(
        self,
        service_id: str,
        limit: int,
        **kwargs,
    ):
        super().__init__(
            code=ErrorCode.CONCURRENCY_LIMIT,
            message=f"Concurrency limit exceeded for service: {service_id}",
            details={"service_id": service_id, "limit": limit},
            retryable=True,
            **kwargs,
        )


# ============ 业务逻辑错误 (3xxx) ============


class SessionError(BusinessError):
    """会话错误"""

    def __init__(
        self,
        session_id: str | None = None,
        reason: str | None = None,
        code: ErrorCode = ErrorCode.SESSION_ERROR,
        **kwargs,
    ):
        details = kwargs.pop("details", {})
        if session_id:
            details["session_id"] = session_id
        if reason:
            details["reason"] = reason

        message = f"Session error: {reason}" if reason else "Session error"
        if session_id:
            message = (
                f"Session error ({session_id}): {reason}"
                if reason
                else f"Session error: {session_id}"
            )

        super().__init__(
            code=code,
            message=message,
            details=details,
            **kwargs,
        )


class TaskError(BusinessError):
    """任务错误"""

    def __init__(
        self,
        task_id: str,
        reason: str | None = None,
        code: ErrorCode = ErrorCode.TASK_ERROR,
        **kwargs,
    ):
        details = kwargs.pop("details", {})
        details["task_id"] = task_id
        if reason:
            details["reason"] = reason

        super().__init__(
            code=code,
            message=f"Task error ({task_id}): {reason}" if reason else f"Task error: {task_id}",
            details=details,
            **kwargs,
        )

    @classmethod
    def cancelled(cls, task_id: str, **kwargs) -> TaskError:
        return cls(task_id=task_id, code=ErrorCode.TASK_CANCELLED, reason="cancelled", **kwargs)

    @classmethod
    def failed(cls, task_id: str, reason: str, **kwargs) -> TaskError:
        return cls(task_id=task_id, code=ErrorCode.TASK_FAILED, reason=reason, **kwargs)


class QuotaExceededError(BusinessError):
    """配额超限错误"""

    def __init__(
        self,
        quota_type: str,
        limit: int | None = None,
        used: int | None = None,
        **kwargs,
    ):
        details = kwargs.pop("details", {})
        details["quota_type"] = quota_type
        if limit is not None:
            details["limit"] = limit
        if used is not None:
            details["used"] = used

        super().__init__(
            code=ErrorCode.QUOTA_EXCEEDED,
            message=f"Quota exceeded: {quota_type}",
            details=details,
            **kwargs,
        )


# ============ 外部依赖错误 (4xxx) ============


class AdapterError(ExternalDependencyError):
    """适配器错误"""

    def __init__(
        self,
        adapter_type: str,
        reason: str | None = None,
        code: ErrorCode = ErrorCode.ADAPTER_ERROR,
        **kwargs,
    ):
        details = kwargs.pop("details", {})
        details["adapter_type"] = adapter_type
        if reason:
            details["reason"] = reason

        super().__init__(
            code=code,
            message=f"Adapter error ({adapter_type}): {reason}"
            if reason
            else f"Adapter error: {adapter_type}",
            details=details,
            **kwargs,
        )


class UpstreamServiceError(ExternalDependencyError):
    """上游服务错误"""

    def __init__(
        self,
        service_name: str,
        status_code: int | None = None,
        reason: str | None = None,
        code: ErrorCode = ErrorCode.UPSTREAM_ERROR,
        **kwargs,
    ):
        details = kwargs.pop("details", {})
        details["upstream_service"] = service_name
        if status_code:
            details["status_code"] = status_code
        if reason:
            details["reason"] = reason

        message = f"Upstream service error ({service_name})"
        if reason:
            message = f"{message}: {reason}"

        super().__init__(
            code=code,
            message=message,
            details=details,
            **kwargs,
        )


class DatabaseError(ExternalDependencyError):
    """数据库错误"""

    def __init__(
        self,
        operation: str | None = None,
        reason: str | None = None,
        code: ErrorCode = ErrorCode.DATABASE_ERROR,
        **kwargs,
    ):
        details = kwargs.pop("details", {})
        if operation:
            details["operation"] = operation
        if reason:
            details["reason"] = reason

        message = "Database error"
        if operation:
            message = f"Database error during {operation}"
        if reason:
            message = f"{message}: {reason}"

        super().__init__(
            code=code,
            message=message,
            details=details,
            **kwargs,
        )


class CacheError(ExternalDependencyError):
    """缓存错误"""

    def __init__(
        self,
        operation: str | None = None,
        reason: str | None = None,
        code: ErrorCode = ErrorCode.CACHE_ERROR,
        **kwargs,
    ):
        details = kwargs.pop("details", {})
        if operation:
            details["operation"] = operation
        if reason:
            details["reason"] = reason

        message = "Cache error"
        if operation:
            message = f"Cache error during {operation}"
        if reason:
            message = f"{message}: {reason}"

        super().__init__(
            code=code,
            message=message,
            details=details,
            **kwargs,
        )
