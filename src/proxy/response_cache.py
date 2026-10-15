from __future__ import annotations

import base64
import hashlib
import json
import re
from dataclasses import dataclass
from datetime import datetime, timedelta, timezone
from typing import Any

from ai_gateway_core.logging import get_logger
from ..services.metrics.usage_parser import extract_model
from .config_loader import ProxyServiceConfig
from .context_injector import RequestContext

logger = get_logger(__name__)

_THREAD_RUNS_WAIT_PATTERN = re.compile(r"^/threads/[^/]+/runs/wait$")


@dataclass(frozen=True)
class CachedProxyResponse:
    status_code: int
    headers: dict[str, str]
    body: bytes


class ResponseCache:
    """Response cache adapter built on semantic_cache table."""

    MAX_CACHEABLE_BODY_BYTES = 512 * 1024
    CACHE_SCHEMA_VERSION = "proxy-response-v1"

    def __init__(self, database: Any | None = None):
        self.database = database

    @staticmethod
    def _normalize_path(path: str) -> str:
        normalized = (path or "").strip()
        if not normalized:
            return "/"
        if not normalized.startswith("/"):
            normalized = "/" + normalized
        if normalized != "/":
            normalized = normalized.rstrip("/")
        return normalized

    @classmethod
    def _is_supported_wait_path(cls, path: str) -> bool:
        normalized = cls._normalize_path(path)
        if normalized == "/runs/wait":
            return True
        return bool(_THREAD_RUNS_WAIT_PATTERN.match(normalized))

    @classmethod
    def should_use_cache(
        cls,
        *,
        config: ProxyServiceConfig,
        method: str,
        path: str,
        stream: bool,
    ) -> bool:
        if stream:
            return False
        if not config.cache_enabled:
            return False
        if method.upper() != "POST":
            return False
        return cls._is_supported_wait_path(path)

    @staticmethod
    def _stable_json_payload(payload: Any) -> str:
        try:
            return json.dumps(payload, ensure_ascii=False, separators=(",", ":"), sort_keys=True)
        except Exception:
            return "{}"

    @classmethod
    def _normalize_body(
        cls,
        body: bytes | None,
        *,
        parsed_body: dict[str, Any] | None = None,
    ) -> tuple[str, str | None]:
        if isinstance(parsed_body, dict):
            stable_payload = cls._stable_json_payload(parsed_body)
            model = extract_model(parsed_body)
            return stable_payload, (model or "").strip() or None
        if not body:
            return "{}", None
        try:
            payload = json.loads(body.decode("utf-8"))
            stable_payload = cls._stable_json_payload(payload)
            model = extract_model(payload)
            return stable_payload, (model or "").strip() or None
        except Exception:
            return base64.b64encode(body).decode("utf-8"), None

    @staticmethod
    def _normalize_query_params(params: dict[str, Any]) -> dict[str, Any]:
        normalized: dict[str, Any] = {}
        for key in sorted(params.keys()):
            value = params[key]
            if isinstance(value, list):
                normalized[key] = [str(v) for v in value]
            else:
                normalized[key] = str(value)
        return normalized

    def _build_cache_hash(
        self,
        *,
        context: RequestContext,
        config: ProxyServiceConfig,
        method: str,
        path: str,
        body: bytes | None,
        query_params: dict[str, Any],
        parsed_body: dict[str, Any] | None = None,
    ) -> tuple[str, str | None]:
        normalized_body, extracted_model = self._normalize_body(
            body,
            parsed_body=parsed_body,
        )
        normalized_path = self._normalize_path(path)

        identity = (context.api_key_id or context.user_id or "anonymous").strip() or "anonymous"
        cache_payload = {
            "tenant_id": context.tenant_id or "default",
            "identity": identity,
            "service_id": config.service_id,
            "service_name": config.service_name,
            "assistant_id": config.assistant_id or "",
            "graph_id": config.graph_id or "",
            "model": extracted_model or config.default_model or "",
            "method": method.upper(),
            "path": normalized_path,
            "query": self._normalize_query_params(query_params),
            "body": normalized_body,
        }
        digest = hashlib.sha256(
            self._stable_json_payload(cache_payload).encode("utf-8")
        ).hexdigest()
        return digest, (extracted_model or config.default_model)

    async def get_cached_response(
        self,
        *,
        config: ProxyServiceConfig,
        context: RequestContext,
        method: str,
        path: str,
        body: bytes | None,
        query_params: dict[str, Any],
        stream: bool,
        parsed_body: dict[str, Any] | None = None,
    ) -> tuple[str, str | None, CachedProxyResponse | None]:
        if not self.should_use_cache(config=config, method=method, path=path, stream=stream):
            return "BYPASS", None, None
        if not self.database or not getattr(self.database, "enabled", False):
            return "BYPASS", None, None

        cache_hash, _ = self._build_cache_hash(
            context=context,
            config=config,
            method=method,
            path=path,
            body=body,
            query_params=query_params,
            parsed_body=parsed_body,
        )

        try:
            row = await self.database.get_cache(config.service_id, cache_hash)
        except Exception as exc:
            logger.debug(f"[ResponseCache] cache read failed: {exc}")
            return "BYPASS", cache_hash, None

        if not row:
            return "MISS", cache_hash, None

        output_data = row.get("output_data")
        if not isinstance(output_data, dict):
            return "MISS", cache_hash, None
        metadata = row.get("metadata") if isinstance(row.get("metadata"), dict) else {}
        if metadata.get("cache_schema") != self.CACHE_SCHEMA_VERSION:
            return "MISS", cache_hash, None

        encoded_body = output_data.get("body_base64", "")
        try:
            cached_body = base64.b64decode(encoded_body) if encoded_body else b""
        except Exception:
            return "MISS", cache_hash, None

        headers = output_data.get("headers") if isinstance(output_data.get("headers"), dict) else {}
        status_code = int(output_data.get("status_code", 200) or 200)
        return (
            "HIT",
            cache_hash,
            CachedProxyResponse(
                status_code=status_code,
                headers={str(k): str(v) for k, v in headers.items()},
                body=cached_body,
            ),
        )

    async def save_response(
        self,
        *,
        cache_hash: str | None,
        config: ProxyServiceConfig,
        context: RequestContext,
        method: str,
        path: str,
        body: bytes | None,
        query_params: dict[str, Any],
        response_status: int,
        response_headers: dict[str, str],
        response_body: bytes,
        stream: bool,
        parsed_body: dict[str, Any] | None = None,
    ) -> None:
        if response_status >= 400:
            return
        if len(response_body or b"") > self.MAX_CACHEABLE_BODY_BYTES:
            return
        if not self.should_use_cache(config=config, method=method, path=path, stream=stream):
            return
        if not self.database or not getattr(self.database, "enabled", False):
            return

        model: str | None
        if cache_hash:
            _, extracted_model = self._normalize_body(body, parsed_body=parsed_body)
            model = extracted_model or config.default_model
        else:
            cache_hash, model = self._build_cache_hash(
                context=context,
                config=config,
                method=method,
                path=path,
                body=body,
                query_params=query_params,
                parsed_body=parsed_body,
            )
        ttl_seconds = max(int(config.cache_ttl or 0), 1)
        expires_at = datetime.now(timezone.utc) + timedelta(seconds=ttl_seconds)
        normalized_path = self._normalize_path(path)

        output_data = {
            "status_code": int(response_status),
            "headers": dict(response_headers),
            "body_base64": base64.b64encode(response_body or b"").decode("utf-8"),
        }
        metadata = {
            "cache_schema": self.CACHE_SCHEMA_VERSION,
            "tenant_id": context.tenant_id or "default",
            "user_id": context.user_id or "anonymous",
            "api_key_id": context.api_key_id or "",
            "service_id": config.service_id,
            "path": normalized_path,
            "method": method.upper(),
            "model": model or "",
        }
        try:
            await self.database.save_cache(
                service_id=config.service_id,
                input_hash=cache_hash,
                input_text=f"{method.upper()} {normalized_path}",
                output_text=(response_body or b"").decode("utf-8", errors="ignore")[:2000],
                output_data=output_data,
                metadata=metadata,
                expires_at=expires_at,
            )
        except Exception as exc:
            logger.debug(f"[ResponseCache] cache write failed: {exc}")
