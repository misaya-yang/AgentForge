"""Shared pure-ASGI middleware base class."""

from __future__ import annotations

from starlette.types import ASGIApp, Message, Receive, Scope, Send

from .paths import is_streaming_path


class PureASGIMiddleware:
    """
    纯 ASGI 中间件基类

    不使用 BaseHTTPMiddleware，避免缓冲 StreamingResponse。
    子类需要实现 process_request 和 process_response 方法。
    """

    def __init__(self, app: ASGIApp):
        self.app = app

    async def __call__(self, scope: Scope, receive: Receive, send: Send) -> None:
        if scope["type"] != "http":
            await self.app(scope, receive, send)
            return

        # 流式路径直接传递
        path = scope.get("path", "")
        if is_streaming_path(path):
            await self._handle_streaming(scope, receive, send)
            return

        # 非流式路径可以进行处理
        await self._handle_normal(scope, receive, send)

    async def _handle_streaming(self, scope: Scope, receive: Receive, send: Send) -> None:
        """处理流式请求 - 直接传递，不缓冲"""
        # 仅做必要的请求处理（如注入 state）
        await self.process_streaming_request(scope, receive)
        await self.app(scope, receive, send)

    async def _handle_normal(self, scope: Scope, receive: Receive, send: Send) -> None:
        """处理非流式请求 - 可以进行完整处理"""
        # 请求处理
        should_continue = await self.process_request(scope, receive)
        if not should_continue:
            # 如果返回 False，说明已经发送了响应
            return

        # 包装 send 以处理响应
        await self.app(scope, receive, self._wrap_send(scope, send))

    def _wrap_send(self, scope: Scope, send: Send) -> Send:
        """包装 send 以处理响应"""

        async def wrapped_send(message: Message) -> None:
            if message["type"] == "http.response.start":
                message = await self.process_response_start(scope, message)
            await send(message)

        return wrapped_send

    async def process_streaming_request(self, scope: Scope, receive: Receive) -> None:
        """处理流式请求（仅必要处理，如注入 state）"""
        pass

    async def process_request(self, scope: Scope, receive: Receive) -> bool:
        """处理请求，返回 True 继续，False 表示已发送响应"""
        _ = scope, receive
        return True

    async def process_response_start(self, scope: Scope, message: Message) -> Message:
        """处理响应开始，可以修改响应头"""
        _ = scope
        return message
