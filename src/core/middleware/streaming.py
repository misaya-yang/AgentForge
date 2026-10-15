"""
流式响应友好的纯 ASGI 中间件

解决 BaseHTTPMiddleware 缓冲 StreamingResponse 的问题。

关键点：
1. BaseHTTPMiddleware 的 call_next() 会缓冲整个响应体
2. 纯 ASGI 中间件直接传递 send/receive，不缓冲响应
3. 对于流式路径，必须使用纯 ASGI 实现

The implementation is split across ``._streaming`` modules. This facade keeps
the original import path and symbol surface stable for callers.
"""

from __future__ import annotations

# These imports intentionally preserve the historical public module surface.
import asyncio
import contextlib
import secrets
import time
import uuid
from dataclasses import dataclass, field
from typing import Any

from ai_gateway_core.logging import get_logger
from starlette.responses import JSONResponse
from starlette.types import ASGIApp, Message, Receive, Scope, Send

from ...services.metrics import get_metrics_recorder
from ..client_ip import get_client_ip_from_scope
from ._streaming import (
    STREAMING_KEYWORDS,
    STREAMING_PATH_PREFIXES,
    STREAMING_PATHS,
    STREAMING_SUFFIXES,
    PureASGIMiddleware,
    RequestContextBridgeMiddleware,
    SecurityHeadersMiddleware,
    StreamingAdmissionConfig,
    StreamingAnonymousConfig,
    StreamingAnonymousMiddleware,
    StreamingAuthConfig,
    StreamingAuthMiddleware,
    StreamingLogConfig,
    StreamingLoggingMiddleware,
    StreamingRateLimitConfig,
    StreamingRateLimitMiddleware,
    StreamingTracingConfig,
    StreamingTracingMiddleware,
    _is_valid_uuid,
    is_streaming_path,
)
from .rate_limit_http import RateLimitInfo, SlidingWindowRateLimiter

logger = get_logger(__name__)

# Preserve the historical dotted names used by repr, pickling, and diagnostics
# while implementations live in focused private modules.
for _compat_symbol in (
    PureASGIMiddleware,
    RequestContextBridgeMiddleware,
    StreamingAdmissionConfig,
    StreamingAnonymousConfig,
    StreamingAnonymousMiddleware,
    StreamingAuthConfig,
    StreamingAuthMiddleware,
    StreamingLogConfig,
    StreamingLoggingMiddleware,
    StreamingRateLimitConfig,
    StreamingRateLimitMiddleware,
    StreamingTracingConfig,
    StreamingTracingMiddleware,
    _is_valid_uuid,
    is_streaming_path,
):
    _compat_symbol.__module__ = __name__
del _compat_symbol

__all__ = [
    "ASGIApp",
    "Any",
    "JSONResponse",
    "Message",
    "PureASGIMiddleware",
    "RequestContextBridgeMiddleware",
    "RateLimitInfo",
    "Receive",
    "STREAMING_KEYWORDS",
    "STREAMING_PATHS",
    "STREAMING_PATH_PREFIXES",
    "STREAMING_SUFFIXES",
    "Scope",
    "Send",
    "SlidingWindowRateLimiter",
    "StreamingAdmissionConfig",
    "StreamingAnonymousConfig",
    "StreamingAnonymousMiddleware",
    "StreamingAuthConfig",
    "StreamingAuthMiddleware",
    "StreamingLogConfig",
    "StreamingLoggingMiddleware",
    "StreamingRateLimitConfig",
    "StreamingRateLimitMiddleware",
    "StreamingTracingConfig",
    "StreamingTracingMiddleware",
    "SecurityHeadersMiddleware",
    "annotations",
    "asyncio",
    "contextlib",
    "dataclass",
    "field",
    "get_client_ip_from_scope",
    "get_logger",
    "get_metrics_recorder",
    "is_streaming_path",
    "logger",
    "secrets",
    "time",
    "uuid",
]
