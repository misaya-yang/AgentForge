"""Internal share reads must recheck identity and immutable KB sources."""

from __future__ import annotations

import json
import uuid
from datetime import datetime, timedelta, timezone
from types import SimpleNamespace

import pytest
from fastapi import HTTPException, Response

from src.api.v1 import _internal_share_scope as scope
from src.api.v1 import conversation_shares as conversations
from src.api.v1 import quiz as quizzes
from src.core.auth.user_resolver import UserContext

DATASET = "dataset-a"
DOCUMENT = "document-a"
VERSION = 2
SOURCE_HASH = "b" * 64
SOURCE_SCOPE = scope.freeze_source_scope(
    frozenset({DATASET}),
    frozenset({(DATASET, DOCUMENT)}),
    frozenset({(DATASET, DOCUMENT, VERSION, SOURCE_HASH)}),
)


class _KBProxy:
    datasets = True
    documents = True
    versions = True
    current_version = VERSION

    async def list_datasets(self, _user):
        return [{"dataset_id": DATASET}] if self.datasets else []

    async def authorize_documents(self, _user, _dataset_id, document_ids):
        return set(document_ids) if self.documents else set()

    async def authorize_document_sources(self, _user, _dataset_id, references):
        if not self.versions:
            return set()
        return {(item["document_id"], self.current_version, item["source_hash"]) for item in references}


class _DB:
    account_status = "active"

    def __init__(self, row):
        self.row = row
        self.attempts = {}

    async def get_user(self, user_id):
        return {"user_id": user_id, "tenant_id": "tenant-a", "status": self.account_status}

    async def fetchrow(self, query, *args):
        if "FROM conversation_share_quiz_attempts" in query:
            return self.attempts.get((args[0], args[1], str(args[2])))
        return self.row

    async def execute(self, query, *args):
        if "INSERT INTO conversation_share_quiz_attempts" in query:
            self.attempts[(args[1], args[2], str(args[3]))] = {"result": json.loads(args[4])}
        return "OK"


class _Storage:
    async def get_artifact(self, _artifact_id):
        return SimpleNamespace(
            session_id="session-a", tenant_id="tenant-a", user_id="owner-a",
            source="code_execution", size_bytes=4, mime_type="text/plain", filename="a.txt",
        )

    async def download_artifact(self, _artifact_id):
        return b"data"


def _request(row):
    db = _DB(row)
    proxy = _KBProxy()
    request = SimpleNamespace(
        app=SimpleNamespace(state=SimpleNamespace(
            database=db, settings=object(), kb_proxy=proxy,
            artifact_storage=_Storage(), multi_rate_limiter=None,
        )),
        state=SimpleNamespace(), headers={}, client=SimpleNamespace(host="127.0.0.1"),
    )
    return request, db, proxy


def _viewer(tenant_id="tenant-a", *, authenticated=True):
    return UserContext(user_id="viewer-a", tenant_id=tenant_id, is_authenticated=authenticated)


def _conversation_row(quiz_id):
    return {
        "snapshot": {
            "share_snapshot_version": 3,
            "source_policy": "verified_internal_source_scope",
            "messages": [{"role": "assistant", "content": "Answer"}],
            "artifacts": [{"artifact_id": "artifact-a"}],
            "quiz_answer_keys": {quiz_id: {"questions": []}},
        },
        "share_code": "code-a", "title": "Answer", "tenant_id": "tenant-a",
        "user_id": "owner-a", "session_id": "session-a", "audience": "internal",
        "source_scope": SOURCE_SCOPE, "is_active": True, "expires_at": None,
        "message_count": 1, "artifact_count": 1, "view_count": 0,
        "created_at": datetime.now(timezone.utc),
    }


@pytest.mark.asyncio
async def test_internal_conversation_read_blob_and_quiz_recheck_rights(monkeypatch):
    quiz_id = str(uuid.uuid4())
    row = _conversation_row(quiz_id)
    request, db, proxy = _request(row)

    async def user_context(_request, *, settings):  # noqa: ARG001
        return _viewer()

    monkeypatch.setattr(scope, "get_user_context", user_context)
    page = await conversations.get_share("code-a", request)
    assert page.headers["cache-control"] == "no-store"
    assert '"source_scope":' not in page.body.decode()
    assert SOURCE_HASH not in page.body.decode()
    assert "quiz_answer_keys" not in page.body.decode()

    blob = await conversations.download_shared_artifact("code-a", "artifact-a", request)
    assert blob.headers["cache-control"] == "no-store"
    assert b"".join([part async for part in blob.body_iterator]) == b"data"

    graded = await conversations.submit_shared_quiz(
        "code-a", quiz_id, conversations.SharedQuizSubmitRequest(answers={}), request,
    )
    assert graded.headers["cache-control"] == "no-store"
    assert ("code-a", "user:viewer-a", quiz_id) in db.attempts

    for changed in ("datasets", "documents", "versions"):
        setattr(proxy, changed, False)
        for read in (
            conversations.get_share("code-a", request),
            conversations.download_shared_artifact("code-a", "artifact-a", request),
            conversations.submit_shared_quiz(
                "code-a", quiz_id, conversations.SharedQuizSubmitRequest(answers={}), request,
            ),
        ):
            with pytest.raises(HTTPException) as denied:
                await read
            assert denied.value.status_code == 403
        setattr(proxy, changed, True)
    proxy.current_version = VERSION + 1
    with pytest.raises(HTTPException) as changed_version:
        await conversations.get_share("code-a", request)
    assert changed_version.value.status_code == 403


@pytest.mark.asyncio
async def test_internal_share_requires_active_same_tenant_account(monkeypatch):
    row = _conversation_row(str(uuid.uuid4()))
    request, db, _proxy = _request(row)
    actor = _viewer(authenticated=False)

    async def user_context(_request, *, settings):  # noqa: ARG001
        return actor

    monkeypatch.setattr(scope, "get_user_context", user_context)
    for candidate, expected in [(_viewer(authenticated=False), 401), (_viewer("tenant-b"), 404)]:
        actor = candidate
        with pytest.raises(HTTPException) as denied:
            await conversations.get_share("code-a", request)
        assert denied.value.status_code == expected
    actor = _viewer()
    db.account_status = "disabled"
    with pytest.raises(HTTPException) as denied:
        await conversations.get_share("code-a", request)
    assert denied.value.status_code == 403


@pytest.mark.asyncio
async def test_internal_conversation_revocation_and_expiry_remain_effective(monkeypatch):
    row = _conversation_row(str(uuid.uuid4()))
    request, _db, _proxy = _request(row)

    async def user_context(_request, *, settings):  # noqa: ARG001
        return _viewer()

    monkeypatch.setattr(scope, "get_user_context", user_context)
    row["is_active"] = False
    for read in (
        conversations.get_share("code-a", request),
        conversations.download_shared_artifact("code-a", "artifact-a", request),
    ):
        with pytest.raises(HTTPException) as revoked:
            await read
        assert revoked.value.status_code == 404
    row["is_active"] = True
    row["expires_at"] = datetime.now(timezone.utc) - timedelta(seconds=1)
    for read in (
        conversations.get_share("code-a", request),
        conversations.download_shared_artifact("code-a", "artifact-a", request),
    ):
        with pytest.raises(HTTPException) as expired:
            await read
        assert expired.value.status_code == 410


@pytest.mark.asyncio
async def test_internal_quiz_entrypoints_use_same_access_guard(monkeypatch):
    row = {
        "audience": "internal", "source_scope": SOURCE_SCOPE,
        "tenant_id": "tenant-a", "is_active": True, "expires_at": None,
    }
    request, _db, proxy = _request(row)

    async def user_context(_request, *, settings):  # noqa: ARG001
        return _viewer()

    monkeypatch.setattr(scope, "get_user_context", user_context)

    class _Manager:
        calls = []

        async def get_public_artifact(self, _code, *, audience):
            self.calls.append(("get", audience))
            return {"questions": [], "audience": audience}

        async def start_attempt(self, _code, *, audience, user_id=None):
            self.calls.append(("start", audience, user_id))
            return {"attempt_token": "token", "started_at": datetime.now(timezone.utc), "expires_at": datetime.now(timezone.utc)}

        async def get_attempt_result(self, _code, _token, *, audience, user_id=None):
            self.calls.append(("result", audience, user_id))
            return {"attempt_id": uuid.uuid4(), "total_score": 1, "correct_count": 1, "total_count": 1, "per_question": []}

        async def submit_attempt(self, **kwargs):
            self.calls.append(("submit", kwargs["audience"], kwargs["user_id"]))
            return {"attempt_id": uuid.uuid4(), "total_score": 1, "correct_count": 1, "total_count": 1, "per_question": []}

    manager = _Manager()
    monkeypatch.setattr(quizzes, "_get_share_manager", lambda _request: manager)
    await quizzes.get_shared_quiz("code-a", request, Response())
    await quizzes.start_shared_quiz_attempt("code-a", request, Response())
    await quizzes.get_shared_quiz_attempt_result(
        "code-a", quizzes.PublicQuizAttemptResultRequest(attempt_token="token"), request, Response(),
    )
    await quizzes.submit_shared_quiz(
        "code-a", quizzes.PublicQuizSubmitRequest(answers={}), request, Response(),
    )
    assert manager.calls == [
        ("get", "internal"),
        *((name, "internal", "viewer-a") for name in ("start", "result", "submit")),
    ]

    proxy.versions = False
    for read in (
        quizzes.get_shared_quiz("code-a", request, Response()),
        quizzes.start_shared_quiz_attempt("code-a", request, Response()),
        quizzes.get_shared_quiz_attempt_result(
            "code-a", quizzes.PublicQuizAttemptResultRequest(attempt_token="token"), request, Response(),
        ),
        quizzes.submit_shared_quiz(
            "code-a", quizzes.PublicQuizSubmitRequest(answers={}), request, Response(),
        ),
    ):
        with pytest.raises(HTTPException) as denied:
            await read
        assert denied.value.status_code == 403
    assert len(manager.calls) == 4


def test_internal_scope_rejects_unversioned_or_tampered_sources():
    with pytest.raises(HTTPException) as unversioned:
        scope.freeze_source_scope(frozenset({DATASET}), frozenset({(DATASET, DOCUMENT)}), frozenset())
    assert unversioned.value.status_code == 409

    with pytest.raises(HTTPException) as tampered:
        scope._parse_source_scope({**SOURCE_SCOPE, "source_versions": []})
    assert tampered.value.status_code == 410
