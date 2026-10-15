from __future__ import annotations

import asyncio
import contextlib
from typing import Any

import httpx

from ..models.service import ServiceDefinition
from .base import BaseConnector


class HTTPConnector(BaseConnector):
    """
    HTTP Connector with optimized connection pooling.

    Key optimizations for latency:
    - Connection pooling (reuse TCP connections)
    - Keep-alive connections with extended expiry
    - Optimized timeout settings for streaming
    - Explicit pool limits for better resource management
    - Connection pre-warming for critical paths
    - Context manager support for proper resource cleanup
    """

    # Default connection pool settings (optimized for high concurrency)
    DEFAULT_MAX_CONNECTIONS = 200
    DEFAULT_KEEPALIVE_CONNECTIONS = 50
    DEFAULT_KEEPALIVE_EXPIRY = 120.0  # 2 minutes

    # Default timeout settings
    DEFAULT_CONNECT_TIMEOUT = 3.0  # Fast connection establishment
    DEFAULT_READ_TIMEOUT = 300.0  # Long for streaming
    DEFAULT_WRITE_TIMEOUT = 60.0
    DEFAULT_POOL_TIMEOUT = 10.0  # Fail fast if pool exhausted

    def __init__(self, service: ServiceDefinition):
        super().__init__(service)
        config = service.connector_config or {}
        self.base_url = str(config.get("base_url", "")).rstrip("/")

        # Timeout configuration - optimized for streaming
        connect_timeout = config.get("connect_timeout", self.DEFAULT_CONNECT_TIMEOUT)
        default_timeout = config.get("timeout", service.timeout) or 60.0
        stream_timeout = config.get("stream_timeout", self.DEFAULT_READ_TIMEOUT)
        pool_timeout = config.get("pool_timeout", self.DEFAULT_POOL_TIMEOUT)

        timeout = httpx.Timeout(
            connect=connect_timeout,
            read=stream_timeout,
            write=default_timeout,
            pool=pool_timeout,
        )

        # Connection pool limits - optimized for high concurrency
        max_connections = config.get("max_connections", self.DEFAULT_MAX_CONNECTIONS)
        keepalive_connections = config.get(
            "keepalive_connections", self.DEFAULT_KEEPALIVE_CONNECTIONS
        )
        keepalive_expiry = config.get("keepalive_expiry", self.DEFAULT_KEEPALIVE_EXPIRY)

        limits = httpx.Limits(
            max_connections=max_connections,
            max_keepalive_connections=keepalive_connections,
            keepalive_expiry=keepalive_expiry,
        )

        headers = config.get("headers") or {}

        # HTTP/2 support - configurable, disabled by default for compatibility
        http2_enabled = config.get("http2", False)

        self._client = httpx.AsyncClient(
            base_url=self.base_url,
            timeout=timeout,
            limits=limits,
            headers=headers,
            http2=http2_enabled,
        )

        # Track if connections have been warmed up
        self._warmed_up = False

    async def __aenter__(self) -> HTTPConnector:
        """异步上下文管理器入口"""
        return self

    async def __aexit__(self, exc_type, exc_val, exc_tb) -> None:
        """异步上下文管理器出口，确保资源释放"""
        await self.close()

    async def warm_up_connections(self, count: int = 5) -> None:
        """
        Pre-warm connection pool by establishing connections.

        This can significantly reduce first-request latency for critical services
        by establishing TCP connections and TLS handshakes in advance.

        Args:
            count: Number of connections to pre-establish (default: 5)
        """
        if self._warmed_up:
            return

        async def warm_one():
            # HEAD request is lightweight; ignore failures during warm-up
            with contextlib.suppress(Exception):
                await self._client.head("/", timeout=2.0)

        # Warm up connections in parallel
        tasks = [warm_one() for _ in range(min(count, 10))]
        await asyncio.gather(*tasks, return_exceptions=True)
        self._warmed_up = True

    async def request(self, method: str, url: str, **kwargs: Any) -> Any:
        # 支持动态传递额外的 headers（与 client 默认 headers 合并）
        extra_headers = kwargs.pop("headers", None)
        if extra_headers:
            merged_headers = dict(self._client.headers)
            merged_headers.update(extra_headers)
            kwargs["headers"] = merged_headers

        response = await self._client.request(method, url, **kwargs)
        response.raise_for_status()
        content_type = response.headers.get("content-type", "")
        if "application/json" in content_type:
            return response.json()
        return response

    async def health_check(self, headers: dict = None) -> bool:
        """
        健康检查。

        Args:
            headers: 可选的认证头部，用于需要认证的服务
        """
        config = self.service.connector_config or {}
        endpoint = config.get("health_endpoint", "/health")
        # 对于需要认证的服务，可以配置 health_skip_auth 跳过健康检查的认证
        skip_auth = config.get("health_skip_auth", False)

        try:
            request_headers = None
            if headers and not skip_auth:
                # 合并客户端默认头部和传入的认证头部
                request_headers = dict(self._client.headers)
                request_headers.update(headers)

            response = await self._client.get(endpoint, headers=request_headers)
            # 403 表示认证问题，但服务本身是健康的
            return response.status_code < 500
        except Exception:
            return False

    async def close(self) -> None:
        """关闭 HTTP client，释放连接池资源"""
        if self._client and not self._client.is_closed:
            await self._client.aclose()
