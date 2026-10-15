from __future__ import annotations

import json
from collections.abc import AsyncIterator
from typing import Any

import websockets

from ..models.service import ServiceDefinition
from .base import BaseConnector


class WebSocketConnection:
    def __init__(self, websocket):
        self._ws = websocket

    async def send_json(self, data: Any) -> None:
        await self._ws.send(json.dumps(data))

    async def recv_json(self) -> Any:
        msg = await self._ws.recv()
        return json.loads(msg)

    def __aiter__(self) -> AsyncIterator[Any]:
        return self

    async def __anext__(self) -> Any:
        msg = await self._ws.recv()
        return json.loads(msg)

    async def close(self) -> None:
        await self._ws.close()


class WebSocketConnector(BaseConnector):
    def __init__(self, service: ServiceDefinition):
        super().__init__(service)
        config = service.connector_config or {}
        self.base_url = str(config.get("base_url", "")).rstrip("/")
        self.path = str(config.get("ws_path", "/ws"))

    async def websocket(self) -> WebSocketConnection:
        ws = await websockets.connect(self.base_url + self.path)
        return WebSocketConnection(ws)

    async def request(self, method: str, url: str, **kwargs: Any) -> Any:
        raise NotImplementedError
