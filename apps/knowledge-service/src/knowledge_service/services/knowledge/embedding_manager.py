"""Embedding provider management and configuration resolution."""
from __future__ import annotations

from typing import TYPE_CHECKING, Any

from ...config import get_settings
from ...core.exceptions import ValidationFailedError
from ...core.observability.logging import get_logger
from .common import ensure_dict as _ensure_dict
from .embedding import (
    BaseEmbedding,
    EmbeddingConfig,
    _embeddings_endpoint,
    create_embedding,
)

if TYPE_CHECKING:
    from .embedding import UnifiedMultimodalEmbedding

logger = get_logger(__name__)

_SERVER_OWNED_EMBEDDING_FIELD_ALIASES = frozenset(
    {
        "apikey",
        "key",
        "accesskey",
        "secret",
        "secretkey",
        "token",
        "bearertoken",
        "authorization",
        "auth",
        "credentials",
        "headers",
        "baseurl",
        "endpoint",
        "endpointurl",
        "apibase",
        "apiurl",
        "url",
        "host",
    }
)


def _contains_server_owned_embedding_field(value: Any) -> bool:
    """Reject legacy row credentials/endpoints, including common aliases."""

    if isinstance(value, dict):
        for key, nested in value.items():
            normalized = "".join(character for character in str(key).lower() if character.isalnum())
            if any(
                normalized.endswith(alias)
                for alias in _SERVER_OWNED_EMBEDDING_FIELD_ALIASES
            ):
                return True
            if _contains_server_owned_embedding_field(nested):
                return True
    elif isinstance(value, (list, tuple)):
        return any(_contains_server_owned_embedding_field(item) for item in value)
    return False


def _require_server_owned_embedding_config(embedding_config: Any) -> dict[str, Any]:
    config = _ensure_dict(embedding_config)
    if _contains_server_owned_embedding_field(config):
        raise ValidationFailedError(
            "dataset embedding_config contains a legacy credential or endpoint; "
            "remove it before embedding"
        )
    return config


class EmbeddingManager:
    """Creates and configures text/multimodal embedders from dataset settings."""

    def __init__(self, settings: Any, credential_resolver: Any | None = None):
        self.settings = settings
        self.credential_resolver = credential_resolver

    async def _tenant_credential(self, tenant_id: str, provider: str) -> Any | None:
        if self.credential_resolver is None:
            return None
        return await self.credential_resolver.resolve(tenant_id, provider)

    def is_multimodal_dataset(self, dataset: dict[str, Any]) -> bool:
        """Check if dataset is configured for multimodal (unified embedding space).

        A dataset is considered multimodal if:
        1. embedding_provider is 'unified_multimodal', 'unified', or 'cross_modal'
        2. OR embedding_model is a known multimodal model
        3. OR index_config explicitly enables multimodal mode

        Returns:
            True if the dataset should use unified multimodal embedding
        """
        provider = str(dataset.get("embedding_provider") or "").lower()
        model = str(dataset.get("embedding_model") or "")

        # Check provider
        multimodal_providers = {
            "unified_multimodal",
            "unified",
            "cross_modal",
            "dashscope_multimodal",
            "multimodal",
        }
        if provider in multimodal_providers:
            return True

        # Check model
        from .embedding import MULTIMODAL_EMBEDDING_MODELS

        if model in MULTIMODAL_EMBEDDING_MODELS:
            return True

        # Check index_config
        index_config = _ensure_dict(dataset.get("index_config"))
        return bool(index_config.get("multimodal_enabled") or index_config.get("enable_multimodal"))

    async def get_unified_multimodal_embedder(
        self,
        dataset: dict[str, Any],
        embedding_config: dict[str, Any] | None = None,
    ) -> UnifiedMultimodalEmbedding:
        """Create UnifiedMultimodalEmbedding for multimodal datasets.

        This ensures text and images are embedded in the same vector space,
        enabling true cross-modal retrieval.

        Uses settings.knowledge.multimodal_embedding_* configuration.
        """
        ec = _require_server_owned_embedding_config(
            embedding_config
            if embedding_config is not None
            else dataset.get("embedding_config")
        )

        from ai_gateway_core.config import resolve_dashscope

        from .embedding import UnifiedMultimodalEmbedding

        resolved_key, resolved_url = resolve_dashscope("embedding")
        credential = await self._tenant_credential(
            str(dataset.get("tenant_id") or ""),
            "dashscope",
        )
        api_key = (
            str(getattr(credential, "api_key", "") or "").strip()
            or str(self.settings.knowledge.dashscope.api_key or "").strip()
            or resolved_key
        )
        if not api_key:
            raise ValidationFailedError(
                "server-owned DashScope embedding credentials are unavailable"
            )

        # Use model from dataset or fall back to settings
        model = dataset.get("embedding_model") or self.settings.knowledge.multimodal_embedding_model
        max_concurrent = (
            ec.get("max_concurrent") or self.settings.knowledge.multimodal_embedding_max_concurrent
        )

        return UnifiedMultimodalEmbedding(
            api_key=api_key,
            model=model,
            base_url=getattr(credential, "base_url", None) or resolved_url,
            max_concurrent=max_concurrent,
        )

    async def get_text_embedder(
        self,
        dataset: dict[str, Any],
        embedding_config: dict[str, Any] | None = None,
    ) -> BaseEmbedding:
        """Create embedder for text-only datasets using dataset-level config."""
        ec = _require_server_owned_embedding_config(
            embedding_config
            if embedding_config is not None
            else dataset.get("embedding_config")
        )
        provider = str(dataset.get("embedding_provider") or "").lower() or "local"
        model = str(dataset.get("embedding_model") or "")
        dimension = int(dataset.get("embedding_dimension") or 0) or None
        default_model = {
            "gemini": "gemini-embedding-001",
            "google": "gemini-embedding-001",
            "dashscope": "text-embedding-v3",
            "aliyun": "text-embedding-v3",
            "siliconflow": "BAAI/bge-m3",
            "silicon": "BAAI/bge-m3",
            "sf": "BAAI/bge-m3",
            "openai_compatible": "Qwen/Qwen3-Embedding-0.6B",
            "openai-compatible": "Qwen/Qwen3-Embedding-0.6B",
            "vllm": "Qwen/Qwen3-Embedding-0.6B",
            "tei": "Qwen/Qwen3-Embedding-0.6B",
        }.get(provider, "hash-384")
        resolved_model = model or default_model

        econf = await self.resolve_embedding_config(
            provider=provider,
            model=resolved_model,
            embedding_config=ec,
            tenant_id=str(dataset.get("tenant_id") or ""),
        )
        resolved_dimension = dimension
        if resolved_dimension is None and provider not in {"local", "builtin", "hash"}:
            resolved_dimension = self.settings.knowledge.text_embedding_dimension
        return create_embedding(econf, dimension=resolved_dimension)

    async def resolve_embedding_config(
        self,
        provider: str,
        model: str,
        embedding_config: dict[str, Any],
        *,
        tenant_id: str = "",
    ) -> EmbeddingConfig:
        embedding_config = _require_server_owned_embedding_config(embedding_config)
        provider_key = (provider or "").lower()
        api_key: str | None = None
        base_url: str | None = None

        if provider_key in {"local", "builtin", "hash"}:
            return EmbeddingConfig(
                provider="local",
                model=model or "hash-384",
                api_key=None,
                base_url=None,
                timeout_seconds=5.0,
                extra=embedding_config or {},
            )
        credential = await self._tenant_credential(tenant_id, provider_key)
        if credential is not None:
            api_key = str(getattr(credential, "api_key", "") or "").strip()
            base_url = str(getattr(credential, "base_url", "") or "").strip() or None
        if provider_key in {"gemini", "google"}:
            if not api_key:
                api_key = str(self.settings.knowledge.gemini.api_key or "").strip()
            # Env-level fallback: VERTEX_EMBEDDING_API_KEY / GEMINI_API_KEY /
            # GOOGLE_API_KEY are resolved through Settings here (mirrors
            # ai_gateway_core.config.endpoints; duplicated intentionally
            # because knowledge-service ships as a standalone image that
            # does not depend on ai-gateway-core).
            if not api_key:
                env = get_settings()
                api_key = (
                    env.vertex_embedding_api_key.strip()
                    if env.google_embedding_backend.strip().lower() == "vertex"
                    else ""
                ) or env.gemini_api_key.strip() or env.google_api_key.strip()
            if not api_key:
                raise ValidationFailedError(
                    "server-owned Gemini embedding credentials are unavailable"
                )
        elif provider_key in {"dashscope", "aliyun"}:
            # Use the canonical per-domain resolver from ai_gateway_core
            # so ``DASHSCOPE_BASE_URL`` / ``DASHSCOPE_EMBEDDING_BASE_URL``
            # / per-key fallbacks all behave identically to the gateway
            # and Agent capability paths. The resolver also normalises
            # the URL suffix (``/compatible-mode`` → ``/api/v1`` for the
            # SDK) so a single operator env keeps all three domains
            # working — incident 2026-04-28.
            from ai_gateway_core.config import resolve_dashscope

            resolved_key, resolved_url = resolve_dashscope("embedding")
            if not api_key:
                api_key = (
                    str(self.settings.knowledge.dashscope.api_key or "").strip()
                    or resolved_key
                )
            if not base_url:
                base_url = resolved_url
            if not api_key:
                raise ValidationFailedError(
                    "server-owned DashScope embedding credentials are unavailable"
                )
        elif provider_key in {"siliconflow", "silicon", "sf"}:
            if not api_key:
                api_key = str(self.settings.knowledge.siliconflow.api_key or "").strip()
            if not api_key:
                raise ValidationFailedError(
                    "server-owned SiliconFlow embedding credentials are unavailable"
                )
            endpoint = base_url or (
                str(self.settings.knowledge.siliconflow.base_url or "").strip()
                or "https://api.siliconflow.cn/v1"
            )
            base_url = _embeddings_endpoint(endpoint)
        elif provider_key in {"openai_compatible", "openai-compatible", "vllm", "tei"}:
            # Self-hosted OpenAI-compatible server (vLLM / TEI) — the
            # Qwen3-Embedding upgrade hook. Deliberately no cloud default
            # endpoint: the operator supplies the server address via env
            # (KNOWLEDGE_EMBEDDINGS__OPENAI_COMPATIBLE_BASE_URL) and the
            # API key stays optional for unauthenticated internal serving.
            embeddings = get_settings().embeddings
            if not api_key:
                api_key = str(embeddings.openai_compatible_api_key or "").strip() or None
            endpoint = base_url or str(embeddings.openai_compatible_base_url or "").strip()
            if not endpoint:
                raise ValidationFailedError(
                    "openai_compatible embedding provider requires a server-owned "
                    "base URL for the vLLM / TEI endpoint"
                )
            base_url = _embeddings_endpoint(endpoint)
        else:
            raise ValidationFailedError(f"Unsupported embedding provider: {provider}")

        return EmbeddingConfig(
            provider=provider_key,
            model=model,
            api_key=api_key or None,
            base_url=base_url,
            timeout_seconds=30.0,
            extra={
                k: v
                for k, v in (embedding_config or {}).items()
                if k not in {"api_key", "base_url"}
            },
        )
