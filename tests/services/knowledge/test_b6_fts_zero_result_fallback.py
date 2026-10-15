"""B6 (PRD T1-8): PG FTS 'simple' vs multilingual tokenizer + zero-result fallback.

The text_search tsvector is built with ``to_tsvector('simple', text)`` whose
default parser emits ONE lexeme per unbroken CJK run, so jieba-style Chinese
tokens cannot match as tsquery terms and the GIN leg returns zero rows even
when every segment contains the terms. The retrieval service therefore falls
back to the documented substring ILIKE matcher (same tenant/dataset/enabled
predicates) when the FTS leg comes back EMPTY for a CJK-bearing query.

These tests pin: (d) zero-result triggers the fallback; the fallback is not
wired for legitimately-empty Latin queries; filters/tenant scope reach the
fallback unchanged (e); and the fallback is inert when the db object does not
expose it. The live-PostgreSQL counterpart (config-vs-tokenizer evidence and
isolation against real SQL) lives in
tests/database/test_kb_fts_config_fallback.py.
"""

from __future__ import annotations

from types import SimpleNamespace

from knowledge_service.services.knowledge.retrieval_service import _CJK_TEXT_RE

from tests.services.knowledge.test_retrieve_batch import (
    FakeDatabase,
    _make_bm25_service,
)


def _zh_rows(segment_id: str = "seg-zh"):
    return [
        {
            "segment_id": segment_id,
            "dataset_id": "kb-demo",
            "document_id": "doc-zh",
            "position": 0,
            "text": "这是一个机器学习的问题。",
            "token_count": 6,
            "metadata": {},
            "source_type": "manual",
            "language": "zh",
        }
    ]


class FtsFallbackDatabase(FakeDatabase):
    """Fake db whose GIN FTS leg always returns zero rows (zh mismatch)."""

    def __init__(self, fts_rows, ilike_rows, *, ilike_raises: bool = False):
        super().__init__(fts_rows)
        self.ilike_rows = ilike_rows
        self.ilike_raises = ilike_raises
        self.ilike_calls = []

    async def _search_segments_ilike(self, **kwargs):
        self.ilike_calls.append(kwargs)
        if self.ilike_raises:
            raise RuntimeError("simulated sequential-scan failure")
        return self.ilike_rows


def _make_service(fts_rows, ilike_rows, *, ilike_raises: bool = False):
    svc, _database = _make_bm25_service(fts_rows)
    database = FtsFallbackDatabase(fts_rows, ilike_rows, ilike_raises=ilike_raises)
    svc.db = database
    return svc, database


async def _retrieve(svc, query: str, **kwargs):
    return await svc.retrieve(
        user=SimpleNamespace(),
        dataset_id="kb-demo",
        query=query,
        top_k=3,
        mode="bm25",
        rerank=False,
        mmr=False,
        **kwargs,
    )


class TestCJKDetection:
    def test_cjk_ranges_detected(self):
        assert _CJK_TEXT_RE.search("机器学习 model")
        assert _CJK_TEXT_RE.search("日本語のテスト")
        assert _CJK_TEXT_RE.search("한국어 테스트")
        assert not _CJK_TEXT_RE.search("machine learning query")
        # Arabic matches the 'simple' parser at word boundaries and is not
        # part of the tokenizer-mismatch fallback trigger.
        assert not _CJK_TEXT_RE.search("تعليم الآلة")


class TestZeroResultFallback:
    async def test_fts_zero_result_reaches_ilike_fallback(self):
        svc, database = _make_service([], _zh_rows())

        results, meta = await _retrieve(svc, "机器学习")

        assert [r.segment_id for r in results] == ["seg-zh"]
        assert meta["bm25_fts_ilike_fallback_queries"] == ["机器学习"]
        # The fallback reaches the documented ILIKE path exactly once.
        assert len(database.ilike_calls) == 1

    async def test_fallback_preserves_tenant_and_dataset_scope(self):
        svc, database = _make_service([], _zh_rows())

        await _retrieve(svc, "机器学习")

        call = database.ilike_calls[0]
        assert call["dataset_id"] == "kb-demo"
        assert call["tenant_id"] == "tenant-a"
        assert call["terms"], "jieba-style tokens must drive the substring match"
        assert isinstance(call["limit"], int) and call["limit"] > 0

    async def test_fallback_carries_document_and_other_filters(self):
        svc, database = _make_service([], _zh_rows())

        await _retrieve(svc, "机器学习", document_id="doc-zh", source_type_filter="manual")

        call = database.ilike_calls[0]
        assert call["document_id"] == "doc-zh"
        assert call["source_type"] == "manual"

    async def test_latin_zero_result_does_not_pay_ilike_scan(self):
        svc, database = _make_service([], _zh_rows())

        results, meta = await _retrieve(svc, "alpha")

        assert results == []
        assert database.ilike_calls == []
        assert "bm25_fts_ilike_fallback_queries" not in meta

    async def test_nonzero_fts_result_skips_fallback(self):
        svc, database = _make_service(_zh_rows("seg-fts"), _zh_rows("seg-ilike"))

        results, _meta = await _retrieve(svc, "机器学习")

        assert [r.segment_id for r in results] == ["seg-fts"]
        assert database.ilike_calls == []

    async def test_db_without_ilike_path_stays_empty_leg(self):
        # Plain FakeDatabase has no _search_segments_ilike — the fallback is
        # inert and the leg degrades to zero candidates, not a crash.
        svc, _database = _make_bm25_service([])

        results, _meta = await _retrieve(svc, "机器学习")

        assert results == []

    async def test_fallback_error_is_swallowed(self):
        svc, database = _make_service([], _zh_rows(), ilike_raises=True)

        results, meta = await _retrieve(svc, "机器学习")

        assert results == []
        assert len(database.ilike_calls) == 1
        assert "bm25_fts_ilike_fallback_queries" not in meta

    async def test_mixed_script_query_triggers_fallback(self):
        svc, database = _make_service([], _zh_rows())

        results, _meta = await _retrieve(svc, "机器学习 model")

        assert [r.segment_id for r in results] == ["seg-zh"]
        assert len(database.ilike_calls) == 1
