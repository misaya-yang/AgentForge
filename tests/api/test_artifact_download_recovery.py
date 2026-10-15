"""Offline mounted-router checks; storage/identity doubles are not live acceptance."""
from types import SimpleNamespace
from unittest.mock import AsyncMock

import pytest
from fastapi import FastAPI, HTTPException
from httpx import ASGITransport, AsyncClient

from src.api.v1._assistant_routes import artifacts
from src.core.auth.user_resolver import UserContext

USER = UserContext(user_id="user-a", tenant_id="tenant-a", is_authenticated=True)
URL = "/api/v1/assistant/artifacts/art-a/download"
BODY = {"session_id": "session-a", "type": "document", "format": "txt", "title": "Report",
        "filename": "report.txt", "content_base64": "Ynl0ZXM="}


@pytest.fixture
def setup(monkeypatch):
    artifact = SimpleNamespace(
        artifact_id="art-a", session_id="session-a", tenant_id=USER.tenant_id,
        user_id=USER.user_id, type="document", format="txt", title="Report",
        filename="report.txt", size_bytes=5, mime_type="text/plain", source="ai",
        message_id=None, metadata={}, created_at=None,
    )
    storage = AsyncMock()
    storage.get_artifact.return_value = artifact
    storage.get_session_artifacts.return_value = [artifact]
    storage.create_artifact.return_value = artifact
    storage.download_artifact.return_value = b"bytes"
    storage.get_presigned_download_url.side_effect = RuntimeError("private signer failure")
    source_access = AsyncMock()
    monkeypatch.setattr(artifacts, "get_artifact_storage", lambda: storage)
    monkeypatch.setattr(artifacts, "require_artifact_source_access", source_access)
    app = FastAPI()
    app.include_router(artifacts.router, prefix="/api/v1/assistant")
    app.dependency_overrides[artifacts.get_user_context] = lambda: USER
    app.state.session_manager = SimpleNamespace(get=AsyncMock(return_value=artifact))
    return app, storage, artifact, source_access


@pytest.mark.asyncio
@pytest.mark.parametrize("operation", ["list", "detail", "create"])
async def test_metadata_succeeds_without_unused_signing(setup, operation):
    app, storage, _, _ = setup
    async with AsyncClient(transport=ASGITransport(app=app), base_url="http://test") as client:
        if operation == "list":
            response = await client.get("/api/v1/assistant/sessions/session-a/artifacts")
        elif operation == "detail":
            response = await client.get("/api/v1/assistant/artifacts/art-a")
        else:
            response = await client.post("/api/v1/assistant/artifacts", json=BODY)
        assert response.status_code == 200, response.text
        data = response.json()
        if operation == "list":
            assert data["total"] == 1
            data = data["artifacts"][0]
        assert data["download_url"] == URL
        assert data["ready"] is True
        storage.get_presigned_download_url.assert_not_awaited()
        if operation == "create":
            storage.create_artifact.assert_awaited_once()
        # Metadata is not a byte-availability guarantee; every click reads afresh.
        storage.download_artifact.side_effect = RuntimeError("private storage failure")
        failed = await client.get(URL)
        assert failed.status_code == 500
        assert failed.json() == {"detail": "Failed to download artifact"}
        storage.download_artifact.side_effect = None
        recovered = await client.get(URL)
        assert recovered.status_code == 200 and recovered.content == b"bytes"
        assert storage.download_artifact.await_count == 2


@pytest.mark.asyncio
@pytest.mark.parametrize("field", ["user_id", "tenant_id"])
async def test_metadata_and_download_still_conceal_other_owner(setup, field):
    app, storage, artifact, source_access = setup
    setattr(artifact, field, "other")
    async with AsyncClient(transport=ASGITransport(app=app), base_url="http://test") as client:
        for suffix in ["", "/download"]:
            response = await client.get("/api/v1/assistant/artifacts/art-a" + suffix)
            assert response.status_code == 404
    source_access.assert_not_awaited()
    storage.download_artifact.assert_not_awaited()
    storage.get_presigned_download_url.assert_not_awaited()


@pytest.mark.asyncio
async def test_metadata_success_does_not_authorize_download_after_revocation(setup):
    app, storage, _, source_access = setup
    async with AsyncClient(transport=ASGITransport(app=app), base_url="http://test") as client:
        metadata = await client.get("/api/v1/assistant/artifacts/art-a")
        assert metadata.status_code == 200
        source_access.side_effect = HTTPException(403, "Source access revoked")
        response = await client.get(URL)
        assert response.status_code == 403
        listed = await client.get("/api/v1/assistant/sessions/session-a/artifacts")
        assert listed.json() == {"artifacts": [], "total": 0}
    storage.download_artifact.assert_not_awaited()
    storage.get_presigned_download_url.assert_not_awaited()
