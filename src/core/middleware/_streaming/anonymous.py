"""Stable anonymous identity middleware for the pure ASGI stack."""

from __future__ import annotations

import uuid
from dataclasses import dataclass

from starlette.types import ASGIApp, Message, Receive, Scope, Send

from .base import PureASGIMiddleware


@dataclass
class StreamingAnonymousConfig:
    """流式友好的匿名身份配置"""

    enabled: bool = True
    header_name: str = "X-AG-Anonymous-Id"
    cookie_name: str = "ag_anon_id"
    ttl_days: int = 365
    same_site: str = "lax"


def _is_valid_uuid(value: str) -> bool:
    try:
        uuid.UUID(str(value))
        return True
    except (ValueError, AttributeError):
        return False


class StreamingAnonymousMiddleware(PureASGIMiddleware):
    """
    流式友好的匿名身份中间件

    使用纯 ASGI 实现，为匿名用户提供稳定的标识符。
    """

    def __init__(self, app: ASGIApp, config: StreamingAnonymousConfig):
        super().__init__(app)
        self.config = config

    async def __call__(self, scope: Scope, receive: Receive, send: Send) -> None:
        if scope["type"] != "http":
            await self.app(scope, receive, send)
            return

        if not self.config.enabled:
            await self.app(scope, receive, send)
            return

        # 提取或生成匿名 ID
        headers = dict(scope.get("headers", []))

        # 尝试从 header 获取
        raw_id = headers.get(self.config.header_name.lower().encode(), b"").decode()

        # 尝试从 cookie 获取
        if not raw_id:
            cookie_header = headers.get(b"cookie", b"").decode()
            if cookie_header:
                for part in cookie_header.split(";"):
                    if "=" in part:
                        name, value = part.strip().split("=", 1)
                        if name == self.config.cookie_name:
                            raw_id = value
                            break

        has_valid = bool(raw_id) and _is_valid_uuid(raw_id)
        anon_id = raw_id if has_valid else str(uuid.uuid4())

        # 注入到 state
        if "state" not in scope:
            scope["state"] = {}
        scope["state"]["anonymous_id"] = anon_id

        # 如果需要设置 cookie
        need_set_cookie = not has_valid

        async def anon_send(message: Message) -> None:
            if message["type"] == "http.response.start" and need_set_cookie:
                headers = list(message.get("headers", []))

                # 设置 cookie
                max_age = int(self.config.ttl_days) * 86400
                cookie_value = (
                    f"{self.config.cookie_name}={anon_id}; "
                    f"Max-Age={max_age}; "
                    f"Path=/; "
                    f"HttpOnly; "
                    f"SameSite={self.config.same_site}"
                )
                headers.append((b"set-cookie", cookie_value.encode()))

                # 设置 header
                headers.append((self.config.header_name.encode(), anon_id.encode()))

                message = {**message, "headers": headers}

            await send(message)

        await self.app(scope, receive, anon_send)
