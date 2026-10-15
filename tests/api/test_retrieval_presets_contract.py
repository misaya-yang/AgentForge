"""GET /knowledge/retrieval/presets — live HTTP shape pin.

Recorded in tmp/kb-api-baseline-2026-08-28.md §3.6 as a live surface
(preset list + recommended_default + methodology notes). The eval workbench
round-trips preset configs (tests/api/test_retrieval_evaluate.py) but nothing
pinned the endpoint's response contract itself: the exact preset vocabulary,
the recommended default, per-preset metadata completeness, and that every
config block re-parses through RetrievalConfig.from_dict so the dropdown can
hydrate every control from one selection.
"""

from __future__ import annotations

import pytest
from knowledge_service.api.routes.knowledge import list_retrieval_presets
from knowledge_service.services.knowledge.retrieval_config import RetrievalConfig

EXPECTED_PRESET_NAMES = {"fast", "balanced", "accurate", "diverse", "sota"}
CONFIG_BLOCKS = ("vector", "keyword", "fusion", "rerank", "mmr")


@pytest.mark.asyncio
async def test_presets_surface_names_and_recommended_default() -> None:
    payload = await list_retrieval_presets()

    names = {item["name"] for item in payload["presets"]}
    assert names == EXPECTED_PRESET_NAMES
    assert payload["recommended_default"] == "balanced"
    assert payload["recommended_default"] in names
    # Baseline §3.6 notes keys shipped today.
    assert set(payload["notes"]) == {"rrf_k", "mmr", "rerank_top_n", "score_threshold"}


@pytest.mark.asyncio
async def test_every_preset_carries_complete_metadata_and_round_trips() -> None:
    payload = await list_retrieval_presets()

    for item in payload["presets"]:
        for key in ("name", "label", "summary", "recommended_for", "config"):
            assert key in item, f"preset {item.get('name')} missing {key}"
        assert item["label"] and item["summary"] and item["recommended_for"]
        config = item["config"]
        # The dropdown hydrates every control from the single selection:
        # all relevance blocks must be present and re-parseable.
        for block in CONFIG_BLOCKS:
            assert block in config, f"preset {item['name']} missing block {block}"
        parsed = RetrievalConfig.from_dict(dict(config))
        assert parsed.to_dict()["vector"]["top_k"] == config["vector"]["top_k"]
