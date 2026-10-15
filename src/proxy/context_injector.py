"""
上下文注入器

在转发请求时注入网关上下文信息到 HTTP 头部：
- X-GW-User-ID: 用户标识
- X-GW-Request-ID: 请求追踪 ID
- X-GW-Tenant-ID: 租户标识
- X-GW-User-Tier: 用户层级
"""

from __future__ import annotations

import hashlib
import uuid
from dataclasses import dataclass, field

from ai_gateway_core.logging import get_logger

logger = get_logger(__name__)


@dataclass
class RequestContext:
    """请求上下文"""

    # 用户信息
    user_id: str = ""
    api_key_id: str = ""
    tenant_id: str = ""
    user_tier: str = "anonymous"
    is_authenticated: bool = False
    roles: list[str] = field(default_factory=list)

    # 请求信息
    request_id: str = ""
    trace_id: str = ""
    span_id: str = ""
    traceparent: str = ""

    # 客户端信息
    client_ip: str = ""
    user_agent: str = ""

    # 原始请求头（用于选择性转发）
    original_headers: dict[str, str] = field(default_factory=dict)

    def __post_init__(self):
        if not self.request_id:
            self.request_id = str(uuid.uuid4())
        if not self.trace_id:
            self.trace_id = uuid.uuid4().hex
        if not self.span_id:
            self.span_id = uuid.uuid4().hex[:16]
        if not self.traceparent and self.trace_id and self.span_id:
            self.traceparent = f"00-{self.trace_id}-{self.span_id}-01"


class ContextInjector:
    """
    上下文注入器

    负责在转发请求时注入网关上下文信息。
    """

    # 网关注入的头部前缀
    GW_HEADER_PREFIX = "X-GW-"

    # 标准头部映射（网关内部格式）
    HEADER_MAPPINGS = {
        "user_id": "X-GW-User-ID",
        "tenant_id": "X-GW-Tenant-ID",
        "request_id": "X-GW-Request-ID",
        "trace_id": "X-Trace-ID",
        "span_id": "X-Span-ID",
        "user_tier": "X-GW-User-Tier",
        "client_ip": "X-Forwarded-For",
    }

    # LangGraph 兼容头部映射
    # LangGraph auth 期望的头部格式（不带 X-GW- 前缀）
    LANGGRAPH_HEADER_MAPPINGS = {
        "user_id": "X-User-Id",
        "tenant_id": "X-Tenant-Id",
        "user_type": "X-User-Type",
        "user_tier": "X-User-Tier",
        "user_name": "X-User-Name",
        "user_permissions": "X-User-Permissions",
    }

    # 需要转发的原始头部（白名单）
    FORWARD_HEADERS = {
        "authorization",
        "content-type",
        "accept",
        "accept-language",
        "accept-encoding",
        "user-agent",
        "x-request-id",
        "x-trace-id",
        "traceparent",
        "x-correlation-id",
    }

    # 禁止转发的头部（黑名单）
    BLOCKED_HEADERS = {
        "host",
        "content-length",
        "transfer-encoding",
        "connection",
        "keep-alive",
        "proxy-authenticate",
        "proxy-authorization",
        "te",
        "trailers",
        "upgrade",
    }

    # 敏感身份头部（客户端不允许传递，必须由网关强制覆盖）
    # 防止身份伪造攻击：攻击者可能发送 X-User-Id: admin 冒充管理员
    SENSITIVE_HEADERS = {
        "x-user-id",
        "x-gw-user-id",
        "x-user-type",
        "x-gw-user-type",
        "x-user-tier",
        "x-gw-user-tier",
        "x-gw-user-roles",
        "x-gw-authenticated",
        "x-tenant-id",
        "x-gw-tenant-id",
        "x-user-permissions",
        "x-gw-user-permissions",
        "x-user-name",
        "x-gw-user-name",
        "x-forwarded-for",
        "x-real-ip",
    }

    def __init__(
        self,
        inject_user_info: bool = True,
        inject_request_info: bool = True,
        forward_auth: bool = True,
        forward_all_headers: bool = False,
        custom_headers: dict[str, str] | None = None,
    ):
        """
        初始化上下文注入器

        Args:
            inject_user_info: 是否注入用户信息
            inject_request_info: 是否注入请求追踪信息
            forward_auth: 是否转发 Authorization 头
            custom_headers: 自定义静态头部
        """
        self.inject_user_info = inject_user_info
        self.inject_request_info = inject_request_info
        self.forward_auth = forward_auth
        self.forward_all_headers = forward_all_headers
        self.custom_headers = custom_headers or {}

    def build_headers(
        self,
        context: RequestContext,
        service_auth_token: str | None = None,
        forward_all_headers: bool | None = None,
    ) -> dict[str, str]:
        """
        构建转发请求的头部

        Args:
            context: 请求上下文
            service_auth_token: 服务内部认证 token（覆盖原始 Authorization）

        Returns:
            构建好的请求头部字典
        """
        headers: dict[str, str] = {}
        if forward_all_headers is None:
            forward_all_headers = self.forward_all_headers

        def _header_exists(name: str) -> bool:
            name_lower = name.lower()
            return any(k.lower() == name_lower for k in headers)

        def _set_if_missing(name: str, value: str | None) -> None:
            if value is None:
                return
            if not _header_exists(name):
                headers[name] = value

        def _force_set(name: str, value: str | None) -> None:
            """强制设置头部值（覆盖已有值），用于防止身份伪造"""
            if value is not None:
                headers[name] = value

        # 1. Forward original headers (transparent mode can pass all)
        # 记录原始请求头中的认证相关头部（脱敏处理）
        original_auth_headers = {
            k: "[PRESENT]"
            if k.lower() in ("authorization", "x-api-key")
            else (v[:30] + "..." if len(v) > 30 else v)
            for k, v in context.original_headers.items()
            if k.lower() in ("x-user-id", "x-user-type", "authorization", "x-api-key")
        }
        if original_auth_headers:
            logger.info(
                f"[ContextInjector] Original auth headers from request: {original_auth_headers}"
            )

        for name, value in context.original_headers.items():
            name_lower = name.lower()

            # Skip hop-by-hop headers
            if name_lower in self.BLOCKED_HEADERS:
                continue

            # 安全：剔除敏感身份头部（防止客户端伪造）
            if name_lower in self.SENSITIVE_HEADERS:
                logger.debug(f"[ContextInjector] Blocking sensitive header from client: {name}")
                continue

            # Transparent mode: forward everything except blocked
            if forward_all_headers:
                if name_lower == "authorization":
                    if self.forward_auth and not service_auth_token:
                        headers[name] = value
                else:
                    headers[name] = value
                continue

            # Non-transparent mode: forward only whitelist
            if name_lower in self.FORWARD_HEADERS:
                if name_lower == "authorization":
                    if self.forward_auth and not service_auth_token:
                        headers[name] = value
                else:
                    headers[name] = value

        # 2. 注入用户信息（使用 _force_set 强制覆盖，防止客户端伪造身份）
        logger.info(
            f"[ContextInjector] inject_user_info={self.inject_user_info}, user_id={context.user_id}"
        )
        if self.inject_user_info:
            # 2a. 网关标准头部（X-GW- 前缀）- 强制覆盖
            if context.user_id:
                _force_set(self.HEADER_MAPPINGS["user_id"], context.user_id)
            if context.tenant_id:
                _force_set(self.HEADER_MAPPINGS["tenant_id"], context.tenant_id)
            if context.user_tier:
                _force_set(self.HEADER_MAPPINGS["user_tier"], context.user_tier)

            # 用户角色（逗号分隔）- 强制覆盖
            if context.roles:
                _force_set("X-GW-User-Roles", ",".join(context.roles))

            # 认证状态 - 强制覆盖
            _force_set("X-GW-Authenticated", "true" if context.is_authenticated else "false")

            # 2b. LangGraph 兼容头部（无前缀，LangGraph auth 期望的格式）
            # LangGraph 需要 X-User-Id，即使是匿名用户也要发送一个有效的 ID
            original_user_id = context.user_id
            langgraph_user_id = original_user_id
            if not langgraph_user_id:
                # 没有 user_id，生成一个匿名 ID
                langgraph_user_id = f"anonymous-{uuid.uuid4().hex[:8]}"
                logger.info(f"[ContextInjector] Generated anonymous user_id: {langgraph_user_id}")
            elif langgraph_user_id.startswith("anon:"):
                # 移除 "anon:" 前缀，LangGraph 可能不接受这种格式
                # 转换为更标准的格式：anonymous-{hash}
                anon_suffix = langgraph_user_id[5:]  # 去掉 "anon:"
                if anon_suffix:
                    # 使用 hash 来保持一致性（同一 IP 生成相同的 ID）
                    hash_suffix = hashlib.md5(anon_suffix.encode()).hexdigest()[:8]
                    langgraph_user_id = f"anonymous-{hash_suffix}"
                else:
                    langgraph_user_id = f"anonymous-{uuid.uuid4().hex[:8]}"
                logger.info(
                    f"[ContextInjector] Transformed user_id: {original_user_id} -> {langgraph_user_id}"
                )

            # 2b. LangGraph 兼容头部 - 强制覆盖（防止身份伪造）
            _force_set(self.LANGGRAPH_HEADER_MAPPINGS["user_id"], langgraph_user_id)

            if context.tenant_id:
                _force_set(self.LANGGRAPH_HEADER_MAPPINGS["tenant_id"], context.tenant_id)
            if context.user_tier:
                _force_set(self.LANGGRAPH_HEADER_MAPPINGS["user_tier"], context.user_tier)

            # X-User-Type: user/guest/anonymous - 强制覆盖
            user_type = (
                "user"
                if context.is_authenticated
                else ("guest" if context.user_id else "anonymous")
            )
            _force_set(self.LANGGRAPH_HEADER_MAPPINGS["user_type"], user_type)

            # X-User-Name: 可选的用户名 - 强制覆盖
            if context.user_id:
                _force_set(self.LANGGRAPH_HEADER_MAPPINGS["user_name"], f"User-{context.user_id}")

            # X-User-Permissions: 基于角色推断的权限 - 强制覆盖
            permissions = ["read"]
            if context.is_authenticated:
                permissions.append("write")
            if "admin" in context.roles or context.user_tier == "admin":
                permissions.extend(["admin", "delete"])
            _force_set(self.LANGGRAPH_HEADER_MAPPINGS["user_permissions"], ",".join(permissions))

        # 3. 注入请求追踪信息
        if self.inject_request_info:
            _set_if_missing(self.HEADER_MAPPINGS["request_id"], context.request_id)
            if context.trace_id:
                _set_if_missing(self.HEADER_MAPPINGS["trace_id"], context.trace_id)
            if context.span_id:
                _set_if_missing(self.HEADER_MAPPINGS["span_id"], context.span_id)
            if context.traceparent:
                _set_if_missing("traceparent", context.traceparent)

        # 4. 客户端 IP（X-Forwarded-For）
        if context.client_ip:
            existing_xff = headers.get("X-Forwarded-For", "")
            if existing_xff:
                headers["X-Forwarded-For"] = f"{existing_xff}, {context.client_ip}"
            else:
                headers["X-Forwarded-For"] = context.client_ip

        # 5. 服务内部认证 token
        if service_auth_token:
            if "\r" in service_auth_token or "\n" in service_auth_token:
                raise ValueError("service_auth_token must not contain CRLF characters")
            if service_auth_token.startswith("Bearer "):
                headers["Authorization"] = service_auth_token
            elif ":" in service_auth_token:
                raise ValueError("service_auth_token must be a Bearer token or opaque API key")
            else:
                # 默认作为 X-Api-Key（LangGraph 兼容）
                headers["X-Api-Key"] = service_auth_token

        # 6. 自定义静态头部
        headers.update(self.custom_headers)

        # 7. 确保 Content-Type
        if "content-type" not in {k.lower() for k in headers}:
            headers["Content-Type"] = "application/json"

        # 记录关键的认证头部（INFO 级别用于调试，敏感值脱敏）
        auth_headers = {
            k: "[PRESENT]"
            if k.lower() in ("authorization", "x-api-key")
            else (v[:30] + "..." if len(v) > 30 else v)
            for k, v in headers.items()
            if k.lower()
            in (
                "x-user-id",
                "x-user-type",
                "x-user-name",
                "authorization",
                "x-api-key",
                "x-gw-user-id",
            )
        }
        if auth_headers:
            logger.info(f"[ContextInjector] Forwarding auth headers: {auth_headers}")

        return headers
