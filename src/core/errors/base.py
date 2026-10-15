"""
统一异常基类

所有网关异常都继承自 GatewayException，提供：
- 错误码 (code)
- 错误消息 (message)
- 详细信息 (details)
- 是否可重试 (retryable)
- HTTP 状态码映射
"""

from __future__ import annotations

from typing import Any

from .codes import ErrorCategory, ErrorCode


class GatewayException(Exception):
    """
    网关异常基类

    所有网关异常都应继承此类，提供统一的错误处理接口。

    Attributes:
        code: 错误码
        message: 人类可读的错误消息
        details: 额外的错误详情（可选）
        retryable: 是否建议客户端重试
        retry_after: 建议的重试等待时间（秒）
        trace_id: 请求追踪 ID（用于日志关联）
    """

    def __init__(
        self,
        code: ErrorCode,
        message: str | None = None,
        details: dict[str, Any] | None = None,
        retryable: bool | None = None,
        retry_after: int | None = None,
        trace_id: str | None = None,
    ):
        self.code = code
        self.message = message or code.message
        self.details = details or {}
        self.retryable = retryable if retryable is not None else code.retryable
        self.retry_after = retry_after
        self.trace_id = trace_id

        super().__init__(self.message)

    @property
    def http_status(self) -> int:
        """获取对应的 HTTP 状态码"""
        return self.code.http_status

    @property
    def category(self) -> ErrorCategory:
        """获取错误类别"""
        return self.code.category

    def to_dict(self, include_trace: bool = True) -> dict[str, Any]:
        """
        转换为字典格式，用于 API 响应

        Args:
            include_trace: 是否包含追踪信息（生产环境可设为 False）
        """
        result = {
            "error": {
                "code": self.code.value,
                "message": self.message,
            }
        }

        if self.details:
            result["error"]["details"] = self.details

        if self.retryable:
            result["error"]["retryable"] = True
            if self.retry_after:
                result["error"]["retry_after"] = self.retry_after

        if include_trace and self.trace_id:
            result["error"]["trace_id"] = self.trace_id

        return result

    def __repr__(self) -> str:
        return (
            f"{self.__class__.__name__}("
            f"code={self.code.name}, "
            f"message={self.message!r}, "
            f"details={self.details!r}"
            f")"
        )


class ClientError(GatewayException):
    """
    客户端错误基类 (1xxx)

    用于请求参数错误、认证失败、权限不足等客户端引起的错误。
    """

    pass


class ServerError(GatewayException):
    """
    服务端错误基类 (2xxx)

    用于服务不可用、超时、内部错误等服务端问题。
    """

    pass


class BusinessError(GatewayException):
    """
    业务逻辑错误基类 (3xxx)

    用于会话错误、任务错误、配额超限等业务层面的错误。
    """

    pass


class ExternalDependencyError(GatewayException):
    """
    外部依赖错误基类 (4xxx)

    用于适配器错误、上游服务错误、数据库错误等外部依赖问题。
    """

    pass
