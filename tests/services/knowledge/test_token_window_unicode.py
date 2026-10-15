"""Actual cached tokenizer and production chunking; no service or model required."""
from __future__ import annotations

import json
from pathlib import Path

import pytest
from knowledge_service.core.exceptions import ValidationFailedError
from knowledge_service.services.knowledge.chunking import (
    BaseChunker,
    ChunkingConfig,
    ChunkingMode,
    _split_text_strict_by_tokens,
    get_token_counter,
    process_document,
)

CASES = json.loads(
    (Path(__file__).parents[2] / "fixtures/knowledge/token_window_unicode.json").read_text()
)


def assert_coverage(text: str, chunks: list[str], limit: int, overlap: int) -> None:
    encoder = get_token_counter().encoder
    assert encoder is not None, "real cl100k_base tokenizer is required"
    previous_start = -1
    covered = 0
    for chunk in chunks:
        assert chunk and len(encoder.encode(chunk)) <= limit
        # Match exact source substrings, covering every non-trimmed byte in order.
        candidates = []
        for start in range(previous_start + 1, covered + 1):
            if not text.startswith(chunk, start) or start + len(chunk) <= covered:
                continue
            if len(encoder.encode(text[start:covered])) <= overlap:
                candidates.append(start)
        if not candidates:
            start = covered
            while start < len(text) and text[start].isspace():
                start += 1
            assert text.startswith(chunk, start)
            candidates.append(start)
        start = min(candidates)
        assert start <= covered or text[covered:start].isspace()
        previous_start = start
        covered = start + len(chunk)
    assert not text[covered:].strip()
    if "\ufffd" not in text:
        assert all("\ufffd" not in chunk for chunk in chunks)


@pytest.mark.parametrize("case", CASES, ids=lambda case: case["name"])
def test_fixed_token_windows_preserve_unicode(case: dict) -> None:
    if case["error"]:
        with pytest.raises(ValidationFailedError, match="increase token_limit"):
            BaseChunker._split_by_tokens_fixed(None, case["text"], case["limit"], case["overlap"])
        return
    chunks = BaseChunker._split_by_tokens_fixed(None, case["text"], case["limit"], case["overlap"])
    assert_coverage(case["text"], chunks, case["limit"], case["overlap"])


@pytest.mark.parametrize("case", [case for case in CASES if not case["overlap"]], ids=lambda case: case["name"])
def test_strict_enforcement_uses_same_safe_windows(case: dict) -> None:
    if case["error"]:
        with pytest.raises(ValidationFailedError, match="increase token_limit"):
            _split_text_strict_by_tokens(case["text"], case["limit"])
        return
    chunks = _split_text_strict_by_tokens(case["text"], case["limit"])
    assert_coverage(case["text"], chunks, case["limit"], 0)


def test_document_pipeline_preserves_realistic_unicode_boundary() -> None:
    text = "hello " * 510 + "世界😀"
    config = ChunkingConfig(
        mode=ChunkingMode.FIXED_SIZE,
        use_token_count=True,
        token_limit=512,
        chunk_overlap=0,
        min_chunk_size=50,
        remove_extra_spaces=False,
        normalize_whitespace=False,
        extract_metadata=False,
    )
    chunks = process_document(text, config, document_id="unicode-fixture")
    assert_coverage(text, [chunk.text for chunk in chunks], 512, 0)


def test_terminal_overlap_does_not_emit_duplicate_tail() -> None:
    assert BaseChunker._split_by_tokens_fixed(None, "hello world", 4, 3) == ["hello world"]


@pytest.mark.parametrize("text", ["A世|B😀|Cक्षि|D\u0301|E�Z", "甲x乙y🚀z丙", "qé:rالعربية:s"])
@pytest.mark.parametrize("limit,overlap", [(limit, overlap) for limit in range(1, 9) for overlap in range(limit)])
def test_small_budget_overlap_cross_product(text: str, limit: int, overlap: int) -> None:
    encoder = get_token_counter().encoder
    assert encoder is not None
    try:
        chunks = BaseChunker._split_by_tokens_fixed(None, text, limit, overlap)
    except ValidationFailedError as error:
        assert "increase token_limit" in str(error)
        assert any(len(encoder.encode(character)) > limit for character in text)
    else:
        assert_coverage(text, chunks, limit, overlap)
