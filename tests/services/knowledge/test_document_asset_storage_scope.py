from __future__ import annotations

from types import SimpleNamespace

import pytest
from ai_gateway_core.storage.image_storage import ImageStorageService
from knowledge_service.core.crypto import verify_signed_url


class RecordingBackend:
    def __init__(self) -> None:
        self.downloads: list[str] = []
        self.deleted_prefixes: list[str] = []

    async def download(self, key: str) -> bytes:
        self.downloads.append(key)
        return b"owned-image"

    async def delete_prefix(self, prefix: str) -> int:
        self.deleted_prefixes.append(prefix)
        return 2


def make_storage() -> tuple[ImageStorageService, RecordingBackend]:
    backend = RecordingBackend()
    storage = ImageStorageService.__new__(ImageStorageService)
    storage._backend = backend  # type: ignore[assignment]
    return storage, backend


@pytest.mark.asyncio
async def test_document_asset_cleanup_uses_only_exact_owned_prefixes() -> None:
    storage, backend = make_storage()

    deleted = await storage.delete_document_assets(" tenant-a ", " document-a ")

    assert deleted == 4
    assert backend.deleted_prefixes == [
        "knowledge/confluence/tenant-a/document-a/images/",
        "knowledge/documents/tenant-a/document-a/original/",
    ]


@pytest.mark.asyncio
@pytest.mark.parametrize(
    ("tenant_id", "document_id"),
    [
        ("", "document-a"),
        ("tenant-a", "   "),
        ("tenant/a", "document-a"),
        ("tenant-a", "../document-a"),
        ("tenant-a", "document\\a"),
    ],
)
async def test_document_asset_cleanup_rejects_ambiguous_scope(
    tenant_id: str,
    document_id: str,
) -> None:
    storage, backend = make_storage()

    with pytest.raises(ValueError, match="invalid"):
        await storage.delete_document_assets(tenant_id, document_id)

    assert backend.deleted_prefixes == []


@pytest.mark.asyncio
async def test_scoped_document_image_download_uses_backend_only() -> None:
    storage, backend = make_storage()
    key = "knowledge/confluence/tenant-a/document-a/images/attachment_image.png"

    result = await storage.download_document_image("tenant-a", "document-a", key)

    assert result == b"owned-image"
    assert backend.downloads == [key]


@pytest.mark.asyncio
@pytest.mark.parametrize(
    "storage_key",
    [
        "http://127.0.0.1:8092/internal/metadata",
        "knowledge/confluence/tenant-b/document-a/images/image.png",
        "knowledge/confluence/tenant-a/document-b/images/image.png",
        "knowledge/confluence/tenant-a/document-a/images/",
        "knowledge/confluence/tenant-a/document-a/images/../secret",
        "knowledge/confluence/tenant-a/document-a/images/folder\\secret",
        " knowledge/confluence/tenant-a/document-a/images/image.png ",
    ],
)
async def test_scoped_document_image_download_rejects_unowned_or_unsafe_key(
    storage_key: str,
) -> None:
    storage, backend = make_storage()

    with pytest.raises(ValueError, match="outside"):
        await storage.download_document_image("tenant-a", "document-a", storage_key)

    assert backend.downloads == []


def test_local_image_url_signing_matches_knowledge_route_verifier() -> None:
    storage = ImageStorageService.__new__(ImageStorageService)
    storage.config = SimpleNamespace(url_expiry_seconds=60)  # type: ignore[assignment]
    storage._signing_key = "test-signing-key"

    signed = storage._sign_url_if_local("file:///tmp/owned-image.png")

    assert "expires=" in signed and "sig=" in signed
    assert verify_signed_url(signed, "test-signing-key") == (True, "")


def test_local_image_url_signing_fails_closed_without_key() -> None:
    storage = ImageStorageService.__new__(ImageStorageService)
    storage.config = SimpleNamespace(url_expiry_seconds=60)  # type: ignore[assignment]
    storage._signing_key = ""

    with pytest.raises(RuntimeError, match="signing key"):
        storage._sign_url_if_local("file:///tmp/owned-image.png")
