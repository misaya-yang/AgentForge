"""Authentication and identity injection for the streaming middleware stack."""

from __future__ import annotations

import secrets
import uuid
from dataclasses import dataclass, field
from typing import Any

from ai_gateway_core.logging import get_logger
from starlette.types import ASGIApp, Message, Receive, Scope

from ...client_ip import get_client_ip_from_scope
from .base import PureASGIMiddleware

logger = get_logger("src.core.middleware.streaming")


@dataclass
class StreamingAuthConfig:
    """流式友好的鉴权配置"""

    jwt_enabled: bool = False
    jwt_secret: str = ""
    jwt_algorithms: list[str] = field(default_factory=lambda: ["HS256"])
    api_key_enabled: bool = False
    api_key_header: str = "X-API-Key"
    api_keys: list[str] = field(default_factory=list)
    guest_session_enabled: bool = True
    guest_session_header: str = "X-Guest-Session"
    anonymous_enabled: bool = True
    anonymous_cookie: str = "ag_anon_id"
    anonymous_header: str = "X-AG-Anonymous-Id"
    jwt_audience: str | None = None
    jwt_issuer: str | None = None
    whitelist_paths: list[str] = field(
        default_factory=lambda: [
            "/health",
            "/health/live",
            "/health/ready",

            "/docs",
            "/openapi.json",
        ]
    )


class StreamingAuthMiddleware(PureASGIMiddleware):
    """
    流式友好的鉴权中间件

    对于流式路径，直接传递请求，仅注入用户信息到 scope["state"]。
    对于非流式路径，执行完整的鉴权流程。
    """

    def __init__(self, app: ASGIApp, config: StreamingAuthConfig):
        super().__init__(app)
        self.config = config

    async def process_streaming_request(self, scope: Scope, receive: Receive) -> None:
        """流式请求仅注入用户信息"""
        _ = receive
        # 确保 state 存在
        if "state" not in scope:
            scope["state"] = {}

        # 提取用户信息并注入 state
        user_info = self._extract_user_info(scope)
        scope["state"]["user_info"] = user_info

    async def process_request(self, scope: Scope, receive: Receive) -> bool:
        """非流式请求执行完整鉴权"""
        _ = receive
        path = scope.get("path", "")

        # 白名单路径跳过
        if self._is_whitelisted(path):
            return True

        # 确保 state 存在
        if "state" not in scope:
            scope["state"] = {}

        # 提取用户信息
        user_info = self._extract_user_info(scope)
        scope["state"]["user_info"] = user_info

        return True

    async def process_response_start(self, scope: Scope, message: Message) -> Message:
        """添加用户信息到响应头"""
        user_info = scope.get("state", {}).get("user_info")
        if user_info and message["type"] == "http.response.start":
            headers = list(message.get("headers", []))
            headers = [
                (name, value)
                for name, value in headers
                if name.lower() not in {b"x-user-id", b"x-user-type"}
            ]
            headers.append((b"x-user-id", user_info.get("user_id", "unknown").encode()))
            headers.append((b"x-user-type", user_info.get("user_type", "unknown").encode()))
            message = {**message, "headers": headers}
        return message

    def _extract_user_info(self, scope: Scope) -> dict[str, Any]:
        """从请求头提取用户信息

        Supports:
        - Authorization: Bearer <jwt> header
        - X-API-Key header
        - X-Guest-Session header
        - X-AG-Anonymous-Id header or cookie
        """
        headers = dict(scope.get("headers", []))
        client_ip = self._get_client_ip(scope)

        # Try to extract and VERIFY JWT user if present
        # CRITICAL: Must verify signature before trusting claims
        auth_header = headers.get(b"authorization", b"").decode()
        if auth_header.lower().startswith("bearer ") and getattr(self.config, "jwt_enabled", True):
            jwt_secret = getattr(self.config, "jwt_secret", "")
            jwt_algorithms = getattr(self.config, "jwt_algorithms", ["HS256"])

            # Skip JWT auth if no secret configured
            if not jwt_secret:
                logger.debug("JWT secret not configured, skipping JWT authentication")
            else:
                try:
                    # SPO-02 / GW3: verify ONCE here with the same strictness
                    # as the API deps (same canonical decoder, same secret /
                    # algorithms / audience / issuer sources), and stash the
                    # verified claims so deps reuse them instead of decoding
                    # the token a second time.
                    from ...auth.jwt import decode_jwt_token
                    from ...auth.jwt_config import get_jwt_algorithms, get_jwt_secret

                    token = auth_header.split(" ", 1)[1]

                    payload = decode_jwt_token(
                        token,
                        secret=get_jwt_secret(jwt_secret),
                        algorithms=get_jwt_algorithms(jwt_algorithms),
                        audience=getattr(self.config, "jwt_audience", None),
                        issuer=getattr(self.config, "jwt_issuer", None),
                    )
                    scope.setdefault("state", {})["verified_jwt_claims"] = payload

                    user_id = str(payload.get("sub") or payload.get("user_id") or "")
                    if user_id:
                        return {
                            "user_id": user_id,
                            "user_type": "user",
                            "tenant_id": str(payload.get("tenant_id", "")),
                            "tier": str(payload.get("tier", "normal")),
                            "is_authenticated": True,
                            "ip": client_ip,
                            "roles": payload.get("roles", ["user"]),
                        }
                except Exception as e:
                    # Same never-reject contract as before: a bad token falls
                    # through to API key / guest / anonymous; the API deps
                    # still raise the strict 401 on their own verification.
                    logger.warning(f"JWT verification failed from {client_ip}: {e}")
                # Fall through to other auth methods or anonymous

        # Try configured static API keys. Unknown client-supplied keys must not
        # become authenticated identities before endpoint-level API key checks.
        api_key = headers.get(self.config.api_key_header.lower().encode(), b"").decode()
        if api_key and self.config.api_key_enabled and self._is_configured_api_key(api_key):
            import hashlib

            key_hash = hashlib.sha256(api_key.encode()).hexdigest()[:16]
            return {
                "user_id": f"apikey:{key_hash}",
                "user_type": "user",
                "tenant_id": "",
                "tier": "normal",
                "is_authenticated": True,
                "ip": client_ip,
                "roles": ["user"],
            }
        if api_key:
            logger.warning("Unverified streaming API key ignored from %s", client_ip)

        # Try guest session
        guest_session = headers.get(self.config.guest_session_header.lower().encode(), b"").decode()
        if guest_session and self.config.guest_session_enabled:
            # Validate session ID format: must be UUID v4 (what GuestSessionManager generates).
            # Blocks arbitrary strings like "admin", "../../etc", SQL injection attempts.
            if not self._is_valid_session_id(guest_session):
                logger.warning(
                    "Rejected malformed guest session ID from %s: %s",
                    client_ip, guest_session[:32],
                )
                # Fall through to anonymous — do NOT trust the malformed ID
            else:
                return {
                    "user_id": guest_session,
                    "user_type": "guest",
                    "tenant_id": "public",
                    "tier": "anonymous",
                    "is_authenticated": False,
                    "session_id": guest_session,
                    "ip": client_ip,
                    "roles": ["guest"],
                }

        # Fallback to anonymous
        anon_id_raw = (
            headers.get(b"x-ag-anonymous-id", b"").decode()
            or self._extract_cookie(headers, self.config.anonymous_cookie)
        )
        # Sanitize anonymous ID: strip control chars, limit length,
        # reject values that look like header injection attempts.
        anon_id = self._sanitize_anon_id(anon_id_raw) or client_ip

        return {
            "user_id": f"anon:{anon_id}",
            "user_type": "anonymous",
            "tenant_id": "public",
            "tier": "anonymous",
            "is_authenticated": False,
            "ip": client_ip,
            "roles": ["guest"],
        }

    def _extract_cookie(self, headers: dict[bytes, bytes], cookie_name: str) -> str:
        """Extract a specific cookie value from headers"""
        cookie_header = headers.get(b"cookie", b"").decode()
        if cookie_header:
            for part in cookie_header.split(";"):
                if "=" in part:
                    name, value = part.strip().split("=", 1)
                    if name == cookie_name:
                        return value
        return ""

    def _is_configured_api_key(self, api_key: str) -> bool:
        return any(
            secrets.compare_digest(api_key, configured)
            for configured in self.config.api_keys
        )

    @staticmethod
    def _is_valid_session_id(value: str) -> bool:
        """Validate guest session ID format (defense against identity spoofing).

        Accepts:
        - UUID format: standard uuid.UUID strings
        - Guest prefix: ``guest_{hex}`` (what GuestSessionManager.create_session generates)

        Rejects arbitrary strings to prevent identity spoofing ("admin"),
        header injection, and path traversal via session IDs.
        """
        if not value or len(value) > 128:
            return False
        # Block control characters and whitespace (header injection / path traversal)
        if not value.isprintable() or " " in value:
            return False
        # guest_{uuid_hex} format
        if value.startswith("guest_"):
            suffix = value[6:].lower()
            return len(suffix) >= 16 and all(ch in "0123456789abcdef-" for ch in suffix)
        # Standard UUID format
        try:
            parsed = uuid.UUID(value)
            return parsed.version == 4 and str(parsed) == value.lower()
        except (ValueError, AttributeError):
            return False

    @staticmethod
    def _sanitize_anon_id(value: str) -> str:
        """Sanitize anonymous ID: strip control chars, limit length, block injection."""
        if not value:
            return ""
        allowed = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789._:-"
        cleaned = "".join(ch for ch in value if ch in allowed)
        # Limit length — anonymous IDs should be short client-generated tokens
        return cleaned[:64]

    def _is_whitelisted(self, path: str) -> bool:
        """检查路径是否在白名单"""
        return any(path == wp or path.startswith(wp + "/") for wp in self.config.whitelist_paths)

    def _get_client_ip(self, scope: Scope) -> str:
        """获取客户端 IP"""
        return get_client_ip_from_scope(scope)
