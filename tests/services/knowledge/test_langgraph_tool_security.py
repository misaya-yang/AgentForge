from __future__ import annotations

from types import SimpleNamespace
from typing import Any

import pytest
from knowledge_service.core.exceptions import ValidationFailedError
from knowledge_service.services.knowledge.langgraph_tools import (
    DifyCompatibleKBAPI,
    KnowledgeBaseTool,
    KnowledgeRetriever,
    MultiDatasetRetriever,
    MultiKnowledgeBaseTool,
)


class ProbeKnowledgeService:
    def __init__(self) -> None:
        self.retrieve_calls = 0
        self.v2_calls = 0
        self.last_retrieve_kwargs: dict[str, Any] | None = None

    async def retrieve(self, **kwargs: Any) -> tuple[list[Any], dict[str, Any]]:
        self.retrieve_calls += 1
        self.last_retrieve_kwargs = kwargs
        return [], {}

    async def retrieve_with_images_v2(
        self,
        **_kwargs: Any,
    ) -> tuple[list[Any], dict[str, Any]]:
        self.v2_calls += 1
        raise AssertionError("text-only knowledge tools must not call multimodal retrieval")


def _user() -> SimpleNamespace:
    return SimpleNamespace(is_authenticated=True)


@pytest.mark.parametrize("tool_type", [MultiDatasetRetriever, MultiKnowledgeBaseTool])
@pytest.mark.parametrize("dataset_ids", [[], [f"dataset-{index}" for index in range(9)]])
def test_multi_dataset_tool_constructor_rejects_unbounded_dataset_ids(
    tool_type: type,
    dataset_ids: list[str],
) -> None:
    service = ProbeKnowledgeService()

    with pytest.raises(ValidationFailedError, match="dataset_ids"):
        tool_type(service, dataset_ids, _user())

    assert service.retrieve_calls == 0
    assert service.v2_calls == 0


@pytest.mark.parametrize("default_top_k", [0, 21, True, "5"])
def test_tool_constructor_rejects_invalid_default_top_k(default_top_k: Any) -> None:
    service = ProbeKnowledgeService()

    with pytest.raises(ValidationFailedError, match="top_k"):
        KnowledgeRetriever(
            service,
            "dataset-a",
            _user(),
            default_top_k=default_top_k,
        )

    assert service.retrieve_calls == 0
    assert service.v2_calls == 0


def test_tool_schemas_are_text_only_and_bounded() -> None:
    service = ProbeKnowledgeService()
    retriever = KnowledgeRetriever(service, "dataset-a", _user())
    knowledge_tool = KnowledgeBaseTool(service, "dataset-a", _user())
    multi_tool = MultiKnowledgeBaseTool(service, ["dataset-a"], _user())

    schemas = [
        retriever.as_langchain_tool()["args_schema"],
        retriever.as_openai_function()["function"]["parameters"],
        knowledge_tool.args_schema,
        multi_tool.args_schema,
    ]
    for schema in schemas:
        properties = schema["properties"]
        assert properties["query"]["maxLength"] == 4096
        assert properties["top_k"]["minimum"] == 1
        assert properties["top_k"]["maximum"] == 20
        assert properties["intent"]["enum"] == ["general", "find_document"]
        assert "find_image" not in str(schema)


@pytest.mark.asyncio
@pytest.mark.parametrize(
    "input_data",
    [
        {"query": "q" * 4097},
        {"query": "query", "top_k": 21},
        {"query": "query", "top_k": True},
        {"query": "query", "top_k": "5"},
        {"query": "query", "intent": "find_image"},
        {"query": "query", "score_threshold": float("nan")},
        {"query": "query", "score_threshold": 1.1},
    ],
)
async def test_knowledge_retriever_rejects_invalid_execution_before_service(
    input_data: dict[str, Any],
) -> None:
    service = ProbeKnowledgeService()
    retriever = KnowledgeRetriever(service, "dataset-a", _user())

    with pytest.raises(ValidationFailedError):
        await retriever.retrieve(**input_data)

    assert service.retrieve_calls == 0
    assert service.v2_calls == 0


@pytest.mark.asyncio
@pytest.mark.parametrize(
    "input_data",
    [
        {"query": "q" * 4097},
        {"query": "query", "top_k": 21},
        {"query": "query", "intent": "find_image"},
        {"query": "query", "score_threshold": float("inf")},
    ],
)
@pytest.mark.parametrize("multi", [False, True])
async def test_langchain_tool_rejects_invalid_execution_before_service(
    input_data: dict[str, Any],
    multi: bool,
) -> None:
    service = ProbeKnowledgeService()
    tool = (
        MultiKnowledgeBaseTool(service, ["dataset-a"], _user())
        if multi
        else KnowledgeBaseTool(service, "dataset-a", _user())
    )

    with pytest.raises(ValidationFailedError):
        await tool._arun(**input_data)

    assert service.retrieve_calls == 0
    assert service.v2_calls == 0


@pytest.mark.asyncio
@pytest.mark.parametrize(
    "retrieval_model",
    [
        {"top_k": 21},
        {"top_k": True},
        {"top_k": "5"},
        {"score_threshold": float("nan")},
        {"score_threshold": float("inf")},
        {"score_threshold": -0.1},
    ],
)
async def test_dify_adapter_rejects_invalid_bounds_before_service(
    retrieval_model: dict[str, Any],
) -> None:
    service = ProbeKnowledgeService()
    adapter = DifyCompatibleKBAPI(service, _user())

    with pytest.raises(ValidationFailedError):
        await adapter.retrieve("dataset-a", "query", retrieval_model)

    assert service.retrieve_calls == 0
    assert service.v2_calls == 0


@pytest.mark.asyncio
async def test_text_only_tool_uses_regular_retrieval_at_documented_boundaries() -> None:
    service = ProbeKnowledgeService()
    retriever = KnowledgeRetriever(service, "dataset-a", _user())

    results = await retriever.retrieve(
        "query",
        top_k=20,
        intent="find_document",
        score_threshold=1.0,
    )

    assert results == []
    assert service.retrieve_calls == 1
    assert service.v2_calls == 0
    assert service.last_retrieve_kwargs is not None
    assert service.last_retrieve_kwargs["query"] == "query"
    assert service.last_retrieve_kwargs["top_k"] == 20
    assert service.last_retrieve_kwargs["score_threshold"] == 1.0
