"""Actual source-aware DocumentService path with a controlled transactional DB."""

import asyncio
import copy
import uuid
from contextlib import asynccontextmanager
from types import SimpleNamespace
from unittest.mock import AsyncMock

import pytest
from knowledge_service.auth.user_context import UserContext
from knowledge_service.core.exceptions import ValidationFailedError
from knowledge_service.persistence.database import make_dataset_index_deletion_fence
from knowledge_service.services.knowledge.document_service import DocumentService


class SourceDatabase:
    def __init__(self, shared=True):
        self.document_id = str(
            uuid.uuid5(uuid.NAMESPACE_URL, "ai-platform:confluence:dataset:connection:page")
        )
        self.dataset = {"dataset_id": "dataset", "tenant_id": "tenant", "index_config": {}}
        self.document = {
            "document_id": self.document_id,
            "dataset_id": "dataset",
            "source_type": "confluence",
            "metadata": {"confluence_page_id": "page"},
        }
        self.bindings = {
            key: {
                "binding_id": key,
                "dataset_id": "dataset",
                "tenant_id": "tenant",
                "connection_id": "connection",
                "created_by": "owner",
            }
            for key in ["first", "second"]
        }
        self.pages = {
            key: {
                "id": f"legacy-{key}",
                "binding_id": key,
                "page_id": "page",
                "document_id": self.document_id,
                "version": 1,
                "updated_at": "observation-1",
            }
            for key in (["first", "second"] if shared else ["first"])
        }
        self.events = []
        self.leased = False
        self.fail_clear = False
        self.lock = asyncio.Lock()

    @asynccontextmanager
    async def dataset_index_delete_lease(self, dataset_id):
        assert dataset_id == "dataset"
        async with self.lock:
            self.leased = True
            self.events.append("lease-enter")
            try:
                yield self
            finally:
                self.leased = False
                self.events.append("lease-exit")

    @asynccontextmanager
    async def transaction(self):
        assert self.leased
        snapshot = copy.deepcopy((self.dataset, self.document, self.pages))
        self.events.append("begin")
        try:
            yield
        except BaseException:
            self.dataset, self.document, self.pages = snapshot
            self.events.append("rollback")
            raise
        else:
            self.events.append("commit")

    async def fetchrow(self, query, *args):
        assert self.leased
        if "FROM confluence_space_bindings" in query:
            return copy.deepcopy(self.bindings.get(args[0]))
        if "FROM confluence_pages" in query:
            return copy.deepcopy(next((p for p in self.pages.values() if p["id"] == args[0]), None))
        raise AssertionError(query)

    async def fetchval(self, query, *args):
        assert self.leased and "SELECT EXISTS" in query
        self.events.append("classify-references")
        return any(p["document_id"] == args[0] and p["id"] != args[1] for p in self.pages.values())

    async def execute(self, query, *args):
        assert self.leased and "DELETE FROM confluence_pages" in query
        for key, p in list(self.pages.items()):
            if (p["id"], p["binding_id"], p["page_id"], p["document_id"]) == args:
                del self.pages[key]
                self.events.append("delete-reference")
                return "DELETE 1"
        return "DELETE 0"

    async def get_dataset(self, dataset_id, *, connection=None):
        assert dataset_id == "dataset" and connection in (None, self)
        return copy.deepcopy(self.dataset)

    async def get_document(self, document_id, *, connection=None):
        assert connection in (None, self)
        return copy.deepcopy(self.document) if document_id == self.document_id else None

    async def set_dataset_index_deletion_fence(
        self, dataset_id, *, operation, target_id, connection
    ):
        assert connection is self and self.leased and dataset_id == "dataset"
        self.events.append("set-fence")
        self.dataset["index_config"] = {
            "retrieval": {
                "_index_deletion_fence": make_dataset_index_deletion_fence(operation, target_id)
            }
        }
        return copy.deepcopy(self.dataset), True

    async def clear_dataset_index_deletion_fence(
        self, dataset_id, *, operation, target_id, connection
    ):
        assert connection is self and self.leased and dataset_id == "dataset"
        assert operation == "document_delete" and target_id == self.document_id
        self.events.append("clear-fence")
        if self.fail_clear:
            return False
        self.dataset["index_config"] = {}
        return True

    async def delete_document(self, document_id, *, connection):
        assert connection is self and self.leased
        self.events.append("delete-document")
        self.document = None
        for p in self.pages.values():
            if p["document_id"] == document_id:
                p["document_id"] = None
        return True


def fixture(shared=True):
    db = SourceDatabase(shared)
    svc = DocumentService(SimpleNamespace(), db)

    async def vectors(**kwargs):
        assert db.leased
        db.events.append("delete-vectors")

    storage = SimpleNamespace(delete_document_assets=AsyncMock())
    svc._ks = SimpleNamespace(
        require_dataset_access=AsyncMock(),
        vector_store=SimpleNamespace(delete_document_points=AsyncMock(side_effect=vectors)),
        image_storage_service=storage,
    )

    # AsyncMock side_effect must itself be async rather than return a coroutine.
    async def access(*args, **kwargs):
        return copy.deepcopy(db.dataset)

    svc._ks.require_dataset_access = AsyncMock(side_effect=access)
    return svc, db, UserContext("owner", "tenant")


async def detach(svc, db, user, key, *, page=None):
    return await svc.delete_document(
        user,
        "dataset",
        db.document_id,
        _confluence_source=(copy.deepcopy(db.bindings[key]), copy.deepcopy(page or db.pages[key])),
    )


@pytest.mark.asyncio
async def test_shared_detach_preserves_other_then_last_reference_deletes_once():
    svc, db, user = fixture()
    assert await detach(svc, db, user, "first") is False
    assert db.document is not None and set(db.pages) == {"second"}
    svc._ks.vector_store.delete_document_points.assert_not_awaited()
    assert await detach(svc, db, user, "second") is True
    assert db.document is None and db.pages == {}
    svc._ks.vector_store.delete_document_points.assert_awaited_once()
    assert db.events.index("classify-references") < db.events.index("delete-reference")
    final = db.events[-5:]
    assert final == ["delete-reference", "delete-document", "clear-fence", "commit", "lease-exit"]


@pytest.mark.asyncio
async def test_external_failure_retains_reference_and_fence_for_exact_retry():
    svc, db, user = fixture(False)
    svc._ks.image_storage_service.delete_document_assets.side_effect = RuntimeError(
        "fixture storage failure"
    )
    with pytest.raises(RuntimeError):
        await detach(svc, db, user, "first")
    assert db.document is not None and "first" in db.pages
    assert db.dataset["index_config"]
    svc._ks.image_storage_service.delete_document_assets.side_effect = None
    assert await detach(svc, db, user, "first") is True
    assert db.pages == {} and db.document is None and db.dataset["index_config"] == {}


@pytest.mark.asyncio
async def test_final_transaction_rolls_back_reference_document_and_fence_clear():
    svc, db, user = fixture(False)
    db.fail_clear = True
    with pytest.raises(ValidationFailedError):
        await detach(svc, db, user, "first")
    assert db.document is not None and "first" in db.pages
    assert db.dataset["index_config"] and "rollback" in db.events
    db.fail_clear = False
    assert await detach(svc, db, user, "first") is True


@pytest.mark.asyncio
@pytest.mark.parametrize("kind", ["version", "record", "tenant", "source_type", "identity"])
async def test_scope_or_source_mismatch_never_reaches_external_delete(kind):
    svc, db, user = fixture(False)
    page = copy.deepcopy(db.pages["first"])
    if kind == "version":
        db.pages["first"]["version"] = 2
    if kind == "record":
        db.pages["first"]["document_id"] = "different"
    if kind == "tenant":
        db.bindings["first"]["tenant_id"] = "other"
    if kind == "source_type":
        db.document["source_type"] = "upload"
    if kind == "identity":
        db.document_id = "unknown-legacy"
        db.document["document_id"] = db.document_id
        db.pages["first"]["document_id"] = db.document_id
        page = copy.deepcopy(db.pages["first"])
    with pytest.raises(ValidationFailedError):
        await detach(svc, db, user, "first", page=page)
    assert db.document is not None and db.pages
    svc._ks.vector_store.delete_document_points.assert_not_awaited()


@pytest.mark.asyncio
async def test_any_other_reference_preserves_document_even_foreign_binding():
    svc, db, user = fixture()
    db.bindings.pop("second")
    assert await detach(svc, db, user, "first") is False
    assert db.document is not None and "second" in db.pages


@pytest.mark.asyncio
async def test_concurrent_two_detaches_serialize_and_delete_only_last():
    svc, db, user = fixture()
    results = await asyncio.gather(detach(svc, db, user, "first"), detach(svc, db, user, "second"))
    assert sorted(results) == [False, True]
    assert not db.pages and db.document is None
    svc._ks.vector_store.delete_document_points.assert_awaited_once()
