from __future__ import annotations

from typing import Any

from ..models.service import ServiceDefinition
from .base import BaseConnector


class GRPCConnector(BaseConnector):
    def __init__(self, service: ServiceDefinition):
        super().__init__(service)

    async def request(self, method: str, url: str, **kwargs: Any) -> Any:
        raise NotImplementedError
