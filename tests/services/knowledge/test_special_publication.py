"""Cross-store special publication keeps old generations until durable commit."""

from __future__ import annotations

import hashlib
import json
from contextlib import asynccontextmanager
from types import SimpleNamespace
from typing import Any

import pytest
from knowledge_service.services.knowledge.special_publication import (
    SpecialPublicationCoordinator,
    SpecialSourceUnverifiableError,
)
from qdrant_client.http.models import PointStruct

GENERATION = "00000000-0000-4000-8000-000000000001"
TEXT_GENERATION = "00000000-0000-4000-8000-000000000002"
DATASET = {"dataset_id": "dataset-a", "tenant_id": "tenant-a", "collection_name": "base"}


class Connection:
    def __init__(self, db: Database) -> None:
        self.db = db
        self.depth = 0

    @asynccontextmanager
    async def transaction(self):
        self.depth += 1
        try:
            yield self
        finally:
            self.depth -= 1
            if self.depth == 0 and self.db.fail_after_commit_once and self.db.special["phase"] == "committed":
                self.db.fail_after_commit_once = False
                raise RuntimeError("injected post-commit interruption")

    async def fetchrow(self, query: str, *_args: Any) -> dict[str, Any] | None:
        if "SELECT title, current_version" in query:
            return dict(self.db.document)
        if "FROM document_versions" in query or query.lstrip().startswith("UPDATE document_versions"):
            version_number = int(_args[1])
            if query.lstrip().startswith("UPDATE"):
                self.db.versions[version_number - 1]["metadata"]["_special_source_manifest"] = json.loads(_args[3])
                return {"version_id": str(version_number)}
            return (
                dict(self.db.versions[version_number - 1])
                if 0 < version_number <= len(self.db.versions) else None
            )
        raise AssertionError(query)

    async def fetchval(self, query: str, *_args: Any) -> int:
        assert "MAX(version_number)" in query
        return len(self.db.versions) + 1

    async def execute(self, query: str, *_args: Any) -> str:
        assert "SET version_count" in query and self.depth > 0
        self.db.document["version_count"] = len(self.db.versions)
        return "UPDATE 1"


class Database:
    def __init__(self, object_keys: list[str]) -> None:
        self.document = {
            "title": "Document", "current_version": 0,
            "metadata": {
                "_document_pipeline_execution_id": GENERATION,
                "_special_publication_generation_id": GENERATION,
                "original_file_key": "source/original.pdf",
            },
            "content": "old", "status": "indexing", "version_count": 1,
        }
        self.special = {
            "generation_id": GENERATION,
            "source_hash": hashlib.sha256(b"source").hexdigest(),
            "plan_hash": None,
            "planned_object_keys": sorted(object_keys),
            "phase": "preparing", "collections": {}, "objects": {},
        }
        self.revision = 1
        self.events: list[str] = []
        self.connection = Connection(self)
        self.fail_commit = False
        self.fail_after_commit_once = False
        self.versions: list[dict[str, Any]] = []
        self.image_rows: list[dict[str, Any]] = []
        self.execution_status = "running"
        self.active_execution_id = GENERATION
        self.execution_id = GENERATION
        self.operator_disabled_positions: set[int] = set()
        self.lease_special_ids: list[str | None] = []
        self.shared_lease_flags: list[bool] = []

    def owner(self) -> dict[str, Any]:
        return {
            "execution_id": self.execution_id, "document_id": "document-a",
            "dataset_id": "dataset-a", "execution_status": self.execution_status,
            **self.special,
        }

    async def get_dataset(self, _dataset_id: str) -> dict[str, Any]:
        return {**DATASET, "content_revision": self.revision}

    async def get_document(self, _document_id: str) -> dict[str, Any]:
        return {"dataset_id": "dataset-a", **self.document}

    async def get_image_segments_by_document(
        self, _document_id: str, *, connection: Any,
    ) -> list[dict[str, Any]]:
        assert connection is self.connection
        return list(self.image_rows)

    async def get_operator_disabled_segment_positions(
        self, _document_id: str, content_type: str, *, connection: Any,
    ) -> set[int]:
        assert content_type == "text" and connection is self.connection
        return set(self.operator_disabled_positions)

    async def resume_special_preparing_execution(
        self, _execution_id: str, _document_id: str, _dataset_id: str,
        _generation_id: str, _source_hash: str, *, connection: Any,
    ) -> None:
        assert connection is self.connection and connection.depth > 0
        assert self.special["phase"] == "preparing" and self.execution_status == "running"
        self.special = {}
        self.document["status"] = "waiting"
        self.document["metadata"].pop("_special_publication_generation_id", None)
        self.events.append("preparing_requeued")

    async def get_special_publication_manifest(
        self, _execution_id: str, *, connection: Any = None,
    ) -> dict[str, Any]:
        del connection
        return self.owner()

    async def get_active_special_publication_for_dataset(
        self, _dataset_id: str, *, connection: Any = None,
    ) -> dict[str, Any]:
        assert connection is None or connection is self.connection
        return {**self.owner(), "execution_id": self.active_execution_id}

    async def advance_special_publication_manifest(
        self, _execution_id: str, _document_id: str,
        _dataset_id: str, _generation_id: str, *, expected_phase: str,
        next_phase: str, source_hash: str, plan_hash: str | None = None,
        collections: dict[str, Any] | None = None,
        objects: dict[str, str] | None = None,
        connection: Any = None,
    ) -> dict[str, Any]:
        del connection
        assert self.special["phase"] == expected_phase
        assert self.special["source_hash"] == source_hash
        if collections is not None:
            self.special["collections"] = collections
        if objects is not None:
            self.special["objects"] = objects
        self.special["plan_hash"] = plan_hash or self.special["plan_hash"]
        self.special["phase"] = next_phase
        self.events.append(next_phase)
        return dict(self.special)

    @asynccontextmanager
    async def dataset_index_publication_lease(
        self, _dataset_id: str, *, expected_ingestion_identity: str,
        special_execution_id: str | None = None,
        document_shared_lease_held: bool = False,
    ):
        assert expected_ingestion_identity
        self.lease_special_ids.append(special_execution_id)
        self.shared_lease_flags.append(document_shared_lease_held)
        recovered = self.revision < 0
        if not recovered:
            self.revision = -self.revision
        if special_execution_id is not None:
            self.special["publication_revision"] = abs(self.revision)
        yield SimpleNamespace(
            connection=self.connection, revision=self.revision, recovered=recovered,
        )

    async def commit_text_segment_publication(self, **kwargs: Any) -> tuple[int, int]:
        assert self.connection.depth > 0
        assert kwargs["finish_publication"] is False
        assert kwargs["delete_all_excess"] is True
        assert kwargs["finalize_document"] is True
        assert kwargs["defer_terminal_until_fence_release"] is True
        assert kwargs["staged_segment_ids"] == kwargs["keep_segment_ids"]
        if self.fail_commit:
            raise RuntimeError("injected PG commit failure")
        self.document["status"] = "completed"
        self.document["content"] = kwargs["candidate_content"]
        self.document["metadata"].update(kwargs["candidate_metadata_patch"])
        if kwargs["delete_all_excess"]:
            self.image_rows = []
        if kwargs["candidate_version_number"] is not None:
            self.document["current_version"] = kwargs["candidate_version_number"]
            self.versions[kwargs["previous_version_number"] - 1]["change_type"] = "updated"
            self.versions[kwargs["candidate_version_number"] - 1]["change_type"] = "restored"
        self.events.append("pg_committed")
        return len(kwargs["segment_rows"]), 0

    async def save_document_summary(self, row: dict[str, Any], *, connection: Any) -> bool:
        assert connection.depth > 0
        assert row["document_id"] == "document-a"
        self.events.append("summary_saved")
        return True

    async def delete_document_summary(self, _document_id: str, *, connection: Any) -> bool:
        assert connection.depth > 0
        self.events.append("summary_deleted")
        return True

    async def create_document_version(
        self, _document_id: str, content: str, content_hash: str,
        _change_type: str, *, title: str, metadata: dict[str, Any],
        connection: Any, activate: bool = True,
    ) -> dict[str, Any]:
        assert title == "Document" and connection.depth > 0
        assert content_hash == hashlib.sha256(content.encode()).hexdigest()
        number = len(self.versions) + 1
        self.versions.append({
            "content": content, "content_hash": content_hash,
            "metadata": metadata,
        })
        if activate:
            self.document["current_version"] = number
            self.document["version_count"] += 1
        self.events.append("version_created" if activate else "baseline_saved")
        return {"version_number": number}

    async def update_document_status(
        self, _document_id: str, *, status: str, error: str, connection: Any,
    ) -> None:
        assert connection.depth > 0 and error
        self.document["status"] = status
        self.events.append(status)

    async def finish_index_publication(self, _dataset_id: str, *, connection: Any) -> int:
        assert connection.depth > 0 and self.revision < 0
        self.revision = abs(self.revision) + 1
        self.events.append("fence_closed")
        return self.revision

    async def reconcile_special_publication_execution(
        self, *_args: Any, connection: Any,
    ) -> str:
        assert connection.depth > 0 and self.revision < 0
        self.events.append("reconciled")
        return "reconciled"


class Vectors:
    def __init__(self) -> None:
        self.points: dict[str, set[str]] = {
            "base": {"old-base"}, "base_sections": {"old-section"},
        }
        self.events: list[str] = []
        self.fail_upsert: str | None = None
        self.fail_old_delete = False

    async def document_point_ids_by_collection(self, **_kwargs: Any) -> dict[str, list[str]]:
        self.events.append("freeze_old")
        return {name: sorted(ids) for name, ids in self.points.items() if ids}

    async def ensure_collection(self, **kwargs: Any) -> str:
        self.events.append("ensure")
        return kwargs["collection_name"]

    async def upsert(self, collection: str, points: list[PointStruct], **_kwargs: Any) -> None:
        self.events.append("upsert")
        self.points.setdefault(collection, set()).update(str(point.id) for point in points)
        if self.fail_upsert == collection:
            raise RuntimeError("injected candidate upsert failure")

    async def delete_points(
        self, _collection: str, _point_ids: list[str], **_kwargs: Any,
    ) -> None:
        raise AssertionError("special publication used the old point deletion API")

    async def delete_document_points_by_ids(
        self, collection: str, point_ids: list[str], *, tenant_id: str,
        dataset_id: str, document_id: str, lifecycle_lease_held: bool,
    ) -> None:
        assert (tenant_id, dataset_id, document_id) == (
            "tenant-a", "dataset-a", "document-a",
        )
        assert lifecycle_lease_held is True
        if collection not in self.points:
            self.events.append("delete_missing_collection")
            return
        if self.fail_old_delete and any(value.startswith("old-") for value in point_ids):
            self.fail_old_delete = False
            raise RuntimeError("injected old cleanup failure")
        self.events.append("delete_old" if any(
            value.startswith("old-") for value in point_ids
        ) else "delete_candidate")
        self.points[collection].difference_update(point_ids)


class Storage:
    def __init__(self) -> None:
        self.objects: set[str] = set()
        self.deleted: list[str] = []
        self.object_data: dict[str, bytes] = {}
        self.fail_delete = False
        self.false_delete = False

    @staticmethod
    def _generate_key(
        tenant_id: str, document_id: str, attachment_id: str, filename: str,
    ) -> str:
        return f"knowledge/confluence/{tenant_id}/{document_id}/images/{attachment_id}_{filename}"

    async def delete_image(self, **kwargs: Any) -> bool:
        if self.fail_delete:
            raise RuntimeError("injected object cleanup failure")
        if self.false_delete:
            return False
        key = self._generate_key(**kwargs)
        self.deleted.append(key)
        self.objects.discard(key)
        self.object_data.pop(key, None)
        return True

    async def download_image(self, **kwargs: Any) -> bytes:
        return self.object_data[self._generate_key(**kwargs)]

    async def image_exists(self, **kwargs: Any) -> bool:
        return self._generate_key(**kwargs) in self.objects


class ActiveLifecycle:
    def __init__(self, db: Database, vectors: Vectors) -> None:
        self.db = db
        self.vectors = vectors
        self.events: list[str] = []

    async def active_publication_context(self, _dataset_id: str) -> dict[str, Any]:
        return {"active": True}

    async def recertify_active_publication(
        self, _context: dict[str, Any], *, publication_revision: int,
        connection: Connection,
    ) -> dict[str, int]:
        assert connection.depth > 0 and publication_revision < 0
        assert "old-base" not in self.vectors.points["base"]
        assert self.db.revision < 0
        self.events.append("recertified")
        return {"target_revision": abs(self.db.revision) + 1}

    async def settle_active_publication(
        self, _context: dict[str, Any], _certification: dict[str, int],
        *, connection: Connection,
    ) -> None:
        assert connection.depth > 0 and self.db.revision > 0
        self.events.append("settled")


def _plan(storage: Storage, *, image: bool = False) -> SimpleNamespace:
    point_a = PointStruct(
        id="00000000-0000-4000-8000-000000000011", vector=[0.1, 0.2],
        payload={"document_id": "document-a", "text": "new"},
    )
    point_b = PointStruct(
        id="00000000-0000-4000-8000-000000000012", vector=[0.2, 0.3],
        payload={"document_id": "document-a", "text": "section"},
    )
    key = storage._generate_key(
        "tenant-a", "document-a", f"page_1_g{GENERATION.replace('-', '')}", "page_1.png",
    )
    objects = [{
        "storage_key": key, "sha256": hashlib.sha256(b"page").hexdigest(),
        "attachment_id": f"page_1_g{GENERATION.replace('-', '')}",
    }] if image else []
    if image:
        storage.objects.add(key)
    return SimpleNamespace(
        generation_id=GENERATION, document_id="document-a", dataset_id="dataset-a",
        source_hash=hashlib.sha256(b"source").hexdigest(),
        content="[Page 1]\nnew content" if image else "new content",
        total_pages=1 if image else None,
        extracted_texts={1: "new content"} if image else None,
        points_by_collection={"base": [point_a], "base_sections": [point_b]},
        segment_rows=[
            {"segment_id": str(point_a.id), "dataset_id": "dataset-a",
             "document_id": "document-a", "position": 0, "text": "new",
             "content_type": "text", "enabled": False, "status": "indexing", "metadata": {}},
            {"segment_id": str(point_b.id), "dataset_id": "dataset-a",
             "document_id": "document-a", "position": 0, "text": "section",
             "content_type": "section", "enabled": False, "status": "indexing", "metadata": {}},
        ],
        segment_ids=[str(point_a.id), str(point_b.id)],
        object_manifest=objects,
        summary_row={"document_id": "document-a", "vector_id": str(point_b.id)},
    )


def _setup(*, image: bool = False) -> tuple[SpecialPublicationCoordinator, Any, Database, Vectors, Storage]:
    storage = Storage()
    plan = _plan(storage, image=image)
    db = Database([receipt["storage_key"] for receipt in plan.object_manifest])
    vectors = Vectors()
    service = SimpleNamespace(
        db=db, vector_store=vectors, image_storage_service=storage,
        bm25_v2_lifecycle_service=None,
    )
    return SpecialPublicationCoordinator(service), plan, db, vectors, storage


def _prepare_text_restore_after_vision(
    db: Database, storage: Storage, vision_plan: Any,
) -> SimpleNamespace:
    historical = dict(db.versions[0]["metadata"]["_special_source_manifest"])
    assert historical["source_kind"] == "text"
    assert historical["source_hash"] == hashlib.sha256(b"old").hexdigest()
    old_key = vision_plan.object_manifest[0]["storage_key"]
    storage.object_data[old_key] = b"page"
    db.image_rows = [{
        "image_attachment_id": vision_plan.object_manifest[0]["attachment_id"],
        "image_filename": "page_1.png", "text": vision_plan.content,
        "metadata": {"page_number": 1},
    }]
    db.versions.extend([
        {"content": vision_plan.content,
         "content_hash": hashlib.sha256(vision_plan.content.encode()).hexdigest(),
         "metadata": dict(db.document["metadata"]),
         "change_type": "pending_before_restore"},
        {"content": "old", "content_hash": hashlib.sha256(b"old").hexdigest(),
         "metadata": {"_special_source_manifest": historical},
         "change_type": "pending_restore"},
    ])
    db.document["metadata"]["_document_pending_restore_version"] = {
        "previous_version": 3, "candidate_version": 4,
    }
    db.document["metadata"].update({
        "processing_mode": "scanned", "extracted_images": [{"storage_key": old_key}],
        "image_count": 1, "images_embedded": True, "embedded_image_count": 1,
    })
    db.document["status"] = "indexing"
    db.execution_id = TEXT_GENERATION
    db.active_execution_id = TEXT_GENERATION
    db.special = {
        "generation_id": TEXT_GENERATION,
        "source_hash": hashlib.sha256(b"old").hexdigest(),
        "plan_hash": None, "planned_object_keys": [], "phase": "preparing",
        "collections": {}, "objects": {},
    }
    point = PointStruct(
        id="00000000-0000-4000-8000-000000000021", vector=[0.1, 0.2],
        payload={"document_id": "document-a", "text": "old"},
    )
    return SimpleNamespace(
        generation_id=TEXT_GENERATION, document_id="document-a",
        dataset_id="dataset-a", source_kind="text",
        source_hash=hashlib.sha256(b"old").hexdigest(), content="old",
        points_by_collection={"base": [point]},
        segment_rows=[{
            "segment_id": str(point.id), "dataset_id": "dataset-a",
            "document_id": "document-a", "position": 0, "text": "old",
            "content_type": "text", "enabled": False,
            "status": "indexing", "metadata": {},
        }],
        segment_ids=[str(point.id)], object_manifest=[], summary_row=None,
    )


@pytest.mark.asyncio
async def test_multi_collection_publication_commits_before_old_point_cleanup() -> None:
    coordinator, plan, db, vectors, _storage = _setup()
    result = await coordinator.publish(plan, DATASET, GENERATION, "old")
    assert result == plan.segment_ids
    assert db.special["phase"] == "committed" and db.revision > 0
    assert vectors.points == {
        "base": {plan.segment_ids[0]}, "base_sections": {plan.segment_ids[1]},
    }
    assert vectors.events.index("freeze_old") < vectors.events.index("upsert")
    assert db.events.index("pg_committed") < db.events.index("fence_closed")
    assert db.events.index("summary_saved") < db.events.index("committed")
    assert all(point.payload["source_version"] == 2 for points in plan.points_by_collection.values() for point in points)
    assert all(row["metadata"]["source_hash"] == hashlib.sha256(b"new content").hexdigest() for row in plan.segment_rows)
    assert [version["content"] for version in db.versions] == ["old", "new content"]
    assert db.versions[0]["metadata"]["_special_source_manifest"]["content_hash"] == hashlib.sha256(b"old").hexdigest()
    assert db.versions[1]["metadata"]["_special_source_manifest"]["generation_id"] == GENERATION
    assert db.versions[1]["metadata"]["_special_source_manifest"]["source_kind"] == "hierarchy"
    assert db.versions[1]["metadata"]["_special_source_manifest"]["original_source_key"] == "source/original.pdf"
    assert db.versions[1]["metadata"]["_special_source_manifest"]["index_config"] == {}
    assert db.document["current_version"] == 2
    assert db.document["version_count"] == 2
    assert db.lease_special_ids == [GENERATION]


@pytest.mark.asyncio
async def test_first_text_snapshot_keeps_pinned_index_config() -> None:
    coordinator, plan, db, _vectors, _storage = _setup()
    index_config = {"chunking": {"mode": "paragraph"}}
    await coordinator.publish(plan, {**DATASET, "index_config": index_config}, GENERATION, "old")
    assert db.versions[0]["metadata"]["_special_source_manifest"]["index_config"] == index_config


@pytest.mark.asyncio
async def test_existing_document_shared_lease_is_forwarded_to_publication() -> None:
    coordinator, plan, db, _vectors, _storage = _setup()
    await coordinator.publish(
        plan, DATASET, GENERATION, "old", document_shared_lease_held=True,
    )
    assert db.shared_lease_flags == [True]


@pytest.mark.asyncio
async def test_first_ingest_has_only_candidate_source_version() -> None:
    coordinator, plan, db, vectors, _storage = _setup()
    vectors.points.clear()
    await coordinator.publish(plan, DATASET, GENERATION, "old")
    assert [version["content"] for version in db.versions] == ["new content"]
    assert db.document["current_version"] == 1
    assert db.document["version_count"] == 1
    assert all(
        point.payload["source_version"] == 1
        for points in plan.points_by_collection.values() for point in points
    )


@pytest.mark.asyncio
async def test_vision_version_manifest_keeps_ordered_page_text_receipts() -> None:
    coordinator, plan, db, _vectors, _storage = _setup(image=True)
    await coordinator.publish(plan, DATASET, GENERATION, "old")
    source = db.versions[-1]["metadata"]["_special_source_manifest"]
    assert source["source_kind"] == "vision"
    assert source["original_source_key"] == "source/original.pdf"
    assert source["source_hash"] == plan.source_hash
    assert source["content_hash"] == hashlib.sha256(plan.content.encode()).hexdigest()
    assert source["page_texts"] == [{
        "page_number": 1, "text": "new content",
        "storage_key": plan.object_manifest[0]["storage_key"],
        "sha256": plan.object_manifest[0]["sha256"],
    }]
    assert db.document["metadata"]["_special_source_manifest"] == source


def test_vision_page_receipts_sort_independent_of_upload_order() -> None:
    coordinator, plan, _db, _vectors, storage = _setup(image=True)
    key = storage._generate_key(
        "tenant-a", "document-a", f"page_2_g{GENERATION.replace('-', '')}", "page_2.png",
    )
    plan.object_manifest.insert(0, {
        "storage_key": key, "sha256": hashlib.sha256(b"page-2").hexdigest(),
        "attachment_id": f"page_2_g{GENERATION.replace('-', '')}",
    })
    plan.total_pages = 2
    plan.extracted_texts = {2: "second", 1: "new content"}
    plan.content = "[Page 1]\nnew content\n\n[Page 2]\nsecond"
    pages = coordinator._vision_page_texts(plan, coordinator._object_plan(plan))
    assert [page["page_number"] for page in pages] == [1, 2]
    assert [page["text"] for page in pages] == ["new content", "second"]


@pytest.mark.asyncio
async def test_existing_version_content_divergence_refuses_mixed_old_snapshot() -> None:
    coordinator, plan, db, vectors, _storage = _setup()
    db.versions = [{
        "content": "different old", "content_hash": hashlib.sha256(b"different old").hexdigest(),
        "metadata": {},
    }]
    db.document["current_version"] = 1
    with pytest.raises(SpecialSourceUnverifiableError, match="serving content differ"):
        await coordinator.publish(plan, DATASET, GENERATION, "old")
    assert "upsert" not in vectors.events
    assert db.special["phase"] == "aborted"


def _pending_restore(db: Database, plan: SimpleNamespace) -> None:
    content_hash = hashlib.sha256(plan.content.encode()).hexdigest()
    db.document["current_version"] = 1
    db.document["metadata"]["_document_pending_restore_version"] = {
        "previous_version": 2, "candidate_version": 3,
    }
    db.versions = [
        {"content": "old", "content_hash": hashlib.sha256(b"old").hexdigest(),
         "metadata": {}, "change_type": "created"},
        {"content": "old", "content_hash": hashlib.sha256(b"old").hexdigest(),
         "metadata": {}, "change_type": "pending_before_restore"},
        {"content": plan.content, "content_hash": content_hash,
         "change_type": "pending_restore", "metadata": {
             "_special_source_manifest": {
                 "source_kind": "hierarchy", "source_hash": plan.source_hash,
                 "content_hash": content_hash, "index_config": {},
                 "original_source_key": "historical/source.txt", "page_texts": [],
                 "objects": {},
             },
         }},
    ]


@pytest.mark.asyncio
async def test_pending_restore_publishes_only_exact_saved_special_source() -> None:
    coordinator, plan, db, _vectors, _storage = _setup()
    _pending_restore(db, plan)
    await coordinator.publish(plan, DATASET, GENERATION, "old")
    assert db.document["current_version"] == 3
    assert db.document["version_count"] == 3
    assert all(
        point.payload["source_version"] == 3
        for points in plan.points_by_collection.values() for point in points
    )
    source = db.versions[2]["metadata"]["_special_source_manifest"]
    assert source["source_kind"] == "hierarchy"
    assert source["original_source_key"] == "historical/source.txt"


@pytest.mark.asyncio
async def test_pending_restore_content_mismatch_refuses_before_upsert() -> None:
    coordinator, plan, db, vectors, _storage = _setup()
    _pending_restore(db, plan)
    db.versions[2]["content"] = "another candidate"
    db.versions[2]["content_hash"] = hashlib.sha256(b"another candidate").hexdigest()
    with pytest.raises(SpecialSourceUnverifiableError, match="content differs"):
        await coordinator.publish(plan, DATASET, GENERATION, "old")
    assert "upsert" not in vectors.events
    assert db.special["phase"] == "aborted"


@pytest.mark.asyncio
async def test_first_text_to_vision_to_text_restores_one_fenced_generation() -> None:
    coordinator, vision_plan, db, vectors, storage = _setup(image=True)
    await coordinator.publish(vision_plan, DATASET, GENERATION, "old")
    text_plan = _prepare_text_restore_after_vision(db, storage, vision_plan)
    old_object = vision_plan.object_manifest[0]["storage_key"]

    assert await coordinator.publish(
        text_plan, DATASET, TEXT_GENERATION, vision_plan.content,
    ) == text_plan.segment_ids
    assert db.revision > 0 and db.special["phase"] == "committed"
    assert vectors.points == {"base": {text_plan.segment_ids[0]}, "base_sections": set()}
    assert db.document["content"] == "old"
    assert db.document["current_version"] == 4
    assert db.versions[3]["change_type"] == "restored"
    assert db.versions[3]["metadata"]["_special_source_manifest"]["source_kind"] == "text"
    assert db.document["metadata"]["_special_source_manifest"]["source_kind"] == "text"
    assert db.document["metadata"]["processing_mode"] == "text_only"
    assert db.document["metadata"]["extracted_images"] == []
    assert db.document["metadata"]["image_count"] == 0
    assert db.image_rows == []
    assert old_object in storage.objects  # Historical vision version still owns its bytes.
    assert db.events.index("pg_committed") < db.events.index("fence_closed")


@pytest.mark.asyncio
async def test_text_restore_point_keeps_operator_disabled_position() -> None:
    coordinator, vision_plan, db, _vectors, storage = _setup(image=True)
    await coordinator.publish(vision_plan, DATASET, GENERATION, "old")
    text_plan = _prepare_text_restore_after_vision(db, storage, vision_plan)
    db.operator_disabled_positions.add(0)

    await coordinator.publish(text_plan, DATASET, TEXT_GENERATION, vision_plan.content)

    assert text_plan.points_by_collection["base"][0].payload["enabled"] is False


@pytest.mark.asyncio
async def test_text_restore_candidate_failure_keeps_vision_serving() -> None:
    coordinator, vision_plan, db, vectors, storage = _setup(image=True)
    await coordinator.publish(vision_plan, DATASET, GENERATION, "old")
    text_plan = _prepare_text_restore_after_vision(db, storage, vision_plan)
    before = {name: set(ids) for name, ids in vectors.points.items()}
    vectors.fail_upsert = "base"

    with pytest.raises(RuntimeError, match="candidate upsert failure"):
        await coordinator.publish(text_plan, DATASET, TEXT_GENERATION, vision_plan.content)
    assert vectors.points == before
    assert db.document["content"] == vision_plan.content
    assert db.document["current_version"] == 2
    assert db.image_rows
    assert storage.objects == {vision_plan.object_manifest[0]["storage_key"]}
    assert db.special["phase"] == "aborted" and db.revision > 0


@pytest.mark.asyncio
async def test_text_restore_post_commit_crash_reconciles_old_collections() -> None:
    coordinator, vision_plan, db, vectors, storage = _setup(image=True)
    await coordinator.publish(vision_plan, DATASET, GENERATION, "old")
    text_plan = _prepare_text_restore_after_vision(db, storage, vision_plan)
    db.fail_after_commit_once = True

    with pytest.raises(RuntimeError, match="post-commit interruption"):
        await coordinator.publish(text_plan, DATASET, TEXT_GENERATION, vision_plan.content)
    assert db.special["phase"] == "committed" and db.revision < 0
    assert "base_sections" in vectors.points
    assert await coordinator.recover_unfinished(DATASET)
    assert db.revision > 0
    assert vectors.points == {"base": {text_plan.segment_ids[0]}, "base_sections": set()}


@pytest.mark.asyncio
@pytest.mark.parametrize("missing", ["content_hash", "source_hash", "source_kind", "index_config"])
async def test_text_restore_missing_source_evidence_refuses_before_upsert(missing: str) -> None:
    coordinator, vision_plan, db, vectors, storage = _setup(image=True)
    await coordinator.publish(vision_plan, DATASET, GENERATION, "old")
    text_plan = _prepare_text_restore_after_vision(db, storage, vision_plan)
    if missing == "content_hash":
        db.versions[3].pop("content_hash")
    else:
        db.versions[3]["metadata"]["_special_source_manifest"].pop(missing)
    before = {name: set(ids) for name, ids in vectors.points.items()}
    upserts_before = vectors.events.count("upsert")

    with pytest.raises(SpecialSourceUnverifiableError):
        await coordinator.publish(text_plan, DATASET, TEXT_GENERATION, vision_plan.content)
    assert vectors.events.count("upsert") == upserts_before
    assert vectors.points == before
    assert db.special["phase"] == "aborted"


@pytest.mark.asyncio
async def test_text_restore_requires_current_special_source_receipt() -> None:
    coordinator, vision_plan, db, vectors, storage = _setup(image=True)
    await coordinator.publish(vision_plan, DATASET, GENERATION, "old")
    text_plan = _prepare_text_restore_after_vision(db, storage, vision_plan)
    db.document["metadata"].pop("_special_source_manifest")
    upserts_before = vectors.events.count("upsert")

    with pytest.raises(SpecialSourceUnverifiableError, match="serving source receipt"):
        await coordinator.publish(text_plan, DATASET, TEXT_GENERATION, vision_plan.content)
    assert vectors.events.count("upsert") == upserts_before
    assert db.special["phase"] == "aborted"


@pytest.mark.asyncio
async def test_legacy_old_point_without_tenant_is_deleted_by_document_scope() -> None:
    coordinator, plan, _db, vectors, _storage = _setup()
    # This old point stands for a legacy payload with no tenant_id. The old
    # scope-aware delete API would reject it; the new document-scoped one may
    # remove it after checking the document/dataset owner.
    vectors.points["base"].add("legacy-no-tenant")
    await coordinator.publish(plan, DATASET, GENERATION, "old")
    assert vectors.points["base"] == {plan.segment_ids[0]}


@pytest.mark.asyncio
async def test_failed_candidate_upsert_cleans_only_candidate_and_its_objects() -> None:
    coordinator, plan, db, vectors, storage = _setup(image=True)
    vectors.fail_upsert = "base_sections"
    old = {name: set(ids) for name, ids in vectors.points.items()}
    with pytest.raises(RuntimeError, match="candidate upsert failure"):
        await coordinator.publish(plan, DATASET, GENERATION, "old")
    assert vectors.points == old
    assert db.special["phase"] == "aborted" and db.revision > 0
    assert storage.objects == set()
    assert storage.deleted == db.special["planned_object_keys"]
    assert db.events.index("error") < db.events.index("aborted")


@pytest.mark.asyncio
async def test_pg_commit_failure_cleans_candidate_and_preserves_old_points() -> None:
    coordinator, plan, db, vectors, _storage = _setup()
    db.fail_commit = True
    with pytest.raises(RuntimeError, match="PG commit failure"):
        await coordinator.publish(plan, DATASET, GENERATION, "old")
    assert db.special["phase"] == "aborted" and db.revision > 0
    assert vectors.points == {"base": {"old-base"}, "base_sections": {"old-section"}}
    assert db.versions == []


@pytest.mark.asyncio
async def test_committed_cleanup_failure_keeps_negative_until_recovery() -> None:
    coordinator, plan, db, vectors, _storage = _setup()
    vectors.fail_old_delete = True
    with pytest.raises(RuntimeError, match="old cleanup failure"):
        await coordinator.publish(plan, DATASET, GENERATION, "old")
    assert db.special["phase"] == "committed" and db.revision < 0
    assert "old-base" in vectors.points["base"]
    assert await coordinator.recover_unfinished(DATASET) is True
    assert db.revision > 0 and vectors.points["base"] == {plan.segment_ids[0]}
    assert db.lease_special_ids == [GENERATION, None]


@pytest.mark.asyncio
async def test_post_commit_interruption_never_aborts_committed_generation() -> None:
    coordinator, plan, db, vectors, _storage = _setup()
    db.fail_after_commit_once = True
    with pytest.raises(RuntimeError, match="post-commit interruption"):
        await coordinator.publish(plan, DATASET, GENERATION, "old")
    assert db.special["phase"] == "committed" and db.revision < 0
    assert await coordinator.recover_unfinished(DATASET)
    assert db.revision > 0 and vectors.points["base"] == {plan.segment_ids[0]}


@pytest.mark.asyncio
async def test_prepare_failure_aborts_with_predeclared_generation_objects() -> None:
    coordinator, plan, db, vectors, storage = _setup(image=True)
    old_objects = storage._generate_key("tenant-a", "document-a", "page_1_gold", "page_1.png")
    storage.objects.add(old_objects)
    assert await coordinator.abort_preparing(
        DATASET, GENERATION, GENERATION, plan.source_hash,
        document_shared_lease_held=True,
    )
    assert db.special["phase"] == "aborted" and db.revision > 0
    assert storage.objects == {old_objects}
    assert vectors.points == {"base": {"old-base"}, "base_sections": {"old-section"}}
    assert db.lease_special_ids == [GENERATION]
    assert db.shared_lease_flags == [True]


@pytest.mark.asyncio
async def test_active_bm25_receipt_settles_inside_final_authority_transaction() -> None:
    coordinator, plan, db, vectors, _storage = _setup()
    lifecycle = ActiveLifecycle(db, vectors)
    coordinator.service.bm25_v2_lifecycle_service = lifecycle
    await coordinator.publish(plan, DATASET, GENERATION, "old")
    assert lifecycle.events == ["recertified", "settled"]
    assert db.revision > 0


@pytest.mark.asyncio
async def test_old_vision_object_hash_is_read_back_into_existing_version() -> None:
    coordinator, plan, db, _vectors, storage = _setup()
    old_key = storage._generate_key(
        "tenant-a", "document-a", "page_1", "page_1.png",
    )
    storage.object_data[old_key] = b"old-image-bytes"
    storage.objects.add(old_key)
    db.image_rows = [{
        "image_attachment_id": "page_1", "image_filename": "page_1.png",
        "text": "[Page 1]\nold",
    }]
    db.versions = [{
        "content": "[Page 1]\nold",
        "content_hash": hashlib.sha256(b"[Page 1]\nold").hexdigest(),
        "metadata": {},
    }]
    db.document["current_version"] = 1
    db.document["content"] = "[Page 1]\nold"

    await coordinator.publish(plan, DATASET, GENERATION, "[Page 1]\nold")
    assert len(db.versions) == 2
    assert db.versions[0]["metadata"]["_special_source_manifest"]["objects"] == {
        old_key: hashlib.sha256(b"old-image-bytes").hexdigest(),
    }
    assert old_key in storage.objects
    assert db.document["metadata"]["_special_source_manifest"]["generation_id"] == GENERATION


@pytest.mark.asyncio
async def test_deleted_empty_vision_page_cannot_rewrite_prior_version_manifest() -> None:
    coordinator, plan, db, vectors, storage = _setup()
    old_hex = "00000000-0000-4000-8000-000000000002".replace("-", "")
    first_attachment = f"page_1_g{old_hex}"
    second_attachment = f"page_2_g{old_hex}"
    first_key = storage._generate_key(
        "tenant-a", "document-a", first_attachment, "page_1.png",
    )
    second_key = storage._generate_key(
        "tenant-a", "document-a", second_attachment, "page_2.png",
    )
    first_hash = hashlib.sha256(b"old-page-one").hexdigest()
    second_hash = hashlib.sha256(b"old-page-two").hexdigest()
    old_content = "[Page 1]\nalpha"
    prior = {
        "source_kind": "vision", "generation_id": "old-generation",
        "source_hash": hashlib.sha256(b"old-pdf").hexdigest(),
        "content_hash": hashlib.sha256(old_content.encode()).hexdigest(),
        "original_source_key": "source/old.pdf", "index_config": {},
        "objects": {first_key: first_hash, second_key: second_hash},
        "page_texts": [
            {"page_number": 1, "text": "alpha", "storage_key": first_key, "sha256": first_hash},
            {"page_number": 2, "text": "", "storage_key": second_key, "sha256": second_hash},
        ],
    }
    db.document["content"] = old_content
    db.document["current_version"] = 1
    db.document["metadata"]["_special_source_manifest"] = prior
    db.versions = [{
        "content": old_content, "content_hash": prior["content_hash"],
        "metadata": {"_special_source_manifest": prior},
    }]
    # The second page was lost from current rows. Because its OCR was empty,
    # comparing only reconstructed document text would miss the deletion.
    db.image_rows = [{
        "image_attachment_id": first_attachment, "image_filename": "page_1.png",
        "text": old_content, "metadata": {"page_number": 1},
    }]
    storage.object_data[first_key] = b"old-page-one"
    with pytest.raises(SpecialSourceUnverifiableError, match="page receipts differ"):
        await coordinator.publish(plan, DATASET, GENERATION, old_content)
    assert db.versions[0]["metadata"]["_special_source_manifest"] == prior
    assert "upsert" not in vectors.events


@pytest.mark.asyncio
async def test_missing_old_vision_bytes_refuse_before_candidate_upsert() -> None:
    coordinator, plan, db, vectors, storage = _setup(image=True)
    db.image_rows = [{
        "image_attachment_id": "page_1", "image_filename": "page_1.png",
    }]
    with pytest.raises(SpecialSourceUnverifiableError, match="cannot be read"):
        await coordinator.publish(plan, DATASET, GENERATION, "old")
    assert "upsert" not in vectors.events
    assert vectors.points == {"base": {"old-base"}, "base_sections": {"old-section"}}
    assert db.special["phase"] == "aborted" and db.revision > 0
    assert storage.objects == set()


@pytest.mark.asyncio
@pytest.mark.parametrize("phase", ["prepared", "points_written"])
async def test_recovery_cleans_uncommitted_candidate_after_crash(phase: str) -> None:
    coordinator, plan, db, vectors, storage = _setup(image=True)
    db.revision = -1
    db.special.update({
        "phase": phase, "plan_hash": hashlib.sha256(b"plan").hexdigest(),
        "collections": {
            "base": {"candidate_point_ids": [plan.segment_ids[0]],
                     "old_point_ids": ["old-base"]},
            "base_sections": {"candidate_point_ids": [plan.segment_ids[1]],
                              "old_point_ids": ["old-section"]},
        },
        "objects": {receipt["storage_key"]: receipt["sha256"] for receipt in plan.object_manifest},
    })
    vectors.points["base"].add(plan.segment_ids[0])
    vectors.points["base_sections"].add(plan.segment_ids[1])
    assert await coordinator.recover_unfinished(DATASET)
    assert db.special["phase"] == "aborted" and db.revision > 0
    assert vectors.points == {"base": {"old-base"}, "base_sections": {"old-section"}}
    assert storage.objects == set()


@pytest.mark.asyncio
async def test_prepared_recovery_tolerates_collection_not_yet_created() -> None:
    coordinator, _plan_value, db, vectors, _storage = _setup()
    db.revision = -1
    db.special.update({
        "phase": "prepared", "plan_hash": hashlib.sha256(b"plan").hexdigest(),
        "collections": {
            "candidate_not_created": {
                "candidate_point_ids": ["00000000-0000-4000-8000-000000000099"],
                "old_point_ids": [],
            },
        },
    })
    assert await coordinator.recover_unfinished(DATASET)
    assert "delete_missing_collection" in vectors.events
    assert db.special["phase"] == "aborted" and db.revision > 0
    assert vectors.points == {"base": {"old-base"}, "base_sections": {"old-section"}}


@pytest.mark.asyncio
async def test_failed_object_cleanup_preserves_negative_until_recovery() -> None:
    coordinator, plan, db, _vectors, storage = _setup(image=True)
    storage.fail_delete = True
    with pytest.raises(RuntimeError, match="object cleanup failure"):
        await coordinator.abort_preparing(DATASET, GENERATION, GENERATION, plan.source_hash)
    assert db.special["phase"] == "preparing" and db.revision < 0
    storage.fail_delete = False
    assert await coordinator.recover_unfinished(DATASET)
    assert db.special == {} and db.document["status"] == "waiting" and db.revision > 0
    assert db.execution_status == "running"
    assert storage.objects == set()


@pytest.mark.asyncio
async def test_false_object_delete_preserves_negative_until_verified_recovery() -> None:
    coordinator, plan, db, _vectors, storage = _setup(image=True)
    storage.false_delete = True
    with pytest.raises(RuntimeError, match="cleanup could not be verified"):
        await coordinator.abort_preparing(DATASET, GENERATION, GENERATION, plan.source_hash)
    assert db.special["phase"] == "preparing" and db.revision < 0
    assert storage.objects == set(db.special["planned_object_keys"])
    storage.false_delete = False
    assert await coordinator.recover_unfinished(DATASET)
    assert storage.objects == set() and db.revision > 0
    assert db.special == {} and db.document["status"] == "waiting"


@pytest.mark.asyncio
@pytest.mark.parametrize("negative", [False, True])
@pytest.mark.parametrize("stale", ["execution_error", "another_owner"])
async def test_stale_preparing_execution_cannot_delete_objects(
    negative: bool, stale: str,
) -> None:
    coordinator, plan, db, _vectors, storage = _setup(image=True)
    db.revision = -1 if negative else 1
    if stale == "execution_error":
        db.execution_status = "error"
    else:
        db.active_execution_id = "other-execution"
        db.document["metadata"]["_document_pipeline_execution_id"] = "other-execution"
    with pytest.raises(RuntimeError, match="document owner"):
        await coordinator.abort_preparing(DATASET, GENERATION, GENERATION, plan.source_hash)
    assert storage.objects == set(db.special["planned_object_keys"])
    assert storage.deleted == []
    assert db.special["phase"] == "preparing"
    assert db.revision == (-1 if negative else 1)
