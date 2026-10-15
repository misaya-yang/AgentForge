"""Owner-scoped, source-checked conversation share preview and creation."""

from __future__ import annotations

import json
from types import SimpleNamespace

import pytest
from fastapi import HTTPException

from src.api.v1 import conversation_shares as shares
from src.core.auth.user_resolver import UserContext


class _ShareDB:
    def __init__(self, *, knowledge: bool = False, legacy: list | None = None, context_document: bool = False, versioned_context: bool = False):
        self.knowledge = knowledge
        self.context_document = context_document
        self.versioned_context = versioned_context
        self.legacy = legacy or []
        self.inserted: dict | None = None
        self.inserted_scope: dict | None = None

    async def get_user(self, user_id: str):
        return {"user_id": user_id, "tenant_id": "tenant-a", "status": "active"}

    async def fetchrow(self, sql: str, *args):
        if "FROM assistant.sessions" in sql:
            assert args in {
                ("session-a", "user-a", "tenant-a"),
                ("session-a", "tenant-a", "user-a"),
            }
            return {"history": self.legacy, "metadata": {"title": "Safe session"}}
        if "FROM conversation_shares" in sql:
            return None
        raise AssertionError(sql)

    async def fetch(self, sql: str, *args):
        if "FROM assistant_runtime_items" in sql:
            assert args == ("session-a", "tenant-a", "user-a")
            if self.versioned_context:
                return [{"run_id": "run-a", "chunks": [{"dataset_id": "private", "document_id": "doc-a", "source_version": 2, "source_hash": "a" * 64}]}]
            return [{"run_id": "run-a", "chunks": [{"dataset_id": "private", "document_id": "doc-a"}]}] if self.context_document else []
        if "FROM assistant_runtime_snapshots" in sql:
            assert args == ("session-a", "tenant-a", "user-a")
            items = [{"kind": "knowledge", "payload": {"dataset_id": "private"}}] if self.knowledge else []
            return [{"run_id": "run-a", "snapshot": {"readonly_capabilities": {"items": items}}}]
        if "FROM assistant.artifacts" in sql:
            assert args == ("session-a", "tenant-a", "user-a")
            return [{
                "artifact_id": "artifact-a", "type": "file", "format": "txt",
                "title": "Result", "filename": "result.txt", "size_bytes": 6,
                "mime_type": "text/plain", "source": "code_execution",
            }]
        raise AssertionError(sql)

    async def execute(self, sql: str, *args):
        if "INSERT INTO conversation_shares" in sql:
            self.inserted = json.loads(args[5])
            self.inserted_scope = json.loads(args[10]) if args[10] is not None else None
            return "INSERT 1"
        raise AssertionError(sql)


class _Store:
    async def get_for_session(self, **scope):
        assert scope == {"tenant_id": "tenant-a", "user_id": "user-a", "session_id": "session-a"}
        return SimpleNamespace(runtime_thread_id="thread-a")

    async def history_messages(self, **scope):
        assert scope["runtime_thread_id"] == "thread-a"
        return ([
            {"role": "user", "content": "Make a file", "metadata": {"runtime_run_id": "run-a"}},
            {"role": "assistant", "content": "Done", "metadata": {
                "runtime_run_id": "run-a", "thinking_content": "secret reasoning",
                "tool_calls": [{"arguments": {"token": "secret"}}],
                "artifact_ids": ["artifact-a", "unowned"],
            }},
        ], 2)


class _Proxy:
    allowed = True

    async def list_datasets(self, _user):
        return [{"dataset_id": "private", "name": "Private"}] if self.allowed else []

    async def authorize_documents(self, _user, _dataset_id, document_ids):
        return set(document_ids) if self.allowed else set()

    async def authorize_document_sources(self, _user, _dataset_id, references):
        return {(item["document_id"], item["source_version"], item["source_hash"]) for item in references} if self.allowed else set()


def _request(db: _ShareDB):
    return SimpleNamespace(app=SimpleNamespace(state=SimpleNamespace(
        database=db, agent_thread_store=_Store(), kb_proxy=_Proxy(),
    )))


def _user():
    return UserContext(user_id="user-a", tenant_id="tenant-a", is_authenticated=True)


@pytest.mark.asyncio
async def test_preview_and_create_share_use_same_safe_snapshot() -> None:
    db = _ShareDB()
    request = _request(db)
    preview = await shares.preview_share("session-a", request, _user(), True, None)
    assert preview["audience"] == "public"
    assert preview["message_count"] == 2
    assert preview["artifact_count"] == 1
    assert preview["messages"][1]["metadata"] == {"artifact_ids": ["artifact-a"]}
    assert "secret" not in json.dumps(preview)

    result = await shares.create_share(
        "session-a",
        shares.CreateShareRequest(preview_hash=preview["preview_hash"]),
        request,
        _user(),
    )
    assert result.message_count == 2
    assert db.inserted is not None
    assert db.inserted["source_policy"] == "verified_no_private_knowledge"
    assert "secret" not in json.dumps(db.inserted)


@pytest.mark.asyncio
async def test_public_artifact_download_checks_share_on_each_request() -> None:
    class _DownloadDB:
        active = True

        async def fetchrow(self, _sql: str, *_args):
            return {
                "snapshot": {
                    "share_snapshot_version": 2,
                    "source_policy": "verified_no_private_knowledge",
                    "artifacts": [{"artifact_id": "artifact-a"}],
                },
                "expires_at": None, "is_active": self.active,
                "session_id": "session-a", "tenant_id": "tenant-a", "user_id": "user-a",
            }

    class _Storage:
        async def get_artifact(self, _artifact_id: str):
            return SimpleNamespace(
                session_id="session-a", tenant_id="tenant-a", user_id="user-a",
                source="code_execution", size_bytes=6, mime_type="text/plain", filename="result.txt",
            )

        async def get_presigned_download_url(self, _artifact):
            raise AssertionError("public download must not issue a storage URL")

        async def download_artifact(self, _artifact_id: str):
            return b"result"

    db = _DownloadDB()
    request = SimpleNamespace(app=SimpleNamespace(state=SimpleNamespace(
        database=db, artifact_storage=_Storage(),
    )))
    response = await shares.download_shared_artifact("share-a", "artifact-a", request)
    assert response.status_code == 200
    assert "location" not in response.headers
    assert b"".join([chunk async for chunk in response.body_iterator]) == b"result"

    db.active = False
    with pytest.raises(HTTPException) as exc_info:
        await shares.download_shared_artifact("share-a", "artifact-a", request)
    assert exc_info.value.status_code == 404


@pytest.mark.asyncio
async def test_private_knowledge_and_unverified_legacy_history_cannot_be_shared() -> None:
    with pytest.raises(HTTPException) as private:
        await shares.preview_share("session-a", _request(_ShareDB(knowledge=True)), _user(), True, None)
    assert private.value.status_code == 409

    with pytest.raises(HTTPException) as event_provenance:
        await shares.preview_share(
            "session-a", _request(_ShareDB(context_document=True)), _user(), True, None,
        )
    assert event_provenance.value.status_code == 409

    legacy = [{"role": "assistant", "content": "old", "metadata": {"model_id": "old"}}]
    with pytest.raises(HTTPException) as unknown:
        await shares.preview_share("session-a", _request(_ShareDB(legacy=legacy)), _user(), True, None)
    assert unknown.value.status_code == 409


@pytest.mark.asyncio
async def test_create_rejects_missing_or_stale_preview_hash() -> None:
    db = _ShareDB()
    with pytest.raises(HTTPException) as mismatch:
        await shares.create_share(
            "session-a", shares.CreateShareRequest(), _request(db), _user(),
        )
    assert mismatch.value.status_code == 409
    assert db.inserted is None


@pytest.mark.asyncio
async def test_internal_share_freezes_version_outside_snapshot() -> None:
    db = _ShareDB(knowledge=True, versioned_context=True)
    request = _request(db)
    preview = await shares.preview_share("session-a", request, _user(), True, None, "internal")
    assert preview["audience"] == "internal"
    assert "source_scope" not in json.dumps(preview)
    result = await shares.create_share(
        "session-a", shares.CreateShareRequest(audience="internal", preview_hash=preview["preview_hash"]),
        request, _user(),
    )
    assert result.audience == "internal"
    assert db.inserted_scope == {
        "version": 1,
        "dataset_ids": ["private"],
        "document_ids": [["private", "doc-a"]],
        "source_versions": [["private", "doc-a", 2, "a" * 64]],
    }
    assert '"source_scope":' not in json.dumps(db.inserted)
    assert "a" * 64 not in json.dumps(db.inserted)


@pytest.mark.asyncio
async def test_internal_share_rejects_unversioned_and_revoked_sources() -> None:
    with pytest.raises(HTTPException) as unversioned:
        await shares.preview_share("session-a", _request(_ShareDB(knowledge=True, context_document=True)), _user(), True, None, "internal")
    assert unversioned.value.status_code == 409

    request = _request(_ShareDB(knowledge=True, versioned_context=True))
    request.app.state.kb_proxy.allowed = False
    with pytest.raises(HTTPException) as revoked:
        await shares.preview_share("session-a", request, _user(), True, None, "internal")
    assert revoked.value.status_code == 403
