from __future__ import annotations

import asyncio
import contextlib
import copy
import hashlib
import json
import tempfile
import uuid
from dataclasses import dataclass
from pathlib import Path
from typing import TYPE_CHECKING, Any

from ...core.exceptions import ValidationFailedError
from ...core.observability.logging import get_logger
from ...persistence.database import (
    CONFLUENCE_SYNC_GENERATION_KEY,
    DOCUMENT_INGEST_ACTION_KEY,
    DOCUMENT_LIFECYCLE_REINDEX_KEY,
    DOCUMENT_PENDING_RESTORE_VERSION_KEY,
    DOCUMENT_PIPELINE_EXECUTION_KEY,
    DOCUMENT_RECOVER_STAGE_KEY,
    DOCUMENT_UPLOAD_GENERATION_KEY,
    INGEST_ACTION_VOCABULARY,
    IndexLeaseUnavailableError,
    dataset_index_deletion_fence,
    dataset_ingestion_identity,
)
from ...persistence.document_batches import (
    ClaimedDocumentBatchItem,
    DocumentBatchStore,
)
from ..gateway_actor import resolve_runtime_actor
from .chunking import validate_persisted_chunking_config
from .common import SPECIALIZED_REBUILD_UNAVAILABLE
from .extracted_text_budget import (
    _require_extracted_text_budget,
    _require_extracted_text_counts_budget,
)
from .lexical_config import LexicalConfig
from .lifecycle_errors import Bm25V2LifecycleError
from .processing_mode import ProcessingMode, parse_processing_mode
from .streaming_loader import StreamingDocumentLoader

if TYPE_CHECKING:
    from .document_detector import DocumentTypeDetector
    from .hierarchical_indexer import HierarchicalIndexer
    from .knowledge_service import KnowledgeService
    from .vision_pdf_processor import VisionPDFProcessor
    from .vlm_ocr_service import VLMOCRService


logger = get_logger(__name__)

# Default large file threshold (50MB); overridden from settings in __init__
DEFAULT_LARGE_FILE_THRESHOLD = 50 * 1024 * 1024
REPLAY_SNAPSHOT_ACTIONS = frozenset({"reprocess", "recover", "retry"})
CHUNKING_VALIDATION_EXEMPT_ACTIONS = REPLAY_SNAPSHOT_ACTIONS | {"reembed"}
PROCESSING_MODE_VALUES = frozenset(mode.value for mode in ProcessingMode)


def _special_generation_id(execution_id: str) -> str:
    """Use one UUID spelling for the durable manifest and candidate planner."""

    try:
        return str(uuid.UUID(str(execution_id)))
    except (TypeError, ValueError, AttributeError) as exc:
        raise RuntimeError("special publication execution ID is not a UUID") from exc


def _require_enqueue_bm25_v2_available(service: Any, index_config: dict[str, Any]) -> None:
    lexical = LexicalConfig.from_index_config(index_config)
    if lexical.reads_bm25_v2 and not service.vector_store.bm25_v2_enabled:
        raise Bm25V2LifecycleError(
            "bm25_v2 active writes are unavailable while the service kill switch is off",
            code="bm25_v2_disabled",
            http_status=503,
        )


def _extract_pdf_text_sync(content: bytes) -> str:
    """Sync PDF text extraction (SPO-04 / K2: runs inside asyncio.to_thread)."""
    try:
        import pymupdf as fitz  # type: ignore
    except ImportError:
        import fitz  # type: ignore
    doc = fitz.open(stream=content, filetype="pdf")
    text_parts = []
    text_chars = 0
    text_bytes = 0
    try:
        for page in doc:
            page_text = page.get_text() or ""
            separator_size = 2 if text_parts else 0
            text_chars += separator_size + len(page_text)
            text_bytes += separator_size + len(page_text.encode("utf-8"))
            _require_extracted_text_counts_budget(text_chars, text_bytes)
            text_parts.append(page_text)
    finally:
        doc.close()
    return "\n\n".join(text_parts)


def _extract_docx_text_sync(content: bytes) -> str:
    """Sync DOCX text extraction (SPO-04 / K2: runs inside asyncio.to_thread)."""
    from io import BytesIO

    import docx

    document = docx.Document(BytesIO(content))
    text_parts = []
    text_chars = 0
    text_bytes = 0
    for paragraph in document.paragraphs:
        paragraph_text = paragraph.text
        if not paragraph_text.strip():
            continue
        separator_size = 2 if text_parts else 0
        text_chars += separator_size + len(paragraph_text)
        text_bytes += separator_size + len(paragraph_text.encode("utf-8"))
        _require_extracted_text_counts_budget(text_chars, text_bytes)
        text_parts.append(paragraph_text)
    return "\n\n".join(text_parts)


def _pdf_page_count_sync(pdf_bytes: bytes) -> int:
    """Open, count, and close a PDF on one worker thread."""
    import fitz

    pdf_doc = fitz.open(stream=pdf_bytes, filetype="pdf")
    try:
        return len(pdf_doc)
    finally:
        pdf_doc.close()


def _render_pdf_pages_sync(pdf_bytes: bytes, batch_start: int, batch_end: int) -> list[bytes]:
    """Sync page rasterization for VLM OCR (SPO-04 / K2)."""
    import fitz

    from .document_processor import MAX_PDF_PAGE_MARKERS

    max_pixmap_bytes = 8 * 1024 * 1024
    pdf_doc = fitz.open(stream=pdf_bytes, filetype="pdf")
    images: list[bytes] = []
    try:
        if len(pdf_doc) > MAX_PDF_PAGE_MARKERS:
            raise ValueError(
                f"PDF exceeds the {MAX_PDF_PAGE_MARKERS} page marker limit"
            )
        for pn in range(batch_start, min(batch_end, MAX_PDF_PAGE_MARKERS)):
            page = pdf_doc[pn]
            dpi = 200.0
            pix = page.get_pixmap(matrix=fitz.Matrix(dpi / 72, dpi / 72))
            raw = pix.width * pix.height * pix.n
            if raw > max_pixmap_bytes and raw > 0:
                scale = (max_pixmap_bytes / raw) ** 0.5
                dpi = max(36.0, dpi * scale)
                pix = page.get_pixmap(matrix=fitz.Matrix(dpi / 72, dpi / 72))
            images.append(pix.tobytes("png"))
    finally:
        pdf_doc.close()
    return images


@dataclass(frozen=True)
class KnowledgeIngestTask:
    dataset_id: str
    document_id: str


@dataclass(frozen=True)
class ReplayConfigSnapshot:
    """Validated immutable configuration for one replay generation."""

    index_config: dict[str, Any]
    processing_mode: str
    restore_source_version: int | None = None
    restore_source_kind: str | None = None
    restore_source_hash: str | None = None

    @property
    def chunking(self) -> dict[str, Any]:
        value = self.index_config.get("chunking", {})
        if not isinstance(value, dict):
            raise RuntimeError("validated replay snapshot lost its chunking config")
        return copy.deepcopy(value)


class _KnowledgeReplayAdmission:
    """Shared read-only replay admission for API producer and worker roles."""

    service: KnowledgeService

    @staticmethod
    def _document_metadata(document: dict[str, Any]) -> dict[str, Any]:
        metadata = document.get("metadata")
        if metadata is None:
            return {}
        if not isinstance(metadata, dict):
            raise RuntimeError("document metadata is malformed")
        return dict(metadata)

    @staticmethod
    def _has_image_generation_receipt(metadata: dict[str, Any]) -> bool:
        return any(
            bool(metadata.get(key))
            for key in (
                "extracted_images", "image_count", "images_embedded",
                "embedded_image_count",
            )
        )

    @staticmethod
    def _hierarchical_opted_in(index_config: Any) -> bool:
        """PRD T9-1 grayscale gate: hierarchical indexing is explicit opt-in.

        With the indexer finally wired into the default worker, dispatch must
        not flip every ordinary text document onto the hierarchical path —
        only datasets whose stored chunking mode is "hierarchical" take it.
        AUTOMATIC and every malformed shape stay on the standard path.
        """

        if not isinstance(index_config, dict):
            return False
        chunking = index_config.get("chunking")
        if not isinstance(chunking, dict):
            return False
        return str(chunking.get("mode") or "").strip().lower() == "hierarchical"

    async def _require_special_rebuild_without_serving_version(
        self,
        task: KnowledgeIngestTask,
        document: dict[str, Any],
        *,
        pathway: str,
    ) -> None:
        """Keep existing multi-collection versions safe until publication is atomic."""

        metadata = self._document_metadata(document)
        action = str(metadata.get(DOCUMENT_INGEST_ACTION_KEY) or "ingest").strip().lower()
        if action not in REPLAY_SNAPSHOT_ACTIONS:
            return
        dataset = await self.service.db.get_dataset(task.dataset_id)
        tenant_id = str((dataset or {}).get("tenant_id") or "").strip()
        has_segments = getattr(self.service.db, "document_has_completed_segments", None)
        has_points = getattr(self.service.vector_store, "document_has_points", None)
        if not tenant_id or not callable(has_segments) or not callable(has_points):
            raise RuntimeError("specialized document serving authority is unavailable")
        if await has_segments(task.dataset_id, tenant_id, task.document_id) or await has_points(
            tenant_id=tenant_id,
            dataset_id=task.dataset_id,
            document_id=task.document_id,
        ):
            logger.warning(
                "Refused %s replay with existing or partial points before multi-collection publication",
                pathway,
                extra={"dataset_id": task.dataset_id, "document_id": task.document_id},
            )
            raise ValidationFailedError(SPECIALIZED_REBUILD_UNAVAILABLE)

    async def require_safe_special_replay_admission(
        self,
        task: KnowledgeIngestTask,
        document: dict[str, Any],
        dataset: dict[str, Any],
        *,
        action: str,
    ) -> None:
        """Reject unsafe specialized replay before recording a new execution."""
        metadata = self._document_metadata(document)
        if DOCUMENT_PENDING_RESTORE_VERSION_KEY in metadata:
            return  # Version restore uses the standard text publication path.
        mode = str(metadata.get("processing_mode") or "text_only").strip().lower()
        index_config = dataset.get("index_config") or {}
        from .special_publication import SOURCE_MANIFEST_KEY

        source_receipt = metadata.get(SOURCE_MANIFEST_KEY)
        if (
            isinstance(source_receipt, dict)
            and source_receipt.get("source_kind") == "text"
            and not self._hierarchical_opted_in(index_config)
        ):
            # An ordinary replay would leave the pinned source receipt and
            # current version behind while replacing the restored text.
            raise ValidationFailedError(SPECIALIZED_REBUILD_UNAVAILABLE)
        if (
            mode == "scanned"
            and bool(metadata.get("original_file_key"))
            and (
                isinstance(self, DurableEnqueueProxy)
                or callable(getattr(
                    getattr(self, "vision_processor", None), "prepare_document", None,
                ))
            )
        ):
            return
        if (
            mode == "text_only"
            and self._hierarchical_opted_in(index_config)
            and bool(metadata.get("original_file_key"))
            and not self._has_image_generation_receipt(metadata)
            and (
                isinstance(self, DurableEnqueueProxy)
                or callable(getattr(
                    getattr(self, "hierarchical_indexer", None), "prepare_document", None,
                ))
            )
        ):
            # The hierarchical candidate publisher preserves every old
            # collection until a single fenced generation switch succeeds.
            return
        # A missing processor or original upload must not turn old image/L2/L3
        # points into a standard-text replay. Use persisted mode/configuration.
        specialized = mode == "scanned" or self._has_image_generation_receipt(metadata) or (
            mode not in {"multimodal", "scanned"}
            and self._hierarchical_opted_in(index_config)
        ) or (mode == "auto" and bool(metadata.get("original_file_key")))
        if not specialized:
            tenant_id = str(dataset.get("tenant_id") or "").strip()
            has_segments = getattr(self.service.db, "document_has_specialized_segments", None)
            has_points = getattr(self.service.vector_store, "document_has_specialized_points", None)
            if not tenant_id or not callable(has_segments) or not callable(has_points):
                raise RuntimeError("specialized replay authority is unavailable")
            specialized = await has_segments(task.dataset_id, tenant_id, task.document_id)
            if not specialized:
                specialized = await has_points(
                    tenant_id=tenant_id,
                    dataset_id=task.dataset_id,
                    document_id=task.document_id,
                )
        if not specialized:
            return
        replay_document = dict(document)
        replay_document["metadata"] = {**metadata, DOCUMENT_INGEST_ACTION_KEY: action}
        await self._require_special_rebuild_without_serving_version(
            task, replay_document, pathway="specialized admission",
        )

    async def require_safe_restore_admission(
        self,
        task: KnowledgeIngestTask,
        dataset: dict[str, Any],
        *,
        connection: Any | None = None,
    ) -> None:
        """Keep a standard-text restore from stranding older special points."""
        document = await self.service.db.get_document(
            task.document_id, connection=connection,
        )
        if document:
            from .special_publication import SOURCE_MANIFEST_KEY

            current_metadata = self._document_metadata(document)
            if (
                isinstance(current_metadata.get(SOURCE_MANIFEST_KEY), dict)
                or self._has_image_generation_receipt(current_metadata)
            ):
                # A legacy target without a pinned source receipt cannot
                # safely replace a serving special/text-restored generation.
                raise ValidationFailedError(SPECIALIZED_REBUILD_UNAVAILABLE)
        tenant_id = str(dataset.get("tenant_id") or "").strip()
        has_segments = getattr(self.service.db, "document_has_specialized_segments", None)
        has_points = getattr(self.service.vector_store, "document_has_specialized_points", None)
        if not tenant_id or not callable(has_segments) or not callable(has_points):
            raise RuntimeError("specialized restore authority is unavailable")
        if await has_segments(
            task.dataset_id, tenant_id, task.document_id, connection=connection,
        ) or await has_points(
            tenant_id=tenant_id,
            dataset_id=task.dataset_id,
            document_id=task.document_id,
        ):
            raise ValidationFailedError(SPECIALIZED_REBUILD_UNAVAILABLE)



class DurableEnqueueProxy(_KnowledgeReplayAdmission):
    """API-role producer that publishes only the durable PostgreSQL generation."""

    def __init__(self, service: KnowledgeService) -> None:
        self.service = service
        self.queue: asyncio.Queue[KnowledgeIngestTask] = asyncio.Queue()
        self._running = False
        self._workers: list[asyncio.Task] = []

    async def enqueue(
        self,
        dataset_id: str,
        document_id: str,
        *,
        action: str | None = None,
        recover_stage: str | None = None,
        execution_id: str | None = None,
    ) -> bool:
        dataset = await self.service.db.get_dataset(dataset_id)
        if not dataset:
            return False
        index_config = dataset.get("index_config") or {}
        if not isinstance(index_config, dict):
            raise RuntimeError("dataset index_config is invalid")
        _require_enqueue_bm25_v2_available(self.service, index_config)
        # Replay verbs were validated and pinned before this claim. Re-reading
        # the live chunking config here would reintroduce submission/claim
        # drift. reembed never parses; only the default ingest uses live config.
        if action not in CHUNKING_VALIDATION_EXEMPT_ACTIONS:
            validate_persisted_chunking_config(index_config.get("chunking", {}))
        claim = getattr(self.service.db, "claim_document_for_enqueue", None)
        if not callable(claim):
            raise RuntimeError("durable document enqueue is unavailable")
        return bool(
            await claim(
                dataset_id,
                document_id,
                action=action,
                recover_stage=recover_stage,
                execution_id=execution_id,
                pin_execution_rule=action in REPLAY_SNAPSHOT_ACTIONS,
            )
        )

    async def enqueue_claimed(self, dataset_id: str, document_id: str) -> None:
        _ = (dataset_id, document_id)
        # The caller already committed a queued row. The worker-role process
        # discovers it through its bounded durable dispatcher.


class KnowledgeWorker(_KnowledgeReplayAdmission):
    """
    Knowledge base document ingestion worker.

    Processes documents with:
    - Intelligent type detection (auto mode)
    - Streaming processing for large files (>50MB)
    - Hierarchical indexing (L1/L2/L3)
    - Multiple processing modes (text/scanned/multimodal)
    """

    def __init__(
        self,
        service: KnowledgeService,
        vision_processor: VisionPDFProcessor | None = None,
        detector: DocumentTypeDetector | None = None,
        hierarchical_indexer: HierarchicalIndexer | None = None,
        vlm_ocr_service: VLMOCRService | None = None,
    ):
        self.service = service
        self.vision_processor = vision_processor
        self.detector = detector
        self.hierarchical_indexer = hierarchical_indexer
        self.vlm_ocr_service = vlm_ocr_service
        self.queue: asyncio.Queue[KnowledgeIngestTask] = asyncio.Queue()
        self._workers: list[asyncio.Task] = []
        self._recovery_task: asyncio.Task | None = None
        self._durable_dispatch_task: asyncio.Task | None = None
        self._document_batch_task: asyncio.Task | None = None
        self._document_batch_store: DocumentBatchStore | None = None
        self._document_batch_worker_id = f"knowledge-worker-{id(self):x}"
        self._scheduled_tasks: set[tuple[str, str]] = set()
        self._durable_tenant_cursor: str | None = None
        self._running = False
        knowledge_settings = getattr(self.service.settings, "knowledge", None)
        self.large_file_threshold = getattr(
            knowledge_settings,
            "large_file_threshold",
            DEFAULT_LARGE_FILE_THRESHOLD,
        )
        self._pdf_split_enabled = getattr(knowledge_settings, "pdf_split_enabled", True)
        self._pdf_split_max_size = getattr(
            knowledge_settings, "pdf_split_max_size_bytes", 20 * 1024 * 1024
        )
        self._pdf_split_min_pages = getattr(knowledge_settings, "pdf_split_min_pages_per_part", 5)
        self._ocr_strategy = getattr(knowledge_settings, "ocr_strategy", "hybrid")
        self._recovery_interval_seconds = max(
            float(
                getattr(
                    knowledge_settings,
                    "document_recovery_interval_seconds",
                    60.0,
                )
            ),
            1.0,
        )
        self._recovery_threshold_minutes = max(
            int(
                getattr(
                    knowledge_settings,
                    "document_stuck_threshold_minutes",
                    15,
                )
            ),
            1,
        )
        self._durable_poll_interval_seconds = max(
            float(
                getattr(
                    knowledge_settings,
                    "durable_queue_poll_interval_seconds",
                    1.0,
                )
            ),
            0.1,
        )
        self._shutdown_drain_timeout_seconds = max(
            float(
                getattr(
                    knowledge_settings,
                    "worker_shutdown_drain_timeout_seconds",
                    10.0,
                )
            ),
            0.1,
        )

        # allow KnowledgeService.enqueue_ingest() convenience
        self.service._worker = self

    async def start(self, concurrency: int | None = None) -> None:
        """Start worker with configurable concurrency.

        Args:
            concurrency: Number of parallel workers. If None, uses
                service.settings.knowledge.document_worker_concurrency (default: 3)
        """
        if self._running:
            return

        # Use settings-based concurrency if not explicitly provided
        if concurrency is None:
            # Get settings from the KnowledgeService instance
            knowledge_settings = getattr(self.service.settings, "knowledge", None)
            concurrency = (
                getattr(knowledge_settings, "document_worker_concurrency", 3)
                if knowledge_settings
                else 3
            )

        num_workers = max(int(concurrency), 1)
        pool_capacity = getattr(self.service.db, "connection_pool_max_size", None)
        if not callable(pool_capacity):
            raise RuntimeError(
                "knowledge worker requires a verifiable database pool capacity"
            )
        # Each special worker can hold its document lease and a distinct
        # publication connection while querying lifecycle/ledger authority on
        # a third connection. Reserve capacity for that progress and one
        # concurrent recovery pass instead of allowing a pool wait cycle.
        required_capacity = 2 * num_workers + 3
        actual_capacity = int(pool_capacity())
        if actual_capacity < required_capacity:
            raise RuntimeError(
                "knowledge worker database pool is too small for document leases: "
                f"requires at least {required_capacity}, found {actual_capacity}"
            )
        await self._recover_unfinished_special_publications()
        self._running = True
        logger.info(f"Starting KnowledgeWorker with {num_workers} parallel workers")

        for _ in range(num_workers):
            self._workers.append(asyncio.create_task(self._run()))
        self._recovery_task = asyncio.create_task(self._recovery_loop())
        self._durable_dispatch_task = asyncio.create_task(
            self._durable_dispatch_loop()
        )
        pool = getattr(self.service.db, "_pool", None)
        if pool is not None:
            self._document_batch_store = DocumentBatchStore(pool)
            self._document_batch_task = asyncio.create_task(
                self._document_batch_dispatch_loop()
            )

    async def stop(self) -> None:
        # Stop discovery first so the bounded drain has a stable queue tail.
        if self._durable_dispatch_task is not None:
            self._durable_dispatch_task.cancel()
            await asyncio.gather(
                self._durable_dispatch_task,
                return_exceptions=True,
            )
            self._durable_dispatch_task = None
        if self._document_batch_task is not None:
            self._document_batch_task.cancel()
            await asyncio.gather(self._document_batch_task, return_exceptions=True)
            self._document_batch_task = None

        try:
            await asyncio.wait_for(
                self.queue.join(),
                timeout=self._shutdown_drain_timeout_seconds,
            )
        except asyncio.TimeoutError:
            logger.warning(
                "Knowledge worker drain timed out; cancelling owned generations",
                extra={"queue_size": self.queue.qsize()},
            )

        self._running = False
        tasks = [*self._workers]
        if self._recovery_task is not None:
            tasks.append(self._recovery_task)
        for t in tasks:
            t.cancel()
        if tasks:
            await asyncio.gather(*tasks, return_exceptions=True)
        self._workers = []
        self._recovery_task = None
        self._document_batch_store = None
        self._scheduled_tasks.clear()
        self._durable_tenant_cursor = None

    async def _durable_dispatch_loop(self) -> None:
        list_queued = getattr(self.service.db, "list_queued_documents", None)
        if not callable(list_queued):
            raise RuntimeError("durable queued-document discovery is unavailable")
        while self._running:
            try:
                rows = await list_queued(
                    limit=100,
                    tenant_cursor=self._durable_tenant_cursor,
                )
                for row in rows:
                    tenant_id = str(row.get("tenant_id") or "").strip()
                    if tenant_id:
                        # The next poll starts after the last tenant observed,
                        # including rows already present in the local queue.
                        # This prevents a short fetch window from repeatedly
                        # favoring the same lexical tenant ordering.
                        self._durable_tenant_cursor = tenant_id
                    dataset_id = str(row.get("dataset_id") or "").strip()
                    document_id = str(row.get("document_id") or "").strip()
                    key = (dataset_id, document_id)
                    if not all(key) or key in self._scheduled_tasks:
                        continue
                    self._scheduled_tasks.add(key)
                    await self.queue.put(
                        KnowledgeIngestTask(
                            dataset_id=dataset_id,
                            document_id=document_id,
                        )
                    )
            except asyncio.CancelledError:
                return
            except Exception:
                logger.exception("Knowledge durable queue discovery failed")
            try:
                await asyncio.sleep(self._durable_poll_interval_seconds)
            except asyncio.CancelledError:
                return

    async def _document_batch_dispatch_loop(self) -> None:
        store = self._document_batch_store
        if store is None:
            return
        while self._running:
            try:
                item = await store.claim_next_item(
                    worker_id=self._document_batch_worker_id,
                )
                if item is None:
                    await asyncio.sleep(self._durable_poll_interval_seconds)
                    continue
                await self._dispatch_document_batch_item(store, item)
            except asyncio.CancelledError:
                return
            except Exception:
                logger.exception("Knowledge document batch dispatch failed")
                await asyncio.sleep(self._durable_poll_interval_seconds)

    async def _dispatch_document_batch_item(
        self,
        store: DocumentBatchStore,
        item: ClaimedDocumentBatchItem,
    ) -> None:
        try:
            if item.operation in {"reembed", "delete"}:
                # A durable batch can outlive the submitting account or role.
                # Stored actor_roles are provenance, never current authority.
                actor = await resolve_runtime_actor(
                    tenant_id=item.tenant_id, user_id=item.created_by,
                )
                if (
                    not actor.is_authenticated
                    or actor.user_id != item.created_by
                    or actor.tenant_id != item.tenant_id
                ):
                    raise ValidationFailedError("document batch actor authority changed")
                dataset = await self.service.require_dataset_access(
                    actor, item.dataset_id, required="editor",
                )
                if dataset.get("tenant_id") != item.tenant_id:
                    raise ValidationFailedError("document batch dataset authority changed")
            if item.operation == "reembed":
                queued = await self.enqueue(
                    item.dataset_id,
                    item.document_id,
                    action="reembed",
                )
                await store.complete_item(
                    operation_id=item.operation_id,
                    document_id=item.document_id,
                    status="queued" if queued else "skipped",
                    error_code=None if queued else "already_queued_or_ineligible",
                )
                return

            if item.operation == "delete":
                deleted = await self.service.delete_document(
                    actor,
                    item.dataset_id,
                    item.document_id,
                )
                await store.complete_item(
                    operation_id=item.operation_id,
                    document_id=item.document_id,
                    status="queued" if deleted else "skipped",
                    error_code=None if deleted else "not_found",
                )
                return

            await store.complete_item(
                operation_id=item.operation_id,
                document_id=item.document_id,
                status="failed",
                error_code="unsupported_operation",
            )
        except IndexLeaseUnavailableError:
            await store.release_item(
                operation_id=item.operation_id,
                document_id=item.document_id,
            )
        except Exception as exc:
            logger.exception(
                "Knowledge document batch item failed",
                extra={
                    "operation_id": item.operation_id,
                    "dataset_id": item.dataset_id,
                    "document_id": item.document_id,
                },
            )
            await store.complete_item(
                operation_id=item.operation_id,
                document_id=item.document_id,
                status="failed",
                error_code="operation_failed",
                error=str(exc),
            )

    async def _recovery_loop(self) -> None:
        """Periodically replay atomically claimed durable ingestion rows."""

        while self._running:
            try:
                await self._recover_unfinished_special_publications()
                await self.service.recover_stuck_documents(
                    self._recovery_threshold_minutes,
                    worker=self,
                )
            except asyncio.CancelledError:
                return
            except Exception:
                logger.exception("Knowledge document recovery pass failed")
            try:
                await asyncio.sleep(self._recovery_interval_seconds)
            except asyncio.CancelledError:
                return

    async def _recover_unfinished_special_publications(self) -> None:
        """Resolve durable negative special generations before generic claims."""

        from .special_publication import SpecialPublicationCoordinator

        coordinator = SpecialPublicationCoordinator(self.service)
        list_preparations = getattr(self.service.db, "list_unbound_special_preparations", None)
        lease_factory = getattr(self.service.db, "document_index_update_lease", None)
        if not callable(list_preparations) or not callable(lease_factory):
            raise RuntimeError("special preparation recovery catalog is unavailable")
        for item in await list_preparations():
            dataset_id = item["dataset_id"]
            document_id = item["document_id"]
            resume = False
            try:
                async with lease_factory(dataset_id, document_id) as connection:
                    owner = await self.service.db.get_special_publication_manifest(
                        item["execution_id"], connection=connection,
                    )
                    if (
                        not owner or owner.get("phase") != "preparing"
                        or owner.get("publication_revision") is not None
                        or owner.get("generation_id") != item["generation_id"]
                    ):
                        continue
                    dataset = await self.service.db.get_dataset(
                        dataset_id, connection=connection,
                    )
                    if not isinstance(dataset, dict):
                        raise RuntimeError("special preparation dataset disappeared")
                    await coordinator.abort_preparing(
                        dataset, item["execution_id"], item["generation_id"],
                        item["source_hash"], document_shared_lease_held=True,
                        resume_same_execution=True,
                    )
                    resume = True
                if resume:
                    await self.enqueue_claimed(dataset_id, document_id)
            except Exception:
                logger.exception(
                    "Unbound special preparation recovery failed",
                    extra={"dataset_id": dataset_id, "document_id": document_id},
                )
        list_datasets = getattr(
            self.service.db, "list_unfinished_special_publication_datasets", None,
        )
        if not callable(list_datasets):
            raise RuntimeError("special publication recovery catalog is unavailable")
        datasets = await list_datasets()
        if not datasets:
            return
        for dataset in datasets:
            try:
                owner = await self.service.db.get_active_special_publication_for_dataset(
                    str(dataset["dataset_id"]),
                )
                await coordinator.recover_unfinished(dataset)
                if owner.get("phase") != "committed":
                    if owner.get("phase") == "preparing":
                        await self.enqueue_claimed(
                            str(owner["dataset_id"]), str(owner["document_id"]),
                        )
                    else:
                        await self.enqueue(
                            str(owner["dataset_id"]), str(owner["document_id"]),
                            action="recover", recover_stage="indexing",
                        )
            except Exception:
                logger.exception(
                    "Special publication recovery remains fenced",
                    extra={"dataset_id": dataset.get("dataset_id")},
                )

    async def enqueue(
        self,
        dataset_id: str,
        document_id: str,
        *,
        action: str | None = None,
        recover_stage: str | None = None,
        execution_id: str | None = None,
    ) -> bool:
        """Durably claim one generation before publishing it to local memory."""

        dataset = await self.service.db.get_dataset(dataset_id)
        if not dataset:
            return False
        index_config = dataset.get("index_config") or {}
        if not isinstance(index_config, dict):
            raise RuntimeError("dataset index_config is invalid")
        _require_enqueue_bm25_v2_available(self.service, index_config)
        # Replay verbs were validated and pinned before this claim. Re-reading
        # the live chunking config here would reintroduce submission/claim
        # drift. reembed never parses; only the default ingest uses live config.
        if action not in CHUNKING_VALIDATION_EXEMPT_ACTIONS:
            validate_persisted_chunking_config(index_config.get("chunking", {}))
        claim = getattr(self.service.db, "claim_document_for_enqueue", None)
        if not callable(claim):
            raise RuntimeError("durable document enqueue is unavailable")
        if not await claim(
            dataset_id,
            document_id,
            action=action,
            recover_stage=recover_stage,
            execution_id=execution_id,
            pin_execution_rule=action in REPLAY_SNAPSHOT_ACTIONS,
        ):
            logger.info(
                "Skipped duplicate/ineligible document enqueue",
                extra={"dataset_id": dataset_id, "document_id": document_id},
            )
            return False
        await self.enqueue_claimed(dataset_id, document_id)
        return True

    async def enqueue_claimed(self, dataset_id: str, document_id: str) -> None:
        """Publish a generation already claimed by the DB recovery CTE."""

        await self.queue.put(KnowledgeIngestTask(dataset_id=dataset_id, document_id=document_id))
        logger.info(
            f"Enqueued document {document_id} for ingestion (dataset={dataset_id}), queue size ~{self.queue.qsize()}"
        )

    async def _run(self) -> None:
        while self._running:
            try:
                task = await self.queue.get()
            except asyncio.CancelledError:
                return
            try:
                lease_factory = getattr(
                    self.service.db,
                    "document_index_update_lease",
                    None,
                )
                claim = getattr(
                    self.service.db,
                    "claim_queued_document_for_processing",
                    None,
                )
                if not callable(lease_factory) or not callable(claim):
                    raise RuntimeError("durable document consumer lease is unavailable")
                async with lease_factory(
                    task.dataset_id,
                    task.document_id,
                ) as lease_connection:
                    queued_dataset = await self.service.db.get_dataset(
                        task.dataset_id,
                        connection=lease_connection,
                    )
                    if not queued_dataset:
                        raise RuntimeError("dataset was deleted before consumer claim")
                    queued_index_config = queued_dataset.get("index_config") or {}
                    if not isinstance(queued_index_config, dict):
                        raise RuntimeError("dataset index_config is invalid")
                    try:
                        _require_enqueue_bm25_v2_available(
                            self.service,
                            queued_index_config,
                        )
                    except Bm25V2LifecycleError:
                        logger.warning(
                            "Deferred active BM25 v2 queue claim while the kill switch is off",
                            extra={
                                "dataset_id": task.dataset_id,
                                "document_id": task.document_id,
                            },
                        )
                        continue
                    if not await claim(
                        task.dataset_id,
                        task.document_id,
                        connection=lease_connection,
                    ):
                        logger.info(
                            "Skipped stale or duplicate in-memory ingestion task",
                            extra={
                                "dataset_id": task.dataset_id,
                                "document_id": task.document_id,
                            },
                        )
                        continue
                    generation_prepared = False
                    execution_id = ""
                    # PRD T9-1 failure attribution: the dispatched path records
                    # swallowed stage failures (hierarchical attempt -> silent
                    # standard fallback) here so the execution receipt can
                    # distinguish them from a plain successful generation.
                    stage_receipt: list[dict[str, str]] = []
                    try:
                        # PRD T1 items 3/4: the queued row pins its verb and its
                        # replay-snapshot execution; both survive requeue because
                        # they live in document metadata, not worker memory.
                        queued_doc = await self.service.db.get_document(
                            task.document_id,
                            connection=lease_connection,
                        )
                        queued_meta = (
                            self._document_metadata(queued_doc) if queued_doc else {}
                        )
                        ingest_action = (
                            str(
                                queued_meta.get(DOCUMENT_INGEST_ACTION_KEY) or "ingest"
                            )
                            .strip()
                            .lower()
                        )
                        if ingest_action not in INGEST_ACTION_VOCABULARY:
                            ingest_action = "ingest"
                        execution_id = await self._ensure_pipeline_execution(
                            task,
                            ingest_action,
                            str(
                                queued_meta.get(DOCUMENT_PIPELINE_EXECUTION_KEY) or ""
                            ).strip(),
                            connection=lease_connection,
                        )
                        await self._prepare_document_generation(
                            task,
                            connection=lease_connection,
                        )
                        generation_prepared = True
                        manifest = await self._process_task(
                            task,
                            connection=lease_connection,
                            stage_receipt=stage_receipt,
                        )
                        current = await self.service.db.get_document(
                            task.document_id,
                            connection=lease_connection,
                        )
                        if not current or str(current.get("status") or "") != "completed":
                            reason = str((current or {}).get("error") or "").strip()
                            raise RuntimeError(reason[:1_000] or "document processor returned without a completed generation")
                        await self._finish_pipeline_execution(
                            execution_id,
                            status="completed",
                            manifest=manifest,
                            stage_receipt=stage_receipt,
                        )
                    except asyncio.CancelledError:
                        dataset = await self.service.db.get_dataset(
                            task.dataset_id, connection=lease_connection,
                        )
                        if dataset and int(dataset.get("content_revision") or 0) < 0:
                            current = await self.service.db.get_document(
                                task.document_id, connection=lease_connection,
                            )
                            if self._document_metadata(current or {}).get(
                                "_special_publication_generation_id"
                            ):
                                # The durable owner reconciles this negative
                                # publication after restart; requeueing here
                                # would rewrite a possibly committed document.
                                raise
                        requeue = getattr(
                            self.service.db,
                            "requeue_cancelled_document_generation",
                            None,
                        )
                        if callable(requeue):
                            requeue_task = asyncio.create_task(
                                requeue(
                                    task.dataset_id,
                                    task.document_id,
                                    connection=lease_connection,
                                )
                            )
                            try:
                                await asyncio.shield(requeue_task)
                            except asyncio.CancelledError:
                                await requeue_task
                        raise
                    except IndexLeaseUnavailableError:
                        # The generation was already claimed, but a later
                        # publication fence (for example embedding backfill)
                        # won the race. Preserve the running execution receipt
                        # and staged rows, then return the document to the
                        # durable queue for a contention-free retry.
                        requeue = getattr(
                            self.service.db,
                            "requeue_cancelled_document_generation",
                            None,
                        )
                        if not callable(requeue) or not await requeue(
                            task.dataset_id,
                            task.document_id,
                            connection=lease_connection,
                        ):
                            raise RuntimeError(
                                "retryable index contention could not requeue the "
                                "claimed document generation"
                            )
                        logger.info(
                            "Requeued ingestion after a concurrent index transition",
                            extra={
                                "dataset_id": task.dataset_id,
                                "document_id": task.document_id,
                            },
                        )
                    except Exception as exc:
                        # The document owner lease is still held here. Only a
                        # successfully claimed generation may write its failed
                        # terminal; lease contention and duplicate local tasks
                        # never enter this branch.
                        logger.exception(
                            "KB ingest task failed",
                            extra={
                                "dataset_id": task.dataset_id,
                                "document_id": task.document_id,
                            },
                        )
                        dataset = await self.service.db.get_dataset(
                            task.dataset_id,
                            connection=lease_connection,
                        )
                        if dataset and int(dataset.get("content_revision") or 0) < 0:
                            current = await self.service.db.get_document(
                                task.document_id, connection=lease_connection,
                            )
                            current_metadata = self._document_metadata(current or {})
                            if current_metadata.get("_special_publication_generation_id"):
                                # A committed generation may already be the
                                # serving PG authority; a cleanup failure must
                                # keep its running manifest and completed doc
                                # behind the negative fence for deterministic
                                # recovery. Generic error closing destroys both.
                                logger.warning(
                                    "Special generation remains fenced for recovery",
                                    extra={
                                        "dataset_id": task.dataset_id,
                                        "document_id": task.document_id,
                                        "execution_id": execution_id,
                                    },
                                )
                                continue
                        await self._finish_pipeline_execution(
                            execution_id,
                            status="error",
                            error=str(exc),
                            stage_receipt=stage_receipt,
                        )
                        deletion_fence = (
                            dataset_index_deletion_fence(dataset) if dataset else None
                        )
                        LexicalConfig.from_index_config(
                            (dataset or {}).get("index_config") or {}
                        )
                        if dataset and deletion_fence is None:
                            # T1: NO failure sweep. A partial generation leaves
                            # its staged rows disabled (status='indexing') and
                            # the previous serving generation intact; staged
                            # rows carry deterministic lineage ids, so the next
                            # run resumes them (hash match -> flip, no
                            # re-embedding) instead of rebuilding from zero.
                            # Sweeping here would destroy the serving rows the
                            # staging contract exists to protect. Retry uses
                            # the same atomic incremental publication path.
                            if generation_prepared:
                                logger.info(
                                    "Partial generation kept staged for retry",
                                    extra={
                                        "dataset_id": task.dataset_id,
                                        "document_id": task.document_id,
                                    },
                                )
                            await self.service.db.update_document_status(
                                task.document_id,
                                status="error",
                                progress=100,
                                error=str(exc),
                                connection=lease_connection,
                            )
                        else:
                            logger.warning(
                                "Skipped failed terminal for a non-writable dataset",
                                extra={
                                    "dataset_id": task.dataset_id,
                                    "document_id": task.document_id,
                                },
                            )
            except IndexLeaseUnavailableError:
                # Keep the durable queued row untouched. The recovery loop will
                # publish it again after the lifecycle/deletion barrier clears.
                logger.info(
                    "Deferred ingestion while its dataset lifecycle lease is busy",
                    extra={
                        "dataset_id": task.dataset_id,
                        "document_id": task.document_id,
                    },
                )
            except asyncio.CancelledError:
                # Graceful shutdown — mark task as incomplete, not failed.
                logger.info("Worker cancelled during task processing")
                return
            except Exception:
                # Dispatch/lease failures before a successful claim must never
                # overwrite the durable queued state.
                logger.exception(
                    "KB ingest task could not acquire its durable generation",
                    extra={"dataset_id": task.dataset_id, "document_id": task.document_id},
                )
            finally:
                self._scheduled_tasks.discard((task.dataset_id, task.document_id))
                self.queue.task_done()

    @staticmethod
    def _validate_rebuildable_image_source(
        document: dict[str, Any],
        image_segments: list[dict[str, Any]],
    ) -> None:
        """Fail before index regeneration when image bytes cannot be rebuilt."""

        metadata = KnowledgeWorker._document_metadata(document)
        extracted = metadata.get("extracted_images", [])
        if not isinstance(extracted, list):
            raise RuntimeError("durable extracted_images receipt must be a list")
        valid_images = [
            item
            for item in extracted
            if isinstance(item, dict) and str(item.get("storage_url") or "").strip()
        ]
        if len(valid_images) != len(extracted):
            raise RuntimeError(
                "durable image receipt is incomplete; refusing index generation cleanup"
            )

        processing_mode = str(
            metadata.get("processing_mode") or "text_only"
        ).strip().lower()
        original_file_key = str(metadata.get("original_file_key") or "").strip()
        source_type = str(document.get("source_type") or "").strip().lower()
        confluence_source = metadata.get("_confluence_image_source_generation")
        confluence_complete = (
            isinstance(confluence_source, dict)
            and confluence_source.get("complete") is True
        )

        if source_type == "confluence" and image_segments and not confluence_complete:
            raise RuntimeError(
                "Confluence image rows lack a complete durable source generation"
            )

        legacy_embedded_count = int(metadata.get("embedded_image_count") or 0)
        declared_image_count = int(metadata.get("image_count") or 0)
        has_legacy_image_receipt = bool(metadata.get("images_embedded")) or (
            legacy_embedded_count > 0
        )
        target_is_explicitly_empty = (
            source_type == "confluence" and confluence_complete and not extracted
        )
        image_generation_exists = bool(
            image_segments
            or extracted
            or has_legacy_image_receipt
            or declared_image_count > 0
        )
        rebuildable = bool(original_file_key or valid_images or target_is_explicitly_empty)
        if image_generation_exists and not rebuildable:
            raise RuntimeError(
                "document image generation has no durable rebuild source"
            )
        if (
            image_generation_exists
            and not target_is_explicitly_empty
            and processing_mode not in {"multimodal", "scanned"}
        ):
            raise RuntimeError(
                "document image generation is not routed through an image-capable mode"
            )
        if (
            has_legacy_image_receipt
            and not original_file_key
            and not target_is_explicitly_empty
            and len(valid_images) < max(legacy_embedded_count, 1)
        ):
            raise RuntimeError(
                "legacy image receipt cannot be rebuilt completely"
            )

    async def _ensure_pipeline_execution(
        self,
        task: KnowledgeIngestTask,
        action: str,
        linked_execution_id: str,
        *,
        connection: Any,
    ) -> str:
        """Return the durable execution row for one claimed generation.

        A linked replay row is immutable authority: missing/closed rows are a
        hard failure, never a reason to rebuild a replacement from live
        configuration. Automatic recoveries without a link reuse the
        document's pinned process rule.
        """

        record = getattr(self.service.db, "record_pipeline_execution", None)
        get_execution = getattr(self.service.db, "get_pipeline_execution", None)
        link = getattr(self.service.db, "link_pipeline_execution", None)
        replay_action = action in REPLAY_SNAPSHOT_ACTIONS
        if not callable(record) or not callable(get_execution) or not callable(link):
            if replay_action:
                raise RuntimeError("pipeline execution persistence is unavailable")
            return linked_execution_id

        if linked_execution_id:
            existing = await get_execution(linked_execution_id, connection=connection)
            if existing and str(existing.get("status") or "") == "running":
                return linked_execution_id
            if replay_action:
                raise RuntimeError(
                    "linked replay execution is missing or no longer running"
                )

        document = await self.service.db.get_document(
            task.document_id, connection=connection
        )
        if not isinstance(document, dict):
            raise RuntimeError("document disappeared before execution snapshot")
        doc_meta = self._document_metadata(document)

        if action == "recover":
            replay_snapshot, process_rule_id = await self._load_pinned_process_rule(
                document,
                expected_dataset_id=task.dataset_id,
                connection=connection,
            )
            index_config = replay_snapshot.index_config
            processing_mode = replay_snapshot.processing_mode
        elif action in {"reprocess", "retry"}:
            raise RuntimeError(
                f"{action} generation is missing its submitted execution"
            )
        else:
            dataset = await self.service.db.get_dataset(
                task.dataset_id, connection=connection
            )
            raw_index_config = (dataset or {}).get("index_config") or {}
            if not isinstance(raw_index_config, dict):
                raise RuntimeError("dataset index_config is invalid")
            index_config = copy.deepcopy(raw_index_config)
            processing_mode = str(
                doc_meta.get("processing_mode") or "text_only"
            ).strip().lower()
            try:
                process_rule_id = await self._record_generation_process_rule(
                    task,
                    action=action,
                    index_config=index_config,
                    processing_mode=processing_mode,
                    connection=connection,
                )
            except Exception:
                # A specialized publisher can leave a durable preparing
                # manifest before any external write. Its crash replay needs
                # this exact rule; never begin that publisher without one.
                if processing_mode in {"auto", "multimodal", "scanned"} or self._hierarchical_opted_in(
                    index_config
                ):
                    raise RuntimeError(
                        "special generation requires a durable process-rule snapshot"
                    )
                logger.warning(
                    "Process-rule snapshot record failed; non-replay generation continues",
                    extra={
                        "dataset_id": task.dataset_id,
                        "document_id": task.document_id,
                    },
                    exc_info=True,
                )
                process_rule_id = None

        chunking = index_config.get("chunking", {})
        snapshot = {
            "index_config": copy.deepcopy(index_config),
            "chunking": copy.deepcopy(chunking) if isinstance(chunking, dict) else {},
            "processing_mode": processing_mode,
        }
        trigger_source = "recover" if action == "recover" else "worker"

        @contextlib.asynccontextmanager
        async def execution_transaction():
            begin = getattr(connection, "transaction", None)
            if callable(begin):
                async with begin():
                    yield
            else:
                yield

        try:
            async with execution_transaction():
                execution_id = str(await record(
                    task.document_id,
                    task.dataset_id,
                    action=action,
                    trigger_source=trigger_source,
                    process_rule_id=process_rule_id,
                    input_snapshot=snapshot,
                    connection=connection,
                ) or "").strip()
                if not execution_id:
                    raise RuntimeError("pipeline execution ledger returned no id")
                linked = await link(
                    task.document_id, execution_id, connection=connection,
                )
                if not linked:
                    raise RuntimeError("pipeline execution link was not persisted")
        except Exception:
            if replay_action:
                raise
            logger.warning(
                "Pipeline execution record/link failed; non-replay generation continues",
                extra={
                    "dataset_id": task.dataset_id,
                    "document_id": task.document_id,
                },
                exc_info=True,
            )
            return linked_execution_id
        return execution_id

    async def _record_generation_process_rule(
        self,
        task: KnowledgeIngestTask,
        *,
        action: str,
        index_config: dict[str, Any],
        processing_mode: str,
        connection: Any,
    ) -> str | None:
        """Record + pin the immutable rule snapshot for this generation.

        The canonical payload includes the complete index_config plus the
        compatibility chunking field and processing mode. reembed returns
        None because vector repair never executes a chunking dialect.
        """

        if action == "reembed":
            return None
        record_rule = getattr(self.service.db, "record_process_rule", None)
        pin_rule = getattr(self.service.db, "pin_document_process_rule", None)
        if not callable(record_rule) or not callable(pin_rule):
            raise RuntimeError("process-rule snapshot persistence is unavailable")
        chunking_config = index_config.get("chunking", {})
        if not isinstance(chunking_config, dict):
            raise RuntimeError("snapshot chunking config is invalid")
        mode = (
            str(chunking_config.get("mode") or "automatic").strip().lower()
            or "automatic"
        )
        rule_id = str(
            await record_rule(
                task.dataset_id,
                mode=mode,
                rules={
                    "index_config": copy.deepcopy(index_config),
                    "chunking": copy.deepcopy(chunking_config),
                    "processing_mode": processing_mode,
                },
                connection=connection,
            )
            or ""
        ).strip()
        if not rule_id:
            raise RuntimeError("process-rule snapshot returned no id")
        if not await pin_rule(task.document_id, rule_id, connection=connection):
            raise RuntimeError("process-rule document pin was not persisted")
        return rule_id

    async def _finish_pipeline_execution(
        self,
        execution_id: str,
        *,
        status: str,
        error: str | None = None,
        manifest: list[str] | None = None,
        stage_receipt: list[dict[str, str]] | None = None,
    ) -> None:
        """Close the generation's execution row (best effort)."""

        normalized = str(execution_id or "").strip()
        if not normalized:
            return
        complete = getattr(self.service.db, "complete_pipeline_execution", None)
        if not callable(complete):
            return
        # PRD T9-1: swallow-and-retry used to make a hierarchical-stage failure
        # that fell back to standard indexing indistinguishable from a clean
        # run on the receipt. Record which stage failed (jsonb manifest — no
        # schema change) so scan-vs-hierarchical failures stay attributable.
        receipt: dict[str, Any] = {}
        if manifest is not None:
            receipt["segment_ids"] = manifest
        fallbacks = [entry for entry in (stage_receipt or []) if entry]
        if fallbacks:
            receipt["stage_fallbacks"] = fallbacks
        try:
            await complete(
                normalized,
                status=status,
                error=error,
                manifest=receipt or None,
            )
        except Exception:
            logger.warning(
                "Pipeline execution ledger completion failed",
                extra={"execution_id": normalized},
                exc_info=True,
            )

    async def _dataset_hierarchical_opted_in(self, dataset_id: str) -> bool:
        """Opt-in check for call sites that have no index_config in scope.

        A read failure degrades to the standard path — the hierarchical route
        is an enhancement, never a prerequisite for ingestion.
        """

        try:
            dataset = await self.service.db.get_dataset(str(dataset_id or ""))
        except Exception:
            logger.warning(
                "Hierarchical opt-in dataset lookup failed; using the standard path",
                extra={"dataset_id": dataset_id},
                exc_info=True,
            )
            return False
        if not isinstance(dataset, dict):
            return False
        return self._hierarchical_opted_in(dataset.get("index_config"))

    @staticmethod
    def _note_stage_fallback(
        stage_receipt: list[dict[str, str]] | None,
        *,
        stage: str,
        error: Any,
    ) -> None:
        """Record a swallowed stage failure on the run's stage receipt."""

        if stage_receipt is None:
            return
        stage_receipt.append({"stage": str(stage), "error": str(error)[:500]})

    async def _ingest_document(
        self,
        task: KnowledgeIngestTask,
        *,
        chunking_config_override: dict[str, Any] | None = None,
        index_config_override: dict[str, Any] | None = None,
        source_text_override: str | None = None,
        candidate_metadata_patch: dict[str, Any] | None = None,
    ) -> list[str] | None:
        """Call the standard engine with the complete pinned replay config."""

        if (
            chunking_config_override is None
            and index_config_override is None
            and source_text_override is None
            and candidate_metadata_patch is None
        ):
            return await self.service.ingest_document(
                task.dataset_id,
                task.document_id,
            )
        candidate_kwargs: dict[str, Any] = {}
        if source_text_override is not None:
            candidate_kwargs["source_text_override"] = source_text_override
        if candidate_metadata_patch is not None:
            candidate_kwargs["candidate_metadata_patch"] = candidate_metadata_patch
        return await self.service.ingest_document(
            task.dataset_id,
            task.document_id,
            chunking_config_override=chunking_config_override,
            index_config_override=index_config_override,
            **candidate_kwargs,
        )

    async def _apply_parsing_ir(
        self,
        task: KnowledgeIngestTask,
        text: str,
        *,
        index_config: dict[str, Any] | None,
    ) -> str:
        """Apply the same pinned T4 parser contract to specialized paths."""

        ingestion_service = getattr(self.service, "ingestion_service", None)
        load_or_parse = getattr(
            ingestion_service,
            "_load_or_parse_document_ir",
            None,
        )
        if not callable(load_or_parse):
            return text
        dataset = await self.service.db.get_dataset(task.dataset_id)
        document = await self.service.db.get_document(task.document_id)
        if not isinstance(dataset, dict) or not isinstance(document, dict):
            raise RuntimeError("parsing IR source ownership is unavailable")
        effective_index_config = (
            index_config
            if isinstance(index_config, dict)
            else dataset.get("index_config") or {}
        )
        if not isinstance(effective_index_config, dict):
            raise RuntimeError("dataset index_config is invalid")
        return await load_or_parse(
            dataset=dataset,
            document=document,
            index_config=effective_index_config,
            source_text=text,
        )

    async def _finalize_scanned_image_generation(
        self,
        task: KnowledgeIngestTask,
        *,
        dataset: dict[str, Any],
        collection: str,
        desired_segment_ids: set[str],
    ) -> None:
        """Replace associations/bindings, then remove stale scanned points."""

        associate = getattr(self.service, "associate_images_to_chunks", None)
        if callable(associate):
            await associate(
                document_id=task.document_id,
                max_images_per_chunk=10,
                proximity_threshold=0.3,
                image_segment_ids=desired_segment_ids,
            )
        replace_bindings = getattr(
            self.service.db,
            "replace_document_attachment_bindings",
            None,
        )
        tenant_id = str(dataset.get("tenant_id") or "").strip()
        if callable(replace_bindings):
            await replace_bindings(
                task.document_id,
                task.dataset_id,
                tenant_id=tenant_id,
            )

        existing = await self.service.db.get_image_segments_by_document(
            task.document_id
        )
        stale = [
            segment
            for segment in existing
            if str(segment.get("segment_id") or "") not in desired_segment_ids
        ]
        if not stale:
            return
        cleanup = getattr(
            getattr(self.service, "ingestion_service", None),
            "_cleanup_stale_image_generation",
            None,
        )
        if not callable(cleanup):
            raise RuntimeError("scanned image cleanup protocol is unavailable")
        await cleanup(
            collection=collection,
            stale_image_segments=stale,
            tenant_id=tenant_id,
            dataset_id=task.dataset_id,
            document_id=task.document_id,
            expected_ingestion_identity=dataset_ingestion_identity(dataset),
        )

    @staticmethod
    def _decode_snapshot_object(value: Any, *, label: str) -> dict[str, Any]:
        """Decode one jsonb snapshot without hiding corrupt storage values."""

        decoded = value
        if isinstance(decoded, str):
            try:
                decoded = json.loads(decoded)
            except (json.JSONDecodeError, TypeError) as exc:
                raise RuntimeError(f"{label} contains invalid JSON") from exc
        if not isinstance(decoded, dict):
            raise RuntimeError(f"{label} must be a JSON object")
        return decoded

    @classmethod
    def _normalize_replay_snapshot(
        cls,
        value: Any,
        *,
        label: str,
    ) -> ReplayConfigSnapshot:
        """Validate and normalize new and legacy snapshot payloads."""

        payload = cls._decode_snapshot_object(value, label=label)
        raw_index_config = payload.get("index_config")
        raw_chunking = payload.get("chunking")
        if raw_index_config is None:
            if not isinstance(raw_chunking, dict):
                raise RuntimeError(f"{label} has no pinned chunking config")
            index_config = {"chunking": copy.deepcopy(raw_chunking)}
        elif isinstance(raw_index_config, dict):
            index_config = copy.deepcopy(raw_index_config)
            index_chunking = index_config.get("chunking")
            if index_chunking is None and isinstance(raw_chunking, dict):
                index_config["chunking"] = copy.deepcopy(raw_chunking)
            elif not isinstance(index_chunking, dict):
                raise RuntimeError(f"{label} index_config.chunking is invalid")
            elif isinstance(raw_chunking, dict) and index_chunking != raw_chunking:
                raise RuntimeError(f"{label} chunking aliases disagree")
        else:
            raise RuntimeError(f"{label} index_config is invalid")

        chunking = index_config.get("chunking")
        if not isinstance(chunking, dict):
            raise RuntimeError(f"{label} has no pinned chunking config")
        validate_persisted_chunking_config(chunking)

        processing_mode = payload.get("processing_mode")
        if not isinstance(processing_mode, str) or not processing_mode.strip():
            raise RuntimeError(f"{label} has no pinned processing mode")
        normalized_mode = processing_mode.strip().lower()
        if normalized_mode not in PROCESSING_MODE_VALUES:
            raise RuntimeError(f"{label} processing mode is invalid")
        restore_version = payload.get("restore_source_version")
        restore_kind = payload.get("restore_source_kind")
        restore_hash = payload.get("restore_source_hash")
        if restore_version is not None and (
            not isinstance(restore_version, int) or restore_version <= 0
        ):
            raise RuntimeError(f"{label} restore version is invalid")
        if restore_kind is not None and restore_kind not in {"text", "hierarchy", "vision"}:
            raise RuntimeError(f"{label} restore source kind is invalid")
        if restore_kind is not None and (
            not isinstance(restore_hash, str)
            or len(restore_hash) != 64
            or any(char not in "0123456789abcdef" for char in restore_hash)
            or restore_version is None
        ):
            raise RuntimeError(f"{label} restore source identity is invalid")
        if restore_kind is None and restore_hash is not None:
            raise RuntimeError(f"{label} restore source hash has no kind")
        return ReplayConfigSnapshot(
            index_config=index_config,
            processing_mode=normalized_mode,
            restore_source_version=restore_version,
            restore_source_kind=restore_kind,
            restore_source_hash=restore_hash,
        )

    async def _load_replay_snapshot(
        self,
        task: KnowledgeIngestTask,
        execution_id: str,
        document: dict[str, Any],
        *,
        expected_action: str,
        connection: Any | None = None,
    ) -> ReplayConfigSnapshot:
        """Load and cross-check both immutable replay snapshots.

        Missing, unreadable, malformed, or disagreeing execution/process-rule
        rows stop the generation. A replay must never drift to live config.
        """

        normalized = str(execution_id or "").strip()
        if not normalized:
            raise RuntimeError("replay execution id is missing")
        get_execution = getattr(self.service.db, "get_pipeline_execution", None)
        get_rule = getattr(self.service.db, "get_process_rule", None)
        if not callable(get_execution) or not callable(get_rule):
            raise RuntimeError("replay snapshot persistence is unavailable")

        execution = await get_execution(normalized, connection=connection)
        if not isinstance(execution, dict):
            raise RuntimeError("replay execution snapshot is missing")
        if str(execution.get("status") or "") != "running":
            raise RuntimeError("replay execution snapshot is not running")
        if str(execution.get("action") or "") != expected_action:
            raise RuntimeError("replay execution action does not match queued verb")
        if execution.get("document_id") not in (None, task.document_id):
            raise RuntimeError("replay execution belongs to another document")
        if execution.get("dataset_id") not in (None, task.dataset_id):
            raise RuntimeError("replay execution belongs to another dataset")

        process_rule_id = str(execution.get("process_rule_id") or "").strip()
        if not process_rule_id:
            raise RuntimeError("replay execution has no process-rule snapshot")
        document_rule_id = str(document.get("process_rule_id") or "").strip()
        if document_rule_id != process_rule_id:
            raise RuntimeError("document process-rule pin does not match execution")

        execution_snapshot = self._normalize_replay_snapshot(
            execution.get("input_snapshot"),
            label="pipeline execution snapshot",
        )
        rule = await get_rule(process_rule_id, connection=connection)
        if not isinstance(rule, dict):
            raise RuntimeError("pinned process-rule snapshot is missing")
        if rule.get("dataset_id") not in (None, task.dataset_id):
            raise RuntimeError("pinned process-rule belongs to another dataset")
        rule_snapshot = self._normalize_replay_snapshot(
            rule.get("rules"),
            label="process-rule snapshot",
        )
        if rule_snapshot != execution_snapshot:
            raise RuntimeError("execution and process-rule snapshots disagree")
        return execution_snapshot

    async def _load_pinned_process_rule(
        self,
        document: dict[str, Any] | None,
        *,
        expected_dataset_id: str,
        connection: Any | None = None,
    ) -> tuple[ReplayConfigSnapshot, str]:
        """Load the document pin for automatic recovery without a ledger link."""

        rule_id = str((document or {}).get("process_rule_id") or "").strip()
        if not rule_id:
            raise RuntimeError("document has no pinned process-rule snapshot")
        get_rule = getattr(self.service.db, "get_process_rule", None)
        if not callable(get_rule):
            raise RuntimeError("process-rule snapshot persistence is unavailable")
        rule = await get_rule(rule_id, connection=connection)
        if not isinstance(rule, dict):
            raise RuntimeError("pinned process-rule snapshot is missing")
        if str(rule.get("dataset_id") or "") != expected_dataset_id:
            raise RuntimeError("pinned process-rule belongs to another dataset")
        snapshot = self._normalize_replay_snapshot(
            rule.get("rules"),
            label="process-rule snapshot",
        )
        return snapshot, rule_id

    async def _prepare_document_generation(
        self,
        task: KnowledgeIngestTask,
        *,
        connection: Any,
    ) -> None:
        """Validate authority and remove the prior generation before dispatch."""

        dataset = await self.service.db.get_dataset(
            task.dataset_id,
            connection=connection,
        )
        if not dataset:
            raise RuntimeError("dataset was deleted before generation cleanup")
        if dataset_index_deletion_fence(dataset) is not None:
            raise RuntimeError("dataset index deletion is pending")

        document = await self.service.db.get_document(
            task.document_id,
            connection=connection,
        )
        if not document or str(document.get("dataset_id") or "") != task.dataset_id:
            raise RuntimeError("document authority changed before generation cleanup")
        if str(document.get("status") or "") != "parsing":
            raise RuntimeError("document generation is not owned by this worker")
        _require_extracted_text_budget(document.get("content"))

        metadata = self._document_metadata(document)
        if "structured_parsing" in metadata:
            raise RuntimeError(
                "structured parsing is disabled until a trusted source receipt exists"
            )
        verb = str(metadata.get(DOCUMENT_INGEST_ACTION_KEY) or "ingest").strip().lower()
        execution_id = str(metadata.get(DOCUMENT_PIPELINE_EXECUTION_KEY) or "").strip()
        interrupted_ingest = False
        if verb == "ingest" and execution_id:
            execution = await self.service.db.get_pipeline_execution(
                execution_id, connection=connection,
            )
            manifest = (execution or {}).get("manifest") if isinstance(execution, dict) else None
            if isinstance(manifest, str):
                try:
                    manifest = json.loads(manifest)
                except json.JSONDecodeError:
                    manifest = None
            interrupted_ingest = (
                isinstance(manifest, dict)
                and manifest.get("preparing_replayed_after_restart") is True
            )
        if verb in REPLAY_SNAPSHOT_ACTIONS or interrupted_ingest:
            replay_snapshot = await self._load_replay_snapshot(
                task,
                execution_id,
                document,
                expected_action=verb,
                connection=connection,
            )
            effective_index_config = replay_snapshot.index_config
        else:
            effective_index_config = dataset.get("index_config") or {}
            if not isinstance(effective_index_config, dict):
                raise RuntimeError("dataset index_config is invalid")
        LexicalConfig.from_index_config(effective_index_config)
        if verb != "reembed":
            # Vector repair (reembed) must stay possible even when the chunking
            # config is being reworked; every other verb rebuilds chunks and
            # therefore needs a valid persisted config.
            validate_persisted_chunking_config(
                effective_index_config.get("chunking", {})
            )
        lifecycle = metadata.get(DOCUMENT_LIFECYCLE_REINDEX_KEY)
        pending_restore = (
            isinstance(lifecycle, dict)
            and lifecycle.get("status") == "pending"
            and lifecycle.get("desired_enabled") is True
            and lifecycle.get("desired_archived") is False
        )
        ordinarily_active = (
            document.get("enabled", True) is True
            and document.get("archived", False) is False
            and DOCUMENT_LIFECYCLE_REINDEX_KEY not in metadata
        )
        if not ordinarily_active and not pending_restore:
            raise RuntimeError("document is inactive before generation cleanup")
        if (
            DOCUMENT_UPLOAD_GENERATION_KEY in metadata
            or CONFLUENCE_SYNC_GENERATION_KEY in metadata
        ):
            raise RuntimeError("document source generation is not finalized")

        if verb != "reembed":
            # The reembed verb repairs text vectors in place; it never rebuilds
            # the image source, so a non-rebuildable image receipt must not
            # block it. Every other verb regenerates and therefore requires a
            # rebuildable image source.
            get_image_segments = getattr(
                self.service.db,
                "get_image_segments_by_document",
                None,
            )
            if not callable(get_image_segments):
                raise RuntimeError("image generation authority is unavailable")
            image_segments = await get_image_segments(
                task.document_id,
                connection=connection,
            )
            self._validate_rebuildable_image_source(document, image_segments)
        # T1: NO pre-dispatch sweep. The ingestion engine is an incremental
        # in-place upsert keyed by (document_id, content_type, position):
        # unchanged chunks are skipped (zero re-embedding), changed chunks are
        # updated at their existing segment/point identity, new chunks stage
        # disabled with deterministic lineage ids, and excess rows are deleted
        # only after staging succeeds. Sweeping first would destroy the
        # serving generation and defeat the incremental skip. Reprocess,
        # recover, and retry all preserve the prior serving generation until
        # atomic publication succeeds.

    async def _process_task(
        self,
        task: KnowledgeIngestTask,
        *,
        connection: Any | None = None,
        stage_receipt: list[dict[str, str]] | None = None,
    ) -> list[str] | None:
        """Process one queued generation under the dual-verb contract.

        Returns the staged/repaired segment manifest for the execution ledger
        (PRD T1.5) or None when the dispatched path produces no manifest.
        ``stage_receipt`` collects PRD T9-1 fallback attributions recorded by
        dispatched processors (which stage failed and why before the run fell
        back to standard ingestion).
        """

        dataset = await self.service.db.get_dataset(task.dataset_id)
        if not dataset:
            raise ValueError(f"Dataset {task.dataset_id} not found")
        if dataset_index_deletion_fence(dataset) is not None:
            raise ValueError(
                "dataset index deletion is pending; queued ingestion is unavailable"
            )
        live_index_config = dataset.get("index_config") or {}

        # Get document to check processing mode
        doc = await self.service.db.get_document(task.document_id)
        if not doc:
            raise ValueError(f"Document {task.document_id} not found")
        _require_extracted_text_budget(doc.get("content"))

        metadata = doc.get("metadata", {})
        if not isinstance(metadata, dict):
            raise ValueError("document metadata is malformed")
        if "structured_parsing" in metadata:
            raise ValueError(
                "structured parsing is disabled until a trusted source receipt exists"
            )

        # Dual-verb routing (PRD T1 items 3/4, addendum §1-T1). The verb was
        # pinned on the queued row at claim time; recover additionally carries
        # the stage the crashed generation died in.
        verb = str(metadata.get(DOCUMENT_INGEST_ACTION_KEY) or "ingest").strip().lower()
        if verb not in INGEST_ACTION_VOCABULARY:
            verb = "ingest"
        recover_stage = (
            str(metadata.get(DOCUMENT_RECOVER_STAGE_KEY) or "").strip().lower()
        )
        execution_id = str(metadata.get(DOCUMENT_PIPELINE_EXECUTION_KEY) or "").strip()
        replay_interrupted_ingest = False
        if verb == "ingest" and execution_id:
            get_execution = getattr(self.service.db, "get_pipeline_execution", None)
            if callable(get_execution):
                execution = await get_execution(execution_id, connection=connection)
                manifest = (execution or {}).get("manifest") if isinstance(execution, dict) else None
                if isinstance(manifest, str):
                    try:
                        manifest = json.loads(manifest)
                    except json.JSONDecodeError:
                        manifest = None
                replay_interrupted_ingest = (
                    isinstance(manifest, dict)
                    and manifest.get("preparing_replayed_after_restart") is True
                )
        # Addendum §1-T1.3: replay semantics belong to the VERB, not merely to
        # the presence of a ledger row. Both durable snapshots must agree;
        # failure or corruption is terminal instead of silently selecting the
        # dataset's current configuration.
        if verb in REPLAY_SNAPSHOT_ACTIONS or replay_interrupted_ingest:
            replay_snapshot = await self._load_replay_snapshot(
                task,
                execution_id,
                doc,
                expected_action=verb,
                connection=connection,
            )
            replay_index_config = replay_snapshot.index_config
            effective_index_config = replay_index_config
            replay_chunking = replay_snapshot.chunking
            replay_mode: str | None = replay_snapshot.processing_mode
        else:
            if not isinstance(live_index_config, dict):
                raise ValueError("dataset index_config is invalid")
            replay_index_config = None
            effective_index_config = live_index_config
            replay_chunking = None
            replay_mode = None

        lexical_config = LexicalConfig.from_index_config(effective_index_config)
        if lexical_config.reads_bm25_v2 and not self.service.vector_store.bm25_v2_enabled:
            raise ValueError(
                "bm25_v2 active writes are unavailable while the service kill "
                "switch is off"
            )

        if verb == "reembed":
            logger.info(
                f"[Worker] Reembed (vector repair) for document {task.document_id}"
            )
            return await self.service.reembed_document(
                task.dataset_id, task.document_id
            )
        # A crash in indexing does not prove that this generation published
        # its candidate rows or source text. Re-embedding the previous serving
        # rows can silently turn an unfinished reprocess into a false success.
        # The full incremental pipeline reuses matching staged rows and keeps
        # the original document identity and pinned replay snapshot.
        if verb == "recover" and recover_stage == "indexing":
            logger.info(
                "[Worker] Recover document %s from indexing through pinned "
                "incremental ingestion",
                task.document_id,
            )

        if verb != "reembed":
            validate_persisted_chunking_config(
                replay_chunking
                if replay_chunking is not None
                else effective_index_config.get("chunking", {})
            )

        if DOCUMENT_PENDING_RESTORE_VERSION_KEY in metadata:
            if verb == "reembed":
                raise RuntimeError("restore candidate cannot use vector-only repair")
            if verb not in REPLAY_SNAPSHOT_ACTIONS:
                raise RuntimeError("restore candidate has no pinned replay execution")
            if replay_snapshot.restore_source_kind:
                return await self._process_special_restore(
                    task, doc, dataset, replay_snapshot,
                )
            await self.require_safe_restore_admission(task, dataset)
            return await self._ingest_document(
                task,
                chunking_config_override=replay_chunking,
                index_config_override=effective_index_config,
            )

        if verb in REPLAY_SNAPSHOT_ACTIONS:
            await self.require_safe_special_replay_admission(
                task, doc, {**dataset, "index_config": effective_index_config}, action=verb,
            )

        if verb == "retry":
            logger.info(
                "[Worker] Retry document %s through atomic incremental publication",
                task.document_id,
            )

        mode_str = replay_mode or metadata.get("processing_mode", "text_only")
        if replay_interrupted_ingest and replay_mode == "auto":
            # Auto detection may have durably selected a concrete mode before
            # the preparing crash. Reuse that decision for this same execution
            # instead of repeating a CAS that still expects metadata='auto'.
            resolved_mode = metadata.get("processing_mode")
            if resolved_mode in {"text_only", "scanned", "multimodal"}:
                mode_str = resolved_mode
        file_size = doc.get("size_bytes", 0)
        is_large_file = file_size > self.large_file_threshold

        # Handle auto detection mode. The detector owns the parse-stage mode,
        # while every downstream chunk/index choice receives the same pinned
        # config captured at submission.
        if mode_str == "auto" and self.detector:
            await self._process_with_auto_detection(
                task,
                doc,
                is_large_file,
                index_config_override=effective_index_config,
                stage_receipt=stage_receipt,
            )
            return None

        mode = parse_processing_mode(mode_str)

        if replay_chunking is not None:
            logger.info(
                f"[Worker] Replaying document {task.document_id} with its complete "
                f"submission snapshot (mode={mode_str}, large_file={is_large_file})"
            )

        logger.info(
            f"[Worker] Processing document {task.document_id} with mode={mode.value}, "
            f"size={file_size / 1024 / 1024:.1f}MB, large_file={is_large_file}"
        )

        # Update status to processing
        await self.service.db.update_document_status(
            task.document_id,
            status="parsing",
            progress=5,
        )

        # Route to appropriate processor
        if mode == ProcessingMode.SCANNED:
            await self._process_scanned(
                task,
                doc,
                index_config_override=effective_index_config,
                stage_receipt=stage_receipt,
            )
            return None
        elif mode == ProcessingMode.MULTIMODAL:
            # The streaming and hierarchical paths are text-only today. The
            # standard ingestion path owns the complete image receipt.
            return await self._ingest_document(
                task,
                chunking_config_override=replay_chunking,
                index_config_override=effective_index_config,
            )
        elif is_large_file:
            # Use streaming processing for large files
            await self._process_large_file(
                task,
                doc,
                mode,
                index_config_override=effective_index_config,
                stage_receipt=stage_receipt,
            )
            return None
        elif self.hierarchical_indexer and self._hierarchical_opted_in(
            effective_index_config
        ):
            # PRD T9-1 grayscale gate: only datasets whose stored chunking
            # mode is explicitly "hierarchical" take this branch; AUTOMATIC
            # and legacy configs stay on the standard ingestion path even now
            # that the indexer is wired into the default worker.
            await self._process_with_hierarchical_indexer(
                task,
                doc,
                mode,
                index_config_override=effective_index_config,
                stage_receipt=stage_receipt,
            )
            return None
        else:
            # Fallback to standard ingestion
            return await self._ingest_document(
                task,
                chunking_config_override=replay_chunking,
                index_config_override=effective_index_config,
            )

    async def _process_with_auto_detection(
        self,
        task: KnowledgeIngestTask,
        doc: dict,
        is_large_file: bool,
        index_config_override: dict[str, Any] | None = None,
        stage_receipt: list[dict[str, str]] | None = None,
    ) -> None:
        """Process document with automatic type detection."""
        metadata = doc.get("metadata", {})
        original_key = metadata.get("original_file_key")
        chunking_override = (
            copy.deepcopy(index_config_override.get("chunking", {}))
            if isinstance(index_config_override, dict)
            else None
        )

        if not original_key or not self.detector:
            # Fallback to text_only
            await self._ingest_document(
                task,
                chunking_config_override=chunking_override,
                index_config_override=index_config_override,
            )
            return

        # Update status
        await self.service.db.update_document_status(
            task.document_id,
            status="parsing",
            progress=2,
        )

        # Load file for detection (avoid full in-memory load for large files).
        # Detection itself may choose the explicit text fallback, but the
        # resolved mode must be durably published before dispatch. Otherwise a
        # multimodal result can be processed as stale ``auto`` and falsely
        # complete without its image generation.
        temp_path = None
        try:
            if is_large_file:
                temp_path = await self._download_original_to_temp(original_key)
                content = temp_path
            else:
                content = await self.service.image_storage_service.download_original_file(
                    original_key
                )

            # Detect document type
            detection = await self.detector.detect(
                content=content,
                filename=doc.get("title", ""),
                mime_type=doc.get("mime_type", ""),
            )

            mode = detection.recommended_mode
            detection_result = detection.to_dict()

            logger.info(
                f"[Worker] Auto-detected {task.document_id}: "
                f"type={detection.document_type.value}, mode={mode.value}, "
                f"confidence={detection.confidence:.2f}"
            )

        except Exception as e:
            logger.warning(f"[Worker] Detection failed, using text_only: {e}")
            mode = ProcessingMode.TEXT_ONLY
            detection_result = {
                "fallback": True,
                "reason": "detection_failed",
            }

        publish_mode = getattr(
            self.service.db,
            "compare_and_swap_document_processing_mode",
            None,
        )
        if not callable(publish_mode):
            if temp_path:
                await self._cleanup_temp_file(temp_path)
            raise RuntimeError("durable auto-detection mode publication is unavailable")
        try:
            published = await publish_mode(
                task.document_id,
                task.dataset_id,
                expected_mode="auto",
                replacement_mode=mode.value,
                detection_result=detection_result,
            )
        except Exception:
            if temp_path:
                await self._cleanup_temp_file(temp_path)
            raise
        if not published:
            if temp_path:
                await self._cleanup_temp_file(temp_path)
            raise RuntimeError(
                "auto-detection mode publication lost document generation authority"
            )

        await self.service.db.update_document_status(
            task.document_id,
            status="parsing",
            progress=5,
        )

        # Route to appropriate processor
        if mode == ProcessingMode.SCANNED:
            if temp_path:
                await self._cleanup_temp_file(temp_path)
            await self._process_scanned(
                task,
                doc,
                index_config_override=index_config_override,
                stage_receipt=stage_receipt,
            )
        elif mode == ProcessingMode.MULTIMODAL:
            if temp_path:
                await self._cleanup_temp_file(temp_path)
            await self._ingest_document(
                task,
                chunking_config_override=chunking_override,
                index_config_override=index_config_override,
            )
        elif is_large_file:
            await self._process_large_file(
                task,
                doc,
                mode,
                source_path=temp_path,
                index_config_override=index_config_override,
                stage_receipt=stage_receipt,
            )
        else:
            if temp_path:
                await self._cleanup_temp_file(temp_path)
            await self._ingest_document(
                task,
                chunking_config_override=chunking_override,
                index_config_override=index_config_override,
            )

    async def _process_large_file(
        self,
        task: KnowledgeIngestTask,
        doc: dict,
        mode: ProcessingMode,
        source_path: str | None = None,
        index_config_override: dict[str, Any] | None = None,
        stage_receipt: list[dict[str, str]] | None = None,
    ) -> None:
        """Process large file using streaming loader."""
        metadata = doc.get("metadata", {})
        original_key = metadata.get("original_file_key")
        chunking_override = (
            copy.deepcopy(index_config_override.get("chunking", {}))
            if isinstance(index_config_override, dict)
            else None
        )

        if not original_key:
            raise ValueError("No original file key found")

        logger.info(f"[Worker] Starting streaming processing for large file: {task.document_id}")

        temp_path = source_path
        if not temp_path:
            temp_path = await self._download_original_to_temp(original_key)

        # Initialize streaming loader
        knowledge_settings = getattr(self.service.settings, "knowledge", None)
        ocr_enabled = (
            getattr(knowledge_settings, "ocr_enabled", True) if knowledge_settings else True
        )
        batch_size = (
            getattr(knowledge_settings, "streaming_batch_size", 20) if knowledge_settings else 20
        )
        loader = StreamingDocumentLoader(
            batch_size=batch_size,
            extract_images=(mode == ProcessingMode.MULTIMODAL),
            extract_images_if_no_text=(ocr_enabled and mode != ProcessingMode.MULTIMODAL),
            storage_service=self.service.image_storage_service,
        )

        total_pages = 0
        with tempfile.NamedTemporaryFile(delete=False, suffix=".txt") as text_temp_file:
            text_temp_path = text_temp_file.name
        extracted_text_chars = 0
        extracted_text_bytes = 0

        # Process in batches
        async def on_progress(progress: float) -> None:
            await self.service.db.update_document_status(
                task.document_id,
                status="parsing",
                progress=int(5 + progress * 85),
            )

        try:
            try:
                async for batch in loader.iter_batches(temp_path, on_progress):
                    total_pages = batch.total_pages

                    # Collect text from batch (append to temp file to avoid large in-memory buffers)
                    batch_text_parts = []
                    for page in batch.pages:
                        if page.text.strip():
                            batch_text_parts.append(f"[Page {page.page_number}]\n{page.text}")
                        elif ocr_enabled and page.images:
                            try:
                                ocr_text = await self._ocr_image_auto(
                                    self._select_ocr_image(page.images)
                                )
                            except Exception as ocr_err:
                                logger.warning(f"[Worker] OCR failed for page {page.page_number}: {ocr_err}")
                                ocr_text = ""
                            if ocr_text:
                                batch_text_parts.append(f"[Page {page.page_number}]\n{ocr_text}")

                    if batch_text_parts:
                        batch_text = "\n\n".join(batch_text_parts) + "\n\n"
                        extracted_text_chars += len(batch_text)
                        extracted_text_bytes += len(batch_text.encode("utf-8"))
                        _require_extracted_text_counts_budget(
                            extracted_text_chars,
                            extracted_text_bytes,
                        )
                        await asyncio.to_thread(self._append_text, text_temp_path, batch_text)

                    logger.info(
                        f"[Worker] Processed batch {batch.batch_index}: "
                        f"pages {batch.start_page}-{batch.end_page}/{total_pages}"
                    )
            finally:
                if temp_path:
                    await self._cleanup_temp_file(temp_path)

            full_text = await asyncio.to_thread(self._read_text_full, text_temp_path)
            full_text = _require_extracted_text_budget(full_text)
            if not full_text.strip():
                await self.service.db.update_document_status(
                    task.document_id,
                    status="error",
                    progress=100,
                    error="No text extracted from document",
                )
                return

            # Use hierarchical indexer if the dataset explicitly opted in
            # (PRD T9-1 grayscale gate: the default worker now carries one).
            hierarchical_opted_in = (
                self._hierarchical_opted_in(index_config_override)
                if index_config_override is not None
                else await self._dataset_hierarchical_opted_in(task.dataset_id)
            )
            if self.hierarchical_indexer and hierarchical_opted_in:
                await self._require_special_rebuild_without_serving_version(
                    task, doc, pathway="hierarchical large-file",
                )
                # Load chunking config from the replay snapshot when present.
                chunking_config = None
                try:
                    from .chunking import ChunkingConfig

                    if index_config_override is not None:
                        index_config = index_config_override
                    else:
                        dataset = await self.service.db.get_dataset(task.dataset_id)
                        index_config = (dataset or {}).get("index_config") or {}
                    if isinstance(index_config, dict):
                        chunking_dict = (
                            index_config.get("chunking")
                            if isinstance(index_config, dict)
                            else {}
                        )
                        if chunking_dict:
                            chunking_config = ChunkingConfig.from_dict(chunking_dict)
                            # VALIDATION LOG: Log loaded config details
                            logger.info(
                                f"[Worker] Loaded chunking config for {task.document_id}: "
                                f"mode={chunking_config.mode}, "
                                f"token_limit={chunking_config.token_limit}, "
                                f"use_token_count={chunking_config.use_token_count}, "
                                f"child_token_limit={chunking_config.child_token_limit}, "
                                f"parent_token_limit={chunking_config.parent_token_limit}, "
                                f"raw={chunking_dict}"
                            )
                        else:
                            logger.warning(
                                f"[Worker] No chunking config found in dataset {task.dataset_id}"
                            )
                    else:
                        raise RuntimeError("dataset index_config is invalid")
                except Exception as e:
                    if index_config_override is not None:
                        raise RuntimeError(
                            "pinned large-file chunking config is invalid"
                        ) from e
                    logger.warning(f"[Worker] Failed to load chunking config: {e}")

                parsed_text = await self._apply_parsing_ir(
                    task,
                    full_text,
                    index_config=index_config_override,
                )
                result = await self.hierarchical_indexer.index_document(
                    document_id=task.document_id,
                    dataset_id=task.dataset_id,
                    text=parsed_text,
                    metadata=metadata,
                    chunking_config=chunking_config,
                )

                if not result.success:
                    # PRD T9-1: an unsuccessful index otherwise closes the
                    # execution receipt as a clean completed run.
                    self._note_stage_fallback(
                        stage_receipt,
                        stage="hierarchical_indexing_large_file",
                        error="; ".join(str(item) for item in result.errors)
                        or "hierarchical indexing reported no vectors",
                    )
                else:
                    # This legacy multi-collection indexer still needs a
                    # generation-wide publication fence. Keep the old source
                    # on a reported index failure; a successful generation
                    # must at least make its indexed text the visible source.
                    await self.service.db.execute(
                        """
                        UPDATE documents
                        SET content = $1, metadata = metadata || $2::jsonb
                        WHERE document_id = $3
                        """,
                        parsed_text,
                        json.dumps({
                            "total_pages": total_pages,
                            "streaming_processed": True,
                        }),
                        task.document_id,
                    )

                await self.service.db.update_document_status(
                    task.document_id,
                    status="completed" if result.success else "error",
                    progress=100,
                )
                # Update segment counts in metadata
                try:
                    await self.service.db.execute(
                        """UPDATE documents
                           SET metadata = metadata || $1::jsonb
                           WHERE document_id = $2""",
                        json.dumps(
                            {
                                "l1_segments": result.l1_count,
                                "l2_segments": result.l2_count,
                                "l3_segments": result.l3_count,
                                "total_vectors": result.total_vectors,
                            }
                        ),
                        task.document_id,
                    )
                except Exception as e:
                    logger.debug(f"Failed to update segment counts: {e}")
            else:
                # Fallback to standard ingestion
                await self._ingest_document(
                    task,
                    chunking_config_override=chunking_override,
                    index_config_override=index_config_override,
                    source_text_override=full_text,
                    candidate_metadata_patch={
                        "total_pages": total_pages,
                        "streaming_processed": True,
                    },
                )
        finally:
            await self._cleanup_temp_file(text_temp_path)

    async def _download_original_to_temp(self, storage_key: str) -> str:
        """Download a file to a temporary path to avoid large memory spikes."""
        with tempfile.NamedTemporaryFile(delete=False, suffix=".bin") as temp_file:
            temp_path = temp_file.name
        try:
            await self.service.image_storage_service.download_original_file_to_path(
                storage_key, temp_path
            )
            return temp_path
        except Exception:
            await self._cleanup_temp_file(temp_path)
            raise

    async def _cleanup_temp_file(self, path: str | None) -> None:
        """Remove a temporary file safely.

        Args:
            path: Path to the temporary file, or None.
        """
        if not path:
            return
        try:
            await asyncio.to_thread(Path(path).unlink)
        except FileNotFoundError:
            pass  # File already deleted, this is normal
        except PermissionError:
            logger.warning(f"Permission denied when cleaning up temp file: {path}")
        except OSError as e:
            logger.warning(f"OS error when cleaning up temp file {path}: {e}")
        except Exception as e:
            logger.warning(f"Unexpected error when cleaning up temp file {path}: {e}")

    @staticmethod
    def _append_text(path: str, text: str) -> None:
        with open(path, "a", encoding="utf-8") as handle:
            handle.write(text)

    @staticmethod
    def _read_text_preview(path: str, max_chars: int) -> str:
        with open(path, encoding="utf-8") as handle:
            return handle.read(max_chars)

    @staticmethod
    def _read_text_full(path: str) -> str:
        with open(path, encoding="utf-8") as handle:
            return handle.read()

    @staticmethod
    def _select_ocr_image(images: list[bytes]) -> bytes:
        """Pick the largest image for OCR to maximize text capture."""
        return max(images, key=len)

    def _ocr_image_bytes(self, image_bytes: bytes) -> str:
        """Run OCR on a single image using Tesseract CLI.

        Uses shared OCR utilities from ocr_utils module.
        """
        from .ocr_utils import OCRCConfig
        from .ocr_utils import ocr_image_bytes as _ocr_image

        knowledge_settings = getattr(self.service.settings, "knowledge", None)
        config = OCRCConfig.from_settings(knowledge_settings)

        return _ocr_image(image_bytes, config=config, fallback_to_eng=True)

    async def _ocr_image_auto(self, image_bytes: bytes) -> str:
        """Run OCR on a single image using the configured strategy (VLM/Tesseract/Hybrid)."""
        from .ocr_utils import OCRCConfig, ocr_image_bytes_auto

        knowledge_settings = getattr(self.service.settings, "knowledge", None)
        config = OCRCConfig.from_settings(knowledge_settings)
        return await ocr_image_bytes_auto(
            image_bytes,
            vlm_ocr_service=self.vlm_ocr_service,
            config=config,
            strategy=self._ocr_strategy,
        )

    async def _process_scanned_with_vlm_ocr(
        self,
        task: KnowledgeIngestTask,
        doc: dict,
        *,
        chunking_config_override: dict[str, Any] | None = None,
        index_config_override: dict[str, Any] | None = None,
    ) -> None:
        """Process a scanned PDF using VLM OCR for text extraction, then standard ingestion.

        Fallback when VisionPDFProcessor is not available but VLM OCR service is.
        Renders each page to image → VLM OCR → concatenated text → ingest as text.
        """
        metadata = doc.get("metadata", {})
        original_key = metadata.get("original_file_key")
        if not original_key:
            await self.service.db.update_document_status(
                task.document_id, status="error", progress=100, error="no original file"
            )
            return

        await self.service.db.update_document_status(task.document_id, status="parsing", progress=5)

        # Download PDF from storage
        pdf_bytes = await self.service.image_storage_service.download_original_file(original_key)
        if not pdf_bytes:
            await self.service.db.update_document_status(
                task.document_id, status="error", progress=100, error="failed to download file"
            )
            return

        # SPO-04 / K2: open + count + close on one worker thread.
        from .document_processor import MAX_PDF_PAGE_MARKERS

        try:
            total_pages = await asyncio.to_thread(_pdf_page_count_sync, pdf_bytes)
        except Exception as exc:
            await self.service.db.update_document_status(
                task.document_id, status="error", progress=100, error=str(exc)
            )
            return
        if total_pages > MAX_PDF_PAGE_MARKERS:
            await self.service.db.update_document_status(
                task.document_id,
                status="error",
                progress=100,
                error=f"PDF exceeds the {MAX_PDF_PAGE_MARKERS} page marker limit",
            )
            return
        logger.info(f"[Worker] VLM OCR processing {total_pages} pages for {task.document_id}")

        # Render pages to images and OCR in batches
        all_text_parts = []
        extracted_text_chars = 0
        extracted_text_bytes = 0
        batch_size = 5
        for batch_start in range(0, total_pages, batch_size):
            batch_end = min(batch_start + batch_size, total_pages)
            images = await asyncio.to_thread(
                _render_pdf_pages_sync,
                pdf_bytes,
                batch_start,
                batch_end,
            )

            texts = await self.vlm_ocr_service.ocr_pdf_pages(images)
            for i, text in enumerate(texts):
                if text and text.strip():
                    part = f"[Page {batch_start + i + 1}]\n{text}"
                    separator_size = 2 if all_text_parts else 0
                    extracted_text_chars += separator_size + len(part)
                    extracted_text_bytes += separator_size + len(part.encode("utf-8"))
                    _require_extracted_text_counts_budget(
                        extracted_text_chars,
                        extracted_text_bytes,
                    )
                    all_text_parts.append(part)

            progress = 5 + int((batch_end / total_pages) * 60)
            await self.service.db.update_document_status(
                task.document_id, status="parsing", progress=progress,
            )

        if not all_text_parts:
            await self.service.db.update_document_status(
                task.document_id, status="error", progress=100, error="VLM OCR extracted no text"
            )
            return

        full_text = _require_extracted_text_budget("\n\n".join(all_text_parts))
        logger.info(f"[Worker] VLM OCR extracted {len(full_text)} chars from {total_pages} pages")

        # The OCR text is a candidate. The standard ingestion engine parses
        # it once and publishes it with its text segments; a failed embedding
        # leaves the previous source and serving rows untouched.
        await self._ingest_document(
            task,
            chunking_config_override=chunking_config_override,
            index_config_override=index_config_override,
            source_text_override=full_text,
            candidate_metadata_patch={
                "ocr_provider": getattr(self.vlm_ocr_service, "provider", None),
                "ocr_model": getattr(self.vlm_ocr_service, "model", None),
                "ocr_task": getattr(self.vlm_ocr_service, "task", None),
                "vlm_ocr_pages": sum(1 for part in all_text_parts if part.strip()),
            },
        )

    async def _process_with_hierarchical_indexer(
        self,
        task: KnowledgeIngestTask,
        doc: dict,
        mode: ProcessingMode,
        index_config_override: dict[str, Any] | None = None,
        stage_receipt: list[dict[str, str]] | None = None,
    ) -> None:
        """Process document using hierarchical indexer for L2/L3 chunking."""
        del mode
        metadata = doc.get("metadata", {})
        original_key = metadata.get("original_file_key")
        chunking_override = (
            copy.deepcopy(index_config_override.get("chunking", {}))
            if isinstance(index_config_override, dict)
            else None
        )

        if not original_key:
            # No original file, fallback to standard ingestion
            await self._ingest_document(
                task,
                chunking_config_override=chunking_override,
                index_config_override=index_config_override,
            )
            return

        if not callable(getattr(self.hierarchical_indexer, "prepare_document", None)):
            await self._require_special_rebuild_without_serving_version(
                task, doc, pathway="hierarchical legacy",
            )

        logger.info(f"[Worker] Processing with hierarchical indexer: {task.document_id}")

        special_publication_attempted = False
        special_execution_id = ""
        special_generation = ""
        special_source_hash = ""
        special_dataset: dict[str, Any] | None = None
        try:
            # Download and extract text
            content = await self.service.image_storage_service.download_original_file(original_key)
            if not content:
                raise ValueError("Failed to download original file")

            # Extract text based on file type
            full_text = await self._extract_text_from_content(
                content, metadata.get("mime_type", "")
            )
            full_text = _require_extracted_text_budget(full_text)
            full_text = await self._apply_parsing_ir(
                task,
                full_text,
                index_config=index_config_override,
            )

            if not full_text.strip():
                await self.service.db.update_document_status(
                    task.document_id,
                    status="error",
                    progress=100,
                    error="No text extracted from document",
                )
                return

            # Update progress
            await self.service.db.update_document_status(
                task.document_id,
                status="parsing",
                progress=50,
            )

            # Load chunking config
            chunking_config = None
            try:
                from .chunking import ChunkingConfig, ChunkingMode

                if index_config_override is not None:
                    index_config = index_config_override
                else:
                    dataset = await self.service.db.get_dataset(task.dataset_id)
                    index_config = (dataset or {}).get("index_config") or {}
                if isinstance(index_config, dict):
                    chunking_dict = (
                        index_config.get("chunking")
                        if isinstance(index_config, dict)
                        else {}
                    )
                    if chunking_dict:
                        chunking_config = ChunkingConfig.from_dict(chunking_dict)
                        # VALIDATION LOG: Log loaded config details
                        logger.info(
                            f"[Worker] Loaded chunking config for {task.document_id}: "
                            f"mode={chunking_config.mode}, "
                            f"token_limit={chunking_config.token_limit}, "
                            f"use_token_count={chunking_config.use_token_count}, "
                            f"child_token_limit={chunking_config.child_token_limit}, "
                            f"parent_token_limit={chunking_config.parent_token_limit}, "
                            f"raw={chunking_dict}"
                        )
                        if chunking_config.mode == ChunkingMode.AUTOMATIC:
                            explicit_keys = {
                                "parent_chunk_size",
                                "child_chunk_size",
                                "parent_overlap",
                                "child_overlap",
                                "parent_token_limit",
                                "child_token_limit",
                                "token_limit",
                                "chunk_overlap",
                            }
                            if not any(k in chunking_dict for k in explicit_keys):
                                chunking_config.use_token_count = True
                                chunking_config.token_limit = 400
                                chunking_config.child_token_limit = 400
                                chunking_config.parent_token_limit = 1500
                                chunking_config.child_overlap = 50
                                chunking_config.parent_overlap = 50
                                chunking_config.chunk_overlap = 50
                                chunking_config.child_chunk_size = max(
                                    chunking_config.child_chunk_size, 1600
                                )
                                chunking_config.parent_chunk_size = max(
                                    chunking_config.parent_chunk_size, 6000
                                )
                                chunking_config.parent_mode = "fixed"
                    else:
                        logger.warning(
                            f"[Worker] No chunking config found in dataset {task.dataset_id}"
                        )
                else:
                    raise RuntimeError("dataset index_config is invalid")
            except Exception as e:
                if index_config_override is not None:
                    raise RuntimeError(
                        "pinned hierarchical chunking config is invalid"
                    ) from e
                logger.warning(f"[Worker] Failed to load chunking config: {e}")

            if chunking_config and chunking_config.mode not in (
                ChunkingMode.HIERARCHICAL,
                ChunkingMode.AUTOMATIC,
            ):
                logger.info(
                    f"[Worker] Chunking mode {chunking_config.mode} requested; "
                    "bypassing hierarchical indexer."
                )
                await self._ingest_document(
                    task,
                    chunking_config_override=chunking_override,
                    index_config_override=index_config_override,
                )
                return

            prepare_candidate = getattr(self.hierarchical_indexer, "prepare_document", None)
            if callable(prepare_candidate):
                special_publication_attempted = True
                dataset = await self.service.db.get_dataset(task.dataset_id)
                if not isinstance(dataset, dict):
                    raise RuntimeError("hierarchical dataset authority is unavailable")
                if isinstance(index_config_override, dict):
                    dataset = {**dataset, "index_config": index_config_override}
                execution_id = str(metadata.get(DOCUMENT_PIPELINE_EXECUTION_KEY) or "").strip()
                if not execution_id:
                    raise RuntimeError("hierarchical generation has no durable execution")
                source_hash = hashlib.sha256(full_text.encode("utf-8")).hexdigest()
                special_execution_id = execution_id
                special_generation = _special_generation_id(execution_id)
                special_source_hash = source_hash
                special_dataset = dataset
                begin = getattr(self.service.db, "record_special_publication_manifest", None)
                if not callable(begin):
                    raise RuntimeError("special publication manifest is unavailable")
                await begin(
                    execution_id, task.document_id, task.dataset_id, special_generation,
                    source_hash=source_hash, planned_object_keys=[],
                )
                plan = await prepare_candidate(
                    document_id=task.document_id,
                    dataset_id=task.dataset_id,
                    text=full_text,
                    generation_id=special_generation,
                    metadata=metadata,
                    chunking_config=chunking_config,
                )
                from .special_publication import SpecialPublicationCoordinator

                await self.service.db.update_document_status(
                    task.document_id, status="indexing", progress=75,
                )
                await SpecialPublicationCoordinator(self.service).publish(
                    plan, dataset, execution_id, str(doc.get("content") or ""),
                    document_shared_lease_held=True,
                    metadata_patch={
                        "l1_segments": plan.l1_count,
                        "l2_segments": plan.l2_count,
                        "l3_segments": plan.l3_count,
                        "total_vectors": sum(len(points) for points in plan.points_by_collection.values()),
                    },
                )
                return

            # Legacy first-ingest implementation for injected indexers without
            # candidate planning. A real worker uses the path above.
            result = await self.hierarchical_indexer.index_document(
                document_id=task.document_id,
                dataset_id=task.dataset_id,
                text=full_text,
                metadata=metadata,
                chunking_config=chunking_config,
            )

            if not result.success:
                # PRD T9-1: no exception is raised here, so without this the
                # execution receipt would close as a clean completed run.
                self._note_stage_fallback(
                    stage_receipt,
                    stage="hierarchical_indexing",
                    error="; ".join(str(item) for item in result.errors)
                    or "hierarchical indexing reported no vectors",
                )

            await self.service.db.update_document_status(
                task.document_id,
                status="completed" if result.success else "error",
                progress=100,
            )

            # Update metadata with segment counts
            try:
                await self.service.db.execute(
                    """UPDATE documents
                       SET metadata = metadata || $1::jsonb
                       WHERE document_id = $2""",
                    json.dumps(
                        {
                            "l1_segments": result.l1_count,
                            "l2_segments": result.l2_count,
                            "l3_segments": result.l3_count,
                            "total_vectors": result.total_vectors,
                        }
                    ),
                    task.document_id,
                )
            except Exception as e:
                logger.warning(f"Failed to update segment counts: {e}")

            logger.info(
                f"[Worker] Hierarchical indexing completed for {task.document_id}: "
                f"L1={result.l1_count}, L2={result.l2_count}, L3={result.l3_count}"
            )

        except ValidationFailedError:
            raise
        except Exception as e:
            logger.error(f"Hierarchical indexing failed for {task.document_id}: {e}")
            if special_publication_attempted:
                if special_dataset and special_execution_id and special_source_hash:
                    owner = await self.service.db.get_special_publication_manifest(
                        special_execution_id,
                    )
                    if owner and owner.get("phase") == "preparing":
                        from .special_publication import SpecialPublicationCoordinator

                        await SpecialPublicationCoordinator(self.service).abort_preparing(
                            special_dataset, special_execution_id,
                            special_generation, special_source_hash,
                            document_shared_lease_held=True,
                        )
                self._note_stage_fallback(
                    stage_receipt, stage="hierarchical_publication", error=e,
                )
                raise
            # PRD T9-1: the swallow-and-fallback stays, but the failed stage is
            # now recorded on the execution receipt instead of disappearing.
            self._note_stage_fallback(
                stage_receipt, stage="hierarchical_indexing", error=e
            )
            # Fallback to standard ingestion
            await self._ingest_document(
                task,
                chunking_config_override=chunking_override,
                index_config_override=index_config_override,
            )

    async def _extract_text_from_content(self, content: bytes, mime_type: str) -> str:
        """Extract text from file content based on mime type.

        Args:
            content: File content as bytes
            mime_type: MIME type string (may be empty)

        Returns:
            Extracted text or empty string
        """
        if not content:
            return ""

        mime = (mime_type or "").lower()

        # Use magic bytes for more reliable detection
        if content.startswith(b"%PDF") or "pdf" in mime:
            # Extract text from PDF (SPO-04 / K2: rasterize off the event loop)
            try:
                extracted = await asyncio.to_thread(_extract_pdf_text_sync, content)
                return _require_extracted_text_budget(extracted)
            except ValidationFailedError:
                raise
            except Exception as e:
                logger.warning(f"PDF text extraction failed: {e}")
                return ""

        elif content.startswith(b"PK\x03\x04") or "word" in mime or "docx" in mime:
            # Try to extract from DOCX (SPO-04 / K2: parse off the event loop)
            try:
                extracted = await asyncio.to_thread(_extract_docx_text_sync, content)
                return _require_extracted_text_budget(extracted)
            except ValidationFailedError:
                raise
            except Exception as e:
                logger.warning(f"DOCX text extraction failed: {e}")
                return _require_extracted_text_budget(
                    content.decode("utf-8", errors="ignore")
                )

        elif "text" in mime or "plain" in mime or "markdown" in mime or "md" in mime:
            # Plain text
            return _require_extracted_text_budget(content.decode("utf-8", errors="ignore"))

        else:
            # Try UTF-8 decoding as fallback
            try:
                return _require_extracted_text_budget(content.decode("utf-8", errors="ignore"))
            except ValidationFailedError:
                raise
            except Exception:
                return ""

    async def _process_special_restore(
        self,
        task: KnowledgeIngestTask,
        document: dict[str, Any],
        dataset: dict[str, Any],
        snapshot: ReplayConfigSnapshot,
    ) -> list[str]:
        """Rebuild an exact historical special source as one hidden generation."""

        from .special_publication import SOURCE_MANIFEST_KEY, SpecialPublicationCoordinator

        metadata = self._document_metadata(document)
        pending = metadata.get(DOCUMENT_PENDING_RESTORE_VERSION_KEY)
        execution_id = str(metadata.get(DOCUMENT_PIPELINE_EXECUTION_KEY) or "").strip()
        if not isinstance(pending, dict) or not execution_id:
            raise RuntimeError("special restore has no durable candidate owner")
        generation_id = _special_generation_id(execution_id)
        candidate_number = int(pending.get("candidate_version") or 0)
        target = await self.service.db.get_document_version(
            task.document_id, candidate_number, include_pending=True,
        )
        if not target or target.get("change_type") != "pending_restore":
            raise RuntimeError("special restore candidate version is unavailable")
        content = str(target.get("content") or "")
        content_hash = hashlib.sha256(content.encode("utf-8")).hexdigest()
        source = target.get("metadata") or {}
        source = source.get(SOURCE_MANIFEST_KEY) if isinstance(source, dict) else None
        if (
            not isinstance(source, dict)
            or target.get("content_hash") != content_hash
            or source.get("content_hash") != content_hash
            or source.get("source_kind") != snapshot.restore_source_kind
            or source.get("source_hash") != snapshot.restore_source_hash
            or source.get("index_config") != snapshot.index_config
            or snapshot.restore_source_version is None
            or candidate_number <= snapshot.restore_source_version
        ):
            raise RuntimeError("special restore source identity changed")
        tenant_id = str(dataset.get("tenant_id") or "").strip()
        collection = str(dataset.get("collection_name") or "").strip()
        if not tenant_id or not collection:
            raise RuntimeError("special restore dataset identity is unavailable")
        coordinator = SpecialPublicationCoordinator(self.service)
        source_hash = str(source["source_hash"])
        planned_keys: list[str] = []
        pdf_bytes: bytes | None = None
        pinned_page_texts: dict[int, str] | None = None
        if snapshot.restore_source_kind == "vision":
            if self.vision_processor is None:
                raise RuntimeError("special vision restore processor is unavailable")
            original_key = str(source.get("original_source_key") or "")
            expected_prefix = f"knowledge/documents/{tenant_id}/{task.document_id}/original/"
            if not original_key.startswith(expected_prefix):
                raise RuntimeError("special restore original source owner differs")
            pdf_bytes = await self.service.image_storage_service.download_original_file(
                original_key,
            )
            if hashlib.sha256(pdf_bytes).hexdigest() != source_hash:
                raise RuntimeError("special restore original bytes changed")
            pages = source.get("page_texts")
            if not isinstance(pages, list) or not pages:
                raise RuntimeError("special restore page receipts are unavailable")
            pinned_page_texts = {}
            for item in pages:
                if not isinstance(item, dict) or not isinstance(item.get("text"), str):
                    raise RuntimeError("special restore page receipt is invalid")
                page_number = item.get("page_number")
                if not isinstance(page_number, int) or page_number <= 0 or page_number in pinned_page_texts:
                    raise RuntimeError("special restore page order is invalid")
                pinned_page_texts[page_number] = item["text"]
            planned_keys = await self.vision_processor.planned_object_keys(
                pdf_bytes, task.document_id, generation_id=generation_id,
                tenant_id=tenant_id, storage_service=self.service.image_storage_service,
            )
        elif snapshot.restore_source_kind == "hierarchy":
            if self.hierarchical_indexer is None:
                raise RuntimeError("special hierarchy restore processor is unavailable")
            if hashlib.sha256(content.encode("utf-8")).hexdigest() != source_hash:
                raise RuntimeError("special restore hierarchy text changed")
        elif snapshot.restore_source_kind == "text":
            if content_hash != source_hash or source.get("objects") != {} or source.get("page_texts") != []:
                raise RuntimeError("special restore text source is unverifiable")
        else:
            raise RuntimeError("special restore source kind is invalid")

        await self.service.db.record_special_publication_manifest(
            execution_id, task.document_id, task.dataset_id, generation_id,
            source_hash=source_hash, planned_object_keys=planned_keys,
        )
        try:
            if snapshot.restore_source_kind == "vision":
                assert pdf_bytes is not None and pinned_page_texts is not None
                plan = await self.vision_processor.prepare_document(
                    pdf_bytes, task.document_id, task.dataset_id, collection,
                    generation_id=generation_id, tenant_id=tenant_id,
                    storage_service=self.service.image_storage_service,
                    pinned_page_texts=pinned_page_texts,
                )
                receipt = [(item["page_number"], item["text"], item["sha256"])
                           for item in source["page_texts"]]
                candidate_receipt = [(item["page_number"], item["text"], item["sha256"])
                                     for item in coordinator._vision_page_texts(
                                         plan, coordinator._object_plan(plan),
                                     )]
                if candidate_receipt != receipt:
                    raise RuntimeError("special restore rendered pages differ")
                patch = {
                    "pages_processed": plan.processed_pages,
                    "total_pages": plan.total_pages,
                    "segments_created": len(plan.segment_ids),
                    "ocr_strategy": "restored_pinned_page_text",
                }
            elif snapshot.restore_source_kind == "hierarchy":
                from .chunking import ChunkingConfig

                chunking = ChunkingConfig.from_dict(snapshot.chunking)
                plan = await self.hierarchical_indexer.prepare_document(
                    document_id=task.document_id, dataset_id=task.dataset_id,
                    text=content, generation_id=generation_id,
                    metadata=target.get("metadata") or {}, chunking_config=chunking,
                )
                patch = {
                    "l1_segments": plan.l1_count,
                    "l2_segments": plan.l2_count,
                    "l3_segments": plan.l3_count,
                    "total_vectors": sum(len(points) for points in plan.points_by_collection.values()),
                }
            else:
                from .text_restore_candidate import TextRestoreCandidatePlanner

                plan = await TextRestoreCandidatePlanner(self.service).prepare(
                    document_id=task.document_id, dataset=dataset,
                    generation_id=generation_id, content=content,
                    title=str(target.get("title") or document.get("title") or task.document_id),
                    chunking=snapshot.chunking,
                )
                patch = {
                    "l1_segments": 0, "l2_segments": 0, "l3_segments": 0,
                    "total_vectors": len(plan.segment_ids),
                }
            await self.service.db.update_document_status(
                task.document_id, status="indexing", progress=75,
            )
            return await coordinator.publish(
                plan, dataset, execution_id, str(document.get("content") or ""),
                metadata_patch=patch, document_shared_lease_held=True,
            )
        except Exception:
            owner = await self.service.db.get_special_publication_manifest(execution_id)
            if owner and owner.get("phase") == "preparing":
                await coordinator.abort_preparing(
                    dataset, execution_id, generation_id, source_hash,
                    document_shared_lease_held=True,
                )
            raise

    async def _process_scanned_candidate(
        self,
        task: KnowledgeIngestTask,
        doc: dict[str, Any],
        *,
        index_config_override: dict[str, Any] | None,
    ) -> None:
        """Prepare scanned pages, then publish their one durable generation."""

        metadata = self._document_metadata(doc)
        original_key = str(metadata.get("original_file_key") or "").strip()
        execution_id = str(metadata.get(DOCUMENT_PIPELINE_EXECUTION_KEY) or "").strip()
        if not original_key or not execution_id:
            raise RuntimeError("scanned generation lacks source or durable execution")
        generation_id = _special_generation_id(execution_id)
        dataset = await self.service.db.get_dataset(task.dataset_id)
        if not isinstance(dataset, dict):
            raise RuntimeError("scanned dataset authority is unavailable")
        if isinstance(index_config_override, dict):
            dataset = {**dataset, "index_config": index_config_override}
        tenant_id = str(dataset.get("tenant_id") or "").strip()
        if not tenant_id:
            raise RuntimeError("scanned dataset tenant is unavailable")
        pdf_bytes = await self.service.image_storage_service.download_original_file(original_key)
        if not pdf_bytes:
            raise RuntimeError("scanned source bytes are unavailable")
        source_hash = hashlib.sha256(pdf_bytes).hexdigest()
        dimension = int(getattr(self.vision_processor.embedder, "dimension", 1024) or 1024)
        collection = str(dataset.get("collection_name") or "").strip()
        if not collection:
            collection = f"kb_{task.dataset_id}_{dimension}"
        planned_keys = await self.vision_processor.planned_object_keys(
            pdf_bytes, task.document_id,
            generation_id=generation_id, tenant_id=tenant_id,
            storage_service=self.service.image_storage_service,
        )
        await self.service.db.record_special_publication_manifest(
            execution_id, task.document_id, task.dataset_id, generation_id,
            source_hash=source_hash, planned_object_keys=planned_keys,
        )
        from .special_publication import SpecialPublicationCoordinator

        coordinator = SpecialPublicationCoordinator(self.service)
        text_extractor = None
        if self.vlm_ocr_service and self._ocr_strategy in {"vlm", "hybrid"}:
            async def _extract(image_bytes: bytes) -> str:
                from .ocr_utils import OCRCConfig, ocr_image_bytes_auto

                config = OCRCConfig.from_settings(
                    getattr(self.service.settings, "knowledge", None),
                )
                return await ocr_image_bytes_auto(
                    image_bytes, vlm_ocr_service=self.vlm_ocr_service,
                    config=config, strategy=self._ocr_strategy,
                )
            text_extractor = _extract
        try:
            plan = await self.vision_processor.prepare_document(
                pdf_bytes, task.document_id, task.dataset_id, collection,
                generation_id=generation_id, tenant_id=tenant_id,
                storage_service=self.service.image_storage_service,
                text_extractor=text_extractor,
            )
            await self.service.db.update_document_status(
                task.document_id, status="indexing", progress=75,
            )
            await coordinator.publish(
                plan, dataset, execution_id, str(doc.get("content") or ""),
                document_shared_lease_held=True,
                metadata_patch={
                    "pages_processed": plan.processed_pages,
                    "total_pages": plan.total_pages,
                    "segments_created": len(plan.segment_ids),
                    "ocr_strategy": self._ocr_strategy,
                },
            )
        except Exception:
            owner = await self.service.db.get_special_publication_manifest(execution_id)
            if owner and owner.get("phase") == "preparing":
                await coordinator.abort_preparing(
                    dataset, execution_id, generation_id, source_hash,
                    document_shared_lease_held=True,
                )
            raise

    async def _process_scanned(
        self,
        task: KnowledgeIngestTask,
        doc: dict,
        index_config_override: dict[str, Any] | None = None,
        stage_receipt: list[dict[str, str]] | None = None,
    ) -> None:
        """Process a scanned document using VisionPDFProcessor.

        New flow:
        1. Download to temp file
        2. If file > pdf_split_max_size_bytes, split into parts
        3. For each part:
           - VisionPDFProcessor generates vision embeddings
           - Simultaneously, VLM OCR extracts text (reusing rendered images)
        4. Merge all part texts as a candidate for publication
        5. Run hierarchical indexer or standard ingestion for text vectors
        6. Cleanup temp files
        """

        chunking_override = (
            copy.deepcopy(index_config_override.get("chunking", {}))
            if isinstance(index_config_override, dict)
            else None
        )

        if not self.vision_processor:
            if self.vlm_ocr_service:
                logger.info("[Worker] No VisionPDFProcessor, using VLM OCR text extraction for scanned PDF")
                await self._process_scanned_with_vlm_ocr(
                    task,
                    doc,
                    chunking_config_override=chunking_override,
                    index_config_override=index_config_override,
                )
                return
            logger.warning(
                "[Worker] VisionPDFProcessor not available, falling back to standard processing"
            )
            await self._ingest_document(
                task,
                chunking_config_override=chunking_override,
                index_config_override=index_config_override,
            )
            return

        if callable(getattr(self.vision_processor, "prepare_document", None)):
            await self._process_scanned_candidate(
                task, doc, index_config_override=index_config_override,
            )
            return

        await self._require_special_rebuild_without_serving_version(
            task, doc, pathway="scanned image",
        )

        metadata = doc.get("metadata", {})
        original_key = metadata.get("original_file_key")

        if not original_key:
            raise ValueError("No original file key found for scanned document")

        # Get dataset info
        dataset = await self.service.db.get_dataset(task.dataset_id)
        if not dataset:
            raise ValueError(f"Dataset {task.dataset_id} not found")
        effective_index_config = (
            index_config_override
            if index_config_override is not None
            else dataset.get("index_config") or {}
        )
        if not isinstance(effective_index_config, dict):
            raise RuntimeError("dataset index_config is invalid")

        # Determine collection name for multimodal vectors
        vector_dim = 1024
        if self.vision_processor and getattr(self.vision_processor.embedder, "dimension", None):
            vector_dim = int(self.vision_processor.embedder.dimension)
        collection = f"kb_{task.dataset_id}_{vector_dim}"
        lexical_config = LexicalConfig.from_index_config(effective_index_config)
        if lexical_config.reads_bm25_v2 and not self.service.vector_store.bm25_v2_enabled:
            raise ValueError(
                "bm25_v2 active writes are unavailable while the service kill "
                "switch is off"
            )
        tenant_id = str(dataset.get("tenant_id") or "").strip()
        if not tenant_id:
            raise ValueError("dataset tenant_id is required for scanned-document indexing")
        base_collection = str(dataset.get("collection_name") or "")
        is_base_collection = bool(base_collection) and collection == base_collection

        # Ensure collection exists
        await self.service.vector_store.ensure_collection(
            dataset_id=task.dataset_id,
            dimension=vector_dim,
            collection_name=collection,
            tenant_id=tenant_id,
            **(
                {"lexical_config": lexical_config}
                if lexical_config.configured and is_base_collection
                else {
                    "lexical_config": LexicalConfig(),
                    "allow_lexical_transition": True,
                }
            ),
        )

        # Download original file to temp
        logger.info(f"[Worker] Downloading original file from storage: {original_key}")
        temp_path = await self._download_original_to_temp(original_key)

        import tempfile as _tempfile

        tmp_dir_obj: _tempfile.TemporaryDirectory | None = None
        try:
            tmp_dir_obj = _tempfile.TemporaryDirectory(prefix="scanned_split_")
            tmp_dir = tmp_dir_obj.name
            file_size = Path(temp_path).stat().st_size

            # Split if needed
            parts = [temp_path]  # default: single file = original
            split_results = None
            if self._pdf_split_enabled and file_size > self._pdf_split_max_size:
                from .pdf_splitter import PDFSplitter

                splitter = PDFSplitter(
                    max_size_bytes=self._pdf_split_max_size,
                    min_pages_per_part=self._pdf_split_min_pages,
                )
                # SPO-04 / K2: heavy sync fitz splitting off the event loop.
                split_results = await asyncio.to_thread(
                    splitter.split_pdf, temp_path, tmp_dir=tmp_dir
                )
                parts = [sr.path for sr in split_results]
                logger.info(
                    f"[Worker] Split {file_size / 1024 / 1024:.1f}MB PDF into {len(parts)} parts"
                )

            # Build text_extractor callback using VLM OCR (if available)
            text_extractor = None
            if self.vlm_ocr_service and self._ocr_strategy in ("vlm", "hybrid"):

                async def _text_extractor(img_bytes: bytes) -> str:
                    from .ocr_utils import OCRCConfig, ocr_image_bytes_auto

                    knowledge_settings = getattr(self.service.settings, "knowledge", None)
                    config = OCRCConfig.from_settings(knowledge_settings)
                    return await ocr_image_bytes_auto(
                        img_bytes,
                        vlm_ocr_service=self.vlm_ocr_service,
                        config=config,
                        strategy=self._ocr_strategy,
                    )

                text_extractor = _text_extractor

            # Process each part
            all_extracted_texts: dict[int, str] = {}
            extracted_text_chars = 0
            extracted_text_bytes = 0
            total_processed = 0
            total_pages_all = 0
            total_failed = 0
            total_segments = 0
            published_image_segment_ids: set[str] = set()

            for part_idx, part_path in enumerate(parts):
                part_bytes = await asyncio.to_thread(Path(part_path).read_bytes)
                page_offset = 0
                if split_results:
                    page_offset = split_results[part_idx].page_start

                _part_idx = part_idx  # Capture loop var to avoid closure issue

                async def on_progress(current: int, total: int, _pi: int = _part_idx) -> None:
                    # Scale progress across all parts (monotonically increasing)
                    base = int(5 + (_pi / len(parts)) * 85)
                    part_progress = int((current / max(total, 1)) * (85 / len(parts)))
                    await self.service.db.update_document_status(
                        task.document_id,
                        status="parsing",
                        progress=base + part_progress,
                    )

                result = await self.vision_processor.process(
                    pdf_bytes=part_bytes,
                    document_id=task.document_id,
                    dataset_id=task.dataset_id,
                    collection=collection,
                    on_progress=on_progress,
                    storage_service=self.service.image_storage_service,
                    tenant_id=tenant_id,
                    text_extractor=text_extractor,
                    page_offset=page_offset,
                )

                if result.success:
                    total_processed += result.processed_pages
                    total_pages_all += result.total_pages
                    total_failed += result.failed_pages
                    total_segments += result.segments_created
                    published_image_segment_ids.update(result.segment_ids or [])
                    if result.extracted_texts:
                        for page_number, page_text_value in result.extracted_texts.items():
                            page_text = str(page_text_value or "")
                            new_part = f"[Page {page_number}]\n{page_text}"
                            old_text = all_extracted_texts.get(page_number)
                            old_part = (
                                f"[Page {page_number}]\n{old_text}"
                                if old_text is not None
                                else ""
                            )
                            separator_size = 2 if old_text is None and all_extracted_texts else 0
                            next_chars = (
                                extracted_text_chars
                                - len(old_part)
                                + separator_size
                                + len(new_part)
                            )
                            next_bytes = (
                                extracted_text_bytes
                                - len(old_part.encode("utf-8"))
                                + separator_size
                                + len(new_part.encode("utf-8"))
                            )
                            _require_extracted_text_counts_budget(next_chars, next_bytes)
                            all_extracted_texts[page_number] = page_text
                            extracted_text_chars = next_chars
                            extracted_text_bytes = next_bytes
                else:
                    total_failed += result.total_pages
                    raise RuntimeError(
                        f"scanned PDF part {part_idx} failed: {result.error}"
                    )

                logger.info(
                    f"[Worker] Part {part_idx}/{len(parts)} done: "
                    f"{result.processed_pages} pages, {result.segments_created} segments"
                )

            if total_failed or total_segments != total_processed:
                raise RuntimeError(
                    "scanned image generation was incomplete; the previous serving "
                    "generation remains retained"
                )

            ocr_candidate_text: str | None = None
            candidate_metadata_patch: dict[str, Any] | None = None
            # Keep the serving original until the replacement text generation
            # is ready. A later OCR/index failure must not expose new text
            # beside old searchable segments.
            if all_extracted_texts:
                ordered_pages = sorted(all_extracted_texts.keys())
                full_text = "\n\n".join(
                    f"[Page {p}]\n{all_extracted_texts[p]}" for p in ordered_pages
                )
                full_text = _require_extracted_text_budget(full_text)
                ocr_candidate_text = full_text
                candidate_metadata_patch = {
                    "pages_processed": total_processed,
                    "total_pages": total_pages_all,
                    "segments_created": total_segments,
                    "ocr_strategy": self._ocr_strategy,
                    "ocr_provider": getattr(self.vlm_ocr_service, "provider", None),
                    "ocr_model": getattr(self.vlm_ocr_service, "model", None),
                    "ocr_task": getattr(self.vlm_ocr_service, "task", None),
                    "vlm_ocr_pages": len(all_extracted_texts),
                    "pdf_parts": len(parts),
                }

                logger.info(
                    f"[Worker] VLM OCR extracted text from {len(all_extracted_texts)} pages"
                )

            # Update vision stats
            if ocr_candidate_text is None:
                try:
                    await self.service.db.update_document_fields(
                        task.document_id,
                        {
                            "metadata": {
                                **(metadata or {}),
                                "pages_processed": total_processed,
                                "total_pages": total_pages_all,
                                "segments_created": total_segments,
                            }
                        },
                    )
                except Exception as e:
                    logger.debug(f"Failed to update vision metadata: {e}")

            logger.info(
                f"[Worker] Scanned document {task.document_id} vision processing completed: "
                f"{total_processed}/{total_pages_all} pages, {total_segments} segments"
            )

            # Run text ingestion for searchable text vectors
            await self.service.db.update_document_status(
                task.document_id, status="parsing", progress=90
            )

            hierarchical_opted_in = self._hierarchical_opted_in(
                effective_index_config
            )
            text_generation_succeeded = False
            if (
                all_extracted_texts
                and self.hierarchical_indexer
                and hierarchical_opted_in
            ):
                # PRD T9-1 grayscale gate: OCR text goes through the
                # hierarchical indexer only for explicitly opted-in datasets.
                ordered_pages = sorted(all_extracted_texts.keys())
                full_text = "\n\n".join(
                    f"[Page {p}]\n{all_extracted_texts[p]}" for p in ordered_pages
                )
                full_text = _require_extracted_text_budget(full_text)
                full_text = await self._apply_parsing_ir(
                    task,
                    full_text,
                    index_config=index_config_override,
                )
                try:
                    from .chunking import ChunkingConfig

                    chunking_config = (
                        ChunkingConfig.from_dict(chunking_override)
                        if chunking_override
                        else None
                    )
                    idx_result = await self.hierarchical_indexer.index_document(
                        document_id=task.document_id,
                        dataset_id=task.dataset_id,
                        text=full_text,
                        metadata=metadata,
                        chunking_config=chunking_config,
                    )
                    if not idx_result.success:
                        # PRD T9-1: a silently unsuccessful OCR index used to
                        # still close the run as completed.
                        self._note_stage_fallback(
                            stage_receipt,
                            stage="hierarchical_ocr_indexing",
                            error="; ".join(str(item) for item in idx_result.errors)
                            or "hierarchical OCR indexing reported no vectors",
                        )
                    text_generation_succeeded = bool(idx_result.success)
                    if text_generation_succeeded and ocr_candidate_text is not None:
                        await self.service.db.execute(
                            """
                            UPDATE documents
                            SET content = $1, metadata = metadata || $2::jsonb
                            WHERE document_id = $3
                            """,
                            ocr_candidate_text,
                            json.dumps(candidate_metadata_patch or {}),
                            task.document_id,
                        )
                    logger.info(
                        f"[Worker] Hierarchical text indexing done: "
                        f"L1={idx_result.l1_count}, L2={idx_result.l2_count}, L3={idx_result.l3_count}"
                    )
                except Exception as e:
                    logger.warning(f"Hierarchical indexing failed, falling back to standard: {e}")
                    # PRD T9-1: record the swallowed stage before retrying on
                    # the standard path so scan-vs-hierarchical failures stay
                    # distinguishable on the execution receipt.
                    self._note_stage_fallback(
                        stage_receipt, stage="hierarchical_ocr_indexing", error=e
                    )
                    fallback_manifest = await self._ingest_document(
                        task,
                        chunking_config_override=chunking_override,
                        index_config_override=index_config_override,
                        source_text_override=ocr_candidate_text,
                        candidate_metadata_patch=candidate_metadata_patch,
                    )
                    text_generation_succeeded = fallback_manifest is not None
            else:
                # Fallback to standard OCR-backed text ingestion
                fallback_manifest = await self._ingest_document(
                    task,
                    chunking_config_override=chunking_override,
                    index_config_override=index_config_override,
                    source_text_override=ocr_candidate_text,
                    candidate_metadata_patch=candidate_metadata_patch,
                )
                text_generation_succeeded = fallback_manifest is not None

            if not text_generation_succeeded:
                raise RuntimeError(
                    "scanned document text generation did not publish successfully"
                )
            if total_segments and len(published_image_segment_ids) != total_segments:
                raise RuntimeError(
                    "scanned image generation returned an incomplete segment manifest"
                )
            await self._finalize_scanned_image_generation(
                task,
                dataset=dataset,
                collection=collection,
                desired_segment_ids=published_image_segment_ids,
            )

            # Mark completed
            try:
                await self.service.db.update_document_status(
                    task.document_id, status="completed", progress=100
                )
            except Exception as e:
                logger.debug(f"Failed to update final status: {e}")

        except Exception:
            raise
        finally:
            await self._cleanup_temp_file(temp_path)
            if tmp_dir_obj is not None:
                with contextlib.suppress(Exception):
                    tmp_dir_obj.cleanup()
