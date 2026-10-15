from __future__ import annotations

from types import SimpleNamespace

import pytest
from knowledge_service.api.routes.knowledge import update_dataset_config
from knowledge_service.api.schemas.knowledge import DatasetConfigUpdateSchema


@pytest.mark.asyncio
async def test_retrieval_config_lexical_update_is_a_nested_patch() -> None:
    stored_retrieval = {
        "mode": "hybrid",
        "top_k": 13,
        "vector_top_k": 41,
        "keyword_top_k": 37,
        "native_hybrid": False,
        "fusion": {
            "strategy": "weighted",
            "alpha": 0.62,
            "rrf_k": 91,
            "rrf_weights": {"vector": 0.8, "keyword": 0.2},
        },
        "rerank": {
            "enabled": True,
            "provider": "dashscope",
            "model": "gte-rerank-v2",
            "top_n": 17,
        },
        "mmr": {"enabled": True, "lambda": 0.31, "threshold": 0.02},
        "lexical": {
            "active_version": "lexical_v1",
            "bm25_v2": {
                "shadow_write_enabled": True,
                "k": 1.2,
                "b": 0.75,
                "avg_len": 256,
            },
        },
    }
    dataset = {
        "dataset_id": "dataset-a",
        "embedding_dimension": 1024,
        "index_config": {
            "chunking": {"mode": "heading"},
            "retrieval": stored_retrieval,
        },
    }
    updates: list[dict] = []

    class Service:
        async def require_dataset_access(self, *_args, **_kwargs):
            return dataset

        async def update_dataset(self, _user, _dataset_id, patch):
            updates.append(patch)
            return {**dataset, **patch}

    payload = DatasetConfigUpdateSchema.model_validate(
        {
            "retrieval_config": {
                "lexical": {
                    "active_version": "bm25_v2",
                }
            }
        }
    )

    await update_dataset_config(
        "dataset-a",
        payload=payload,
        svc=Service(),
        user=SimpleNamespace(user_id="owner-a"),
    )

    saved = updates[0]["index_config"]
    assert saved["chunking"] == {"mode": "heading"}
    assert saved["retrieval"] == {
        **stored_retrieval,
        "lexical": {
            **stored_retrieval["lexical"],
            "active_version": "bm25_v2",
        },
    }
