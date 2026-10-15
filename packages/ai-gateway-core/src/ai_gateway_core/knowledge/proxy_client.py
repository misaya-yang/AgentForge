"""KB Proxy Client — HTTP client for the knowledge-service microservice.

Gateway-owned runtime paths call the knowledge-service through this shared
client.

Talks to KS over HTTP at ``KB_SERVICE_URL`` (default
``http://knowledge-service:8092``) with HMAC-signed requests via
``AI_PLATFORM_INTERNAL_TOKEN``.
"""

from __future__ import annotations

import logging
import os
import re
import threading
from dataclasses import dataclass, field
from typing import Any

import httpx

from ai_gateway_core.auth.gateway_secret import GatewaySecret
from ai_gateway_core.comm.client import (
    InternalServiceClient,
    InternalServiceClientConfig,
    InternalServiceHTTPError,
)
from ai_gateway_core.comm.retry import RetryPolicy

logger = logging.getLogger(__name__)

KB_SERVICE_URL = os.getenv("KB_SERVICE_URL", "http://knowledge-service:8092")
_TIMEOUT = httpx.Timeout(connect=5.0, read=30.0, write=10.0, pool=10.0)
_LIMITS = httpx.Limits(max_connections=50, max_keepalive_connections=10)

# Lazy singleton — constructed the first time a request is signed so
# deployments without the env set fail loud on first call (and let
# dev envs start without the secret). The ``threading.Lock`` guards
# against two concurrent first-requests each constructing their own
# ``GatewaySecret`` instance; not a correctness issue (both instances
# produce valid signatures) but wasteful and harder to reason about.
_gateway_secret_signer: GatewaySecret | None = None
_signer_lock = threading.Lock()


def _get_signer() -> GatewaySecret | None:
    """Return a signer if ``AI_PLATFORM_INTERNAL_TOKEN`` is set.

    Returns ``None`` in dev environments without the secret configured;
    callers then skip the HMAC header and rely on
    ``KNOWLEDGE_APP__ALLOW_ANONYMOUS=true`` on the KB side. In prod the
    secret is always set, so the signer is always constructed.
    """
    global _gateway_secret_signer
    if _gateway_secret_signer is not None:
        return _gateway_secret_signer
    with _signer_lock:
        if _gateway_secret_signer is not None:
            return _gateway_secret_signer
        secret = os.environ.get("AI_PLATFORM_INTERNAL_TOKEN")
        if not secret:
            return None
        _gateway_secret_signer = GatewaySecret(
            secret=secret,
            caller_service="gateway",
            audience="knowledge-service",
            allowed_path_prefixes=("/api/v1",),
        )
        return _gateway_secret_signer


@dataclass
class ProxyRetrieveResult:
    """Mimics the local RetrieveResult interface for compatibility."""

    text: str
    score: float
    segment_id: str | None = None
    document_id: str | None = None
    metadata: dict[str, Any] = field(default_factory=dict)
    content_type: str = "text"
    image_url: str | None = None
    vlm_description: str | None = None
    associated_images: tuple = ()


class KBProxyClient:
    """HTTP client to call KB microservice retrieve API.

    Drop-in replacement for KnowledgeService.retrieve() and list_datasets()
    when running in microservice mode.
    """

    def __init__(
        self,
        base_url: str | None = None,
        *,
        timeout: httpx.Timeout | float | None = None,
        limits: httpx.Limits | None = None,
        transport: httpx.AsyncBaseTransport | None = None,
        retry_policy: RetryPolicy | None = None,
        gateway_secret: GatewaySecret | None = None,
    ) -> None:
        self.base_url = (base_url or KB_SERVICE_URL).rstrip("/")
        self.timeout = _coerce_timeout(timeout) if timeout is not None else _timeout_from_env()
        self.limits = limits or _limits_from_env()
        self.transport = transport
        self.retry_policy = retry_policy
        self.gateway_secret = gateway_secret
        self._client: httpx.AsyncClient | None = None
        self._service_client: InternalServiceClient | None = None

    def _get_client(self) -> httpx.AsyncClient:
        """Compatibility accessor for tests that inspect httpx config."""
        if self._client is None or self._client.is_closed:
            transport = self.transport
            if transport is None:
                transport = httpx.AsyncHTTPTransport(retries=0)
            self._client = httpx.AsyncClient(
                base_url=self.base_url,
                timeout=self.timeout,
                limits=self.limits,
                transport=transport,
            )
            # Attach OTel CLIENT-span instrumentation so KB hops appear
            # in the same trace tree as the inbound gateway request.
            # Graceful: ``instrument_httpx_client`` catches ImportError
            # so a deploy without OTel libs still works.
            try:
                from ai_gateway_core.tracing import instrument_httpx_client

                instrument_httpx_client(self._client)
            except Exception as exc:  # noqa: BLE001
                logger.warning(
                    "KBProxyClient: OTel httpx instrumentation skipped: %s",
                    exc,
                )
        return self._client

    def _get_service_client(self) -> InternalServiceClient:
        if self._service_client is None:
            self._service_client = InternalServiceClient(
                InternalServiceClientConfig(
                    name="knowledge-service",
                    base_url=self.base_url,
                    timeout=self.timeout,
                    limits=self.limits,
                    retry_policy=self.retry_policy
                    or RetryPolicy(
                        max_attempts=_get_int_env(
                            "KB_PROXY_RETRY_MAX_ATTEMPTS",
                            _get_int_env("SERVICE_RETRY_MAX_ATTEMPTS", 2),
                        ),
                        base_delay_ms=_get_int_env("SERVICE_RETRY_BASE_DELAY_MS", 50),
                        max_delay_ms=_get_int_env("SERVICE_RETRY_MAX_DELAY_MS", 500),
                    ),
                    gateway_secret=self.gateway_secret or _get_signer(),
                ),
                transport=self.transport,
            )
        return self._service_client

    async def close(self) -> None:
        if self._client and not self._client.is_closed:
            await self._client.aclose()
        if self._service_client:
            await self._service_client.close()

    def _user_headers(self, user: Any) -> dict[str, str]:
        """Build headers to pass user context to KB microservice.

        The shared ``InternalServiceClient`` adds request-id propagation and
        HMAC signing. This method only owns trusted user context.
        """
        headers: dict[str, str] = {"Content-Type": "application/json"}
        if user:
            user_id = str(getattr(user, "user_id", "") or "")
            tier = str(getattr(user, "tier", "") or "")
            roles = list(getattr(user, "roles", []) or [])
            user_type = str(getattr(user, "user_type", "") or "user")
            anonymous = (
                getattr(user, "is_authenticated", True) is False
                or user_id.lower() == "anonymous"
                or tier.lower() == "anonymous"
                or user_type.lower() in {"anonymous", "guest"}
                or "guest" in {str(role).lower() for role in roles}
            )
            headers["X-User-Id"] = user_id
            headers["X-Tenant-Id"] = getattr(user, "tenant_id", "") or ""
            headers["X-User-Tier"] = "anonymous" if anonymous else tier
            headers["X-User-Roles"] = "guest" if anonymous else ",".join(roles)
            headers["X-User-Type"] = "anonymous" if anonymous else user_type
        return headers

    async def health_check(self) -> bool:
        """Check the public probe without signing outside the v2 API scope."""
        try:
            resp = await self._get_client().get("/health")
            return resp.status_code == 200
        except Exception:
            return False

    async def list_datasets(self, user: Any) -> list[dict[str, Any]]:
        """List datasets from KB microservice."""
        try:
            datasets: dict[str, dict[str, Any]] = {}
            cursor: str | None = None
            seen: set[str] = set()
            while True:
                response = await self._get_service_client().request(
                    "GET", "/api/v1/knowledge/datasets",
                    query_params={"limit": "200", **({"cursor": cursor} if cursor else {})},
                    headers=self._user_headers(user),
                )
                if response.status_code >= 400:
                    raise InternalServiceHTTPError(response.status_code, response.text, service="knowledge-service")
                data = response.json()
                rows = data if isinstance(data, list) else data.get("datasets", data.get("data", []))
                if not isinstance(rows, list):
                    raise ValueError("invalid dataset catalog response")
                for row in rows:
                    datasets[str(row["dataset_id"])] = row
                cursor = response.headers.get("X-Next-Cursor")
                if not cursor:
                    return list(datasets.values())
                if cursor in seen or len(cursor) > 1024:
                    raise ValueError("dataset pagination did not advance")
                seen.add(cursor)
        except InternalServiceHTTPError as e:
            logger.warning("KB list_datasets failed: %s", e.status_code)
            raise
        except Exception as e:
            logger.warning(f"KB list_datasets error: {e}")
            raise

    async def authorize_documents(
        self, user: Any, dataset_id: str, document_ids: list[str],
    ) -> set[str]:
        """Return currently readable document IDs from the KB authority."""
        requested = sorted({str(item).strip() for item in document_ids if str(item).strip()})
        if not requested:
            return set()
        allowed: set[str] = set()
        for offset in range(0, len(requested), 500):
            batch = requested[offset:offset + 500]
            payload = await self._get_service_client().request_json(
                "POST",
                "/api/v1/internal/knowledge/documents/authorize",
                headers=self._user_headers(user),
                json={"dataset_id": dataset_id, "document_ids": batch},
            )
            raw = payload.get("allowed_document_ids") if isinstance(payload, dict) else None
            if not isinstance(raw, list) or any(not isinstance(item, str) for item in raw):
                raise ValueError("invalid document authorization response")
            response_ids = set(raw)
            if not response_ids <= set(batch):
                raise ValueError("document authorization response contains unknown IDs")
            allowed.update(response_ids)
        return allowed

    async def authorize_document_sources(
        self,
        user: Any,
        dataset_id: str,
        references: list[dict[str, Any]],
    ) -> set[tuple[str, int, str]]:
        """Ask KB for exact saved source versions under current document ACL."""

        requested: set[tuple[str, int, str]] = set()
        for item in references:
            if not isinstance(item, dict):
                raise ValueError("invalid source reference")
            document_id = str(item.get("document_id") or "").strip()
            version = item.get("source_version")
            source_hash = str(item.get("source_hash") or "").strip().lower()
            if (
                not document_id or isinstance(version, bool) or not isinstance(version, int)
                or version <= 0 or not re.fullmatch(r"[0-9a-f]{64}", source_hash)
            ):
                raise ValueError("invalid source reference")
            requested.add((document_id, version, source_hash))
        if not requested:
            return set()
        allowed: set[tuple[str, int, str]] = set()
        ordered = sorted(requested)
        for offset in range(0, len(ordered), 500):
            batch = ordered[offset:offset + 500]
            payload = await self._get_service_client().request_json(
                "POST",
                "/api/v1/internal/knowledge/document-sources/authorize",
                headers=self._user_headers(user),
                json={
                    "dataset_id": dataset_id,
                    "references": [
                        {"document_id": document_id, "source_version": version,
                         "source_hash": source_hash}
                        for document_id, version, source_hash in batch
                    ],
                },
            )
            rows = payload.get("allowed_references") if isinstance(payload, dict) else None
            if not isinstance(rows, list):
                raise ValueError("invalid source authorization response")
            for row in rows:
                if not isinstance(row, dict):
                    raise ValueError("invalid source authorization response")
                identity = (
                    row.get("document_id"), row.get("source_version"), row.get("source_hash"),
                )
                if identity not in batch:
                    raise ValueError("source authorization response contains unknown identity")
                allowed.add(identity)
        return allowed

    async def retrieve(
        self,
        user: Any,
        dataset_id: str,
        query: str,
        top_k: int = 5,
        mode: str = "hybrid",
        score_threshold: float = 0.0,
        **kwargs: Any,
    ) -> tuple[list[ProxyRetrieveResult], dict[str, Any]]:
        """Retrieve chunks from KB microservice.

        Returns (results, metadata) matching KnowledgeService.retrieve() signature.
        """
        payload: dict[str, Any] = {
            "query": query,
            "top_k": top_k,
            "mode": mode,
        }
        if score_threshold > 0:
            payload["score_threshold"] = score_threshold
        supported_fields = {
            "document_id",
            "dense_weight",
            "bm25_weight",
            "fusion_method",
            "alpha",
            "vector_top_k",
            "keyword_top_k",
            "candidate_top_k",
            "keyword_candidate_k",
            "fusion",
            "rrf_k",
            "rrf_weights",
            "rerank",
            "rerank_model",
            "rerank_top_n",
            "mmr",
            "mmr_lambda",
            "mmr_threshold",
            "include_images",
            "include_associated_images",
            "multimodal_rerank",
            "content_type_filter",
            "image_search_enabled",
            "vlm_rerank_weight",
            "image_boost",
            "image_score_threshold",
            "use_separate_thresholds",
            "source_type_filter",
            "language_filter",
            "metadata_filter",
            "hierarchical",
            "hierarchical_strategy",
            "l1_top_k",
            "l2_top_k",
            "include_context",
        }
        payload.update(
            {
                key: value
                for key, value in kwargs.items()
                if key in supported_fields and value is not None
            }
        )

        try:
            data = await self._get_service_client().request_json(
                "POST",
                f"/api/v1/knowledge/{dataset_id}/retrieve",
                json=payload,
                headers=self._user_headers(user),
            )
            results_raw = data.get("results", [])
            meta = data.get("metadata", {})

            results = []
            for r in results_raw:
                results.append(
                    ProxyRetrieveResult(
                        text=r.get("text", ""),
                        score=float(r.get("score", 0.0)),
                        segment_id=r.get("segment_id"),
                        document_id=r.get("document_id"),
                        metadata=r.get("metadata", {}),
                        content_type=r.get("content_type", "text"),
                        image_url=r.get("image_url"),
                        vlm_description=r.get("vlm_description"),
                        associated_images=tuple(r.get("associated_images") or ()),
                    )
                )

            return results, meta

        except InternalServiceHTTPError as e:
            logger.warning("KB retrieve failed for %s: %s", dataset_id, e.status_code)
            return [], {"error": f"HTTP {e.status_code}"}
        except Exception as e:
            logger.warning(f"KB retrieve error for {dataset_id}: {e}")
            return [], {"error": str(e)}

    async def retrieve_with_images(
        self,
        user: Any,
        dataset_id: str,
        query: str,
        top_k: int = 5,
        include_images: bool = True,
        content_type_filter: str | None = None,
        multimodal_rerank: bool = False,
        **kwargs: Any,
    ) -> tuple[list[ProxyRetrieveResult], dict[str, Any]]:
        """Explicit multimodal wrapper over the shared retrieve endpoint."""
        kwargs.update(
            {
                "include_images": include_images,
                "include_associated_images": include_images,
                "content_type_filter": content_type_filter,
                "multimodal_rerank": multimodal_rerank,
            }
        )
        return await self.retrieve(user, dataset_id, query, top_k=top_k, **kwargs)

    async def retrieve_with_images_v2(
        self,
        user: Any,
        dataset_id: str,
        query: str,
        top_k: int = 5,
        intent: str = "general",
        vlm_rerank: bool = True,
        include_images: bool = True,
        **kwargs: Any,
    ) -> tuple[list[ProxyRetrieveResult], dict[str, Any]]:
        """Intent-compatible wrapper for Agent capability callers."""
        include_images = include_images and intent != "find_document"
        return await self.retrieve_with_images(
            user,
            dataset_id,
            query,
            top_k=top_k,
            include_images=include_images,
            multimodal_rerank=vlm_rerank and include_images,
            **kwargs,
        )

    async def require_dataset_access(
        self, user: Any, dataset_id: str, required: str = "viewer"
    ) -> dict:
        """Check dataset access — delegates to KB service.

        In proxy mode, the KB service handles auth via X-User-Id/X-Tenant-Id headers;
        we just verify the dataset exists.
        """
        _ = required
        try:
            return await self._get_service_client().request_json(
                "GET",
                f"/api/v1/knowledge/datasets/{dataset_id}",
                headers=self._user_headers(user),
            )
        except Exception:
            return {}


def _coerce_timeout(timeout: httpx.Timeout | float | None) -> httpx.Timeout:
    if timeout is None:
        return _TIMEOUT
    if isinstance(timeout, httpx.Timeout):
        return timeout
    return httpx.Timeout(timeout)


def _timeout_from_env() -> httpx.Timeout:
    return httpx.Timeout(
        connect=_get_float_env("KB_PROXY_CONNECT_TIMEOUT_SECONDS", _TIMEOUT.connect),
        read=_get_float_env("KB_PROXY_READ_TIMEOUT_SECONDS", _TIMEOUT.read),
        write=_get_float_env("KB_PROXY_WRITE_TIMEOUT_SECONDS", _TIMEOUT.write),
        pool=_get_float_env("KB_PROXY_POOL_TIMEOUT_SECONDS", _TIMEOUT.pool),
    )


def _limits_from_env() -> httpx.Limits:
    return httpx.Limits(
        max_connections=_get_int_env("KB_PROXY_MAX_CONNECTIONS", 50),
        max_keepalive_connections=_get_int_env("KB_PROXY_MAX_KEEPALIVE_CONNECTIONS", 10),
    )


def _get_float_env(key: str, default: float) -> float:
    raw = os.getenv(key, "").strip()
    if not raw:
        return default
    try:
        return float(raw)
    except ValueError:
        logger.warning("Invalid %s=%r; using default %s", key, raw, default)
        return default


def _get_int_env(key: str, default: int) -> int:
    raw = os.getenv(key, "").strip()
    if not raw:
        return default
    try:
        return int(raw)
    except ValueError:
        logger.warning("Invalid %s=%r; using default %s", key, raw, default)
        return default
