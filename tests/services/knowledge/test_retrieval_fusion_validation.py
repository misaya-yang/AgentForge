from __future__ import annotations

import math
from typing import Any

import pytest
from knowledge_service.api.schemas.knowledge import (
    BatchRetrieveRequestSchema,
    RetrieveRequestSchema,
)
from knowledge_service.core.exceptions import ValidationFailedError
from knowledge_service.services.knowledge.knowledge_service import KnowledgeService
from knowledge_service.services.knowledge.retrieval_config import FusionConfig, FusionStrategy
from knowledge_service.services.knowledge.retrieval_service import (
    _has_explicit_rrf_weighting,
)
from pydantic import ValidationError


@pytest.mark.parametrize(
    ("schema", "payload"),
    [
        (RetrieveRequestSchema, {"query": "q", "rrf_k": 0}),
        (RetrieveRequestSchema, {"query": "q", "rrf_k": -1}),
        (
            RetrieveRequestSchema,
            {"query": "q", "dense_weight": 0.0, "bm25_weight": 0.0},
        ),
        (RetrieveRequestSchema, {"query": "q", "rrf_weights": {"vector": math.nan}}),
        (
            BatchRetrieveRequestSchema,
            {"queries": ["q"], "rrf_weights": {"vector": 0.0, "keyword": 0.0}},
        ),
        (
            BatchRetrieveRequestSchema,
            {"queries": [{"query": "q", "dense_weight": -0.1}]},
        ),
        (
            BatchRetrieveRequestSchema,
            {"queries": [{"query": "q", "bm25_weight": math.nan}]},
        ),
    ],
)
def test_production_retrieval_schemas_reject_invalid_fusion_parameters(
    schema: type[RetrieveRequestSchema] | type[BatchRetrieveRequestSchema],
    payload: dict[str, Any],
) -> None:
    with pytest.raises(ValidationError):
        schema.model_validate(payload)


def _resolve_fusion(
    retrieval_defaults: dict[str, Any],
    **overrides: Any,
) -> dict[str, Any]:
    service = object.__new__(KnowledgeService)
    request = {
        "fusion_method": None,
        "fusion": None,
        "alpha": None,
        "dense_weight": None,
        "bm25_weight": None,
        "rrf_k": None,
        "rrf_weights": None,
    }
    request.update(overrides)
    return service._resolve_fusion_config(
        retrieval_defaults=retrieval_defaults,
        **request,
    )


def test_rrf_weighting_is_not_inferred_from_legacy_defaults() -> None:
    assert not _has_explicit_rrf_weighting(
        retrieval_defaults={},
        dense_weight=None,
        bm25_weight=None,
        alpha=None,
        rrf_weights={},
    )


@pytest.mark.parametrize(
    "retrieval_defaults",
    [
        {"adaptive_weights": True},
        {"dense_weight": 0.5, "bm25_weight": 0.5},
        {"fusion": {"rrf_weights": {"dense": 0.7, "bm25": 0.3}}},
    ],
)
def test_dataset_config_can_explicitly_opt_into_weighted_rrf(
    retrieval_defaults: dict[str, Any],
) -> None:
    assert _has_explicit_rrf_weighting(
        retrieval_defaults=retrieval_defaults,
        dense_weight=None,
        bm25_weight=None,
        alpha=None,
        rrf_weights={},
    )


@pytest.mark.parametrize(
    ("rrf_weights", "expected"),
    [
        ({"vector": 0.8, "keyword": 0.2}, (0.8, 0.2)),
        ({"dense": 0.7, "bm25": 0.3}, (0.7, 0.3)),
    ],
)
def test_nested_dataset_rrf_weight_aliases_enter_effective_fusion_config(
    rrf_weights: dict[str, float],
    expected: tuple[float, float],
) -> None:
    resolved = _resolve_fusion(
        {
            "fusion": {
                "strategy": "rrf",
                "rrf_k": 42,
                "rrf_weights": rrf_weights,
            }
        }
    )

    assert resolved["method"] == "rrf"
    assert resolved["rrf_k"] == 42
    assert (resolved["dense_weight"], resolved["bm25_weight"]) == expected


def test_canonical_weighted_fusion_round_trip_keeps_alpha_weights() -> None:
    fusion = FusionConfig(strategy=FusionStrategy.WEIGHTED, alpha=0.8).to_dict()

    resolved = _resolve_fusion({"fusion": fusion})

    assert resolved["method"] == "weighted"
    assert resolved["dense_weight"] == pytest.approx(0.8)
    assert resolved["bm25_weight"] == pytest.approx(0.2)


@pytest.mark.parametrize(
    "retrieval_defaults",
    [
        {"rrf_k": 0},
        {"rrf_k": -1},
        {"rrf_k": math.nan},
        {"rrf_k": 60.5},
        {"fusion": {"rrf_weights": {"vector": 0.0, "keyword": 0.0}}},
        {"fusion": {"rrf_weights": {"dense": math.inf, "bm25": 1.0}}},
        {"dense_weight": -1.0, "bm25_weight": 1.0},
    ],
)
def test_service_fails_closed_on_invalid_dataset_fusion_config(
    retrieval_defaults: dict[str, Any],
) -> None:
    with pytest.raises(ValidationFailedError):
        _resolve_fusion(retrieval_defaults)
