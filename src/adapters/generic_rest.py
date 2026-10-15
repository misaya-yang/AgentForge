from __future__ import annotations

import dataclasses
from enum import Enum
from typing import Any

from jsonpath_ng import parse as jsonpath_parse

from ai_gateway_core.exceptions import ValidationFailedError
from ai_gateway_core.enums import ContentType
from ..models.request import ContentItem, UnifiedRequest
from ..models.response import UnifiedResponse
from .base import ProtocolAdapter


def _to_primitive(value: Any) -> Any:
    if isinstance(value, Enum):
        return value.value
    if dataclasses.is_dataclass(value):
        return {k: _to_primitive(v) for k, v in dataclasses.asdict(value).items()}
    if isinstance(value, dict):
        return {k: _to_primitive(v) for k, v in value.items()}
    if isinstance(value, list):
        return [_to_primitive(v) for v in value]
    return value


class GenericRESTAdapter(ProtocolAdapter):
    def __init__(self, service):
        super().__init__(service)
        self.config = service.connector_config or {}
        self.request_mapping = self.config.get("request_mapping", {})
        self.response_mapping = self.config.get("response_mapping", {})

    async def invoke(self, request: UnifiedRequest) -> UnifiedResponse:
        service_request, method, endpoint = self._build_service_request(request)
        response = await self.connector.request(method=method, url=endpoint, json=service_request)
        outputs = self._build_outputs(response)
        return UnifiedResponse(
            request_id=request.request_id,
            status="success",
            outputs=outputs,
        )

    def _build_service_request(self, request: UnifiedRequest) -> tuple[dict[str, Any], str, str]:
        mapping = self.request_mapping or {}
        endpoint = mapping.get("endpoint") or self.config.get("endpoint") or "/"
        method = mapping.get("method") or self.config.get("method") or "POST"

        req_data = _to_primitive(request)
        body_map = mapping.get("body") or {}
        if not isinstance(body_map, dict):
            raise ValidationFailedError("request_mapping.body must be a dict")
        body: dict[str, Any] = {}
        for key, expr in body_map.items():
            body[key] = self._eval_expr(expr, req_data)
        return body, method, endpoint

    def _build_outputs(self, response: Any) -> list[ContentItem]:
        mapping = self.response_mapping or {}
        outputs_spec = mapping.get("outputs")
        resp_data = response
        if hasattr(response, "json"):
            try:
                resp_data = response.json()
            except Exception:
                resp_data = {"raw": getattr(response, "text", None)}

        if not outputs_spec:
            return [ContentItem(type=ContentType.JSON, data=resp_data)]

        outputs: list[ContentItem] = []
        for spec in outputs_spec:
            ctype = ContentType(spec.get("type", "text"))
            data_expr = spec.get("data")
            url_expr = spec.get("url")
            data_val = self._eval_expr(data_expr, resp_data) if data_expr else None
            url_val = self._eval_expr(url_expr, resp_data) if url_expr else None
            outputs.append(
                ContentItem(
                    type=ctype,
                    data=data_val,
                    url=url_val,
                    mime_type=spec.get("mime_type"),
                    metadata=spec.get("metadata"),
                )
            )
        return outputs

    def _eval_expr(self, expr: Any, data: Any) -> Any:
        if expr is None:
            return None
        if isinstance(expr, str) and expr.strip().startswith("$"):
            matches = [m.value for m in jsonpath_parse(expr).find(data)]
            if not matches:
                return None
            if len(matches) == 1:
                return matches[0]
            return matches
        return expr
