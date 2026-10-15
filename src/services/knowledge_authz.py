"""KS-backed authorization for Agent-bound knowledge datasets (PRD T8.2).

The gateway must not re-implement — or SQL-query — the dataset ACL. The single
authority is knowledge-service: the resolver calls the internal authorization
endpoint over the same HMAC v2 gateway-secret channel the KB proxy uses
(:class:`ai_gateway_core.comm.client.InternalServiceClient`, the
``/internal/eval/ragas`` pattern), passing the authenticated identity as the
trusted headers the signature binds.

Fail-closed contract (unchanged from the retired in-gateway SQL resolver): any
failure — no internal token, transport error, non-2xx, malformed response, or
a denial for any bound dataset — raises
:class:`AgentKnowledgeAuthorizationError` (``AGENT_KNOWLEDGE_UNAVAILABLE``),
and the caller grants the agent no knowledge bindings.

Endpoint contract (knowledge-service side, shipped):

    POST /api/v1/internal/knowledge/datasets/authorize
    headers: X-Tenant-Id, X-User-Id, X-User-Type, X-User-Roles, X-User-Tier +
             X-Gateway-Secret (HMAC v2)
    body:    {"dataset_ids": [...], "is_tenant_admin": bool}
    200:     {"allowed_dataset_ids": [...]}

``X-User-Tier: admin`` is sent only when the gateway's own authenticated
identity says the caller is a tenant admin; ``is_tenant_admin`` in the body
is advisory on the KS side. Headers are bound into the HMAC signature, so a
forged tier can never widen access.
Anonymous Agent readers carry ``X-User-Type: anonymous`` and can only use
public Datasets even though the Agent's real tenant ID is present.
"""

from __future__ import annotations

import os
from typing import Any
from urllib.parse import quote

import httpx
from ai_gateway_core.auth.gateway_secret import GatewaySecret
from ai_gateway_core.comm.client import (
    InternalServiceClient,
    InternalServiceClientConfig,
    InternalServiceHTTPError,
)
from ai_gateway_core.knowledge import KB_SERVICE_URL
from ai_gateway_core.logging import get_logger

logger = get_logger(__name__)

AUTHORIZE_DATASETS_PATH = "/api/v1/internal/knowledge/datasets/authorize"
DATASET_DETAIL_PATH = "/api/v1/knowledge/datasets"


class AgentKnowledgeAuthorizationError(RuntimeError):
    """One or more bound Datasets are unavailable to the current principal."""

    def __init__(self, code: str = "AGENT_KNOWLEDGE_UNAVAILABLE"):
        self.code = code
        super().__init__(code)


class DatasetImpactAuthorityError(RuntimeError):
    """Dataset ownership could not be established for an impact preflight."""

    def __init__(self, status_code: int = 503):
        self.status_code = status_code
        super().__init__("DATASET_IMPACT_AUTHORITY_UNAVAILABLE")


def _get_signer() -> GatewaySecret | None:
    secret = os.environ.get("AI_PLATFORM_INTERNAL_TOKEN", "").strip()
    if not secret:
        return None
    return GatewaySecret(
        secret=secret,
        caller_service="gateway",
        audience="knowledge-service",
        allowed_path_prefixes=("/api/v1",),
    )


class KnowledgeServiceAgentKnowledgeResolver:
    """Authorize Agent Dataset bindings through knowledge-service."""

    def __init__(self, *, timeout_s: float = 5.0) -> None:
        self._base_url = os.getenv("KB_SERVICE_URL", KB_SERVICE_URL).rstrip("/")
        self._timeout = httpx.Timeout(connect=2.0, read=timeout_s, write=timeout_s, pool=2.0)
        self._service_client: InternalServiceClient | None = None

    def _get_service_client(self) -> InternalServiceClient:
        if self._service_client is None:
            self._service_client = InternalServiceClient(
                InternalServiceClientConfig(
                    name="knowledge-service",
                    base_url=self._base_url,
                    timeout=self._timeout,
                    gateway_secret=_get_signer(),
                )
            )
        return self._service_client

    async def close(self) -> None:
        if self._service_client:
            await self._service_client.close()
            self._service_client = None

    async def require_dataset_owner(
        self,
        *,
        dataset_id: str,
        tenant_id: str,
        user_id: str,
        is_tenant_admin: bool = False,
        roles: list[str] | None = None,
    ) -> None:
        """Verify owner access through the signed KS dataset-detail endpoint."""
        if not dataset_id or not tenant_id or not user_id or _get_signer() is None:
            raise DatasetImpactAuthorityError()

        headers = {"X-Tenant-Id": tenant_id, "X-User-Id": user_id}
        if roles:
            headers["X-User-Roles"] = ",".join(str(role) for role in roles)
        if is_tenant_admin:
            headers["X-User-Tier"] = "admin"

        path = f"{DATASET_DETAIL_PATH}/{quote(dataset_id, safe='')}"
        try:
            payload = await self._get_service_client().request_json(
                "GET", path, headers=headers
            )
        except InternalServiceHTTPError as exc:
            if exc.status_code in {403, 404}:
                raise DatasetImpactAuthorityError(exc.status_code) from exc
            logger.error("knowledge-service dataset detail returned HTTP %s", exc.status_code)
            raise DatasetImpactAuthorityError() from exc
        except (httpx.HTTPError, ValueError) as exc:
            logger.error("knowledge-service dataset detail unavailable: %s", type(exc).__name__)
            raise DatasetImpactAuthorityError() from exc

        if not isinstance(payload, dict) or str(payload.get("dataset_id") or "") != dataset_id:
            raise DatasetImpactAuthorityError()
        if str(payload.get("tenant_id") or "") != tenant_id:
            raise DatasetImpactAuthorityError(403)
        if payload.get("my_permission") != "owner":
            raise DatasetImpactAuthorityError(403)

    async def resolve(
        self,
        *,
        tenant_id: str,
        user_id: str,
        bindings: list[dict[str, Any]],
        is_tenant_admin: bool = False,
        roles: list[str] | None = None,
        authenticated: bool = True,
        **_kwargs: Any,
    ) -> list[dict[str, Any]]:
        """Return the bindings the caller is authorized for, or fail closed."""
        normalized = [
            dict(binding)
            for binding in bindings
            if isinstance(binding, dict) and str(binding.get("dataset_id") or "")
        ]
        dataset_ids = [str(binding["dataset_id"]) for binding in normalized]
        if not dataset_ids:
            return []
        if _get_signer() is None:
            # Without the internal token there is no way to attest identity to
            # knowledge-service; authorization cannot be established.
            raise AgentKnowledgeAuthorizationError()

        headers = {
            "X-Tenant-Id": str(tenant_id or ""),
            "X-User-Id": str(user_id or ""),
            "X-User-Type": "user" if authenticated else "anonymous",
        }
        if not authenticated:
            headers["X-User-Tier"] = "anonymous"
            headers["X-User-Roles"] = "guest"
        elif roles:
            headers["X-User-Roles"] = ",".join(str(role) for role in roles)
        if is_tenant_admin and authenticated:
            # KS grants admin scope exclusively from signature-bound identity
            # headers (X-User-Tier / X-User-Roles); the body flag is advisory.
            headers["X-User-Tier"] = "admin"

        try:
            payload = await self._get_service_client().request_json(
                "POST",
                AUTHORIZE_DATASETS_PATH,
                headers=headers,
                json={
                    "dataset_ids": dataset_ids,
                    "is_tenant_admin": bool(is_tenant_admin and authenticated),
                },
            )
        except (InternalServiceHTTPError, httpx.HTTPError, ValueError) as exc:
            logger.error(
                "knowledge-service dataset authorization unavailable: %s",
                type(exc).__name__,
            )
            raise AgentKnowledgeAuthorizationError() from exc

        allowed = self._parse_allowed(payload)
        if allowed is None or not set(dataset_ids) <= allowed:
            if allowed is None:
                logger.error("knowledge-service authorization response was malformed")
            raise AgentKnowledgeAuthorizationError()
        return normalized

    @staticmethod
    def _parse_allowed(payload: Any) -> set[str] | None:
        if not isinstance(payload, dict):
            return None
        raw = payload.get("allowed_dataset_ids")
        if not isinstance(raw, list) or any(not isinstance(item, str) for item in raw):
            return None
        return set(raw)


__all__ = [
    "AUTHORIZE_DATASETS_PATH",
    "DATASET_DETAIL_PATH",
    "AgentKnowledgeAuthorizationError",
    "DatasetImpactAuthorityError",
    "KnowledgeServiceAgentKnowledgeResolver",
]
