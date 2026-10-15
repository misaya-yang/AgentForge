from __future__ import annotations

from datetime import datetime, timedelta, timezone
from types import SimpleNamespace
from typing import Any

import pytest
from knowledge_service.services.knowledge.confluence.image_processor import (
    ImageProcessingResult,
)
from knowledge_service.services.knowledge.confluence.models import (
    ConfluenceAttachment,
    ConfluencePage,
)
from knowledge_service.services.knowledge.confluence.sync_service import (
    ConfluenceSyncError,
    ConfluenceSyncService,
)


def _settings(*, stale_seconds: int = 3600) -> Any:
    return SimpleNamespace(
        confluence=SimpleNamespace(
            client_cache_ttl_seconds=300,
            stale_sync_timeout_seconds=stale_seconds,
        )
    )


def _page() -> ConfluencePage:
    return ConfluencePage(
        page_id="page-a",
        space_key="SPACE",
        title="Page A",
        version=7,
        body_storage="<p>Hello</p>",
        updated_at="2026-08-02T12:00:00Z",
    )


def _attachment(
    attachment_id: str,
    *,
    size: int,
    updated_at: str = "2026-08-02T12:00:00Z",
) -> ConfluenceAttachment:
    return ConfluenceAttachment(
        attachment_id=attachment_id,
        page_id="page-a",
        filename=f"{attachment_id}.png",
        media_type="image/png",
        file_size=size,
        download_link=f"/download/{attachment_id}",
        updated_at=updated_at,
    )


@pytest.mark.asyncio
async def test_source_generation_uses_same_deterministic_image_admission_policy() -> None:
    class Client:
        def __init__(self) -> None:
            self.attachments = [
                _attachment("z", size=5),
                _attachment("b", size=15),
                _attachment("a", size=6),
                _attachment("c", size=7),
            ]

        async def get_page_image_attachments(self, **_kwargs: Any) -> list[Any]:
            return list(self.attachments)

    client = Client()
    processor = SimpleNamespace(max_image_size=10, max_images_per_page=2)
    service = ConfluenceSyncService(
        settings=_settings(),
        database=SimpleNamespace(),
        knowledge_service=SimpleNamespace(),
        image_processor=processor,
    )
    service._get_client = lambda _connection_id: _async_value(client)  # type: ignore[method-assign]

    generation, processed, source = await service._resolve_source_generation(
        connection_id="connection-a",
        page=_page(),
        sync_images=True,
    )

    assert [item["attachment_id"] for item in source] == ["a", "b", "c", "z"]
    assert [item["attachment_id"] for item in processed] == ["a", "c"]

    # Even a policy-skipped attachment remains part of the source fingerprint,
    # so a later size/updated_at change creates a new source generation.
    client.attachments[0] = _attachment(
        "z",
        size=5,
        updated_at="2026-08-02T12:01:00Z",
    )
    changed_generation, _, _ = await service._resolve_source_generation(
        connection_id="connection-a",
        page=_page(),
        sync_images=True,
    )
    assert changed_generation != generation


async def _async_value(value: Any) -> Any:
    return value


@pytest.mark.asyncio
async def test_source_publish_orders_hidden_marker_assets_transaction_and_queue() -> None:
    events: list[str] = []

    class Transaction:
        async def __aenter__(self) -> None:
            events.append("txn-enter")

        async def __aexit__(self, *_args: Any) -> None:
            events.append("txn-exit")

    class Connection:
        def transaction(self) -> Transaction:
            return Transaction()

    class Lease:
        async def __aenter__(self) -> Connection:
            events.append("lease-enter")
            return Connection()

        async def __aexit__(self, *_args: Any) -> None:
            events.append("lease-exit")

    class Database:
        def document_index_update_lease(self, *_args: Any) -> Lease:
            return Lease()

        async def get_dataset(self, *_args: Any, **_kwargs: Any) -> dict[str, Any]:
            events.append("dataset-read")
            return {"dataset_id": "dataset-a", "tenant_id": "tenant-a", "index_config": {}}

        async def begin_confluence_document_sync(self, *_args: Any, **_kwargs: Any) -> bool:
            events.append("begin-hidden")
            return True

        async def get_document(self, *_args: Any, **_kwargs: Any) -> dict[str, Any]:
            events.append("document-read")
            return {
                "document_id": "document-a",
                "dataset_id": "dataset-a",
                "status": "syncing",
                "enabled": True,
                "archived": False,
                "content": "",
                "metadata": {},
            }

        async def create_document_version(self, **_kwargs: Any) -> None:
            raise AssertionError("first generation must not create an empty version")

        async def prepare_confluence_document_update(
            self,
            *_args: Any,
            **kwargs: Any,
        ) -> bool:
            events.append("final-and-page")
            assert kwargs["page_record"]["binding_id"] == "binding-a"
            assert kwargs["page_record"]["page_id"] == "page-a"
            return True

    class Storage:
        async def delete_document_images(self, *_args: Any) -> None:
            events.append("canonical-assets-delete")

    class Worker:
        async def enqueue_claimed(self, *_args: Any) -> None:
            events.append("queue-publish")

    service = ConfluenceSyncService(
        settings=_settings(),
        database=Database(),
        knowledge_service=SimpleNamespace(),
        knowledge_worker=Worker(),
        image_storage_service=Storage(),
    )
    image_count = await service._prepare_existing_page_update(
        binding={
            "binding_id": "binding-a",
            "connection_id": "connection-a",
            "dataset_id": "dataset-a",
            "tenant_id": "tenant-a",
            "sync_images": False,
        },
        document_id="document-a",
        page=_page(),
        sync_generation="generation-a",
        attachment_manifest=[],
        source_attachment_manifest=[],
    )

    assert image_count == 0
    assert events == [
        "lease-enter",
        "txn-enter",
        "dataset-read",
        "begin-hidden",
        "txn-exit",
        "canonical-assets-delete",
        "txn-enter",
        "dataset-read",
        "document-read",
        "final-and-page",
        "txn-exit",
        "lease-exit",
        "queue-publish",
    ]


@pytest.mark.asyncio
async def test_failed_asset_generation_is_exactly_aborted_for_immediate_retry() -> None:
    events: list[str] = []

    class Transaction:
        async def __aenter__(self) -> None:
            events.append("txn-enter")

        async def __aexit__(self, *_args: Any) -> None:
            events.append("txn-exit")

    class Connection:
        def transaction(self) -> Transaction:
            return Transaction()

    class Lease:
        async def __aenter__(self) -> Connection:
            events.append("lease-enter")
            return Connection()

        async def __aexit__(self, *_args: Any) -> None:
            events.append("lease-exit")

    class Database:
        def document_index_update_lease(self, *_args: Any) -> Lease:
            return Lease()

        async def get_dataset(self, *_args: Any, **_kwargs: Any) -> dict[str, Any]:
            return {"dataset_id": "dataset-a", "tenant_id": "tenant-a", "index_config": {}}

        async def begin_confluence_document_sync(self, *_args: Any, **_kwargs: Any) -> bool:
            events.append("begin-hidden")
            return True

        async def prepare_confluence_document_update(self, *_args: Any, **_kwargs: Any) -> bool:
            raise AssertionError("failed assets must not finalize")

        async def abort_confluence_document_sync(
            self,
            document_id: str,
            dataset_id: str,
            *,
            generation: str,
            error: str,
            connection: Any,
        ) -> bool:
            assert (document_id, dataset_id, generation) == (
                "document-a",
                "dataset-a",
                "generation-a",
            )
            assert connection is not None
            assert "RuntimeError" in error
            events.append("abort-exact")
            return True

    service = ConfluenceSyncService(
        settings=_settings(),
        database=Database(),
        knowledge_service=SimpleNamespace(),
        knowledge_worker=SimpleNamespace(),
    )

    async def fail_assets(**_kwargs: Any) -> dict[str, Any]:
        raise RuntimeError("source changed")

    service._build_image_source_metadata = fail_assets  # type: ignore[method-assign]
    with pytest.raises(RuntimeError, match="source changed"):
        await service._prepare_existing_page_update(
            binding={
                "binding_id": "binding-a",
                "connection_id": "connection-a",
                "dataset_id": "dataset-a",
                "tenant_id": "tenant-a",
                "sync_images": False,
            },
            document_id="document-a",
            page=_page(),
            sync_generation="generation-a",
            attachment_manifest=[],
            source_attachment_manifest=[],
        )

    assert events == [
        "lease-enter",
        "txn-enter",
        "begin-hidden",
        "txn-exit",
        "txn-enter",
        "abort-exact",
        "txn-exit",
        "lease-exit",
    ]


@pytest.mark.asyncio
async def test_empty_attachment_is_a_durable_skip_receipt_not_manifest_drift() -> None:
    expected = {
        "attachment_id": "image-a",
        "filename": "image-a.png",
        "media_type": "image/png",
        "file_size": 4,
        "updated_at": "2026-08-02T12:00:00Z",
    }

    class Processor:
        async def process_page_images(self, **_kwargs: Any) -> ImageProcessingResult:
            return ImageProcessingResult(
                page_id="page-a",
                document_id="document-a",
                total_images=1,
                processed_images=0,
                skipped_images=1,
                failed_images=0,
                segments=[],
                errors=[],
                skipped_attachments=[
                    {**expected, "reason": "empty_or_actual_size_exceeded"}
                ],
            )

    class Storage:
        async def delete_document_images(self, *_args: Any) -> None:
            pass

    service = ConfluenceSyncService(
        settings=_settings(),
        database=SimpleNamespace(),
        knowledge_service=SimpleNamespace(),
        image_processor=Processor(),
        image_storage_service=Storage(),
    )
    metadata = await service._build_image_source_metadata(
        connection_id="connection-a",
        tenant_id="tenant-a",
        document_id="document-a",
        page=_page(),
        sync_generation="generation-a",
        expected_attachment_manifest=[expected],
        sync_images=True,
        image_max_size_bytes=None,
    )

    assert metadata["extracted_images"] == []
    assert metadata["image_count"] == 0
    assert metadata["skipped_confluence_attachments"] == [
        {**expected, "reason": "empty_or_actual_size_exceeded"}
    ]


@pytest.mark.asyncio
async def test_unchanged_skipped_attachment_does_not_rebuild_forever() -> None:
    attachment = _attachment("image-a", size=4)
    manifest = {
        "attachment_id": attachment.attachment_id,
        "filename": attachment.filename,
        "media_type": attachment.media_type,
        "file_size": attachment.file_size,
        "updated_at": attachment.updated_at,
    }

    class Database:
        async def get_document(self, _document_id: str) -> dict[str, Any]:
            return {
                "metadata": {
                    "_confluence_image_source_generation": {"complete": True},
                    "_confluence_attachment_manifest": [manifest],
                    "extracted_images": [],
                    "skipped_confluence_attachments": [
                        {**manifest, "reason": "empty_or_actual_size_exceeded"}
                    ],
                }
            }

    class Client:
        async def get_page_image_attachments(self, **_kwargs: Any) -> list[Any]:
            return [attachment]

    service = ConfluenceSyncService(
        settings=_settings(),
        database=Database(),
        knowledge_service=SimpleNamespace(),
    )
    service._get_client = lambda _connection_id: _async_value(Client())  # type: ignore[method-assign]

    assert (
        await service._check_image_updates_needed(
            document_id="document-a",
            connection_id="connection-a",
            page_id="page-a",
        )
        is False
    )


@pytest.mark.asyncio
async def test_same_generation_queued_create_retry_only_republishes_queue() -> None:
    page = _page()
    service = ConfluenceSyncService(
        settings=_settings(),
        database=SimpleNamespace(),
        knowledge_service=SimpleNamespace(),
    )
    generation, _, _ = await service._resolve_source_generation(
        connection_id="connection-a",
        page=page,
        sync_images=False,
    )

    class Database:
        async def get_dataset(self, _dataset_id: str) -> dict[str, Any]:
            return {"dataset_id": "dataset-a", "tenant_id": "tenant-a", "index_config": {}}

        async def get_document(self, document_id: str) -> dict[str, Any]:
            return {
                "document_id": document_id,
                "dataset_id": "dataset-a",
                "source_type": "confluence",
                "status": "waiting",
                "metadata": {
                    "_confluence_image_source_generation": {
                        "sync_generation": generation,
                        "complete": True,
                    },
                    "extracted_images": [],
                    "image_count": 0,
                },
            }

        async def insert_document(self, *_args: Any, **_kwargs: Any) -> None:
            raise AssertionError("same-generation retry must not insert")

        def document_index_update_lease(self, *_args: Any) -> Any:
            raise AssertionError("same-generation retry must not replace source assets")

    calls: list[tuple[str, str]] = []

    class Worker:
        async def enqueue_claimed(self, dataset_id: str, document_id: str) -> None:
            calls.append((dataset_id, document_id))

    service.db = Database()
    service.worker = Worker()
    result = await service._create_document_from_page(
        dataset_id="dataset-a",
        connection_id="connection-a",
        page=page,
        created_by="user-a",
        sync_images=False,
        tenant_id="tenant-a",
    )

    assert result["status"] == "waiting"
    assert calls == [("dataset-a", result["document_id"])]


@pytest.mark.asyncio
async def test_confluence_extra_metadata_cannot_inject_source_receipts() -> None:
    class Database:
        async def get_dataset(self, _dataset_id: str) -> dict[str, Any]:
            return {"dataset_id": "dataset-a", "tenant_id": "tenant-a", "index_config": {}}

    service = ConfluenceSyncService(
        settings=_settings(),
        database=Database(),
        knowledge_service=SimpleNamespace(),
        knowledge_worker=SimpleNamespace(),
    )

    with pytest.raises(ConfluenceSyncError, match="reserved key"):
        await service._create_document_from_page(
            dataset_id="dataset-a",
            connection_id="connection-a",
            page=_page(),
            extra_metadata={"original_file_key": "foreign/object"},
            sync_images=False,
            tenant_id="tenant-a",
        )


@pytest.mark.asyncio
async def test_incremental_sync_recovers_only_explicitly_stale_task_state() -> None:
    old = datetime.now(timezone.utc) - timedelta(minutes=2)
    events: list[tuple[str, Any]] = []

    class Database:
        async def get_confluence_binding(self, _binding_id: str) -> dict[str, Any]:
            return {
                "binding_id": "binding-a",
                "connection_id": "connection-a",
                "dataset_id": "dataset-a",
                "status": "syncing",
                "owner_id": "user-a",
            }

        async def get_dataset(self, _dataset_id: str) -> dict[str, Any]:
            return {"dataset_id": "dataset-a", "tenant_id": "tenant-a", "index_config": {}}

        async def list_confluence_sync_tasks(
            self,
            *,
            status: str,
            **_kwargs: Any,
        ) -> list[dict[str, Any]]:
            if status == "pending":
                return [{"task_id": "stale-task", "status": status, "updated_at": old}]
            return []

        async def update_confluence_sync_task(
            self,
            task_id: str,
            updates: dict[str, Any],
        ) -> None:
            events.append((f"task:{task_id}", dict(updates)))

        async def update_confluence_binding(
            self,
            binding_id: str,
            updates: dict[str, Any],
        ) -> None:
            events.append((f"binding:{binding_id}", dict(updates)))

        async def create_confluence_sync_task(self, **kwargs: Any) -> None:
            events.append(("create-task", dict(kwargs)))

    service = ConfluenceSyncService(
        settings=_settings(stale_seconds=60),
        database=Database(),
        knowledge_service=SimpleNamespace(),
    )

    def discard_background(coro: Any, name: str) -> None:
        assert name == "incremental-sync-binding-"
        coro.close()

    service._create_background_task = discard_background  # type: ignore[method-assign]
    task_id = await service.incremental_sync("binding-a")

    assert task_id
    assert events[0][0] == "task:stale-task"
    assert events[0][1]["status"] == "failed"
    assert events[1] == (
        "binding:binding-a",
        {
            "status": "pending",
            "last_error": "Recovered stale Confluence sync after restart",
        },
    )
    assert events[2][0] == "create-task"
    assert events[3] == ("binding:binding-a", {"status": "syncing"})


@pytest.mark.asyncio
async def test_fresh_sync_task_is_not_superseded() -> None:
    class Database:
        async def get_confluence_binding(self, _binding_id: str) -> dict[str, Any]:
            return {
                "binding_id": "binding-a",
                "dataset_id": "dataset-a",
                "status": "syncing",
            }

        async def get_dataset(self, _dataset_id: str) -> dict[str, Any]:
            return {"dataset_id": "dataset-a", "tenant_id": "tenant-a", "index_config": {}}

        async def list_confluence_sync_tasks(
            self,
            *,
            status: str,
            **_kwargs: Any,
        ) -> list[dict[str, Any]]:
            if status == "processing":
                return [
                    {
                        "task_id": "fresh-task",
                        "status": status,
                        "updated_at": datetime.now(timezone.utc),
                    }
                ]
            return []

    service = ConfluenceSyncService(
        settings=_settings(stale_seconds=60),
        database=Database(),
        knowledge_service=SimpleNamespace(),
    )
    with pytest.raises(ConfluenceSyncError, match="already in progress"):
        await service.incremental_sync("binding-a")
