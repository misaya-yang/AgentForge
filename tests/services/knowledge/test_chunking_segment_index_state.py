from __future__ import annotations

import asyncio
import contextlib
from types import SimpleNamespace
from typing import Any

import pytest
from knowledge_service.auth.user_context import UserContext
from knowledge_service.core.exceptions import ValidationFailedError
from knowledge_service.persistence.database import DOCUMENT_LIFECYCLE_REINDEX_KEY
from knowledge_service.services.knowledge import chunking_manager as chunking_module
from knowledge_service.services.knowledge.chunking_manager import ChunkingManager


def _dataset_row() -> dict[str, Any]:
    return {
        "dataset_id": "dataset-a",
        "tenant_id": "tenant-a",
        "collection_name": "custom-base",
        "embedding_provider": "local",
        "embedding_model": "hash-384",
        "embedding_dimension": 2,
        "embedding_config": {},
        "index_config": {},
    }


class SegmentDatabase:
    def __init__(self, *, enabled: bool = True) -> None:
        self.dataset = _dataset_row()
        self.document: dict[str, Any] = {
            "document_id": "document-a",
            "dataset_id": "dataset-a",
            "enabled": True,
            "archived": False,
            "status": "completed",
            "source_type": "upload",
            "doc_language": "zh",
            "metadata": {},
        }
        self.row: dict[str, Any] = {
            "segment_id": "segment-a",
            "dataset_id": "dataset-a",
            "document_id": "document-a",
            "vector_id": "point-a",
            "position": 0,
            "text": "old Qdrant secret",
            "enabled": enabled,
            "status": "completed",
            "error": None,
        }
        self.rows: dict[str, dict[str, Any]] = {"segment-a": self.row}
        self.index_states: list[str] = []
        self._segment_locks: dict[tuple[str, str], asyncio.Lock] = {}
        self._document_create_locks: dict[tuple[str, str], asyncio.Lock] = {}
        self._dataset_condition = asyncio.Condition()
        self._dataset_readers = 0
        self._dataset_writer = False
        self.delete_requested = asyncio.Event()
        self.delete_entered = asyncio.Event()
        self.segment_exists = True
        self.lease_requests = 0
        self.second_lease_requested = asyncio.Event()
        self.segment_lease_requested = asyncio.Event()
        self.segment_text_updates = 0
        self.insert_calls = 0
        self.create_lease_requests = 0
        self.second_create_requested = asyncio.Event()
        self.create_lease_requested = asyncio.Event()
        self.next_position_calls = 0

    @contextlib.asynccontextmanager
    async def segment_index_update_lease(
        self,
        dataset_id: str,
        document_id: str,
        segment_id: str,
    ):
        self.lease_requests += 1
        self.segment_lease_requested.set()
        if self.lease_requests == 2:
            self.second_lease_requested.set()
        async with self._dataset_condition:
            if self._dataset_writer:
                raise RuntimeError(
                    "dataset index deletion is in progress; refusing segment update"
                )
            self._dataset_readers += 1
        document_lock = self._document_create_locks.setdefault(
            (dataset_id, document_id),
            asyncio.Lock(),
        )
        segment_lock = self._segment_locks.setdefault(
            (dataset_id, segment_id),
            asyncio.Lock(),
        )
        try:
            async with document_lock, segment_lock:
                yield self
        finally:
            async with self._dataset_condition:
                self._dataset_readers -= 1
                self._dataset_condition.notify_all()

    @contextlib.asynccontextmanager
    async def document_segment_create_lease(
        self,
        dataset_id: str,
        document_id: str,
    ):
        self.create_lease_requests += 1
        self.create_lease_requested.set()
        if self.create_lease_requests == 2:
            self.second_create_requested.set()
        async with self._dataset_condition:
            if self._dataset_writer:
                raise RuntimeError(
                    "dataset index deletion is in progress; refusing segment creation"
                )
            self._dataset_readers += 1
        lock = self._document_create_locks.setdefault(
            (dataset_id, document_id),
            asyncio.Lock(),
        )
        try:
            async with lock:
                yield self
        finally:
            async with self._dataset_condition:
                self._dataset_readers -= 1
                self._dataset_condition.notify_all()

    @contextlib.asynccontextmanager
    async def dataset_delete_barrier(self):
        self.delete_requested.set()
        async with self._dataset_condition:
            while self._dataset_writer or self._dataset_readers:
                await self._dataset_condition.wait()
            self._dataset_writer = True
            self.delete_entered.set()
        try:
            yield
        finally:
            async with self._dataset_condition:
                self._dataset_writer = False
                self._dataset_condition.notify_all()

    async def get_segment(
        self,
        segment_id: str,
        *,
        connection: Any | None = None,
    ) -> dict[str, Any] | None:
        assert connection in (None, self)
        if segment_id == "segment-a" and not self.segment_exists:
            return None
        row = self.rows.get(segment_id)
        return dict(row) if row is not None else None

    async def get_dataset(
        self,
        dataset_id: str,
        *,
        connection: Any | None = None,
    ) -> dict[str, Any] | None:
        assert connection in (None, self)
        return dict(self.dataset) if dataset_id == "dataset-a" else None

    async def update_segment(
        self,
        segment_id: str,
        *,
        text: str,
        connection: Any | None = None,
    ) -> None:
        assert connection in (None, self)
        row = self.rows[segment_id]
        self.segment_text_updates += 1
        row["text"] = text

    async def set_segment_index_state(
        self,
        segment_id: str,
        state: str,
        *,
        error: str | None = None,
        connection: Any | None = None,
    ) -> None:
        assert connection in (None, self)
        row = self.rows[segment_id]
        self.index_states.append(state)
        row["status"] = {
            "pending": "indexing",
            "completed": "completed",
            "error": "error",
        }[state]
        row["error"] = error if state == "error" else None

    async def update_segment_fields(
        self,
        segment_id: str,
        fields: dict[str, Any],
        *,
        connection: Any | None = None,
    ) -> None:
        assert connection in (None, self)
        self.rows[segment_id].update(fields)

    async def get_document(
        self,
        document_id: str,
        *,
        connection: Any | None = None,
    ) -> dict[str, Any] | None:
        assert connection in (None, self)
        if document_id != "document-a":
            return None
        return dict(self.document)

    async def list_segments(self, **_kwargs: Any) -> list[dict[str, Any]]:
        return [dict(row) for row in self.rows.values()]

    async def next_segment_position(
        self,
        dataset_id: str,
        document_id: str,
        *,
        connection: Any | None = None,
    ) -> int:
        assert connection is self
        self.next_position_calls += 1
        positions = [
            int(row.get("position") or 0)
            for row in self.rows.values()
            if row.get("dataset_id") == dataset_id
            and row.get("document_id") == document_id
        ]
        return max(positions, default=-1) + 1

    async def insert_segments(
        self,
        rows: list[dict[str, Any]],
        *,
        connection: Any | None = None,
    ) -> None:
        assert connection in (None, self)
        self.insert_calls += 1
        for item in rows:
            stored = dict(item)
            segment_id = str(stored["segment_id"])
            self.rows[segment_id] = stored
            self.row = stored

    async def refresh_document_segment_count(self, _document_id: str) -> None:
        return None


class Knowledge:
    async def require_dataset_access(
        self,
        _user: UserContext,
        dataset_id: str,
        *,
        required: str,
    ) -> dict[str, Any]:
        assert (dataset_id, required) == ("dataset-a", "editor")
        return _dataset_row()

    def _resolve_embedding_config(self, **_kwargs: Any) -> SimpleNamespace:
        return SimpleNamespace(timeout_seconds=0.1)


class Embedder:
    dimension = 2

    async def embed_documents(self, _texts: list[str]) -> list[list[float]]:
        return [[0.1, 0.2]]

    async def close(self) -> None:
        return None


class VectorStore:
    def __init__(self, *, fail: bool, block_first: bool = False) -> None:
        self.fail = fail
        self.block_first = block_first
        self.payload_text = "old Qdrant secret"
        self.point_exists = True
        self.upsert_calls = 0
        self.first_upsert_started = asyncio.Event()
        self.allow_first_upsert = asyncio.Event()
        self.payload_enabled = True
        self.payload_toggle_calls: list[bool] = []
        self.payload_toggle_failures = 0
        self.last_payload: dict[str, Any] | None = None
        self.ensure_collection_calls = 0
        self.point_payloads: dict[str, dict[str, Any]] = {}

    async def ensure_collection(self, **_kwargs: Any) -> str:
        self.ensure_collection_calls += 1
        return "custom-base"

    async def upsert(self, **kwargs: Any) -> None:
        self.upsert_calls += 1
        payload = dict(kwargs["points"][0].payload)
        self.last_payload = payload
        if self.upsert_calls == 1:
            self.first_upsert_started.set()
            if self.block_first:
                await self.allow_first_upsert.wait()
        if self.fail:
            raise RuntimeError("qdrant unavailable")
        self.payload_text = str(payload["text"])
        self.payload_enabled = bool(payload["enabled"])
        self.point_payloads[str(kwargs["points"][0].id)] = payload
        self.point_exists = True

    async def set_segment_payload_enabled(self, **kwargs: Any) -> list[str]:
        assert kwargs["lifecycle_lease_held"] is True
        assert kwargs["segment_id"] == "segment-a"
        desired = bool(kwargs["enabled"])
        self.payload_toggle_calls.append(desired)
        if self.payload_toggle_failures:
            self.payload_toggle_failures -= 1
            raise RuntimeError("secondary payload update failed")
        self.payload_enabled = desired
        return ["custom-base", "custom-base_sections", "old-generation"]

    async def delete_segment_points(self, **_kwargs: Any) -> list[str]:
        self.point_exists = False
        return ["custom-base", "custom-base_sections", "old-generation"]


def _manager(
    monkeypatch: pytest.MonkeyPatch,
    database: SegmentDatabase,
    vector_store: VectorStore,
) -> ChunkingManager:
    monkeypatch.setattr(
        chunking_module,
        "create_embedding",
        lambda *_args, **_kwargs: Embedder(),
    )
    return ChunkingManager(
        SimpleNamespace(),
        database,  # type: ignore[arg-type]
        vector_store,  # type: ignore[arg-type]
        Knowledge(),
    )


def _is_active(row: dict[str, Any]) -> bool:
    return bool(row.get("enabled", True)) and row.get("status") == "completed"


@pytest.mark.asyncio
async def test_update_failure_hides_old_qdrant_secret_and_retry_recovers(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    database = SegmentDatabase(enabled=True)
    vector_store = VectorStore(fail=True)
    manager = _manager(monkeypatch, database, vector_store)
    user = UserContext(user_id="editor-a", tenant_id="tenant-a")

    failed = await manager.update_segment(
        user,
        "dataset-a",
        "segment-a",
        "replacement text",
    )

    assert failed["enabled"] is True
    assert failed["status"] == "error"
    assert "qdrant unavailable" in failed["_vector_error"]
    assert vector_store.payload_text == "old Qdrant secret"
    assert _is_active(failed) is False
    assert database.index_states == ["pending", "error"]

    vector_store.fail = False
    recovered = await manager.update_segment(
        user,
        "dataset-a",
        "segment-a",
        "replacement text",
    )

    assert recovered["enabled"] is True
    assert recovered["status"] == "completed"
    assert recovered["error"] is None
    assert vector_store.payload_text == "replacement text"
    assert _is_active(recovered) is True
    assert database.index_states == ["pending", "error", "pending", "completed"]


@pytest.mark.asyncio
async def test_update_success_preserves_manual_disabled_toggle(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    database = SegmentDatabase(enabled=False)
    database.row.update(
        {
            "level": 2,
            "content_type": "text",
            "source_type": "upload",
            "language": "zh",
            "metadata": {"section": "alpha"},
            "parent_segment_id": "parent-a",
        }
    )
    vector_store = VectorStore(fail=False)
    manager = _manager(monkeypatch, database, vector_store)

    updated = await manager.update_segment(
        UserContext(user_id="editor-a", tenant_id="tenant-a"),
        "dataset-a",
        "segment-a",
        "replacement text",
    )

    assert updated["enabled"] is False
    assert updated["status"] == "completed"
    assert vector_store.last_payload is not None
    assert vector_store.last_payload == {
        "tenant_id": "tenant-a",
        "dataset_id": "dataset-a",
        "document_id": "document-a",
        "segment_id": "segment-a",
        "position": 0,
        "text": "replacement text",
        "enabled": False,
        "status": "completed",
        "level": 2,
        "content_type": "text",
        "source_type": "upload",
        "language": "zh",
        "metadata": {"section": "alpha"},
        "parent_segment_id": "parent-a",
        "summary": None,
        "page_start": None,
        "page_end": None,
        "source_reference": None,
        "citation_text": None,
        "page_number": None,
        "section_header": None,
        "contextual_prefix": None,
        "answer": None,
        "keywords": None,
    }
    assert vector_store.payload_enabled is False
    assert _is_active(updated) is False


@pytest.mark.asyncio
async def test_create_qdrant_failure_commits_non_active_error_state(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    database = SegmentDatabase()
    vector_store = VectorStore(fail=True)
    manager = _manager(monkeypatch, database, vector_store)

    created = await manager.create_segment(
        UserContext(user_id="editor-a", tenant_id="tenant-a"),
        "dataset-a",
        "document-a",
        "new secret",
    )

    assert created["enabled"] is True
    assert created["status"] == "error"
    assert "qdrant unavailable" in str(created["error"])
    assert _is_active(created) is False
    assert database.index_states == ["pending", "error"]


@pytest.mark.asyncio
async def test_create_writes_explicit_enabled_payload(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    database = SegmentDatabase()
    vector_store = VectorStore(fail=False)
    manager = _manager(monkeypatch, database, vector_store)

    created = await manager.create_segment(
        UserContext(user_id="editor-a", tenant_id="tenant-a"),
        "dataset-a",
        "document-a",
        "new secret",
    )

    assert created["enabled"] is True
    assert created["status"] == "completed"
    assert vector_store.last_payload is not None
    assert vector_store.last_payload["segment_id"] == created["segment_id"]
    assert vector_store.last_payload["enabled"] is True
    assert vector_store.last_payload["status"] == "completed"
    assert vector_store.last_payload["level"] == 3
    assert vector_store.last_payload["content_type"] == "text"
    assert vector_store.last_payload["source_type"] == "upload"
    assert vector_store.last_payload["language"] == "zh"
    assert vector_store.last_payload["metadata"] == {}
    assert vector_store.last_payload["parent_segment_id"] is None


@pytest.mark.parametrize(
    "document_updates",
    [
        {"enabled": False},
        {"archived": True},
        {"status": "processing"},
        {
            "metadata": {
                DOCUMENT_LIFECYCLE_REINDEX_KEY: {
                    "status": "pending",
                    "operation": "enable",
                }
            }
        },
    ],
    ids=["disabled", "archived", "processing", "lifecycle-pending"],
)
@pytest.mark.asyncio
async def test_inactive_document_rejects_update_before_database_or_qdrant(
    monkeypatch: pytest.MonkeyPatch,
    document_updates: dict[str, Any],
) -> None:
    database = SegmentDatabase()
    database.document.update(document_updates)
    vector_store = VectorStore(fail=False)
    manager = _manager(monkeypatch, database, vector_store)

    with pytest.raises(ValidationFailedError, match="inactive or has a pending"):
        await manager.update_segment(
            UserContext(user_id="editor-a", tenant_id="tenant-a"),
            "dataset-a",
            "segment-a",
            "must never persist",
        )

    assert database.segment_text_updates == 0
    assert database.index_states == []
    assert vector_store.ensure_collection_calls == 0
    assert vector_store.upsert_calls == 0


@pytest.mark.parametrize(
    "document_updates",
    [
        {"enabled": False},
        {"archived": True},
        {"status": "processing"},
        {
            "metadata": {
                DOCUMENT_LIFECYCLE_REINDEX_KEY: {
                    "status": "pending",
                    "operation": "enable",
                }
            }
        },
    ],
    ids=["disabled", "archived", "processing", "lifecycle-pending"],
)
@pytest.mark.asyncio
async def test_inactive_document_rejects_create_before_database_or_qdrant(
    monkeypatch: pytest.MonkeyPatch,
    document_updates: dict[str, Any],
) -> None:
    database = SegmentDatabase()
    database.document.update(document_updates)
    vector_store = VectorStore(fail=False)
    manager = _manager(monkeypatch, database, vector_store)

    with pytest.raises(ValidationFailedError, match="inactive or has a pending"):
        await manager.create_segment(
            UserContext(user_id="editor-a", tenant_id="tenant-a"),
            "dataset-a",
            "document-a",
            "must never persist",
        )

    assert database.insert_calls == 0
    assert database.index_states == []
    assert vector_store.ensure_collection_calls == 0
    assert vector_store.upsert_calls == 0


@pytest.mark.asyncio
async def test_concurrent_updates_are_serialized_through_qdrant_completion(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    database = SegmentDatabase(enabled=True)
    vector_store = VectorStore(fail=False, block_first=True)
    manager = _manager(monkeypatch, database, vector_store)
    user = UserContext(user_id="editor-a", tenant_id="tenant-a")

    first = asyncio.create_task(
        manager.update_segment(user, "dataset-a", "segment-a", "replacement A")
    )
    await asyncio.wait_for(vector_store.first_upsert_started.wait(), timeout=1)
    second = asyncio.create_task(
        manager.update_segment(user, "dataset-a", "segment-a", "replacement B")
    )
    await asyncio.wait_for(database.second_lease_requested.wait(), timeout=1)

    # B is deliberately ready to finish first, but cannot pass A's segment
    # lease and therefore cannot produce DB=B/Q=A split-brain state.
    assert vector_store.upsert_calls == 1
    assert database.row["text"] == "replacement A"

    vector_store.allow_first_upsert.set()
    await asyncio.wait_for(asyncio.gather(first, second), timeout=1)

    assert vector_store.upsert_calls == 2
    assert database.row["text"] == "replacement B"
    assert vector_store.payload_text == "replacement B"
    assert database.row["status"] == "completed"
    assert database.index_states == [
        "pending",
        "completed",
        "pending",
        "completed",
    ]


@pytest.mark.asyncio
async def test_worker_claim_first_makes_waiting_manual_update_fail_closed(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    database = SegmentDatabase(enabled=True)
    vector_store = VectorStore(fail=False)
    manager = _manager(monkeypatch, database, vector_store)
    user = UserContext(user_id="editor-a", tenant_id="tenant-a")

    async with database.document_segment_create_lease(
        "dataset-a",
        "document-a",
    ):
        update_task = asyncio.create_task(
            manager.update_segment(
                user,
                "dataset-a",
                "segment-a",
                "must never persist",
            )
        )
        await asyncio.wait_for(database.segment_lease_requested.wait(), timeout=1)
        database.document["status"] = "processing"

    with pytest.raises(ValidationFailedError, match="inactive or has a pending"):
        await asyncio.wait_for(update_task, timeout=1)
    assert database.segment_text_updates == 0
    assert database.index_states == []
    assert vector_store.upsert_calls == 0


@pytest.mark.asyncio
async def test_manual_update_finishes_before_short_worker_claim(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    database = SegmentDatabase(enabled=True)
    vector_store = VectorStore(fail=False, block_first=True)
    manager = _manager(monkeypatch, database, vector_store)
    user = UserContext(user_id="editor-a", tenant_id="tenant-a")

    update_task = asyncio.create_task(
        manager.update_segment(
            user,
            "dataset-a",
            "segment-a",
            "replacement before claim",
        )
    )
    await asyncio.wait_for(vector_store.first_upsert_started.wait(), timeout=1)

    async def claim_worker_generation() -> None:
        async with database.document_segment_create_lease(
            "dataset-a",
            "document-a",
        ):
            database.document["status"] = "processing"

    claim_task = asyncio.create_task(claim_worker_generation())
    await asyncio.wait_for(database.create_lease_requested.wait(), timeout=1)
    await asyncio.sleep(0)
    assert claim_task.done() is False

    vector_store.allow_first_upsert.set()
    await asyncio.wait_for(update_task, timeout=1)
    await asyncio.wait_for(claim_task, timeout=1)

    assert vector_store.payload_text == "replacement before claim"
    assert database.row["status"] == "completed"
    assert database.document["status"] == "processing"


@pytest.mark.asyncio
async def test_concurrent_creates_allocate_distinct_positions_without_orphans(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    database = SegmentDatabase(enabled=True)
    vector_store = VectorStore(fail=False, block_first=True)
    manager = _manager(monkeypatch, database, vector_store)
    user = UserContext(user_id="editor-a", tenant_id="tenant-a")

    first = asyncio.create_task(
        manager.create_segment(
            user,
            "dataset-a",
            "document-a",
            "created A",
        )
    )
    await asyncio.wait_for(vector_store.first_upsert_started.wait(), timeout=1)
    second = asyncio.create_task(
        manager.create_segment(
            user,
            "dataset-a",
            "document-a",
            "created B",
        )
    )
    await asyncio.wait_for(database.second_create_requested.wait(), timeout=1)

    # Position allocation, DB insert, and Qdrant publication are one serialized
    # document generation. The second creator cannot overwrite position 1 while
    # the first point is still being published.
    assert database.next_position_calls == 1
    assert database.insert_calls == 1
    assert vector_store.upsert_calls == 1

    vector_store.allow_first_upsert.set()
    created = await asyncio.wait_for(asyncio.gather(first, second), timeout=1)

    created_ids = {str(row["segment_id"]) for row in created}
    assert len(created_ids) == 2
    assert {int(row["position"]) for row in created} == {1, 2}
    assert database.next_position_calls == 2
    assert database.insert_calls == 2
    assert created_ids <= set(database.rows)
    assert set(vector_store.point_payloads) == created_ids
    for segment_id in created_ids:
        assert vector_store.point_payloads[segment_id]["position"] == (
            database.rows[segment_id]["position"]
        )


@pytest.mark.asyncio
async def test_payload_toggle_partial_failures_are_safe_and_retryable(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    database = SegmentDatabase(enabled=True)
    vector_store = VectorStore(fail=False)
    manager = _manager(monkeypatch, database, vector_store)
    user = UserContext(user_id="editor-a", tenant_id="tenant-a")

    vector_store.payload_toggle_failures = 1
    with pytest.raises(RuntimeError, match="secondary payload update failed"):
        await manager.set_segment_enabled(
            user,
            "dataset-a",
            "segment-a",
            False,
        )
    assert database.row["enabled"] is False
    assert vector_store.payload_enabled is True

    disabled = await manager.set_segment_enabled(
        user,
        "dataset-a",
        "segment-a",
        False,
    )
    assert disabled["enabled"] is False
    assert vector_store.payload_enabled is False

    vector_store.payload_toggle_failures = 1
    with pytest.raises(RuntimeError, match="secondary payload update failed"):
        await manager.set_segment_enabled(
            user,
            "dataset-a",
            "segment-a",
            True,
        )
    assert database.row["enabled"] is False
    assert vector_store.payload_enabled is False

    enabled = await manager.set_segment_enabled(
        user,
        "dataset-a",
        "segment-a",
        True,
    )
    assert enabled["enabled"] is True
    assert vector_store.payload_enabled is True
    assert vector_store.payload_toggle_calls == [False, False, True, True]


@pytest.mark.asyncio
async def test_delete_exclusive_rejects_update_before_database_or_qdrant(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    database = SegmentDatabase(enabled=True)
    vector_store = VectorStore(fail=False)
    manager = _manager(monkeypatch, database, vector_store)

    async with database.dataset_delete_barrier():
        with pytest.raises(RuntimeError, match="refusing segment update"):
            await manager.update_segment(
                UserContext(user_id="editor-a", tenant_id="tenant-a"),
                "dataset-a",
                "segment-a",
                "must never persist",
            )

    assert database.row["text"] == "old Qdrant secret"
    assert database.index_states == []
    assert vector_store.upsert_calls == 0


@pytest.mark.asyncio
async def test_update_shared_finishes_before_delete_sweeps_without_orphan(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    database = SegmentDatabase(enabled=True)
    vector_store = VectorStore(fail=False, block_first=True)
    manager = _manager(monkeypatch, database, vector_store)
    user = UserContext(user_id="editor-a", tenant_id="tenant-a")

    update_task = asyncio.create_task(
        manager.update_segment(
            user,
            "dataset-a",
            "segment-a",
            "replacement before delete",
        )
    )
    await asyncio.wait_for(vector_store.first_upsert_started.wait(), timeout=1)

    async def delete_after_barrier() -> None:
        async with database.dataset_delete_barrier():
            await vector_store.delete_segment_points()
            database.segment_exists = False

    delete_task = asyncio.create_task(delete_after_barrier())
    await asyncio.wait_for(database.delete_requested.wait(), timeout=1)
    assert database.delete_entered.is_set() is False

    vector_store.allow_first_upsert.set()
    await asyncio.wait_for(update_task, timeout=1)
    await asyncio.wait_for(delete_task, timeout=1)

    assert database.segment_exists is False
    assert vector_store.point_exists is False
