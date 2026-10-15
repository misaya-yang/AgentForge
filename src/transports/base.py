from __future__ import annotations

import importlib
import os
from abc import ABC, abstractmethod
from typing import Any

from ai_gateway_core.enums import TransportType

from ..models.service import ServiceDefinition

DEFAULT_IN_PROCESS_MODULE_PREFIXES = ("src.", "apps.", "ai_gateway_core.")


class BaseConnector(ABC):
    def __init__(self, service: ServiceDefinition):
        self.service = service

    @abstractmethod
    async def request(self, method: str, url: str, **kwargs: Any) -> Any:
        raise NotImplementedError

    async def get(self, url: str, **kwargs: Any) -> Any:
        return await self.request("GET", url, **kwargs)

    async def post(self, url: str, **kwargs: Any) -> Any:
        return await self.request("POST", url, **kwargs)

    async def health_check(self, _headers: dict = None) -> bool:
        """
        健康检查。

        Args:
            headers: 可选的认证头部，用于需要认证的服务
        """
        return True

    async def close(self) -> None:
        return None


class InProcessConnector(BaseConnector):
    def __init__(self, service: ServiceDefinition):
        super().__init__(service)
        config = service.connector_config or {}
        self.module_path = config.get("module") or config.get("graph_module")
        self.object_name = config.get("callable") or config.get("graph_name")
        self._callable: Any | None = None
        if self.module_path and self.object_name:
            self._validate_import_target(config)
            module = importlib.import_module(self.module_path)
            self._callable = getattr(module, self.object_name, None)

    def _validate_import_target(self, config: dict[str, Any]) -> None:
        configured_prefixes = config.get("allowed_module_prefixes")
        if configured_prefixes is None:
            configured_prefixes = os.getenv("GATEWAY_IN_PROCESS_ALLOWED_MODULE_PREFIXES", "")
        if isinstance(configured_prefixes, str):
            prefixes = [p.strip() for p in configured_prefixes.split(",") if p.strip()]
        else:
            prefixes = [str(p).strip() for p in configured_prefixes if str(p).strip()]
        if not prefixes:
            prefixes = list(DEFAULT_IN_PROCESS_MODULE_PREFIXES)

        if self.object_name.startswith("_") or "." in self.object_name:
            raise ValueError("Unsafe in-process callable name")
        if not any(
            self.module_path == prefix.rstrip(".") or self.module_path.startswith(prefix)
            for prefix in prefixes
        ):
            raise ValueError(f"In-process connector module is not allowed: {self.module_path}")

    async def request(self, _method: str, _url: str, **kwargs: Any) -> Any:
        if not self._callable:
            raise RuntimeError("No in-process callable configured")
        payload = kwargs.get("json") or kwargs.get("data") or kwargs
        result = self._callable(payload)
        if hasattr(result, "__await__"):
            result = await result
        return result


def create_connector(service: ServiceDefinition) -> BaseConnector:
    connector_type = service.connector_type
    if connector_type == TransportType.HTTP:
        from .http import HTTPConnector

        return HTTPConnector(service)
    if connector_type == TransportType.WEBSOCKET:
        from .websocket import WebSocketConnector

        return WebSocketConnector(service)
    if connector_type == TransportType.GRPC:
        from .grpc import GRPCConnector

        return GRPCConnector(service)
    if connector_type == TransportType.MESSAGE_QUEUE:
        from .message_queue import MessageQueueConnector

        return MessageQueueConnector(service)
    if connector_type == TransportType.IN_PROCESS:
        return InProcessConnector(service)
    raise ValueError(f"Unsupported connector type: {connector_type}")
