from __future__ import annotations

from types import SimpleNamespace
from unittest.mock import AsyncMock, Mock

import pytest
from knowledge_service.core.exceptions import ValidationFailedError
from knowledge_service.services.knowledge import chunking as chunking_module
from knowledge_service.services.knowledge import hierarchical_indexer as hierarchical_module
from knowledge_service.services.knowledge.chunking import (
    ChunkingConfig,
    ChunkingMode,
    FixedSizeChunker,
    HeadingChunker,
    PageChunker,
    ParagraphChunker,
    SeparatorChunker,
    TextPreprocessor,
    validate_persisted_chunking_config,
)
from knowledge_service.services.knowledge.hierarchical_indexer import (
    HierarchicalIndexer,
    HierarchicalSegment,
    IndexLevel,
)


class _SplitMustNotRun(str):
    """String sentinel proving an eager ``str.split`` was fenced first."""

    def split(self, *_args: object, **_kwargs: object) -> list[str]:
        raise AssertionError("eager str.split ran before the output-budget fence")


class _MetadataMustStream(str):
    def split(self, *_args: object, **_kwargs: object) -> list[str]:
        raise AssertionError("metadata extraction eagerly split the full document")

    def lower(self) -> str:
        raise AssertionError("metadata extraction copied/lowercased the full document")


class _TokenCounterMustNotRun:
    @property
    def encoder(self) -> object:
        raise AssertionError("encoder access ran before the input-budget fence")

    def count_tokens(self, _text: str) -> int:
        raise AssertionError("token counting ran before the input-budget fence")


def _character_config(mode: ChunkingMode, **overrides: object) -> ChunkingConfig:
    values: dict[str, object] = {
        "mode": mode,
        "use_token_count": False,
        "chunk_size": 5,
        "chunk_overlap": 0,
        "min_chunk_size": 1,
    }
    values.update(overrides)
    return ChunkingConfig(**values)  # type: ignore[arg-type]


def test_fixed_character_output_cap_accepts_exact_and_rejects_plus_one(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setattr(chunking_module, "MAX_CHUNK_OUTPUTS", 3)
    chunker = FixedSizeChunker(_character_config(ChunkingMode.FIXED_SIZE))

    exact = chunker._split_with_overlap("x" * 15, chunk_size=5, overlap=0)

    assert exact == ["x" * 5, "x" * 5, "x" * 5]
    with pytest.raises(ValidationFailedError, match="3 chunk limit"):
        chunker._split_with_overlap("x" * 16, chunk_size=5, overlap=0)


def test_metadata_title_keywords_and_word_count_stream_without_full_lists() -> None:
    text = _MetadataMustStream("# Streamed title\nalpha beta alpha")

    metadata = TextPreprocessor.extract_metadata(
        text,
        ["title", "keywords", "word_count", "char_count"],
    )

    assert metadata["title"] == "Streamed title"
    assert metadata["keywords"][0] == "alpha"
    assert "beta" in metadata["keywords"]
    assert metadata["word_count"] == 6
    assert metadata["char_count"] == len(text)


@pytest.mark.parametrize(
    "config",
    [
        {"mode": "regex"},
        {"regex_pattern": "(a+)+$"},
        {"regex": "(a+)+$"},
        {"heading_patterns": ["(a+)+$"]},
        {"page_marker": "(a+)+$"},
    ],
)
def test_central_validator_rejects_all_custom_regex_surfaces(config: dict[str, object]) -> None:
    with pytest.raises(ValidationFailedError):
        validate_persisted_chunking_config(config)


def test_paragraph_rejects_before_eager_regex_split(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setattr(chunking_module, "MAX_CHUNK_OUTPUTS", 3)
    split = Mock(side_effect=AssertionError("eager re.split ran"))
    monkeypatch.setattr(chunking_module.re, "split", split)

    with pytest.raises(ValidationFailedError, match="3 chunk limit"):
        ParagraphChunker(_character_config(ChunkingMode.PARAGRAPH)).chunk(
            "a\n\nb\n\nc\n\nd"
        )

    split.assert_not_called()


@pytest.mark.parametrize(
    ("chunker", "text"),
    [
        (
            PageChunker(_character_config(ChunkingMode.PAGE)),
            _SplitMustNotRun("a\fb\fc\fd"),
        ),
        (
            HeadingChunker(_character_config(ChunkingMode.HEADING)),
            _SplitMustNotRun("a\nb\nc\nd"),
        ),
        (
            SeparatorChunker(
                _character_config(
                    ChunkingMode.SEPARATOR,
                    separators=["|"],
                    primary_separator="|",
                )
            ),
            _SplitMustNotRun("a|b|c|d"),
        ),
    ],
    ids=["page", "heading", "separator"],
)
def test_literal_modes_reject_before_eager_split(
    monkeypatch: pytest.MonkeyPatch,
    chunker: object,
    text: str,
) -> None:
    monkeypatch.setattr(chunking_module, "MAX_CHUNK_OUTPUTS", 3)

    with pytest.raises(ValidationFailedError, match="3 chunk limit"):
        chunker.chunk(text)  # type: ignore[attr-defined]


@pytest.mark.parametrize("split_method", ["_split_by_tokens", "_split_by_tokens_fixed"])
def test_token_word_and_fixed_fallback_reject_before_token_materialization(
    monkeypatch: pytest.MonkeyPatch,
    split_method: str,
) -> None:
    monkeypatch.setattr(chunking_module, "MAX_CHUNK_OUTPUTS", 3)
    monkeypatch.setattr(
        chunking_module,
        "get_token_counter",
        lambda: _TokenCounterMustNotRun(),
    )
    chunker = FixedSizeChunker(
        ChunkingConfig(
            mode=ChunkingMode.FIXED_SIZE,
            use_token_count=True,
            token_limit=1,
            chunk_overlap=0,
        )
    )

    with pytest.raises(ValidationFailedError, match="3 chunk limit"):
        getattr(chunker, split_method)("xxxx", token_limit=1, overlap_tokens=0)


def test_fixed_encoder_has_absolute_pre_materialization_cap(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setattr(chunking_module, "MAX_WHOLE_ENCODER_INPUT_BYTES", 3)
    encode = Mock(side_effect=AssertionError("whole encoder materialized oversized input"))
    counter = SimpleNamespace(encoder=SimpleNamespace(encode=encode))
    monkeypatch.setattr(chunking_module, "get_token_counter", lambda: counter)
    chunker = FixedSizeChunker(
        ChunkingConfig(
            mode=ChunkingMode.FIXED_SIZE,
            use_token_count=True,
            token_limit=100_000,
            chunk_overlap=0,
        )
    )

    with pytest.raises(ValidationFailedError, match="whole-tokenizer materialization"):
        chunker._split_by_tokens_fixed("xxxx", token_limit=100_000)

    encode.assert_not_called()


def test_hierarchical_aggregate_cap_accepts_exact_and_rejects_plus_one(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setattr(hierarchical_module, "MAX_CHUNK_OUTPUTS", 3)

    HierarchicalIndexer._require_hierarchical_output_budget(1, 1, 1)
    with pytest.raises(ValidationFailedError, match="3 chunk limit"):
        HierarchicalIndexer._require_hierarchical_output_budget(1, 1, 2)


class _PoisonEmbedder:
    def __init__(self) -> None:
        self.dimension_reads = 0
        self.embed_documents = AsyncMock(
            side_effect=AssertionError("embedding ran before hierarchy budget validation")
        )

    @property
    def dimension(self) -> int:
        self.dimension_reads += 1
        raise AssertionError("dimension resolution ran before hierarchy budget validation")


@pytest.mark.asyncio
async def test_hierarchical_aggregate_overflow_has_zero_downstream_calls(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setattr(hierarchical_module, "MAX_CHUNK_OUTPUTS", 3)

    summary_generator = SimpleNamespace(
        summarize_section=AsyncMock(
            side_effect=AssertionError("section summarization ran before hierarchy budget")
        ),
        summarize_document=AsyncMock(
            side_effect=AssertionError("document summarization ran before hierarchy budget")
        ),
    )
    vector_store = SimpleNamespace(
        ensure_collection=AsyncMock(
            side_effect=AssertionError("vector collection resolution ran before hierarchy budget")
        ),
        upsert=AsyncMock(
            side_effect=AssertionError("vector upsert ran before hierarchy budget")
        ),
    )
    database = SimpleNamespace(
        get_dataset=AsyncMock(
            side_effect=AssertionError("database read ran before hierarchy budget")
        ),
        insert_segments=AsyncMock(
            side_effect=AssertionError("database write ran before hierarchy budget")
        ),
        save_document_summary=AsyncMock(
            side_effect=AssertionError("summary persistence ran before hierarchy budget")
        ),
    )
    embedder = _PoisonEmbedder()
    indexer = HierarchicalIndexer(
        vector_store=vector_store,
        database=database,
        embedder=embedder,
        summary_generator=summary_generator,
    )
    l2_segments = [
        HierarchicalSegment(
            segment_id="section-1",
            document_id="document-1",
            dataset_id="dataset-1",
            level=IndexLevel.SECTION,
            text="section text " * 100,
        )
    ]
    l3_segments = [
        HierarchicalSegment(
            segment_id=f"paragraph-{index}",
            document_id="document-1",
            dataset_id="dataset-1",
            level=IndexLevel.PARAGRAPH,
            text=f"paragraph {index}",
        )
        for index in range(2)
    ]
    create_hierarchy = AsyncMock(return_value=(l2_segments, l3_segments))
    monkeypatch.setattr(indexer, "_create_l2_l3_chunks", create_hierarchy)

    result = await indexer.index_document(
        document_id="document-1",
        dataset_id="dataset-1",
        text="source text",
    )

    assert result.total_vectors == 0
    assert result.errors == ["hierarchical output exceeds the 3 chunk limit"]
    create_hierarchy.assert_awaited_once()
    summary_generator.summarize_section.assert_not_awaited()
    summary_generator.summarize_document.assert_not_awaited()
    assert embedder.dimension_reads == 0
    embedder.embed_documents.assert_not_awaited()
    vector_store.ensure_collection.assert_not_awaited()
    vector_store.upsert.assert_not_awaited()
    database.get_dataset.assert_not_awaited()
    database.insert_segments.assert_not_awaited()
    database.save_document_summary.assert_not_awaited()
