"""Tenant-scoped embedding credentials backed by the provider control plane."""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any

from ai_gateway_core.config import normalize_dashscope_base
from ai_gateway_core.security import decrypt_value, is_encrypted

from ...core.exceptions import ValidationFailedError
from ...core.observability.logging import get_logger

logger = get_logger(__name__)


@dataclass(frozen=True)
class TenantEmbeddingCredential:
    """Decrypted credential selected for one tenant and provider family."""

    api_key: str
    base_url: str | None = None


def _provider_family(row: dict[str, Any]) -> str | None:
    provider_id = str(row.get("provider_id") or "").strip().lower()
    api_type = str(row.get("api_type") or "").strip().lower()
    base_url = str(row.get("base_url") or "").strip().lower()

    if (
        provider_id.startswith(("dashscope", "aliyun"))
        or api_type in {"dashscope", "aliyun"}
        or "dashscope.aliyuncs.com" in base_url
        or "dashscope-intl.aliyuncs.com" in base_url
    ):
        return "dashscope"
    if (
        provider_id.startswith(("gemini", "google"))
        or api_type in {"google", "google-ai-studio", "google-vertex", "vertex"}
    ):
        return "gemini"
    if provider_id.startswith(("siliconflow", "silicon")) or "siliconflow" in base_url:
        return "siliconflow"
    return None


def _requested_family(provider: str) -> str | None:
    normalized = str(provider or "").strip().lower()
    if normalized in {"dashscope", "aliyun", "dashscope_multimodal"}:
        return "dashscope"
    if normalized in {"gemini", "google"}:
        return "gemini"
    if normalized in {"siliconflow", "silicon", "sf"}:
        return "siliconflow"
    return None


class TenantEmbeddingCredentialResolver:
    """Read the latest tenant credential before each remote embedding operation.

    No process-global credential is mutated or cached. This keeps UI updates
    effective on the next operation and prevents cross-tenant key reuse.
    """

    def __init__(self, database: Any, *, encryption_key: str = "") -> None:
        self.database = database
        self.encryption_key = encryption_key

    async def resolve(
        self,
        tenant_id: str,
        provider: str,
    ) -> TenantEmbeddingCredential | None:
        family = _requested_family(provider)
        normalized_tenant = str(tenant_id or "").strip()
        if not family or not normalized_tenant or self.database is None:
            return None

        try:
            rows = await self.database.fetch(
                """
                SELECT provider_id, api_type, base_url, api_key_encrypted
                FROM llm_providers
                WHERE tenant_id = $1 AND is_enabled = true
                ORDER BY updated_at DESC, provider_id ASC
                """,
                normalized_tenant,
            )
        except Exception as exc:
            logger.warning(
                "Tenant embedding credential lookup failed (exception_type=%s)",
                type(exc).__name__,
            )
            return None

        row = next(
            (dict(candidate) for candidate in rows if _provider_family(dict(candidate)) == family),
            None,
        )
        if row is None:
            return None

        stored_secret = str(row.get("api_key_encrypted") or "").strip()
        if not stored_secret:
            # Environment/CLI configuration intentionally remains the fallback.
            return None

        api_key = decrypt_value(stored_secret, self.encryption_key)
        if is_encrypted(api_key):
            raise ValidationFailedError(
                "tenant embedding credential could not be decrypted"
            )
        if not api_key:
            raise ValidationFailedError("tenant embedding credential is empty")

        base_url = str(row.get("base_url") or "").strip() or None
        if base_url and family == "dashscope":
            base_url = normalize_dashscope_base(base_url, "embedding")
        elif base_url and family == "siliconflow":
            base_url = base_url.rstrip("/")
            if not base_url.endswith("/embeddings"):
                base_url = f"{base_url}/embeddings"

        return TenantEmbeddingCredential(api_key=api_key, base_url=base_url)


__all__ = ["TenantEmbeddingCredential", "TenantEmbeddingCredentialResolver"]
