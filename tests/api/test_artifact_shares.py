"""Artifact share API + manager tests (product-convergence PC-03).

The manager is kind-generic; quiz shares freeze a payload snapshot with
answer keys. The gateway endpoints create/revoke shares and the public
quiz routes alias over the same rows.
"""

from __future__ import annotations

import asyncio
import json
import uuid
from datetime import datetime, timedelta, timezone

import pytest
from ai_gateway_core.sharing.artifact_share_manager import (
    ArtifactShareManager,
    AttemptConflictError,
    AttemptInputError,
    AttemptLimitReachedError,
    ShareUnavailableError,
)
from fastapi import FastAPI
from fastapi.testclient import TestClient

from src.api.v1.artifact_shares import router as artifact_shares_router
from src.api.v1.quiz import public_router as quiz_public_router


class _FakeDB:
    """Minimal async DB double for the artifact share flow."""

    def __init__(self) -> None:
        self.shares: dict[str, dict] = {}
        self.quiz_rows: dict[str, dict] = {}
        self.attempts: list[dict] = []
        self.submitters: set[tuple[str, str]] = set()
        self.tokens: dict[str, dict] = {}
        self.source_snapshots: list[dict] = []

    def add_verified_quiz(self, quiz_id: str) -> None:
        self.quiz_rows[quiz_id] = {
            "id": quiz_id, "tenant_id": "tenant-a", "created_by": "alex",
            "dataset_ids": [], "session_id": "source-session", "run_id": "source-run",
            "title": "Verified quiz", "description": None, "question_count": 0, "difficulty": "easy",
        }
        self.source_snapshots = [{"run_id": "source-run", "snapshot": {"readonly_capabilities": {"items": []}}}]

    async def get_user(self, user_id: str):
        return {"user_id": user_id, "tenant_id": "tenant-a", "status": "active"}

    async def fetchrow(self, query: str, *args):  # noqa: ANN201
        query = query.replace("assistant.", "")
        if "JOIN artifact_share_attempt_tokens AS token" in query:
            share_id, token_hash = args
            return next((attempt for attempt in self.attempts if attempt.get("share_id") == str(share_id) and attempt.get("token_hash") == token_hash), None)
        if "record_artifact_share_quiz_attempt" in query:
            (
                share_code,
                token_hash,
                display_name,
                attempt_id,
                _quiz_id,
                _answers,
                _score,
                _correct,
                _total,
                _client_ip,
                result_json,
            ) = args
            row = next(
                (s for s in self.shares.values() if s["share_code"] == share_code),
                None,
            )
            now = datetime.now(timezone.utc)
            if not row or not row["is_active"] or (
                row["expires_at"] is not None and row["expires_at"] <= now
            ):
                raise ShareUnavailableError("Share not found or expired")
            if row["require_name"] and not display_name:
                raise AttemptInputError("This share requires a name before submitting")
            if token_hash is not None:
                token = self.tokens.get(token_hash)
                if (
                    not token
                    or token["share_id"] != str(row["id"])
                ):
                    raise AttemptInputError("Attempt token is invalid or expired")
                if token["consumed_at"] is not None:
                    prior = next((item for item in self.attempts if item.get("token_hash") == token_hash), None)
                    if prior:
                        return {"attempt_id": prior["id"], "result_payload": prior["result_payload"]}
                    raise AttemptConflictError("Attempt token has already been consumed")
                if token["expires_at"] <= now:
                    raise AttemptInputError("Attempt token is invalid or expired")
            elif row["time_limit_minutes"] is not None:
                raise AttemptInputError("An attempt token is required for timed shares")
            if row["max_attempts"] is not None and row["attempt_count"] >= row["max_attempts"]:
                raise AttemptLimitReachedError("Maximum attempts reached")
            if display_name:
                key = (str(row["id"]), display_name)
                if key in self.submitters:
                    raise AttemptConflictError("This display name has already submitted")
                self.submitters.add(key)
            row["attempt_count"] += 1
            if token_hash is not None:
                self.tokens[token_hash]["consumed_at"] = now
            payload = json.loads(result_json)
            self.attempts.append({
                "id": attempt_id, "attempt_id": attempt_id, "status": "completed",
                "share_id": str(row["id"]), "token_hash": token_hash,
                "answers": json.loads(_answers), "display_name": display_name,
                "result_payload": payload,
            })
            return {"attempt_id": attempt_id, "result_payload": payload}
        if "FROM quizzes" in query:
            quiz_id = str(args[0])
            row = self.quiz_rows.get(quiz_id)
            return row or None
        if "FROM artifact_shares" in query:
            # Looked up by share_code (public reads) or by id (revoke).
            key = str(args[0])
            row = self.shares.get(key) or next(
                (s for s in self.shares.values() if s["share_code"] == key), None
            )
            if not row or not row["is_active"]:
                return None
            return row
        if "FROM quiz_attempts" in query and "LIMIT 1" in query:
            return self.attempts[0] if self.attempts else None
        return None

    async def fetch(self, query: str, *args):  # noqa: ANN201, ARG002
        if "FROM assistant_runtime_snapshots" in query:
            return self.source_snapshots
        return []

    async def execute(self, query: str, *args):  # noqa: ANN201
        query = query.replace("assistant.", "")
        if "INSERT INTO ARTIFACT_SHARE_ATTEMPT_TOKENS" in query.upper():
            (
                token_id,
                token_hash,
                started_at,
                expires_at,
                share_code,
                retention_hours,
                cleanup_batch_size,
            ) = args
            cutoff = datetime.now(timezone.utc) - timedelta(hours=retention_hours)
            stale = sorted(
                (
                    key
                    for key, token in self.tokens.items()
                    if token["expires_at"] < cutoff
                    or (
                        token["consumed_at"] is not None
                        and token["consumed_at"] < cutoff
                    )
                ),
                key=lambda key: min(
                    self.tokens[key]["expires_at"],
                    self.tokens[key]["consumed_at"]
                    or self.tokens[key]["expires_at"],
                ),
            )[:cleanup_batch_size]
            for key in stale:
                self.tokens.pop(key)
            row = next(
                (s for s in self.shares.values() if s["share_code"] == share_code),
                None,
            )
            if not row or not row["is_active"]:
                return "INSERT 0 0"
            if row["max_attempts"] is not None and row["attempt_count"] >= row["max_attempts"]:
                return "INSERT 0 0"
            self.tokens[token_hash] = {
                "id": token_id,
                "share_id": str(row["id"]),
                "started_at": started_at,
                "expires_at": expires_at,
                "consumed_at": None,
            }
            return "INSERT 0 1"
        if query.lstrip().upper().startswith("INSERT INTO ARTIFACT_SHARE_SUBMITTERS"):
            key = (str(args[0]), args[1])
            if key in self.submitters:
                return "INSERT 0 0"
            self.submitters.add(key)
            return "INSERT 0 1"
        if query.lstrip().upper().startswith("DELETE FROM ARTIFACT_SHARE_SUBMITTERS"):
            self.submitters.discard((str(args[0]), args[1]))
            return "DELETE 1"
        if query.lstrip().upper().startswith("INSERT INTO QUIZ_ATTEMPTS"):
            self.attempts.append({"args": args, "status": "completed"})
            return "OK"
        if query.lstrip().upper().startswith("UPDATE QUIZ_ATTEMPTS"):
            if self.attempts:
                self.attempts[-1]["status"] = "rejected"
            return "OK"
        if query.lstrip().upper().startswith("INSERT INTO ARTIFACT_SHARES"):
            (
                _id, share_code, kind, title, payload, answer_keys, tenant_id,
                created_by, max_attempts, expires_at, require_name,
                time_limit_minutes, created_at,
            ) = args[:13]
            self.shares[str(_id)] = {
                "id": _id,
                "share_code": share_code,
                "kind": kind,
                "title": title,
                "payload": payload,
                "answer_keys": answer_keys,
                "tenant_id": tenant_id,
                "created_by": created_by,
                "is_active": True,
                "max_attempts": max_attempts,
                "expires_at": expires_at,
                "require_name": require_name,
                "time_limit_minutes": time_limit_minutes,
                "attempt_count": 0,
                "created_at": created_at,
                "audience": args[13] if len(args) > 13 else "public",
                "source_scope": json.loads(args[14]) if len(args) > 14 else None,
            }
        elif "UPDATE artifact_shares" in query:
            share_id = args[0]
            row = self.shares.get(str(share_id))
            if row is None:
                return "UPDATE 0"
            if "attempt_count = attempt_count + 1" in query:
                # Mirrors the atomic reservation: only one row-version wins.
                if not row["is_active"]:
                    return "UPDATE 0"
                if (
                    row["max_attempts"] is not None
                    and row["attempt_count"] >= row["max_attempts"]
                ):
                    return "UPDATE 0"
                row["attempt_count"] += 1
                return "UPDATE 1"
            if len(args) == 3 and (
                row["created_by"] != args[1] or row["tenant_id"] != args[2]
            ):
                return "UPDATE 0"
            row["is_active"] = False
            return "UPDATE 1"
        return "OK"


@pytest.fixture
def fake_db() -> _FakeDB:
    return _FakeDB()


@pytest.mark.asyncio
async def test_manager_create_and_public_snapshot(fake_db: _FakeDB) -> None:
    mgr = ArtifactShareManager(db=fake_db)
    share = await mgr.create_share(
        kind="quiz",
        title="Demo Quiz",
        payload={"quiz_id": "q1", "questions": [{"id": "a", "question_text": "Q?"}]},
        answer_keys=[{"id": "a", "correct_answer": ["A"]}],
        tenant_id="tenant-a",
        user_id="alex",
        max_attempts=2,
    )
    assert share["share_code"]
    assert share["kind"] == "quiz"

    public = await mgr.get_public_artifact(share["share_code"])
    assert public is not None
    assert public["title"] == "Demo Quiz"
    assert public["questions"][0]["id"] == "a"
    assert "answer_keys" not in public


@pytest.mark.asyncio
async def test_manager_expiry_and_revoke(fake_db: _FakeDB) -> None:
    mgr = ArtifactShareManager(db=fake_db)
    share = await mgr.create_share(
        kind="quiz", title="T", payload={}, answer_keys=None,
        tenant_id="t", user_id="alex", expires_hours=1,
    )
    row = fake_db.shares[share["share_id"]]
    row["expires_at"] = datetime.now(timezone.utc) - timedelta(minutes=1)
    assert await mgr.get_public_artifact(share["share_code"]) is None

    # Re-activate, then revoke.
    row["expires_at"] = None
    assert await mgr.revoke_share(share["share_id"], "alex", "t")
    assert await mgr.get_public_artifact(share["share_code"]) is None


@pytest.mark.asyncio
async def test_manager_submit_enforces_max_attempts(fake_db: _FakeDB) -> None:
    mgr = ArtifactShareManager(db=fake_db)
    share = await mgr.create_share(
        kind="quiz",
        title="Capped",
        payload={"quiz_id": str(uuid.uuid4())},
        answer_keys=[],
        tenant_id="tenant-a",
        user_id="alex",
        max_attempts=1,
        require_name=False,
    )
    first_token = await mgr.start_attempt(share["share_code"])
    second_token = await mgr.start_attempt(share["share_code"])
    first = await mgr.submit_attempt(share["share_code"], answers={}, attempt_token=first_token["attempt_token"])
    assert first["correct_count"] == 0
    assert fake_db.shares[share["share_id"]]["attempt_count"] == 1

    with pytest.raises(AttemptLimitReachedError):
        await mgr.submit_attempt(share["share_code"], answers={}, attempt_token=second_token["attempt_token"])


@pytest.mark.asyncio
async def test_reserve_attempt_slot_is_atomic_at_the_cap(fake_db: _FakeDB) -> None:
    """The conditional UPDATE must reject when the cap is already consumed."""
    mgr = ArtifactShareManager(db=fake_db)
    share = await mgr.create_share(
        kind="quiz", title="T", payload={}, max_attempts=1, require_name=False,
    )
    row = fake_db.shares[share["share_id"]]
    row["attempt_count"] = 1  # consumed concurrently after the read-side check

    with pytest.raises(AttemptLimitReachedError):
        await mgr._reserve_attempt_slot({"share_id": share["share_id"]})


@pytest.mark.asyncio
async def test_atomic_quiz_attempt_does_not_persist_when_slot_is_lost(
    fake_db: _FakeDB,
) -> None:
    mgr = ArtifactShareManager(db=fake_db)
    share = await mgr.create_share(
        kind="quiz",
        title="Raced",
        payload={"quiz_id": str(uuid.uuid4())},
        answer_keys=[],
        max_attempts=1,
        require_name=False,
    )
    token = await mgr.start_attempt(share["share_code"])
    fake_db.shares[share["share_id"]]["attempt_count"] = 1

    with pytest.raises(AttemptLimitReachedError):
        await mgr.submit_attempt(
            share["share_code"],
            answers={},
            attempt_token=token["attempt_token"],
        )
    assert fake_db.attempts == []


@pytest.mark.asyncio
async def test_display_name_claim_is_atomic(
    fake_db: _FakeDB,
) -> None:
    mgr = ArtifactShareManager(db=fake_db)
    share = await mgr.create_share(
        kind="quiz",
        title="Named",
        payload={"quiz_id": str(uuid.uuid4())},
        answer_keys=[],
        max_attempts=2,
        require_name=True,
    )

    first_token = await mgr.start_attempt(share["share_code"])
    second_token = await mgr.start_attempt(share["share_code"])
    await mgr.submit_attempt(share["share_code"], answers={}, display_name="Alex", attempt_token=first_token["attempt_token"])
    with pytest.raises(AttemptConflictError):
        await mgr.submit_attempt(share["share_code"], answers={}, display_name="Alex", attempt_token=second_token["attempt_token"])


@pytest.mark.asyncio
async def test_timed_attempt_uses_single_use_start_token(fake_db: _FakeDB) -> None:
    mgr = ArtifactShareManager(db=fake_db)
    share = await mgr.create_share(
        kind="quiz",
        title="Timed",
        payload={"quiz_id": str(uuid.uuid4())},
        answer_keys=[],
        require_name=False,
        time_limit_minutes=5,
    )

    with pytest.raises(AttemptInputError):
        await mgr.submit_attempt(share["share_code"], answers={})

    started = await mgr.start_attempt(share["share_code"])
    result = await mgr.submit_attempt(
        share["share_code"],
        answers={},
        attempt_token=started["attempt_token"],
    )
    assert result["attempt_id"]

    replay = await mgr.submit_attempt(
        share["share_code"], answers={}, attempt_token=started["attempt_token"],
    )
    assert replay["cached"] is True
    assert replay["attempt_id"] == result["attempt_id"]

    with pytest.raises(AttemptInputError):
        await mgr.submit_attempt(
            share["share_code"],
            answers={},
            attempt_token="malformed-token",
        )

    expired = await mgr.start_attempt(share["share_code"])
    expired_hash = next(
        key
        for key, token in fake_db.tokens.items()
        if token["consumed_at"] is None
    )
    fake_db.tokens[expired_hash]["expires_at"] = datetime.now(timezone.utc) - timedelta(
        seconds=1
    )
    with pytest.raises(AttemptInputError):
        await mgr.submit_attempt(
            share["share_code"],
            answers={},
            attempt_token=expired["attempt_token"],
        )


@pytest.mark.asyncio
async def test_internal_quiz_token_is_bound_to_signed_in_account(fake_db: _FakeDB) -> None:
    mgr = ArtifactShareManager(db=fake_db)
    share = await mgr.create_share(
        kind="quiz", title="Internal", payload={"quiz_id": str(uuid.uuid4())},
        answer_keys=[], tenant_id="tenant-a", user_id="alex", audience="internal",
        require_name=False,
        source_scope={"version": 1, "dataset_ids": [], "document_ids": [], "source_versions": []},
    )
    started = await mgr.start_attempt(share["share_code"], audience="internal", user_id="user-a")
    token = started["attempt_token"]
    assert await mgr.get_attempt_result(
        share["share_code"], token, audience="internal", user_id="user-b",
    ) is None
    with pytest.raises(AttemptInputError):
        await mgr.submit_attempt(
            share["share_code"], answers={}, attempt_token=token,
            audience="internal", user_id="user-b",
        )
    result = await mgr.submit_attempt(
        share["share_code"], answers={}, attempt_token=token,
        audience="internal", user_id="user-a",
    )
    assert result["attempt_id"]
    assert await mgr.get_attempt_result(
        share["share_code"], token, audience="internal", user_id="user-b",
    ) is None
    assert (await mgr.get_attempt_result(
        share["share_code"], token, audience="internal", user_id="user-a",
    ))["attempt_id"] == result["attempt_id"]


@pytest.mark.asyncio
async def test_start_attempt_prunes_only_a_bounded_stale_batch(fake_db: _FakeDB) -> None:
    mgr = ArtifactShareManager(db=fake_db)
    share = await mgr.create_share(
        kind="quiz",
        title="Cleanup",
        payload={"quiz_id": str(uuid.uuid4())},
        answer_keys=[],
        require_name=False,
    )
    now = datetime.now(timezone.utc)
    for index in range(150):
        fake_db.tokens[f"stale-{index}"] = {
            "id": uuid.uuid4(),
            "share_id": share["share_id"],
            "started_at": now - timedelta(hours=26),
            "expires_at": now - timedelta(hours=25),
            "consumed_at": None,
        }
    fake_db.tokens["live"] = {
        "id": uuid.uuid4(),
        "share_id": share["share_id"],
        "started_at": now,
        "expires_at": now + timedelta(minutes=30),
        "consumed_at": None,
    }
    fake_db.tokens["consumed-old"] = {
        "id": uuid.uuid4(),
        "share_id": share["share_id"],
        "started_at": now - timedelta(hours=26),
        "expires_at": now + timedelta(minutes=30),
        "consumed_at": now - timedelta(hours=30),
    }

    await mgr.start_attempt(share["share_code"])

    assert sum(key.startswith("stale-") for key in fake_db.tokens) == 51
    assert "consumed-old" not in fake_db.tokens
    assert "live" in fake_db.tokens


@pytest.mark.asyncio
async def test_revoke_requires_matching_tenant(fake_db: _FakeDB) -> None:
    mgr = ArtifactShareManager(db=fake_db)
    share = await mgr.create_share(
        kind="quiz",
        title="Tenant scoped",
        payload={},
        tenant_id="tenant-a",
        user_id="alex",
    )

    assert not await mgr.revoke_share(share["share_id"], "alex", "tenant-b")
    assert await mgr.get_public_artifact(share["share_code"]) is not None
    assert await mgr.revoke_share(share["share_id"], "alex", "tenant-a")


def test_create_share_endpoint_rejects_unowned_quiz(fake_db: _FakeDB) -> None:
    app = FastAPI()
    app.include_router(artifact_shares_router)
    app.state.database = fake_db

    class _User:
        user_id = "alex"
        tenant_id = "tenant-a"

    async def _user_override():
        return _User()

    from src.api.v1.artifact_shares import get_user_context

    app.dependency_overrides[get_user_context] = _user_override
    client = TestClient(app)

    resp = client.post(
        "/artifact-shares",
        json={"kind": "quiz", "quiz_id": str(uuid.uuid4()), "require_name": True},
    )
    assert resp.status_code == 404

    resp = client.post(
        "/artifact-shares",
        json={"kind": "conversation", "quiz_id": None},
    )
    assert resp.status_code == 400


def test_create_share_endpoint_returns_typed_contract(fake_db: _FakeDB) -> None:
    app = FastAPI()
    app.include_router(artifact_shares_router)
    app.state.database = fake_db

    class _User:
        user_id = "alex"
        tenant_id = "tenant-a"

    async def _user_override():
        return _User()

    from src.api.v1.artifact_shares import get_user_context

    app.dependency_overrides[get_user_context] = _user_override
    client = TestClient(app)
    quiz_id = uuid.uuid4()
    fake_db.quiz_rows[str(quiz_id)] = {
        "id": quiz_id,
        "tenant_id": "tenant-a",
        "title": "Typed quiz",
        "description": "contract",
        "question_count": 0,
        "difficulty": "medium",
        "dataset_ids": [], "created_by": "alex", "session_id": "source-session", "run_id": "source-run",
    }
    fake_db.source_snapshots = [{"run_id": "source-run", "snapshot": {"readonly_capabilities": {"items": []}}}]

    response = client.post(
        "/artifact-shares",
        json={"kind": "quiz", "quiz_id": str(quiz_id)},
    )

    assert response.status_code == 200
    assert response.json()["quiz_id"] == str(quiz_id)
    assert response.json()["quiz_title"] == "Typed quiz"


def test_create_share_rejects_quiz_derived_from_private_knowledge(fake_db: _FakeDB) -> None:
    app = FastAPI()
    app.include_router(artifact_shares_router)
    app.state.database = fake_db

    class _User:
        user_id = "alex"
        tenant_id = "tenant-a"

    from src.api.v1.artifact_shares import get_user_context

    app.dependency_overrides[get_user_context] = lambda: _User()
    quiz_id = uuid.uuid4()
    fake_db.quiz_rows[str(quiz_id)] = {
        "id": quiz_id, "tenant_id": "tenant-a", "title": "Private quiz",
        "description": "derived", "question_count": 0, "difficulty": "easy",
        "dataset_ids": ["private-kb"],
    }
    response = TestClient(app).post(
        "/artifact-shares", json={"kind": "quiz", "quiz_id": str(quiz_id)},
    )
    assert response.status_code == 409
    assert fake_db.shares == {}


def test_create_share_endpoint_rejects_invalid_bounds(fake_db: _FakeDB) -> None:
    """expires_hours / max_attempts must be positive — schema-enforced."""
    app = FastAPI()
    app.include_router(artifact_shares_router)
    app.state.database = fake_db

    class _User:
        user_id = "alex"
        tenant_id = "tenant-a"

    async def _user_override():
        return _User()

    from src.api.v1.artifact_shares import get_user_context

    app.dependency_overrides[get_user_context] = _user_override
    client = TestClient(app)

    for body in (
        {"kind": "quiz", "quiz_id": None, "max_attempts": 0},
        {"kind": "quiz", "quiz_id": None, "expires_hours": -1},
    ):
        resp = client.post("/artifact-shares", json=body)
        assert resp.status_code == 422, body


def test_artifact_share_endpoints_reject_malformed_uuids(fake_db: _FakeDB) -> None:
    app = FastAPI()
    app.include_router(artifact_shares_router)
    app.state.database = fake_db

    class _User:
        user_id = "alex"
        tenant_id = "tenant-a"

    async def _user_override():
        return _User()

    from src.api.v1.artifact_shares import get_user_context

    app.dependency_overrides[get_user_context] = _user_override
    client = TestClient(app)

    create = client.post(
        "/artifact-shares",
        json={"kind": "quiz", "quiz_id": "not-a-uuid"},
    )
    assert create.status_code == 422
    assert client.delete("/artifact-shares/not-a-uuid").status_code == 422


def test_artifact_share_openapi_has_typed_success_responses() -> None:
    app = FastAPI()
    app.include_router(artifact_shares_router)
    paths = app.openapi()["paths"]

    create_schema = paths["/artifact-shares"]["post"]["responses"]["200"]["content"]
    revoke_schema = paths["/artifact-shares/{share_id}"]["delete"]["responses"]["200"][
        "content"
    ]
    assert create_schema["application/json"]["schema"]["$ref"].endswith(
        "/ArtifactShareCreateResponse"
    )
    assert revoke_schema["application/json"]["schema"]["$ref"].endswith(
        "/ArtifactShareRevokeResponse"
    )


def test_artifact_share_manager_uses_canonical_assistant_schema() -> None:
    """Gateway and Assistant use different search paths in production."""
    import inspect

    source = inspect.getsource(ArtifactShareManager)
    for table in ("artifact_shares", "artifact_share_submitters"):
        assert f"assistant.{table}" in source
    assert "assistant.record_artifact_share_quiz_attempt" in source


def test_revoke_share_endpoint_returns_503_without_database(fake_db: _FakeDB) -> None:
    """Regression: revoke must not AttributeError into a 500 when db is missing."""
    app = FastAPI()
    app.include_router(artifact_shares_router)
    app.state.database = None

    class _User:
        user_id = "alex"
        tenant_id = "tenant-a"

    async def _user_override():
        return _User()

    from src.api.v1.artifact_shares import get_user_context

    app.dependency_overrides[get_user_context] = _user_override
    client = TestClient(app)

    resp = client.delete(f"/artifact-shares/{uuid.uuid4()}")
    assert resp.status_code == 503


def test_public_attempt_start_and_timed_submit_contract(fake_db: _FakeDB) -> None:
    mgr = ArtifactShareManager(db=fake_db)

    quiz_id = str(uuid.uuid4())
    fake_db.add_verified_quiz(quiz_id)

    async def _create():
        return await mgr.create_share(
            kind="quiz",
            title="Timed endpoint",
            payload={"quiz_id": quiz_id, "questions": []},
            answer_keys=[], tenant_id="tenant-a", user_id="alex",
            require_name=False,
            time_limit_minutes=5,
        )

    share = asyncio.run(_create())
    app = FastAPI()
    app.include_router(quiz_public_router, prefix="/api/v1")
    app.state.database = fake_db
    app.state.multi_rate_limiter = None
    client = TestClient(app)

    missing_token = client.post(
        f"/api/v1/quiz/shared/{share['share_code']}/submit",
        json={"answers": {}},
    )
    assert missing_token.status_code == 400
    assert missing_token.json()["detail"]["code"] == "attempt_input_invalid"

    started = client.post(
        f"/api/v1/quiz/public/{share['share_code']}/attempts/start"
    )
    assert started.status_code == 200
    attempt_token = started.json()["attempt_token"]
    assert attempt_token
    assert all(attempt_token not in stored_hash for stored_hash in fake_db.tokens)

    submitted = client.post(
        f"/api/v1/quiz/shared/{share['share_code']}/submit",
        json={"answers": {}, "attempt_token": attempt_token},
    )
    assert submitted.status_code == 200

    replayed = client.post(
        f"/api/v1/quiz/shared/{share['share_code']}/submit",
        json={"answers": {}, "attempt_token": attempt_token},
    )
    assert replayed.status_code == 200
    assert replayed.json()["attempt_id"] == submitted.json()["attempt_id"]
    assert replayed.json()["cached"] is True


def test_public_attempt_errors_use_stable_codes(fake_db: _FakeDB) -> None:
    app = FastAPI()
    app.include_router(quiz_public_router, prefix="/api/v1")
    app.state.database = fake_db
    app.state.multi_rate_limiter = None
    client = TestClient(app)

    missing = client.post("/api/v1/quiz/public/missing/attempts/start")
    assert missing.status_code == 404
    assert missing.json()["detail"] == {
        "code": "share_unavailable",
        "message": "Quiz not found or expired",
    }

    mgr = ArtifactShareManager(db=fake_db)

    quiz_id = str(uuid.uuid4())
    fake_db.add_verified_quiz(quiz_id)

    async def _create():
        return await mgr.create_share(
            kind="quiz",
            title="Capped endpoint",
            payload={"quiz_id": quiz_id, "questions": []},
            answer_keys=[], tenant_id="tenant-a", user_id="alex",
            max_attempts=1,
            require_name=True,
        )

    share = asyncio.run(_create())
    invalid_name = client.post(
        f"/api/v1/quiz/shared/{share['share_code']}/submit",
        json={"answers": {}, "display_name": "<script>"},
    )
    assert invalid_name.status_code == 400
    assert invalid_name.json()["detail"]["code"] == "attempt_input_invalid"

    fake_db.shares[share["share_id"]]["attempt_count"] = 1
    capped = client.post(
        f"/api/v1/quiz/public/{share['share_code']}/attempts/start"
    )
    assert capped.status_code == 429
    assert capped.json()["detail"]["code"] == "attempt_limit_reached"


@pytest.mark.parametrize("origin", ["inherited_private", "unverifiable"])
def test_public_quiz_routes_reject_private_or_unverifiable_origin(fake_db: _FakeDB, origin: str) -> None:
    quiz_id = str(uuid.uuid4())
    fake_db.add_verified_quiz(quiz_id)
    if origin == "inherited_private":
        fake_db.source_snapshots.insert(0, {
            "run_id": "earlier-private", "snapshot": {"readonly_capabilities": {
                "items": [{"kind": "knowledge", "payload": {"dataset_id": "private-kb"}}],
            }},
        })
    else:
        fake_db.source_snapshots = []
    share = asyncio.run(ArtifactShareManager(db=fake_db).create_share(
        kind="quiz", title="Legacy link", payload={"quiz_id": quiz_id, "questions": []},
        answer_keys=[], tenant_id="tenant-a", user_id="alex", require_name=False,
    ))
    app = FastAPI()
    app.state.database = fake_db
    app.state.multi_rate_limiter = None
    app.include_router(quiz_public_router, prefix="/api/v1")
    client = TestClient(app)
    code = share["share_code"]
    for method, url, body in [
        ("get", f"/api/v1/quiz/shared/{code}", None),
        ("post", f"/api/v1/quiz/public/{code}/attempts/start", {}),
        ("post", f"/api/v1/quiz/shared/{code}/submit", {"answers": {}}),
        ("post", f"/api/v1/quiz/public/{code}/attempts/result", {"attempt_token": "opaque"}),
    ]:
        response = client.request(method, url, **({"json": body} if body is not None else {}))
        assert response.status_code == 404
        assert response.json()["detail"]["code"] == "share_unavailable"
    assert fake_db.tokens == {} and fake_db.attempts == []


@pytest.mark.parametrize("origin", ["inherited_private", "unverifiable"])
def test_create_quiz_share_checks_creating_turn_origin(fake_db: _FakeDB, origin: str) -> None:
    quiz_id = str(uuid.uuid4())
    fake_db.add_verified_quiz(quiz_id)
    if origin == "inherited_private":
        fake_db.source_snapshots.insert(0, {
            "run_id": "earlier-private", "snapshot": {"readonly_capabilities": {
                "items": [{"kind": "knowledge", "payload": {"dataset_id": "private-kb"}}],
            }},
        })
    else:
        fake_db.source_snapshots = []
    app = FastAPI()
    app.state.database = fake_db
    app.include_router(artifact_shares_router)
    from src.api.v1.artifact_shares import get_user_context
    from src.core.auth.user_resolver import UserContext
    app.dependency_overrides[get_user_context] = lambda: UserContext(user_id="alex", tenant_id="tenant-a", is_authenticated=True)
    response = TestClient(app).post("/artifact-shares", json={"quiz_id": quiz_id})
    assert response.status_code == 409
    assert fake_db.shares == {}


def test_owner_share_list_is_scoped_and_excludes_grading_payload() -> None:
    from datetime import datetime, timezone
    from types import SimpleNamespace

    from src.api.v1.artifact_shares import get_user_context
    quiz_id = uuid.uuid4()
    class ListDB:
        async def fetch(self, query, *args):
            assert "tenant_id = $1 AND created_by = $2 AND kind = 'quiz'" in query
            assert "payload ->> 'quiz_id' = $3" in query
            assert "SELECT id AS share_id" in query and "answer_keys" not in query
            assert args == ("tenant-a", "alex", str(quiz_id), 50)
            return [{"share_id": uuid.uuid4(), "share_code": "test-code", "is_active": True, "expired": True,
                     "created_at": datetime.now(timezone.utc), "expires_at": datetime.now(timezone.utc),
                     "revoked_at": None, "require_name": True,
                     "payload": {"secret": "private"}, "answer_keys": ["hidden"]}]
    app = FastAPI()
    app.include_router(artifact_shares_router)
    app.state.database = ListDB()
    async def actor():
        return SimpleNamespace(tenant_id="tenant-a", user_id="alex")
    app.dependency_overrides[get_user_context] = actor
    response = TestClient(app).get(f"/artifact-shares?quiz_id={quiz_id}")
    assert response.status_code == 200
    result = response.json()
    assert len(result) == 1 and result[0]["expired"] is True
    assert "payload" not in result[0] and "answer_keys" not in result[0]


def test_internal_quiz_create_keeps_immutable_sources_private(fake_db: _FakeDB, monkeypatch) -> None:
    from src.api.v1 import artifact_shares
    from src.core.auth.user_resolver import UserContext

    quiz_id = str(uuid.uuid4())
    fake_db.add_verified_quiz(quiz_id)
    version_ref = ("dataset-a", "document-a", 3, "c" * 64)

    async def quiz_sources(_request, _quiz_id, _tenant_id, *, require_origin):
        assert require_origin
        return frozenset({"dataset-a"}), frozenset({("dataset-a", "document-a")}), frozenset({version_ref})

    async def allowed(*_args, **_kwargs):
        return True

    monkeypatch.setattr(artifact_shares, "quiz_source_scope", quiz_sources)
    monkeypatch.setattr(artifact_shares, "source_scope_allowed", allowed)
    app = FastAPI()
    app.state.database = fake_db
    app.include_router(artifact_shares_router)
    app.dependency_overrides[artifact_shares.get_user_context] = lambda: UserContext(
        user_id="alex", tenant_id="tenant-a", is_authenticated=True,
    )
    response = TestClient(app).post("/artifact-shares", json={
        "kind": "quiz", "quiz_id": quiz_id, "audience": "internal",
    })
    assert response.status_code == 200
    assert response.json()["audience"] == "internal"
    row = next(iter(fake_db.shares.values()))
    assert row["source_scope"]["source_versions"] == [["dataset-a", "document-a", 3, "c" * 64]]
    assert "source_scope" not in row["payload"]
    manager = ArtifactShareManager(db=fake_db)
    assert asyncio.run(manager.get_public_artifact(row["share_code"])) is None
    internal_payload = asyncio.run(manager.get_public_artifact(row["share_code"], audience="internal"))
    assert internal_payload is not None and internal_payload["audience"] == "internal"
    assert "source_scope" not in internal_payload

    public = TestClient(app).post("/artifact-shares", json={
        "kind": "quiz", "quiz_id": quiz_id, "audience": "public",
    })
    assert public.status_code == 409
