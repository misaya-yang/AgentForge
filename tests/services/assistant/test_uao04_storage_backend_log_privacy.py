from __future__ import annotations

import hashlib
import logging
from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock

import pytest
from ai_gateway_core.storage.file_storage import FileStorageService
from ai_gateway_core.storage.image_storage import OSSStorageBackend, S3StorageBackend

_STORAGE_KEY = (
    "uploads/tenant-storage-log-sentinel/user-storage-log-sentinel/"
    "private-object-storage-log-sentinel.bin"
)
_EXCEPTION_SENTINEL = "provider-exception-storage-log-sentinel"


def _service_with_backend(backend: object) -> FileStorageService:
    service = FileStorageService.__new__(FileStorageService)
    service._backend = backend  # type: ignore[assignment]
    return service


@pytest.mark.asyncio
async def test_governance_storage_delete_success_logs_only_stable_key_hash(
    caplog: pytest.LogCaptureFixture,
) -> None:
    backend = SimpleNamespace(delete=AsyncMock(return_value=True))
    service = _service_with_backend(backend)

    with caplog.at_level(logging.INFO):
        deleted = await service.delete_file(_STORAGE_KEY)

    assert deleted is True
    backend.delete.assert_awaited_once_with(_STORAGE_KEY)
    assert "operation=delete" in caplog.text
    assert hashlib.sha256(_STORAGE_KEY.encode("utf-8")).hexdigest() in caplog.text
    assert _STORAGE_KEY not in caplog.text
    assert "tenant-storage-log-sentinel" not in caplog.text
    assert "user-storage-log-sentinel" not in caplog.text
    assert all(record.exc_info is None for record in caplog.records)


@pytest.mark.asyncio
async def test_governance_storage_success_logging_never_changes_delete_contract(
    caplog: pytest.LogCaptureFixture,
) -> None:
    storage_key = "uploads/private-object-\udcff.bin"
    backend = SimpleNamespace(delete=AsyncMock(return_value=True))
    service = _service_with_backend(backend)

    with caplog.at_level(logging.INFO):
        deleted = await service.delete_file(storage_key)

    assert deleted is True
    assert storage_key not in caplog.text
    assert all(record.exc_info is None for record in caplog.records)


@pytest.mark.asyncio
@pytest.mark.parametrize("provider", ["s3", "oss"])
async def test_governance_storage_delete_failure_redacts_key_and_exception(
    provider: str,
    caplog: pytest.LogCaptureFixture,
) -> None:
    if provider == "s3":
        backend = S3StorageBackend(
            bucket="private-bucket-storage-log-sentinel",
            region="us-east-1",
            access_key="unused",
            secret_key="unused",
        )
        backend._client = SimpleNamespace(
            delete_object=AsyncMock(side_effect=RuntimeError(_EXCEPTION_SENTINEL))
        )
    else:
        backend = OSSStorageBackend(
            bucket="private-bucket-storage-log-sentinel",
            endpoint="private-endpoint-storage-log-sentinel",
            access_key="unused",
            secret_key="unused",
        )
        bucket = MagicMock()
        bucket.delete_object.side_effect = RuntimeError(_EXCEPTION_SENTINEL)
        backend._bucket = bucket

    service = _service_with_backend(backend)
    with caplog.at_level(logging.WARNING):
        deleted = await service.delete_file(_STORAGE_KEY)

    assert deleted is False
    assert f"provider={provider}" in caplog.text
    assert "operation=delete" in caplog.text
    assert "exception_type=RuntimeError" in caplog.text
    assert hashlib.sha256(_STORAGE_KEY.encode("utf-8")).hexdigest() in caplog.text
    for sentinel in (
        _STORAGE_KEY,
        _EXCEPTION_SENTINEL,
        "tenant-storage-log-sentinel",
        "user-storage-log-sentinel",
        "private-bucket-storage-log-sentinel",
        "private-endpoint-storage-log-sentinel",
    ):
        assert sentinel not in caplog.text
    assert all(record.exc_info is None for record in caplog.records)


@pytest.mark.asyncio
@pytest.mark.parametrize("provider", ["s3", "oss"])
async def test_governance_storage_failure_logging_never_changes_delete_contract(
    provider: str,
    caplog: pytest.LogCaptureFixture,
) -> None:
    storage_key = "uploads/private-object-\udcff.bin"
    if provider == "s3":
        backend = S3StorageBackend(
            bucket="unused",
            region="us-east-1",
            access_key="unused",
            secret_key="unused",
        )
        backend._client = SimpleNamespace(
            delete_object=AsyncMock(side_effect=RuntimeError(_EXCEPTION_SENTINEL))
        )
    else:
        backend = OSSStorageBackend(
            bucket="unused",
            endpoint="unused",
            access_key="unused",
            secret_key="unused",
        )
        bucket = MagicMock()
        bucket.delete_object.side_effect = RuntimeError(_EXCEPTION_SENTINEL)
        backend._bucket = bucket

    service = _service_with_backend(backend)
    with caplog.at_level(logging.WARNING):
        deleted = await service.delete_file(storage_key)

    assert deleted is False
    assert storage_key not in caplog.text
    assert _EXCEPTION_SENTINEL not in caplog.text
    assert all(record.exc_info is None for record in caplog.records)
