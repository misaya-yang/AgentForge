"""D6 regression tests for lossless chunking-config API round trips."""

from __future__ import annotations

import pytest
from knowledge_service.api.schemas.knowledge import (
    ChunkingConfigSchema,
    DocumentArchiveSchema,
)
from pydantic import ValidationError


def test_runtime_chunking_fields_round_trip_without_loss() -> None:
    raw = {
        "mode": "hierarchical",
        "chunk_size": 2_000,
        "chunk_overlap": 200,
        "max_chunk_size": 4_000,
        "min_chunk_size": 200,
        "normalize_whitespace": False,
        "strip_html": True,
        "extract_metadata": True,
        "metadata_fields": ["title", "author", "date"],
        "page_marker": r"\f",
        "strict_section_traceability": True,
        "preserve_images": True,
        "image_context_chars": 512,
        "parent_chunk_size": 4_000,
        "parent_overlap": 200,
        "child_chunk_size": 500,
        "child_chunk_overlap": 50,
        "segmentation": {"max_tokens": 768},
    }

    encoded = ChunkingConfigSchema.model_validate(raw).model_dump(exclude_none=True)
    decoded = ChunkingConfigSchema.model_validate(encoded).model_dump(exclude_none=True)

    for key, value in raw.items():
        assert decoded[key] == value


@pytest.mark.parametrize(
    "field,value",
    [
        ("page_marker", "(a+)+$"),
        ("metadata_fields", [""]),
        ("separators", [""]),
        ("segmentation", {"max_tokens": 500, "separator": "\\n"}),
    ],
)
def test_runtime_chunking_fields_remain_fail_closed(field: str, value: object) -> None:
    with pytest.raises(ValidationError):
        ChunkingConfigSchema.model_validate({field: value})


def test_archive_reason_matches_database_column_width() -> None:
    assert DocumentArchiveSchema(archived=True, reason="x" * 255).reason == "x" * 255
    with pytest.raises(ValidationError):
        DocumentArchiveSchema(archived=True, reason="x" * 256)
