"""Shared pure-utility helpers for knowledge-base callers.

These live in ``ai_gateway_core`` so gateway and knowledge-service callers can
ask "is this model multimodal?" without duplicating the registry or making an
HTTP call for a static lookup.

The kb-service's ``services/knowledge/embedding.py`` re-exports the same
symbols for backward compatibility.

Promoted as part of Phase K5b (KB fork merge → kb-service single source).
"""

from __future__ import annotations

# =============================================================================
# Multimodal Embedding Model Registry
# =============================================================================
# Centralized list of embedding models that support multimodal (image) content.
# Used by assistant API to identify multimodal knowledge bases.

MULTIMODAL_EMBEDDING_MODELS: frozenset[str] = frozenset(
    {
        # DashScope multimodal models
        "multimodal-embedding-v1",
        "multimodal-embedding-one-peace-v1",
        "multimodal-embedding-one-peace",
        # Tongyi unified vision models
        "tongyi-embedding-vision-plus",
        # Qwen VL models
        "qwen2.5-vl-embedding",
    }
)


def is_multimodal_embedding_model(model_name: str) -> bool:
    """Check if a model supports multimodal (image) embedding.

    Pure stateless predicate — safe to call from any layer.
    """
    return model_name in MULTIMODAL_EMBEDDING_MODELS


__all__ = [
    "MULTIMODAL_EMBEDDING_MODELS",
    "is_multimodal_embedding_model",
]
