"""
Tests for the (fail-closed) presigned direct-upload API surface.

The in-gateway document-ownership ACL and upload-session cache were removed
under PRD T8.2 (the gateway must not read KB tables; the endpoints already
answered 501 before any of that state was reachable). These tests pin the
remaining public contract: authentication first, then 501 — no storage state
is ever created.
"""

import httpx
import pytest
from fastapi import FastAPI, HTTPException

from src.api.deps import get_user_context
from src.api.v1.presign import (
    PresignedUploadRequest,
    UploadConfirmRequest,
    confirm_upload,
    get_presigned_upload_url,
    get_task_status,
)
from src.api.v1.presign import (
    router as presign_router,
)
from src.core.auth.user_resolver import UserContext


@pytest.fixture
def user():
    return UserContext(
        user_id="user123",
        tenant_id="tenant456",
        is_authenticated=True,
    )


class TestUnavailableDirectUpload:
    @pytest.mark.asyncio
    async def test_upload_fails_before_presigning_or_creating_session(self, user):
        with pytest.raises(HTTPException) as exc_info:
            await get_presigned_upload_url(
                request=PresignedUploadRequest(
                    filename="document.pdf",
                    content_type="application/pdf",
                    document_id="doc123",
                ),
                user=user,
            )

        assert exc_info.value.status_code == 501

    @pytest.mark.asyncio
    async def test_confirm_fails_without_touching_storage(self, user):
        with pytest.raises(HTTPException) as exc_info:
            await confirm_upload(
                request=UploadConfirmRequest(
                    upload_id="upload-1",
                    storage_key="tenant456/doc123/document.pdf",
                    document_id="doc123",
                    filename="document.pdf",
                    content_type="application/pdf",
                ),
                user=user,
            )

        assert exc_info.value.status_code == 501

    @pytest.mark.asyncio
    async def test_status_fails_instead_of_returning_placeholder(self, user):
        with pytest.raises(HTTPException) as exc_info:
            await get_task_status("task-1", user=user)

        assert exc_info.value.status_code == 501

    @pytest.mark.asyncio
    async def test_unauthenticated_upload_rejected_with_401(self):
        anon = UserContext(user_id="anonymous", tenant_id="", is_authenticated=False)
        with pytest.raises(HTTPException) as exc_info:
            await get_presigned_upload_url(
                request=PresignedUploadRequest(
                    filename="document.pdf",
                    content_type="application/pdf",
                    document_id="doc123",
                ),
                user=anon,
            )
        assert exc_info.value.status_code == 401

    @pytest.mark.asyncio
    async def test_upload_route_returns_501_without_storage_service(self, user):
        app = FastAPI()
        app.include_router(presign_router, prefix="/api/v1")
        app.dependency_overrides[get_user_context] = lambda: user

        async with httpx.AsyncClient(
            transport=httpx.ASGITransport(app=app),
            base_url="http://test",
        ) as client:
            response = await client.post(
                "/api/v1/presign/upload",
                json={
                    "filename": "document.pdf",
                    "content_type": "application/pdf",
                    "document_id": "doc123",
                },
            )

        assert response.status_code == 501
        assert response.json()["detail"] == (
            "Direct presigned upload is not implemented; use the standard upload API."
        )
