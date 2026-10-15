from __future__ import annotations

from types import SimpleNamespace
from unittest.mock import AsyncMock

import pytest
from ai_gateway_core.storage.artifact_storage import ArtifactInfo, ArtifactStorageService
from ai_gateway_core.storage.image_storage import StorageBackend, StorageConfig


def _artifact(*, owner_scope: str | None, tenant_id: str, user_id: str) -> ArtifactInfo:
    return ArtifactInfo(
        artifact_id="artifact-1",
        session_id="session-1",
        tenant_id=tenant_id,
        user_id=user_id,
        type="image",
        format="png",
        title="Image",
        filename="image.png",
        storage_key="artifact/image.png",
        owner_scope=owner_scope,
    )


@pytest.mark.asyncio
async def test_variant_url_legacy_null_owner_requires_matching_tenant_and_user() -> None:
    service = object.__new__(ArtifactStorageService)
    service.find_variant = AsyncMock(
        return_value=_artifact(owner_scope=None, tenant_id="tenant-1", user_id="user-1")
    )
    service.get_presigned_download_url = AsyncMock(return_value="https://storage.test/image")

    denied_without_fallback = await service.get_presigned_download_url_for_variant(
        "artifact-1",
        "raw",
        owner_scope="opaque-owner",
    )
    denied_for_other_user = await service.get_presigned_download_url_for_variant(
        "artifact-1",
        "raw",
        owner_scope="opaque-owner",
        tenant_id="tenant-1",
        user_id="other-user",
    )
    allowed = await service.get_presigned_download_url_for_variant(
        "artifact-1",
        "raw",
        owner_scope="opaque-owner",
        tenant_id="tenant-1",
        user_id="user-1",
    )

    assert denied_without_fallback == (None, None)
    assert denied_for_other_user == (None, None)
    assert allowed == ("https://storage.test/image", "raw")


@pytest.mark.asyncio
async def test_variant_url_non_null_owner_scope_must_match_exactly() -> None:
    service = object.__new__(ArtifactStorageService)
    service.find_variant = AsyncMock(
        return_value=_artifact(
            owner_scope="owner-a",
            tenant_id="tenant-1",
            user_id="user-1",
        )
    )
    service.get_presigned_download_url = AsyncMock(return_value="https://storage.test/image")

    denied = await service.get_presigned_download_url_for_variant(
        "artifact-1",
        "raw",
        owner_scope="owner-b",
        tenant_id="tenant-1",
        user_id="user-1",
    )

    assert denied == (None, None)
    service.get_presigned_download_url.assert_not_awaited()


@pytest.mark.asyncio
async def test_deterministic_artifact_replay_reuses_existing_object() -> None:
    artifact_id = "art_1234567890abcdef"
    content = b"same bytes"
    content_sha256 = "a" * 64
    existing = ArtifactInfo(
        artifact_id=artifact_id,
        session_id="session-1",
        tenant_id="tenant-1",
        user_id="user-1",
        type="document",
        format="pdf",
        title="Report",
        filename="report.pdf",
        storage_key=f"artifacts/tenant-1/session-1/{artifact_id}_report.pdf",
        size_bytes=len(content),
        mime_type="application/pdf",
        metadata={"content_sha256": content_sha256, "execution_id": "execution-1"},
    )
    service = object.__new__(ArtifactStorageService)
    service.database = SimpleNamespace(_pool=object())
    service._backend = SimpleNamespace(upload=AsyncMock())
    service.get_artifact = AsyncMock(return_value=existing)

    replayed = await service.create_artifact(
        session_id="session-1",
        tenant_id="tenant-1",
        user_id="user-1",
        type="document",
        format="pdf",
        title="Report",
        filename="report.pdf",
        content=content,
        metadata={"content_sha256": content_sha256, "execution_id": "execution-1"},
        artifact_id=artifact_id,
    )

    assert replayed is existing
    service._backend.upload.assert_not_awaited()


@pytest.mark.asyncio
async def test_deterministic_artifact_replay_rejects_metadata_conflict() -> None:
    artifact_id = "art_1234567890abcdef"
    existing = ArtifactInfo(
        artifact_id=artifact_id,
        session_id="session-1",
        tenant_id="tenant-1",
        user_id="user-1",
        type="document",
        format="pdf",
        title="Report",
        filename="report.pdf",
        storage_key=f"artifacts/tenant-1/session-1/{artifact_id}_report.pdf",
        size_bytes=10,
        mime_type="application/pdf",
        metadata={"content_sha256": "b" * 64},
    )
    service = object.__new__(ArtifactStorageService)
    service.database = SimpleNamespace(_pool=object())
    service._backend = SimpleNamespace(upload=AsyncMock())
    service.get_artifact = AsyncMock(return_value=existing)

    with pytest.raises(ValueError, match="idempotency conflict"):
        await service.create_artifact(
            session_id="session-1",
            tenant_id="tenant-1",
            user_id="user-1",
            type="document",
            format="pdf",
            title="Report",
            filename="report.pdf",
            content=b"same bytes",
            metadata={"content_sha256": "a" * 64},
            artifact_id=artifact_id,
        )

    service._backend.upload.assert_not_awaited()


@pytest.mark.asyncio
async def test_image_blob_public_contract_heads_before_bounded_read(tmp_path) -> None:
    service = ArtifactStorageService(
        StorageConfig(backend=StorageBackend.LOCAL, local_base_path=str(tmp_path))
    )
    blob_id = "iblob_1234567890abcdef12345678"
    storage_key = service.image_blob_storage_key("owner-a", blob_id, "../reference.png")
    assert storage_key.startswith("image-blobs/")
    assert "owner-a" not in storage_key
    assert ".." not in storage_key
    await service.store_image_blob_object(
        storage_key,
        content=b"image",
        mime_type="image/png",
        max_bytes=16,
    )

    info = await service.inspect_image_blob_object(storage_key, max_bytes=16)
    assert info is not None and info.size_bytes == 5
    assert await service.read_image_blob_object(storage_key, max_bytes=16) == b"image"

    with pytest.raises(ValueError, match="size limit"):
        await service.inspect_image_blob_object(storage_key, max_bytes=4)
