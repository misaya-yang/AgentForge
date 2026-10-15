"""Contract tests for the segment hot-update path (PUT /knowledge/{id}/segments/{id}).

Covers the 2026-08 KB upgrade quick win: the route previously dropped
``answer`` and ``keywords`` even though SegmentUpdateSchema accepted them.
These tests pin:

* answer/keywords/content_hash reach the persistence layer,
* ``None`` means "leave untouched" while ``""``/``[]`` clears,
* the content hash is refreshed with the text (incremental skip depends on it),
* the Qdrant point ID stays stable across the edit (upsert, not delete+create),
* the fail-closed index-state machine still hides the segment on failure.
"""

from __future__ import annotations

import hashlib
from contextlib import asynccontextmanager
from types import SimpleNamespace
from typing import Any

import pytest
from knowledge_service.api.schemas.knowledge import SegmentUpdateSchema
from knowledge_service.core.auth.user_resolver import UserContext
from knowledge_service.core.exceptions import ValidationFailedError
from knowledge_service.services.knowledge import document_service as document_module
from knowledge_service.services.knowledge.document_service import DocumentService

USER = UserContext(user_id="editor-a", tenant_id="tenant-a")


def _dataset_row() -> dict[str, Any]:
    return {
        "dataset_id": "dataset-a",
        "tenant_id": "tenant-a",
        "collection_name": "collection-a",
        "embedding_provider": "local",
        "embedding_model": "hash-384",
        "embedding_dimension": 2,
        "embedding_config": {},
        "index_config": {},
    }


class UpdateSegmentDatabase:
    def __init__(self) -> None:
        self.dataset = _dataset_row()
        self.document = {
            "document_id": "document-a",
            "dataset_id": "dataset-a",
            "enabled": True,
            "archived": False,
            "status": "completed",
            "metadata": {},
        }
        self.rows: dict[str, dict[str, Any]] = {
            "segment-a": {
                "segment_id": "segment-a",
                "dataset_id": "dataset-a",
                "document_id": "document-a",
                "vector_id": "point-a",
                "position": 0,
                "text": "original text",
                "answer": "original answer",
                "keywords": ["kept"],
                "content_hash": hashlib.sha256(b"original text").hexdigest(),
                "enabled": True,
                "status": "completed",
                "error": None,
                "level": 3,
                "token_count": 4,
                "metadata": {"content_type": "text"},
            }
        }
        self.update_calls: list[dict[str, Any]] = []
        self.index_states: list[tuple[str, str | None]] = []

    @asynccontextmanager
    async def segment_index_update_lease(self, dataset_id, document_id, segment_id):
        assert (dataset_id, document_id, segment_id) == (
            "dataset-a",
            "document-a",
            "segment-a",
        )
        yield self

    @asynccontextmanager
    async def dataset_index_write_lease(self, dataset_id):
        assert dataset_id == "dataset-a"
        yield self

    async def get_dataset(self, dataset_id, *, connection=None):
        assert connection in (None, self)
        return dict(self.dataset) if dataset_id == "dataset-a" else None

    async def get_document(self, document_id, *, connection=None):
        assert connection in (None, self)
        return dict(self.document) if document_id == "document-a" else None

    async def get_segment(self, segment_id, *, connection=None):
        assert connection in (None, self)
        row = self.rows.get(segment_id)
        return dict(row) if row is not None else None

    async def update_segment(
        self,
        segment_id,
        *,
        text,
        token_count=None,
        metadata=None,
        vector_id=None,
        answer=None,
        keywords=None,
        content_hash=None,
        connection=None,
    ):
        assert connection is self
        # Hot-update must never rewrite the vector identity or shape columns.
        assert token_count is None and vector_id is None
        assert isinstance(metadata, dict)
        assert metadata.get("manual_source_override") is True
        assert "source_version" not in metadata and "source_hash" not in metadata
        self.update_calls.append(
            {
                "segment_id": segment_id,
                "text": text,
                "answer": answer,
                "keywords": keywords,
                "content_hash": content_hash,
            }
        )
        row = self.rows[segment_id]
        row["text"] = text
        row["metadata"] = metadata
        # Mirror the SQL contract: None leaves the column untouched.
        if answer is not None:
            row["answer"] = answer
        if keywords is not None:
            row["keywords"] = list(keywords)
        if content_hash is not None:
            row["content_hash"] = content_hash

    async def set_segment_index_state(
        self, segment_id, state, *, error=None, connection=None
    ):
        assert connection is self
        self.index_states.append((state, error))
        row = self.rows[segment_id]
        row["status"] = {"pending": "indexing", "completed": "completed", "error": "error"}[
            state
        ]
        row["error"] = error if state == "error" else None


class UpdateSegmentVectorStore:
    def __init__(self, *, fail_upserts: int = 0) -> None:
        self.fail_upserts = fail_upserts
        self.upsert_calls: list[dict[str, Any]] = []

    async def ensure_collection(self, **kwargs: Any) -> str:
        return str(kwargs.get("collection_name") or "collection-a")

    async def upsert(self, **kwargs: Any) -> None:
        if self.fail_upserts:
            self.fail_upserts -= 1
            raise RuntimeError("qdrant unavailable")
        self.upsert_calls.append(kwargs)


class UpdateSegmentKnowledge:
    def __init__(self, database: UpdateSegmentDatabase, vector_store) -> None:
        self.database = database
        self.vector_store = vector_store

    async def require_dataset_access(self, _user, dataset_id, *, required):
        assert (dataset_id, required) == ("dataset-a", "editor")
        return dict(self.database.dataset)

    def _sanitize_text_for_db(self, text: str) -> str:
        return text

    def _resolve_embedding_config(self, **_kwargs: Any) -> SimpleNamespace:
        return SimpleNamespace(timeout_seconds=0.1)


class _FakeEmbedder:
    dimension = 2

    def __init__(self, *, fail: bool = False) -> None:
        self.fail = fail
        self.closed = False

    async def embed_documents(self, texts: list[str]) -> list[list[float]]:
        if self.fail:
            raise RuntimeError("embedding unavailable")
        return [[0.1, 0.2] for _ in texts]

    async def close(self) -> None:
        self.closed = True


def _make_service(
    monkeypatch: pytest.MonkeyPatch,
    *,
    embed_fails: bool = False,
    upsert_failures: int = 0,
):
    database = UpdateSegmentDatabase()
    vector_store = UpdateSegmentVectorStore(fail_upserts=upsert_failures)
    knowledge = UpdateSegmentKnowledge(database, vector_store)
    service = DocumentService(SimpleNamespace(), database)  # type: ignore[arg-type]
    service._ks = knowledge  # type: ignore[assignment]

    def _factory(_config: Any, dimension: int | None = None):
        assert dimension == 2
        return _FakeEmbedder(fail=embed_fails)

    monkeypatch.setattr(document_module, "create_embedding", _factory)
    return service, database, vector_store


async def test_update_segment_persists_answer_keywords_and_hash(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    service, database, vector_store = _make_service(monkeypatch)

    result = await service.update_segment(
        USER,
        "dataset-a",
        "segment-a",
        "replacement text",
        new_answer="replacement answer",
        new_keywords=["alpha", "beta"],
    )

    call = database.update_calls[0]
    assert call["text"] == "replacement text"
    assert call["answer"] == "replacement answer"
    assert call["keywords"] == ["alpha", "beta"]
    assert call["content_hash"] == hashlib.sha256(b"replacement text").hexdigest()
    # Stable point identity: the upsert must reuse the existing vector_id.
    assert len(vector_store.upsert_calls) == 1
    points = vector_store.upsert_calls[0]["points"]
    assert [str(point.id) for point in points] == ["point-a"]
    assert points[0].payload["text"] == "replacement text"
    assert [state for state, _ in database.index_states] == ["pending", "completed"]
    assert result["text"] == "replacement text"
    assert result["answer"] == "replacement answer"
    assert result["keywords"] == ["alpha", "beta"]


async def test_update_segment_omitted_fields_keep_stored_values(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    service, database, _vector_store = _make_service(monkeypatch)

    await service.update_segment(USER, "dataset-a", "segment-a", "only text changes")

    call = database.update_calls[0]
    assert call["answer"] is None
    assert call["keywords"] is None
    row = database.rows["segment-a"]
    assert row["answer"] == "original answer"
    assert row["keywords"] == ["kept"]
    # The hash still follows the text even when answer/keywords are omitted.
    assert row["content_hash"] == hashlib.sha256(b"only text changes").hexdigest()


async def test_update_segment_clears_answer_and_keywords_explicitly(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    service, database, _vector_store = _make_service(monkeypatch)

    await service.update_segment(
        USER,
        "dataset-a",
        "segment-a",
        "replacement text",
        new_answer="",
        new_keywords=[],
    )

    row = database.rows["segment-a"]
    assert row["answer"] == ""
    assert row["keywords"] == []


async def test_update_segment_embedding_failure_hides_segment(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    service, database, vector_store = _make_service(monkeypatch, embed_fails=True)

    with pytest.raises(ValidationFailedError, match="remains hidden until retry"):
        await service.update_segment(USER, "dataset-a", "segment-a", "replacement text")

    assert vector_store.upsert_calls == []
    assert database.index_states[-1] == ("error", "vector update failed")
    assert database.rows["segment-a"]["status"] == "error"


async def test_update_segment_upsert_failure_hides_segment(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    service, database, vector_store = _make_service(monkeypatch, upsert_failures=1)

    with pytest.raises(ValidationFailedError, match="remains hidden until retry"):
        await service.update_segment(USER, "dataset-a", "segment-a", "replacement text")

    assert database.index_states[-1] == ("error", "vector update failed")
    assert database.rows["segment-a"]["status"] == "error"


async def test_update_segment_requires_fail_closed_contract(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    service, database, _vector_store = _make_service(monkeypatch)
    monkeypatch.delattr(UpdateSegmentDatabase, "set_segment_index_state")

    with pytest.raises(ValidationFailedError, match="fail-closed index state contract"):
        await service.update_segment(USER, "dataset-a", "segment-a", "replacement text")

    assert database.update_calls == []


def test_segment_update_schema_keyword_bounds() -> None:
    schema = SegmentUpdateSchema(text="body", answer=None, keywords=["ok"])
    assert schema.keywords == ["ok"]
    assert schema.answer is None

    with pytest.raises(ValueError, match="keywords"):
        SegmentUpdateSchema(text="body", keywords=["x" * 257])

    with pytest.raises(ValueError, match="keywords"):
        SegmentUpdateSchema(text="body", keywords=[""])
