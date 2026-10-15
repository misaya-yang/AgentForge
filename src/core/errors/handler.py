"""
全局异常处理器

提供统一的异常处理和响应格式化。
"""

from __future__ import annotations

import logging
import traceback
from collections.abc import Callable
from typing import Any

from fastapi import FastAPI, Request
from fastapi.responses import JSONResponse
from pydantic import ValidationError as PydanticValidationError

from .base import GatewayException
from .codes import ErrorCode
from .exceptions import ValidationError

logger = logging.getLogger(__name__)


def error_response(
    code: ErrorCode | int,
    message: str,
    details: dict[str, Any] | None = None,
    trace_id: str | None = None,
    retryable: bool = False,
    retry_after: int | None = None,
) -> dict[str, Any]:
    """
    构建标准错误响应

    Args:
        code: 错误码
        message: 错误消息
        details: 额外详情
        trace_id: 请求追踪 ID
        retryable: 是否可重试
        retry_after: 建议重试等待时间

    Returns:
        标准化的错误响应字典
    """
    error_code = code if isinstance(code, int) else code.value

    result = {
        "error": {
            "code": error_code,
            "message": message,
        }
    }

    if details:
        result["error"]["details"] = details

    if retryable:
        result["error"]["retryable"] = True
        if retry_after:
            result["error"]["retry_after"] = retry_after

    if trace_id:
        result["error"]["trace_id"] = trace_id

    return result


class ExceptionHandler:
    """
    全局异常处理器

    集中处理所有异常类型，提供统一的错误响应格式。
    支持：
    - GatewayException 及其子类
    - Pydantic 验证错误
    - 原生 Python 异常
    - 自定义异常映射
    """

    def __init__(
        self,
        debug: bool = False,
        include_trace_id: bool = True,
        log_exceptions: bool = True,
    ):
        """
        Args:
            debug: 是否在响应中包含堆栈信息
            include_trace_id: 是否在响应中包含 trace_id
            log_exceptions: 是否记录异常日志
        """
        self.debug = debug
        self.include_trace_id = include_trace_id
        self.log_exceptions = log_exceptions
        self._custom_handlers: dict[type, Callable] = {}

    def register(self, exc_type: type) -> Callable:
        """
        注册自定义异常处理器的装饰器

        Usage:
            @handler.register(MyCustomException)
            def handle_my_exception(request, exc):
                return JSONResponse(...)
        """

        def decorator(func: Callable) -> Callable:
            self._custom_handlers[exc_type] = func
            return func

        return decorator

    def _get_trace_id(self, request: Request) -> str | None:
        """从请求中获取 trace_id"""
        # 优先从请求状态获取
        if hasattr(request.state, "trace_id"):
            return request.state.trace_id

        # 从请求头获取
        return request.headers.get("X-Trace-ID") or request.headers.get("X-Request-ID")

    def _log_exception(
        self,
        exc: Exception,
        request: Request,
        trace_id: str | None,
        level: int = logging.ERROR,
    ) -> None:
        """记录异常日志"""
        if not self.log_exceptions:
            return

        extra = {
            "trace_id": trace_id,
            "method": request.method,
            "path": request.url.path,
            "client_ip": request.client.host if request.client else None,
        }

        if isinstance(exc, GatewayException):
            extra["error_code"] = exc.code.value
            extra["error_details"] = exc.details

        logger.log(
            level,
            f"Exception occurred: {exc}",
            exc_info=True,
            extra=extra,
        )

    async def handle_gateway_exception(
        self,
        request: Request,
        exc: GatewayException,
    ) -> JSONResponse:
        """处理 GatewayException"""
        trace_id = self._get_trace_id(request)
        exc.trace_id = trace_id

        # 根据错误类别决定日志级别
        log_level = logging.WARNING if exc.category.value <= 1 else logging.ERROR
        self._log_exception(exc, request, trace_id, log_level)

        headers = {}
        if exc.retryable and exc.retry_after:
            headers["Retry-After"] = str(exc.retry_after)

        return JSONResponse(
            status_code=exc.http_status,
            content=exc.to_dict(include_trace=self.include_trace_id),
            headers=headers if headers else None,
        )

    async def handle_pydantic_validation_error(
        self,
        request: Request,
        exc: PydanticValidationError,
    ) -> JSONResponse:
        """处理 Pydantic 验证错误"""
        trace_id = self._get_trace_id(request)

        # 转换为我们的 ValidationError 格式
        errors = []
        for error in exc.errors():
            errors.append(
                {
                    "field": ".".join(str(loc) for loc in error["loc"]),
                    "message": error["msg"],
                    "type": error["type"],
                }
            )

        gateway_exc = ValidationError(
            message="Request validation failed",
            errors=errors,
            trace_id=trace_id,
        )

        self._log_exception(exc, request, trace_id, logging.WARNING)

        return JSONResponse(
            status_code=gateway_exc.http_status,
            content=gateway_exc.to_dict(include_trace=self.include_trace_id),
        )

    async def handle_generic_exception(
        self,
        request: Request,
        exc: Exception,
    ) -> JSONResponse:
        """处理未知异常"""
        trace_id = self._get_trace_id(request)
        self._log_exception(exc, request, trace_id)

        # 构建响应
        content = error_response(
            code=ErrorCode.INTERNAL_ERROR,
            message="Internal server error",
            trace_id=trace_id if self.include_trace_id else None,
        )

        # 在 debug 模式下包含堆栈信息
        if self.debug:
            content["error"]["debug"] = {
                "exception_type": type(exc).__name__,
                "exception_message": str(exc),
                "traceback": traceback.format_exc().split("\n"),
            }

        return JSONResponse(
            status_code=500,
            content=content,
        )

    async def __call__(
        self,
        request: Request,
        exc: Exception,
    ) -> JSONResponse:
        """统一异常处理入口"""

        # 检查自定义处理器
        for exc_type, handler in self._custom_handlers.items():
            if isinstance(exc, exc_type):
                return await handler(request, exc)

        # 处理 GatewayException
        if isinstance(exc, GatewayException):
            return await self.handle_gateway_exception(request, exc)

        # 处理 Pydantic 验证错误
        if isinstance(exc, PydanticValidationError):
            return await self.handle_pydantic_validation_error(request, exc)

        # 处理未知异常
        return await self.handle_generic_exception(request, exc)


def setup_exception_handlers(
    app: FastAPI,
    debug: bool = False,
    include_trace_id: bool = True,
) -> ExceptionHandler:
    """
    设置 FastAPI 应用的异常处理器

    Args:
        app: FastAPI 应用实例
        debug: 是否启用调试模式
        include_trace_id: 是否在响应中包含 trace_id

    Returns:
        ExceptionHandler 实例（可用于注册自定义处理器）
    """
    handler = ExceptionHandler(
        debug=debug,
        include_trace_id=include_trace_id,
    )

    # 注册 GatewayException 处理器
    @app.exception_handler(GatewayException)
    async def gateway_exception_handler(request: Request, exc: GatewayException):
        return await handler.handle_gateway_exception(request, exc)

    # 注册 Pydantic 验证错误处理器
    @app.exception_handler(PydanticValidationError)
    async def validation_exception_handler(request: Request, exc: PydanticValidationError):
        return await handler.handle_pydantic_validation_error(request, exc)

    from ..middleware.request_body_limit import RequestBodyLimitExceeded

    @app.exception_handler(RequestBodyLimitExceeded)
    async def request_body_limit_handler(request: Request, exc: RequestBodyLimitExceeded):
        _ = request, exc
        return JSONResponse(
            {"detail": "Request body too large"},
            status_code=413,
            headers={"Retry-After": "0"},
        )

    # 注册通用异常处理器
    @app.exception_handler(Exception)
    async def generic_exception_handler(request: Request, exc: Exception):
        return await handler.handle_generic_exception(request, exc)

    return handler
