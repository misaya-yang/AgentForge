"""Ingestion service for knowledge base.

Handles document processing, chunking, embedding, and indexing.
Migrated from KnowledgeService as part of Phase 2 refactoring (Step 4).
"""

from __future__ import annotations

import asyncio
import contextlib
import hashlib
import json
import math
import uuid
from concurrent.futures import ThreadPoolExecutor
from functools import partial
from typing import TYPE_CHECKING, Any

from ...config.settings import Settings
from ...core.exceptions import ValidationFailedError
from ...core.observability.logging import get_logger
from ...persistence.database import (
    DOCUMENT_LIFECYCLE_REINDEX_KEY,
    DOCUMENT_PENDING_RESTORE_VERSION_KEY,
    DOCUMENT_RESTORED_SOURCE_VERSION_KEY,
    INDEX_PUBLICATION_REVISION_RESERVE,
    DatabaseStorage,
    IndexLeaseUnavailableError,
    dataset_index_deletion_fence,
    dataset_ingestion_identity,
)
from .chunking import (
    MAX_CHUNK_OUTPUTS,
    ChunkingConfig,
    ChunkingMode,
    enforce_token_limits,
    flatten_chunks,
    merge_small_chunks,
    process_document,
    require_chunk_output_budget,
    validate_persisted_chunking_config,
)
from .common import ensure_dict as _ensure_dict
from .common import maybe_await
from .embedding import BaseEmbedding
from .extracted_text_budget import MAX_EXTRACTED_TEXT_BYTES as MAX_EXTRACTED_TEXT_BYTES
from .extracted_text_budget import MAX_EXTRACTED_TEXT_CHARS as MAX_EXTRACTED_TEXT_CHARS
from .extracted_text_budget import _require_extracted_text_budget as _require_extracted_text_budget
from .extracted_text_budget import (
    _require_extracted_text_counts_budget as _require_extracted_text_counts_budget,
)
from .ingestion import ExtractedImage as IngestionExtractedImage
from .lexical_config import LexicalConfig, LexicalConfigError
from .pdf_image_processor import ExtractedImage
from .vector_store import VectorStore

if TYPE_CHECKING:
    from .knowledge_service import KnowledgeService


_INDEX_ROLLBACK_PAYLOAD_KEY = "_kb_index_rollback"


def _require_structured_parsing_budget(value: Any) -> list[dict[str, Any]]:
    if not isinstance(value, dict):
        raise ValidationFailedError("structured parsing receipt must be an object")
    chunks = value.get("chunks")
    if not isinstance(chunks, list) or len(chunks) > MAX_CHUNK_OUTPUTS:
        raise ValidationFailedError(
            f"structured parsing exceeds the {MAX_CHUNK_OUTPUTS} chunk limit"
        )
    total_chars = 0
    total_bytes = 0
    for chunk in chunks:
        if not isinstance(chunk, dict):
            raise ValidationFailedError("structured parsing chunks must be objects")
        text = str(chunk.get("text", chunk.get("content", "")) or "")
        total_chars += len(text)
        total_bytes += len(text.encode("utf-8"))
        if (
            total_chars > MAX_EXTRACTED_TEXT_CHARS
            or total_bytes > MAX_EXTRACTED_TEXT_BYTES
        ):
            raise ValidationFailedError("structured parsing content exceeds the ingestion budget")
    return chunks


def _require_dataset_index_writable(dataset: dict[str, Any]) -> None:
    try:
        deletion_fence = dataset_index_deletion_fence(dataset)
    except RuntimeError as exc:
        raise ValidationFailedError(str(exc)) from exc
    if deletion_fence is not None:
        raise ValidationFailedError(
            "dataset index deletion is pending; ingestion is unavailable"
        )
    try:
        LexicalConfig.from_index_config(
            _ensure_dict(dataset.get("index_config"))
        )
    except LexicalConfigError as exc:
        raise ValidationFailedError(str(exc)) from exc

logger = get_logger(__name__)


class _ImageReceiptPersistenceError(RuntimeError):
    """A complete re-extracted image source generation could not be published."""


class _Bm25V2WriteDisabled(ValidationFailedError):
    """Kill-switch refusal that must not mutate pipeline state."""


def _ingestion_dataset_identity(dataset: dict[str, Any]) -> str:
    """Canonical identity for choices that change persisted index generations."""

    return dataset_ingestion_identity(dataset)


def _stable_segment_id(document_id: str, content_type: str, position: int) -> str:
    """Deterministic lineage id for a chunk position (PRD T1.3 stable identity).

    A crashed or replayed generation re-derives the SAME id for the same
    (document, content_type, position), so its re-run upserts the rows and
    Qdrant points it staged earlier instead of duplicating them. Mirrors the
    uuid5(NAMESPACE_URL, "ai-platform:...") convention used for Confluence
    document ids.
    """

    return str(
        uuid.uuid5(
            uuid.NAMESPACE_URL,
            f"ai-platform:kb-segment:{document_id}:{content_type}:{position}",
        )
    )


def _stable_index_node_id(document_id: str, content_type: str, position: int) -> str:
    """Stable lookup identity persisted to segments.index_node_id."""

    return f"{document_id}::{content_type}::{position}"


def _rollback_backup_point_id(
    dataset_id: str,
    point_id: str,
) -> str:
    """Dataset-unique disabled backup ID for crash-resumable publication.

    The ID deliberately excludes ``content_revision``: migration-076 triggers
    may advance the negative seqlock while a publish is active, but the same
    unfinished publication must still find its durable backups after restart.
    Dataset publications are serialized, so one backup slot per original point
    is sufficient; a fresh publication overwrites any disabled cleanup orphan.
    """

    return str(
        uuid.uuid5(
            uuid.NAMESPACE_URL,
            f"ai-platform:kb-index-rollback:{dataset_id}:{point_id}",
        )
    )


def _process_standard_chunks(
    text: str,
    chunking_config: ChunkingConfig,
    document_id: str,
    document_name: str,
    dataset_id: str,
) -> list[Any]:
    """Run CPU-heavy chunking and normalization outside the API event loop."""
    chunk_objects = process_document(text, chunking_config, document_id)
    flat_chunks = flatten_chunks(chunk_objects)
    if chunking_config.mode not in (
        ChunkingMode.FIXED_SIZE,
        ChunkingMode.HIERARCHICAL,
    ):
        flat_chunks = merge_small_chunks(
            flat_chunks,
            min_size=chunking_config.min_chunk_size,
            max_size=chunking_config.max_chunk_size,
            min_tokens=None,
            max_tokens=None,
        )
    if chunking_config.mode == ChunkingMode.FIXED_SIZE and chunking_config.use_token_count:
        token_limit = int(chunking_config.token_limit or 0)
        if token_limit > 0:
            flat_chunks = enforce_token_limits(
                flat_chunks,
                token_limit,
                min_tokens=None,
            )
    if chunking_config.mode not in (
        ChunkingMode.FIXED_SIZE,
        ChunkingMode.HIERARCHICAL,
    ):
        flat_chunks = merge_small_chunks(
            flat_chunks,
            min_size=chunking_config.min_chunk_size,
            max_size=chunking_config.max_chunk_size,
            min_tokens=None,
            max_tokens=None,
        )
    for index, chunk in enumerate(flat_chunks):
        chunk.index = index
        chunk.metadata["chunk_index"] = index
        chunk.metadata["paragraph_index"] = index
        chunk.metadata["source_document"] = document_name
        chunk.metadata["source_document_id"] = document_id
        chunk.metadata["source_dataset_id"] = dataset_id
    require_chunk_output_budget(flat_chunks)
    return flat_chunks


class IngestionService:
    """Service for ingesting documents into knowledge base.

    Accepts a ``_ks`` (parent KnowledgeService) reference for shared resources
    like ``multimodal_embedding``, ``image_storage_service``, ``embedding_manager``,
    etc.  Set post-init by the parent because vector_store is created after
    sub-service construction.
    """

    _ks: KnowledgeService | None

    def __init__(
        self,
        settings: Settings,
        database: DatabaseStorage,
        vector_store: VectorStore,
    ):
        self.settings = settings
        self.db = database
        self.vector_store = vector_store
        self._ks = None  # Set post-init by KnowledgeService
        knowledge_settings = getattr(settings, "knowledge", None)
        cpu_workers = max(
            int(getattr(knowledge_settings, "worker_concurrency", 2) or 2),
            1,
        )
        self._cpu_executor = ThreadPoolExecutor(
            max_workers=cpu_workers,
            thread_name_prefix="knowledge-ingestion-cpu",
        )
        self._cpu_semaphore = asyncio.Semaphore(cpu_workers)

    async def _run_cpu(self, fn, /, *args):
        if not hasattr(self, "_cpu_semaphore") or not hasattr(
            self,
            "_cpu_executor",
        ):
            # Compatibility for narrow unit fixtures that intentionally bypass
            # __init__. Production instances always use the dedicated pool.
            return await asyncio.to_thread(fn, *args)
        async with self._cpu_semaphore:
            loop = asyncio.get_running_loop()
            return await loop.run_in_executor(
                self._cpu_executor,
                partial(fn, *args),
            )

    async def close(self) -> None:
        executor = getattr(self, "_cpu_executor", None)
        if executor is not None:
            executor.shutdown(wait=False, cancel_futures=True)

    @staticmethod
    def _parsing_cascade_config(
        index_config: dict[str, Any],
    ) -> tuple[Any, dict[str, Any]] | None:
        """Use the same pure parser contract as Dataset configuration writes."""
        from .parsing.config_validation import resolve_parsing_config

        return resolve_parsing_config(index_config)

    async def _load_or_parse_document_ir(
        self,
        *,
        dataset: dict[str, Any],
        document: dict[str, Any],
        index_config: dict[str, Any],
        source_text: str,
    ) -> str:
        """Return the chunking input from a durable document-generation IR.

        A matching generation/config row is the authority for rechunk and is
        rendered without invoking a parser.  On a miss, ParserCascade writes
        each accepted page to PostgreSQL before the document IR is published.
        The current legacy adapter supplies one exact text page; page-oriented
        PDF/scanned producers can pass multiple jobs through the same cache
        without changing this persistence contract.
        """

        resolved = self._parsing_cascade_config(index_config)
        if resolved is None:
            return source_text
        config, config_payload = resolved

        from .parsing import (
            DocIR,
            PageJob,
            PageSignals,
            ParserCascade,
            PostgresPageCache,
            render_document_markdown,
        )

        tenant_id = str(dataset.get("tenant_id") or "").strip()
        dataset_id = str(dataset.get("dataset_id") or "").strip()
        document_id = str(document.get("document_id") or "").strip()
        if not tenant_id or not dataset_id or not document_id:
            raise ValidationFailedError("parsing IR ownership is incomplete")

        source_bytes = source_text.encode("utf-8")
        content_hash = hashlib.sha256(source_bytes).hexdigest()
        generation_key = (
            f"v{int(document.get('current_version') or 1)}:{content_hash}"
        )
        config_json = json.dumps(
            config_payload,
            ensure_ascii=False,
            sort_keys=True,
            separators=(",", ":"),
        )
        parser_config_hash = hashlib.sha256(config_json.encode("utf-8")).hexdigest()

        probe = ParserCascade(config)
        parser_bundle = probe.bundle_version()
        load_ir = getattr(self.db, "load_parsing_ir", None)
        store_ir = getattr(self.db, "store_parsing_ir", None)
        if not callable(load_ir) or not callable(store_ir):
            raise RuntimeError("parsing IR persistence is unavailable")
        row = await load_ir(
            tenant_id=tenant_id,
            dataset_id=dataset_id,
            document_id=document_id,
            generation_key=generation_key,
            parser_bundle=parser_bundle,
            parser_config_hash=parser_config_hash,
        )
        if row is not None:
            payload = row.get("ir")
            if isinstance(payload, str):
                payload = json.loads(payload)
            if not isinstance(payload, dict):
                raise RuntimeError("persisted document parsing IR is malformed")
            doc_ir = DocIR.from_dict(payload)
            if doc_ir.doc_id != document_id or doc_ir.content_hash != content_hash:
                raise RuntimeError("persisted document parsing IR identity mismatch")
            return render_document_markdown(doc_ir)

        job = PageJob(
            doc_id=document_id,
            page_number=1,
            content_hash=content_hash,
            text_layer=source_text,
            filename=str(document.get("title") or ""),
            mime=str(document.get("mime_type") or "") or None,
            signals=PageSignals.derive(
                text_layer=source_text,
                mime=str(document.get("mime_type") or "") or None,
            ),
            options={"parser_config_hash": parser_config_hash},
        )
        page_cache = PostgresPageCache(
            self.db,
            tenant_id=tenant_id,
            dataset_id=dataset_id,
            document_id=document_id,
            generation_key=generation_key,
            parser_config_hash=parser_config_hash,
            page_content_hashes={1: content_hash},
        )
        cascade = ParserCascade(config, cache=page_cache)
        doc_ir = await cascade.parse_document(
            document_id,
            [job],
            content_hash=content_hash,
            filename=str(document.get("title") or ""),
            mime=str(document.get("mime_type") or "") or None,
            metadata={
                "parser_config_hash": parser_config_hash,
                "source_text_sha256": content_hash,
            },
        )
        rendered = render_document_markdown(doc_ir)
        text_layer_only = all(
            page.parser == "text_layer" for page in doc_ir.pages
        )
        if text_layer_only and rendered.encode("utf-8") != source_bytes:
            raise RuntimeError("text-layer IR changed the source byte boundaries")
        stored = await store_ir(
            tenant_id=tenant_id,
            dataset_id=dataset_id,
            document_id=document_id,
            generation_key=generation_key,
            content_hash=content_hash,
            schema_version=doc_ir.schema_version,
            parser_bundle=cascade.bundle_version(),
            parser_config_hash=parser_config_hash,
            cascade_config=config_payload,
            ir=doc_ir.to_dict(),
            stats=doc_ir.stats(),
        )
        if not stored:
            raise RuntimeError("parsing IR lost document ownership during publication")
        return rendered

    # ========================================================================
    # Main Ingestion Pipeline
    # ========================================================================

    @staticmethod
    def _has_complete_durable_image_receipt(
        metadata: dict[str, Any],
        *,
        processing_mode: str,
    ) -> bool:
        if processing_mode not in {"multimodal", "scanned"}:
            return False
        images = metadata.get("extracted_images")
        if not isinstance(images, list) or not images:
            return False
        if any(
            not isinstance(image, dict)
            or not all(
                str(image.get(key) or "").strip()
                for key in ("image_id", "storage_url", "storage_key")
            )
            for image in images
        ):
            return False
        try:
            declared_count = int(metadata.get("image_count") or 0)
        except (TypeError, ValueError):
            return False
        return declared_count == len(images)

    async def _persist_reextracted_image_receipt(
        self,
        *,
        dataset_id: str,
        tenant_id: str,
        document_id: str,
        processing_mode: str,
        doc_metadata: dict[str, Any],
        images: list[IngestionExtractedImage],
    ) -> dict[str, Any]:
        """Publish one all-or-nothing durable source receipt for extracted images."""

        storage = getattr(self._ks, "image_storage_service", None)
        publish_receipt = getattr(self.db, "publish_document_image_receipt", None)
        delete_image = getattr(storage, "delete_image", None)
        if storage is None or not callable(publish_receipt) or not callable(delete_image):
            raise _ImageReceiptPersistenceError(
                "durable image receipt publication is unavailable"
            )

        upload_semaphore = asyncio.Semaphore(5)

        upload_specs = [
            (
                idx,
                image,
                f"reindex_{image.image_id}",
                image.filename
                or f"image_{idx}.{image.mime_type.split('/')[-1]}",
            )
            for idx, image in enumerate(images)
        ]

        async def upload_single_image(
            idx: int,
            img: IngestionExtractedImage,
            attachment_id: str,
            storage_filename: str,
        ) -> tuple[dict[str, Any], str, str]:
            async with upload_semaphore:
                page_number = (
                    getattr(img, "page_number", None)
                    or img.metadata.get("page_number")
                    if hasattr(img, "metadata")
                    else None
                )
                storage_url = await storage.upload_image(
                    tenant_id=tenant_id,
                    document_id=document_id,
                    attachment_id=attachment_id,
                    filename=storage_filename,
                    content=img.content,
                    content_type=img.mime_type,
                    metadata={
                        "width": str(img.width),
                        "height": str(img.height),
                        "source_location": img.source_location,
                        "page_number": str(page_number) if page_number else str(idx),
                    },
                )
                if not str(storage_url or "").strip():
                    raise _ImageReceiptPersistenceError(
                        f"image {img.image_id} storage returned no durable URL"
                    )
                actual_storage_key = storage._generate_key(
                    tenant_id,
                    document_id,
                    attachment_id,
                    storage_filename,
                )
                return (
                    {
                        "image_id": img.image_id,
                        "storage_url": storage_url,
                        "storage_key": actual_storage_key,
                        "filename": storage_filename,
                        "mime_type": img.mime_type,
                        "width": img.width,
                        "height": img.height,
                        "page_number": page_number,
                        "size_bytes": img.size_bytes,
                        "context_text": img.context_text[:200]
                        if img.context_text
                        else "",
                        "source_location": img.source_location,
                    },
                    attachment_id,
                    storage_filename,
                )

        results = await asyncio.gather(
            *(upload_single_image(*spec) for spec in upload_specs),
            return_exceptions=True,
        )
        completed = [result for result in results if not isinstance(result, BaseException)]
        failures = [result for result in results if isinstance(result, BaseException)]

        async def compensate_completed_uploads() -> None:
            cleanup_results = await asyncio.gather(
                *(
                    delete_image(
                        tenant_id=tenant_id,
                        document_id=document_id,
                        attachment_id=attachment_id,
                        filename=filename,
                    )
                    for _idx, _image, attachment_id, filename in upload_specs
                ),
                return_exceptions=True,
            )
            cleanup_failures = [
                result
                for result in cleanup_results
                if isinstance(result, BaseException) or result is not True
            ]
            if cleanup_failures:
                raise _ImageReceiptPersistenceError(
                    "partial image upload compensation was incomplete"
                )

        if failures or len(completed) != len(images):
            await compensate_completed_uploads()
            raise _ImageReceiptPersistenceError(
                "one or more extracted images could not be stored durably"
            ) from (failures[0] if failures else None)

        receipts = [receipt for receipt, _attachment_id, _filename in completed]
        try:
            published = await publish_receipt(
                document_id,
                dataset_id,
                expected_original_file_key=str(
                    doc_metadata.get("original_file_key") or ""
                ),
                expected_processing_mode=processing_mode,
                extracted_images=receipts,
            )
        except BaseException as exc:
            try:
                await asyncio.shield(compensate_completed_uploads())
            except BaseException as cleanup_exc:
                raise _ImageReceiptPersistenceError(
                    "image receipt publication failed and storage compensation was incomplete"
                ) from cleanup_exc
            raise _ImageReceiptPersistenceError(
                "image receipt publication failed"
            ) from exc
        if not published:
            await compensate_completed_uploads()
            raise _ImageReceiptPersistenceError(
                "image receipt publication lost document generation authority"
            )

        published_metadata = dict(doc_metadata)
        published_metadata["extracted_images"] = receipts
        published_metadata["image_count"] = len(receipts)
        return published_metadata

    async def ingest_document(
        self,
        dataset_id: str,
        document_id: str,
        *,
        chunking_config_override: dict[str, Any] | None = None,
        index_config_override: dict[str, Any] | None = None,
        source_text_override: str | None = None,
        candidate_metadata_patch: dict[str, Any] | None = None,
    ) -> list[str] | None:
        """Run the full pipeline for one document.

        ``chunking_config_override`` carries the replay snapshot's pinned
        chunking config (addendum §1-T1.3): reprocess/recover replay the
        snapshot version so an in-flight document cannot drift to a config
        changed after submission. Returns the staged segment manifest for the
        execution ledger (PRD T1.5), or None on failure paths.
        """
        try:
            logger.info(f"Ingest started for document {document_id} (dataset={dataset_id})")
            dataset = await self._ks._get_dataset_or_404(dataset_id)
            _require_dataset_index_writable(dataset)
            if index_config_override is None:
                index_config = _ensure_dict(dataset.get("index_config"))
            elif isinstance(index_config_override, dict):
                index_config = dict(index_config_override)
            else:
                raise ValidationFailedError("pinned index_config must be an object")
            if chunking_config_override is not None:
                chunking_config_dict = dict(chunking_config_override)
            else:
                chunking_config_dict = index_config.get("chunking", {})
            validate_persisted_chunking_config(chunking_config_dict)
            ingestion_identity = _ingestion_dataset_identity(dataset)
            try:
                lexical_config = LexicalConfig.from_index_config(index_config)
            except LexicalConfigError as exc:
                raise ValidationFailedError(str(exc)) from exc
            if lexical_config.reads_bm25_v2 and not self.vector_store.bm25_v2_enabled:
                raise _Bm25V2WriteDisabled(
                    "bm25_v2 active writes are unavailable while the service kill "
                    "switch is off"
                )
            dataset_tenant_id = str(dataset.get("tenant_id") or "").strip()
            if not dataset_tenant_id:
                raise ValidationFailedError("dataset tenant_id is required for indexing")
            doc = await self.db.get_document(document_id)
            if not doc or str(doc.get("dataset_id")) != dataset_id:
                raise ValidationFailedError("document not found")

            doc_meta = _ensure_dict(doc.get("metadata"))
            restored_source_version = doc_meta.get(DOCUMENT_RESTORED_SOURCE_VERSION_KEY)
            if restored_source_version is not None:
                if (
                    type(restored_source_version) is not int
                    or restored_source_version <= 0
                    or int(doc.get("current_version") or 0) != restored_source_version
                ):
                    raise ValidationFailedError("restored source identity is invalid")
                restored = await self.db.get_document_version(
                    document_id, restored_source_version,
                )
                if (
                    not isinstance(restored, dict)
                    or restored.get("change_type") != "restored"
                    or str(restored.get("content") or "") != str(doc.get("content") or "")
                ):
                    raise ValidationFailedError("restored source no longer matches its version")
            pending_restore = doc_meta.get(DOCUMENT_PENDING_RESTORE_VERSION_KEY)
            pending_restore_version: int | None = None
            pending_previous_version: int | None = None
            if pending_restore is not None:
                if source_text_override is not None:
                    raise ValidationFailedError("restore candidate cannot override another source")
                if not isinstance(pending_restore, dict):
                    raise ValidationFailedError("restore candidate identity is invalid")
                pending_restore_version = pending_restore.get("candidate_version")
                pending_previous_version = pending_restore.get("previous_version")
                if (
                    type(pending_restore_version) is not int
                    or type(pending_previous_version) is not int
                    or pending_previous_version <= 0
                    or pending_restore_version <= pending_previous_version
                ):
                    raise ValidationFailedError("restore candidate identity is invalid")
                version = await self.db.get_document_version(
                    document_id,
                    pending_restore_version,
                    include_pending=True,
                )
                if (
                    not isinstance(version, dict)
                    or version.get("change_type") != "pending_restore"
                    or str(version.get("document_id") or "") != document_id
                ):
                    raise ValidationFailedError("restore candidate is unavailable")
                source_text_override = str(version.get("content") or "")
            raw_text = _require_extracted_text_budget(
                source_text_override if source_text_override is not None else doc.get("content")
            )
            candidate_content: str | None = (
                raw_text if source_text_override is not None else None
            )
            if "structured_parsing" in doc_meta:
                raise ValidationFailedError(
                    "structured parsing is disabled until a trusted source receipt exists"
                )

            await self.db.update_document_status(document_id, status="parsing", progress=10)

            min_chars = getattr(self.settings.knowledge, "pdf_min_text_chars_for_ocr", 200)

            # Re-extract from original file when content is empty. If upload already ran
            # extraction (ocr_processed), we still retry here because empty content
            # indicates extraction/OCR likely failed.
            processing_mode = str(
                doc_meta.get("processing_mode") or "text_only"
            ).strip().lower()
            image_processing_mode = processing_mode in {"multimodal", "scanned"}
            original_key = doc_meta.get("original_file_key")
            doc_already_processed = doc_meta.get("ocr_processed", False)
            if (
                source_text_override is None
                and restored_source_version is None
                and original_key
                and self._ks.image_storage_service
                and len(raw_text.strip()) < min_chars
            ):
                if doc_already_processed:
                    logger.info(
                        "Document content below OCR threshold despite ocr_processed=true; re-extracting from original file"
                    )
                try:
                    logger.info(f"Downloading original file for re-extraction: {original_key}")
                    original_bytes = await self._ks.image_storage_service.download_original_file(
                        original_key
                    )
                    original_filename = doc_meta.get("original_filename", "")
                    original_mime = doc_meta.get("original_mime_type", "")

                    # Re-run full extraction pipeline (multimodal may skip OCR)
                    used_multimodal = False
                    if (
                        image_processing_mode
                        and self._ks.multimodal_embedding
                        and self._ks.image_storage_service
                    ):
                        extraction_result = await self._ks.document_image_extractor.extract(
                            filename=original_filename,
                            content=original_bytes,
                        )
                        re_text = extraction_result.text
                        re_extracted_images = extraction_result.embeddable_images
                        used_multimodal = True
                        name_lower = original_filename.lower()
                        if name_lower.endswith(".pdf") or "pdf" in (original_mime or "").lower():
                            min_images = getattr(
                                self.settings.knowledge, "scanned_min_images_for_image_only", 5
                            )
                            embeddable_count = (
                                len(re_extracted_images) if re_extracted_images else 0
                            )
                            if embeddable_count >= min_images:
                                logger.info(
                                    f"Re-extraction: Scanned PDF with {embeddable_count} images, "
                                    f"using multimodal embeddings only"
                                )
                        # Persist extracted images for downstream multimodal embedding
                        if re_extracted_images and not doc_meta.get("extracted_images"):
                            tenant_id = str(dataset.get("tenant_id") or "").strip()
                            if not tenant_id:
                                raise _ImageReceiptPersistenceError(
                                    "dataset tenant is required for durable image storage"
                                )
                            doc_meta = await self._persist_reextracted_image_receipt(
                                dataset_id=dataset_id,
                                tenant_id=tenant_id,
                                document_id=document_id,
                                processing_mode=processing_mode,
                                doc_metadata=doc_meta,
                                images=list(re_extracted_images),
                            )
                            doc["metadata"] = doc_meta
                            logger.info(
                                "[Reindex] Persisted %s images for multimodal embedding",
                                len(re_extracted_images),
                            )
                    else:
                        re_text, _ = await asyncio.to_thread(
                            self._ks._extract_text_from_bytes,
                            original_bytes,
                            original_filename,
                            original_mime,
                        )

                    # If multimodal extraction produced too little text for PDFs, run OCR fallback
                    min_chars = getattr(self.settings.knowledge, "pdf_min_text_chars_for_ocr", 200)
                    if (
                        used_multimodal
                        and len((re_text or "").strip()) < min_chars
                        and (
                            original_filename.lower().endswith(".pdf")
                            or "pdf" in (original_mime or "").lower()
                        )
                        and getattr(self.settings.knowledge, "ocr_enabled", True)
                    ):
                        ocr_text = await asyncio.to_thread(self._ks._ocr_pdf_bytes, original_bytes)
                        if ocr_text and ocr_text.strip():
                            re_text = ocr_text

                    re_text = _require_extracted_text_budget(re_text)
                    if re_text and len(re_text.strip()) > len(raw_text.strip()):
                        raw_text = re_text
                        # Keep the old serving original until its replacement
                        # segments commit. A failed embed must not expose new
                        # raw text beside the old searchable chunks.
                        candidate_content = raw_text
                        logger.info(
                            f"Re-extracted {len(raw_text)} chars from original file "
                            f"(was {len(str(doc.get('content') or ''))} chars)"
                        )
                except _ImageReceiptPersistenceError:
                    raise
                except ValidationFailedError:
                    raise
                except Exception as e:
                    logger.warning(
                        f"Failed to re-extract from original file: {e}, using stored content"
                    )

            raw_text = await self._load_or_parse_document_ir(
                dataset=dataset,
                document=doc,
                index_config=index_config,
                source_text=_require_extracted_text_budget(raw_text),
            )
            text = _require_extracted_text_budget(raw_text).strip()
            has_image_generation = self._has_complete_durable_image_receipt(
                doc_meta,
                processing_mode=processing_mode,
            )
            if not text and not has_image_generation:
                await self.db.update_document_status(
                    document_id, status="error", progress=100, error="empty document"
                )
                return

            await self.db.update_document_status(document_id, status="splitting", progress=25)

            # Check if structured parsing results are available (for enhanced multimodal docs)
            structured_parsing = doc_meta.get("structured_parsing", {})
            use_structured_chunks = (
                structured_parsing.get("enabled")
                and structured_parsing.get("chunks")
                and len(structured_parsing.get("chunks", [])) > 0
            )

            doc_name = str(doc.get("name") or doc.get("title") or document_id)

            logger.info(f"Chunking config for document {document_id}: {chunking_config_dict}")
            chunking_config = ChunkingConfig.from_dict(chunking_config_dict)
            logger.info(
                f"Parsed chunking config: mode={chunking_config.mode.value}, "
                f"chunk_size={chunking_config.chunk_size}, overlap={chunking_config.chunk_overlap}"
            )

            # Fixed-size mode always uses strict fixed chunking (ignore structured parsing)
            if use_structured_chunks and chunking_config.mode != ChunkingMode.FIXED_SIZE:
                # Use structured parsing results for intelligent chunking
                logger.info(f"Using structured parsing results for document {document_id}")
                structured_chunks = _require_structured_parsing_budget(
                    structured_parsing
                )
                flat_chunks = self._ks._convert_structured_chunks(
                    structured_chunks,
                    document_id,
                    doc_name,
                    dataset_id,
                )
                # Enforce token limits and merge tiny fragments for structured chunks
                flat_chunks = self._ks._normalize_structured_chunks(flat_chunks, chunking_config)
                # Re-merge tiny fragments after strict splitting (avoid micro-chunks)
                if chunking_config.mode not in (
                    ChunkingMode.FIXED_SIZE,
                    ChunkingMode.HIERARCHICAL,
                ):
                    flat_chunks = merge_small_chunks(
                        flat_chunks,
                        min_size=chunking_config.min_chunk_size,
                        max_size=chunking_config.max_chunk_size,
                        min_tokens=None,
                        max_tokens=None,
                    )
                logger.info(
                    f"Created {len(flat_chunks)} chunks from structured parsing (normalized)"
                )
            else:
                # Standard chunking flow
                flat_chunks = await self._run_cpu(
                    _process_standard_chunks,
                    text,
                    chunking_config,
                    document_id,
                    doc_name,
                    dataset_id,
                )
                logger.info(
                    f"Generated {len(flat_chunks)} chunks for document {document_id}"
                )

            require_chunk_output_budget(flat_chunks)

            # Convert to the format expected by the rest of the pipeline
            # Include content_hash for incremental update detection
            # Hash the ORIGINAL text (before contextual prefix) so prefix format
            # changes don't invalidate hashes and force unnecessary re-embedding.
            chunks = [
                (
                    c.text,
                    c.token_count,
                    hashlib.sha256(
                        (c.metadata.get("original_text") or c.text).encode()
                    ).hexdigest(),
                    c.metadata,
                )
                for c in flat_chunks
            ]

            if not chunks and not has_image_generation:
                await self.db.update_document_status(
                    document_id, status="error", progress=100, error="no segments generated"
                )
                return

            # Get existing segment hashes for incremental update comparison
            existing_hashes = await self.db.get_segment_hashes_by_document(
                document_id, content_type="text"
            )

            # Classify chunks for the T1 stable-identity incremental upsert:
            # - unchanged: same position AND content_hash AND the row already
            #   serves (status != 'indexing') -> skipped entirely, zero
            #   re-embedding; the row keeps serving with its existing vector.
            # - staged: same position AND content_hash but the row is still in
            #   staging (status='indexing') from an earlier crashed or replayed
            #   generation -> no re-embedding (its vector was persisted
            #   atomically with the row); it only joins the completion flip.
            # - changed: same position, different hash -> KEEP the existing
            #   row's segment_id/vector_id so the row is updated in place and
            #   the Qdrant point is upserted at the SAME point id (no identity
            #   rotation, no FK churn, no delete-then-insert window).
            # - new: no row at that position -> deterministic uuid5 lineage id
            #   so a crash/replay re-derives the same id instead of
            #   duplicating rows and points.
            # New/changed rows are staged enabled=false + status='indexing'
            # and flipped to serving only after the WHOLE generation succeeds;
            # unchanged rows keep serving until then (addendum §1-T1.4).
            # Excess old rows are deleted only after staging succeeds.
            chunks_to_embed = []  # (position, text, token_count, hash, meta, old_seg)
            unchanged_segments = []  # serving segment_ids kept as-is
            staged_resumable = []  # staged segment_ids only needing the flip
            staged_candidates = []  # chunk inputs whose staged points need proof
            excess_segments = []  # old segment_ids beyond the new chunk range
            excess_vectors = []  # vector ids owned by excess segments

            for pos, (text, token_count, content_hash, chunk_meta) in enumerate(chunks):
                old_seg = existing_hashes.get(pos)
                if old_seg and old_seg.get("content_hash") == content_hash:
                    if str(old_seg.get("status") or "completed") == "indexing":
                        # A prior generation persisted this exact content but
                        # never flipped it to serving. Its vector must be
                        # checked before we can finish the flip.
                        staged_candidates.append(
                            (pos, text, token_count, content_hash, chunk_meta, old_seg)
                        )
                    else:
                        unchanged_segments.append(old_seg["segment_id"])
                        logger.debug(
                            f"Segment at position {pos} unchanged, skipping embed"
                        )
                else:
                    # Content changed or new - needs embedding
                    chunks_to_embed.append(
                        (pos, text, token_count, content_hash, chunk_meta, old_seg)
                    )

            # Find excess old segments (positions beyond new chunk count).
            # Changed positions reuse their point ids, so only excess rows own
            # vectors that must be deleted.
            max_new_pos = len(chunks) - 1
            max_existing_pos = max(existing_hashes.keys(), default=-1)
            for pos, seg_info in sorted(existing_hashes.items()):
                if pos > max_new_pos:
                    excess_segments.append(seg_info["segment_id"])
                    if seg_info.get("vector_id"):
                        excess_vectors.append(seg_info["vector_id"])

            logger.info(
                f"Incremental upsert for document {document_id}: "
                f"{len(unchanged_segments)} unchanged, "
                f"{len(staged_candidates)} staged candidates, "
                f"{len(chunks_to_embed)} to embed, "
                f"{len(excess_segments)} excess to delete after staging"
            )

            embedding_config = _ensure_dict(dataset.get("embedding_config"))

            # Check if this is a multimodal dataset - use unified embedding for cross-modal retrieval
            is_multimodal = self._ks._is_multimodal_dataset(dataset)
            skip_image_generation = False
            if has_image_generation and not is_multimodal:
                if not text:
                    raise RuntimeError(
                        "image-only documents require a unified multimodal dataset"
                    )
                receipt_count = len(doc_meta.get("extracted_images") or [])
                doc_meta = dict(doc_meta)
                doc_meta["image_indexing"] = {
                    "status": "skipped",
                    "reason": "text_only_dataset",
                    "receipt_count": receipt_count,
                    "indexed_count": 0,
                }
                await self.db.update_document_fields(
                    document_id,
                    {"metadata": doc_meta},
                    allow_lifecycle_marker_update=True,
                )
                doc["metadata"] = doc_meta
                skip_image_generation = True
                logger.info(
                    "Skipping %s durable image receipt(s) for text-only dataset %s; "
                    "text indexing continues",
                    receipt_count,
                    dataset_id,
                )

            embedder: BaseEmbedding | None = None
            embedding_provider_used = ""
            try:
                if is_multimodal:
                    # Use UnifiedMultimodalEmbedding for consistent text-image vector space
                    logger.info(
                        f"Using UnifiedMultimodalEmbedding for multimodal dataset {dataset_id}"
                    )
                    embedder = await maybe_await(
                        self._ks._get_unified_multimodal_embedder(
                            dataset, embedding_config
                        )
                    )
                    embedding_provider_used = "dashscope_multimodal"
                else:
                    # Use dataset-configured text embedder
                    embedder = await maybe_await(
                        self._ks._get_text_embedder(dataset, embedding_config)
                    )
                    embedding_provider_used = str(
                        dataset.get("embedding_provider") or getattr(embedder, "provider", "local")
                    )
                    logger.info(
                        f"Using {embedding_provider_used} text embedding for dataset {dataset_id} "
                        f"(batch_size={self.settings.knowledge.text_embedding_batch_size})"
                    )

                # If dimension is unknown, dry-run to fetch it.
                if embedder._dimension is None:
                    await embedder.embed_query("test")

                dim = embedder._dimension or 1024  # fallback
                await self._require_ingestion_identity(
                    dataset_id,
                    ingestion_identity,
                )
                collection = await self.vector_store.ensure_collection(
                    dataset_id=dataset_id,
                    dimension=dim,
                    collection_name=str(dataset.get("collection_name") or "") or None,
                    tenant_id=dataset_tenant_id,
                    **({"lexical_config": lexical_config} if lexical_config.configured else {}),
                )
                if staged_candidates:
                    staged_point_ids = [
                        str(candidate[5].get("vector_id") or candidate[5]["segment_id"])
                        for candidate in staged_candidates
                    ]
                    staged_points = await self._snapshot_points_for_rollback(
                        collection=collection,
                        point_ids=staged_point_ids,
                        tenant_id=dataset_tenant_id,
                        dataset_id=dataset_id,
                    )
                    for candidate, point_id in zip(
                        staged_candidates, staged_point_ids, strict=True,
                    ):
                        pos, chunk_text, _, _, _, old_seg = candidate
                        point = staged_points.get(point_id)
                        if point is None:
                            # A crash or external loss can leave a staged row
                            # without its vector. Rebuild it before activation.
                            chunks_to_embed.append(candidate)
                            continue
                        payload = getattr(point, "payload", None)
                        if not isinstance(payload, dict) or any(
                            payload.get(key) != value
                            for key, value in (
                                ("tenant_id", dataset_tenant_id),
                                ("dataset_id", dataset_id),
                                ("document_id", document_id),
                                ("segment_id", old_seg["segment_id"]),
                                ("position", pos),
                                ("text", chunk_text),
                            )
                        ) or payload.get("content_type", "text") != "text":
                            raise RuntimeError(
                                "staged text vector identity changed before replay"
                            )
                        staged_resumable.append(old_seg["segment_id"])
                    chunks_to_embed.sort(key=lambda chunk: chunk[0])
            except IndexLeaseUnavailableError:
                # A concurrent lifecycle or blue-green transition owns the
                # publication fence. This is retryable queue contention, not
                # a failed generation; let the durable worker put the claimed
                # row back to waiting without clearing the old index.
                raise
            except Exception as exc:
                await self._mark_document_failed_if_writable(
                    dataset_id,
                    document_id,
                    str(exc),
                )
                if embedder:
                    await embedder.close()
                return

            await self.db.update_document_status(document_id, status="indexing", progress=35)

            # Incremental update strategy:
            # 1. Only embed chunks that changed or are new
            # 2. Keep unchanged segments and vectors intact
            # 3. Delete excess old segments and their vectors

            segment_rows: list[dict[str, Any]] = []
            points = []
            overwritten_vector_ids: list[str] = []
            try:
                from qdrant_client.http import models as qmodels  # type: ignore

                # If no chunks need embedding, skip to cleanup
                if chunks_to_embed:
                    # Use provider-specific batch sizes from settings
                    # Gemini: 50 (supports 100), DashScope: 10
                    if is_multimodal:
                        batch_size = 10  # DashScope multimodal
                        max_concurrent = self.settings.knowledge.multimodal_embedding_max_concurrent
                    else:
                        batch_size = self.settings.knowledge.text_embedding_batch_size
                        max_concurrent = self.settings.knowledge.text_embedding_max_concurrent

                    total = len(chunks_to_embed)
                    embedded = 0

                    # OPTIMIZATION: Concurrent embedding for massive performance boost
                    # Split chunks into batches
                    batches = []
                    for i in range(0, total, batch_size):
                        batch = chunks_to_embed[i : i + batch_size]
                        batches.append((i // batch_size, batch))

                    # Process batches concurrently with configurable parallelism
                    effective_concurrent = min(max_concurrent, len(batches))
                    semaphore = asyncio.Semaphore(effective_concurrent)

                    logger.info(
                        f"Embedding {total} chunks in {len(batches)} batches "
                        f"(batch_size={batch_size}, max_concurrent={effective_concurrent})"
                    )

                    async def embed_single_batch(
                        batch_idx: int, batch: list
                    ) -> tuple[int, list, list]:
                        """Embed one batch; the provider owns retry and deadline."""
                        texts = [text for _, text, _, _, _, _ in batch]

                        async with semaphore:
                            try:
                                vectors = await embedder.embed_documents(texts)
                                if not isinstance(vectors, list) or len(vectors) != len(batch):
                                    raise RuntimeError("embedding batch is incomplete")
                                if any(
                                    not isinstance(vector, list)
                                    or len(vector) != dim
                                    or any(
                                        isinstance(value, bool)
                                        or not isinstance(value, (int, float))
                                        or not math.isfinite(value)
                                        for value in vector
                                    )
                                    for vector in vectors
                                ):
                                    raise RuntimeError("embedding batch contains an invalid vector")
                                return (batch_idx, vectors, batch)
                            except Exception as embed_err:
                                text_lengths = [len(t) for t in texts]
                                logger.error(
                                    f"Embedding failed for batch {batch_idx + 1}: {embed_err}. "
                                    f"Text lengths: {text_lengths}, Provider: {embedding_provider_used}"
                                )
                            return (batch_idx, [None] * len(batch), batch)

                    # Launch all embedding tasks concurrently
                    tasks = [embed_single_batch(idx, batch) for idx, batch in batches]

                    # Process results as they complete (for progressive updates).
                    # Yield to event loop between batches so API requests aren't starved.
                    failed_batches = 0
                    for coro in asyncio.as_completed(tasks):
                        batch_idx, vectors, batch = await coro
                        await asyncio.sleep(0)  # yield to event loop

                        # Build segments for this batch (skip if vectors are None)
                        for j, (
                            pos,
                            chunk_text,
                            token_count,
                            content_hash,
                            chunk_meta,
                            old_seg,
                        ) in enumerate(batch):
                            # Skip if embedding failed for this chunk
                            if vectors[j] is None:
                                failed_batches += 1
                                continue

                            # Changed content at an existing position: keep the
                            # row's identity so this is a true in-place upsert
                            # (same segment row, same Qdrant point id). A
                            # missing legacy id falls back to the deterministic
                            # lineage id.
                            seg_id = (
                                str(old_seg.get("segment_id") or "").strip()
                                if old_seg
                                else ""
                            )
                            if not seg_id:
                                seg_id = _stable_segment_id(document_id, "text", pos)
                            # Reuse the row's existing vector (point) id when it
                            # has one so the upsert replaces the vector in
                            # place; new positions key the point by segment id.
                            vector_id = (
                                str((old_seg or {}).get("vector_id") or "").strip()
                                or seg_id
                            )
                            if old_seg:
                                overwritten_vector_ids.append(vector_id)
                            seg_metadata = dict(chunk_meta) if chunk_meta else {}
                            seg_metadata["position"] = pos

                            display_text = seg_metadata.pop("original_text", chunk_text)

                            payload_meta = {
                                key: seg_metadata.get(key)
                                for key in (
                                    "source_type",
                                    "citation_text",
                                    "source_reference",
                                    "section_title",
                                    "section_full_path",
                                    "page_number",
                                    "chunk_index",
                                    "paragraph_index",
                                    "source_document",
                                    "document_title",
                                    "madhab",
                                    "language",
                                )
                                if seg_metadata.get(key) is not None
                            }

                            payload = {
                                "tenant_id": dataset_tenant_id,
                                "dataset_id": dataset_id,
                                "document_id": document_id,
                                "segment_id": seg_id,
                                "position": pos,
                                "text": chunk_text,
                                "token_count": token_count,
                                "source_type": payload_meta.get("source_type", "unknown"),
                                "language": payload_meta.get("language", "en"),
                                "metadata": payload_meta,
                                "citation_text": payload_meta.get("citation_text"),
                                "source_reference": payload_meta.get("source_reference"),
                            }
                            points.append(
                                qmodels.PointStruct(
                                    id=vector_id,
                                    vector=vectors[j],
                                    payload=payload,
                                )
                            )
                            segment_rows.append(
                                {
                                    "segment_id": seg_id,
                                    "dataset_id": dataset_id,
                                    "document_id": document_id,
                                    "position": pos,
                                    "text": display_text,
                                    "token_count": token_count,
                                    "vector_id": vector_id,
                                    "content_hash": content_hash,
                                    "metadata": seg_metadata,
                                    # T1 staging: new/changed rows persist
                                    # disabled and 'indexing' until the whole
                                    # generation succeeds; unchanged rows keep
                                    # serving until the completion flip.
                                    "enabled": False,
                                    "status": "indexing",
                                    "index_node_id": _stable_index_node_id(
                                        document_id, "text", pos
                                    ),
                                    "index_node_hash": content_hash,
                                }
                            )

                        embedded += len(batch)
                        progress = 35 + (embedded / max(total, 1)) * 55
                        await self.db.update_document_status(
                            document_id, status="indexing", progress=min(progress, 95)
                        )
                        logger.debug(
                            f"Batch {batch_idx + 1}/{len(batches)} embedded ({embedded}/{total} chunks)"
                        )

                    if failed_batches > 0:
                        raise RuntimeError(
                            f"Embedding failed for {failed_batches} chunks; "
                            "refusing a partial index replacement"
                        )

                    # Do not mutate either serving store here.  Image work and
                    # every other fallible preparation step must finish first;
                    # the short publication phase below snapshots old points,
                    # writes all Qdrant batches, and flips PostgreSQL once.
                    logger.info(
                        f"Prepared {len(points)} replacement points for document {document_id}"
                    )
                else:
                    logger.info(
                        f"All segments unchanged for document {document_id}, no embedding needed"
                    )

                await self._require_ingestion_identity(
                    dataset_id,
                    ingestion_identity,
                )

                # Persist dataset dimension/collection if missing.
                if int(dataset.get("embedding_dimension") or 0) != dim or not dataset.get(
                    "collection_name"
                ):
                    swapped = await self.db.compare_and_swap_dataset_collection_identity(
                        dataset_id,
                        expected_dimension=int(dataset.get("embedding_dimension") or 0),
                        expected_collection_name=str(
                            dataset.get("collection_name") or ""
                        ),
                        replacement_dimension=dim,
                        replacement_collection_name=collection,
                    )
                    current_dataset = await self.db.get_dataset(dataset_id)
                    expected_dataset = dict(dataset)
                    expected_dataset["embedding_dimension"] = dim
                    expected_dataset["collection_name"] = collection
                    already_converged = bool(
                        current_dataset
                        and _ingestion_dataset_identity(current_dataset)
                        == _ingestion_dataset_identity(expected_dataset)
                    )
                    if not swapped and not already_converged:
                        raise RuntimeError(
                            "dataset embedding collection changed concurrently; "
                            "retry ingestion"
                        )
                    if not current_dataset or not already_converged:
                        raise RuntimeError(
                            "dataset embedding collection did not converge; retry ingestion"
                        )
                    dataset = current_dataset
                    ingestion_identity = _ingestion_dataset_identity(current_dataset)

                # Process images if multimodal embedding is available
                await self._require_ingestion_identity(
                    dataset_id,
                    ingestion_identity,
                )
                # Image vectors are admitted only into a verified unified space.
                image_count = 0
                doc_metadata = _ensure_dict(doc.get("metadata"))
                image_metadata_list = doc_metadata.get("extracted_images", [])
                if not isinstance(image_metadata_list, list):
                    raise RuntimeError("durable image receipt must be a list")

                receipt_processing_mode = str(
                    doc_metadata.get("processing_mode") or "text_only"
                ).strip().lower()
                declared_image_count = int(doc_metadata.get("image_count") or 0)
                requires_image_generation = (
                    receipt_processing_mode in {"multimodal", "scanned"}
                    and (bool(image_metadata_list) or declared_image_count > 0)
                )
                desired_image_segment_ids: set[str] | None = (
                    set() if requires_image_generation else None
                )
                if requires_image_generation and not image_metadata_list:
                    raise RuntimeError(
                        "document declares images without a durable rebuild receipt"
                    )

                # Check if images were already embedded during upload (in-memory direct embedding)
                images_already_embedded = doc_metadata.get("images_embedded", False)
                if skip_image_generation:
                    image_count = 0
                elif images_already_embedded:
                    embedded_count = doc_metadata.get("embedded_image_count", 0)
                    logger.info(
                        f"[Ingest] Images already embedded during upload: {embedded_count} images, skipping re-embedding"
                    )
                    image_count = embedded_count
                    persisted_ids = {
                        str(item.get("segment_id") or "").strip()
                        for item in image_metadata_list
                        if isinstance(item, dict)
                        and str(item.get("segment_id") or "").strip()
                    }
                    desired_image_segment_ids = (
                        persisted_ids
                        if len(persisted_ids) == len(image_metadata_list)
                        else None
                    )
                else:
                    # Determine which multimodal embedder to use
                    multimodal_embedder = None
                    if (
                        receipt_processing_mode in {"multimodal", "scanned"}
                        and is_multimodal
                        and embedder
                        and getattr(embedder, "supports_multimodal", False)
                    ):
                        # Dataset is configured for multimodal - use the unified embedder
                        multimodal_embedder = embedder
                        logger.info(
                            f"Using dataset's multimodal embedder for {len(image_metadata_list)} images"
                        )
                    if requires_image_generation and (
                        multimodal_embedder is None or not self._ks.image_storage_service
                    ):
                        raise RuntimeError(
                            "image indexing requires the dataset's unified multimodal "
                            "embedding space"
                        )

                    if multimodal_embedder and self._ks.image_storage_service and image_metadata_list:
                        await self.db.update_document_status(
                            document_id, status="indexing", progress=85
                        )
                        try:
                            image_collection = collection
                            mm_dim = getattr(multimodal_embedder, "dimension", None)
                            if mm_dim and int(mm_dim) != int(dim):
                                raise RuntimeError(
                                    "dataset multimodal image dimension does not match its "
                                    "authoritative collection"
                                )

                            max_text_pos = max(max_new_pos, max_existing_pos)
                            image_base_position = max_text_pos + 1

                            image_count = await self._process_document_images_with_embedder(
                                embedder=multimodal_embedder,
                                dataset_id=dataset_id,
                                document_id=document_id,
                                image_metadata_list=image_metadata_list,
                                collection=image_collection,
                                base_position=image_base_position,
                                tenant_id=str(dataset.get("tenant_id") or "default"),
                                expected_ingestion_identity=ingestion_identity,
                            )
                            current_image_segments = (
                                await self.db.get_image_segments_by_document(
                                    document_id
                                )
                            )
                            desired_positions = {
                                image_base_position + idx
                                for idx in range(len(image_metadata_list))
                            }
                            desired_image_segment_ids = {
                                str(segment.get("segment_id") or "").strip()
                                for segment in current_image_segments
                                if int(segment.get("position") or 0)
                                in desired_positions
                                and str(segment.get("segment_id") or "").strip()
                            }
                            if len(desired_image_segment_ids) != len(
                                image_metadata_list
                            ):
                                raise RuntimeError(
                                    "image segment persistence did not publish the "
                                    "complete attachment generation"
                                )
                            logger.info(
                                f"Processed {image_count} images for document {document_id}"
                            )
                        except Exception as img_err:
                            raise RuntimeError(
                                "image embedding failed; the document generation remains "
                                "retryable"
                            ) from img_err

                # T1 revision publication: image work and every embedding batch
                # have now succeeded.  Publish text points/rows under the
                # dataset seqlock; readers overlapping the short critical
                # section retry from their entrypoint and can only return the
                # complete old or complete new revision.
                staged_manifest = [
                    str(row.get("segment_id") or "").strip()
                    for row in segment_rows
                    if str(row.get("segment_id") or "").strip()
                ] + staged_resumable
                keep_segment_ids = unchanged_segments + staged_manifest
                existing_image_segments = await self.db.get_image_segments_by_document(document_id)
                finalize_pure_text = (
                    not lexical_config.reads_bm25_v2
                    and not requires_image_generation
                    and not image_metadata_list
                    and not existing_image_segments
                    and DOCUMENT_LIFECYCLE_REINDEX_KEY not in doc_metadata
                )
                if points or staged_manifest or excess_segments or candidate_content is not None:
                    promoted, deleted_count = await self._publish_text_generation(
                        collection=collection,
                        points=points,
                        segment_rows=segment_rows,
                        excess_vector_ids=excess_vectors,
                        overwritten_point_ids=overwritten_vector_ids,
                        keep_segment_ids=keep_segment_ids,
                        staged_segment_ids=staged_manifest,
                        delete_excess=bool(excess_segments),
                        tenant_id=dataset_tenant_id,
                        dataset_id=dataset_id,
                        document_id=document_id,
                        expected_ingestion_identity=ingestion_identity,
                        candidate_content=candidate_content,
                        expected_content=str(doc.get("content") or ""),
                        candidate_metadata_patch=candidate_metadata_patch,
                        candidate_word_count=(
                            len(raw_text.split()) if source_text_override is not None else None
                        ),
                        candidate_version_number=pending_restore_version,
                        previous_version_number=pending_previous_version,
                        finalize_document=finalize_pure_text,
                    )
                    logger.info(
                        f"Activated {promoted}/{len(staged_manifest)} staged segments "
                        f"and removed {deleted_count} excess rows for document {document_id}"
                    )
                    if finalize_pure_text:
                        try:
                            await self.db.clear_dataset_needs_reindex(dataset_id)
                        except Exception:
                            logger.warning("Published text revision kept needs_reindex flag", exc_info=True)
                        try:
                            await self.db.update_document_fields(
                                document_id,
                                {
                                    "embedding_model": str(dataset.get("embedding_model") or getattr(embedder, "model", "") or ""),
                                    "embedding_model_version": str(dataset.get("embedding_model_version") or getattr(embedder, "model_version", "") or ""),
                                    "embedding_dimension": int(dim or 0) or None,
                                },
                            )
                        except Exception:
                            logger.warning("Published text revision kept prior embedding provenance", exc_info=True)
                        return staged_manifest

                # Build associations only after the new text rows exist.  The
                # old attachment receipt remains untouched until both the
                # association pass and its tenant-scoped replacement succeed.
                await self.db.update_document_status(
                    document_id, status="indexing", progress=95
                )
                if existing_image_segments:
                    try:
                        association_result = await self._ks.associate_images_to_chunks(
                            document_id=document_id,
                            max_images_per_chunk=10,
                            proximity_threshold=0.3,
                            image_segment_ids=desired_image_segment_ids,
                        )
                        logger.info(
                            f"Associated {association_result.get('associations_created', 0)} "
                            f"images to {association_result.get('segments_with_images', 0)} text chunks "
                            f"(total image segments: {len(existing_image_segments)})"
                        )
                    except Exception as assoc_err:
                        if requires_image_generation:
                            raise RuntimeError(
                                "image association failed; refusing an incomplete "
                                "multimodal publication"
                            ) from assoc_err
                        logger.warning(
                            f"Image association failed for document {document_id}: {assoc_err}"
                        )

                replace_bindings = getattr(
                    self.db,
                    "replace_document_attachment_bindings",
                    None,
                )
                if callable(replace_bindings):
                    await replace_bindings(
                        document_id,
                        dataset_id,
                        tenant_id=dataset_tenant_id,
                    )
                elif existing_image_segments:
                    logger.warning(
                        "Attachment binding persistence is unavailable; "
                        "the runtime schema is older than migration 106"
                    )

                if desired_image_segment_ids is not None:
                    stale_image_segments = [
                        segment
                        for segment in existing_image_segments
                        if str(segment.get("segment_id") or "")
                        not in desired_image_segment_ids
                    ]
                    await self._cleanup_stale_image_generation(
                        collection=collection,
                        stale_image_segments=stale_image_segments,
                        tenant_id=dataset_tenant_id,
                        dataset_id=dataset_id,
                        document_id=document_id,
                        expected_ingestion_identity=ingestion_identity,
                    )

                # Keep the denormalized count correct even for an unchanged
                # generation that needed no cross-store publication.
                await self.db.refresh_document_segment_count(document_id)

                # Clear needs_reindex only after the new serving revision is
                # provably published.  Clearing it earlier could advertise a
                # failed replacement as healthy.
                try:
                    await self.db.clear_dataset_needs_reindex(dataset_id)
                except Exception as clear_err:
                    logger.warning(
                        f"Failed to clear needs_reindex flag for {dataset_id}: {clear_err}"
                    )

                # T3 embedding provenance: record which model embedded this
                # document's vectors (migration 102 columns). Degrade-safe — a
                # missing column (pre-102 DB) must never fail the generation.
                try:
                    await self.db.update_document_fields(
                        document_id,
                        {
                            "embedding_model": str(
                                dataset.get("embedding_model")
                                or getattr(embedder, "model", "")
                                or ""
                            ),
                            "embedding_model_version": str(
                                dataset.get("embedding_model_version")
                                or getattr(embedder, "model_version", "")
                                or ""
                            ),
                            "embedding_dimension": int(dim or 0) or None,
                        },
                    )
                except Exception as prov_err:
                    logger.warning(
                        f"Failed to stamp embedding provenance for document "
                        f"{document_id}: {prov_err}"
                    )

                await self._complete_document_generation(
                    collection=collection,
                    tenant_id=dataset_tenant_id,
                    dataset_id=dataset_id,
                    document_id=document_id,
                    expected_ingestion_identity=ingestion_identity,
                    lexical_config=lexical_config,
                )
                return staged_manifest
            except IndexLeaseUnavailableError:
                raise
            except Exception as exc:
                logger.error(
                    f"Embedding/vector store failed for document {document_id}: {exc}",
                    exc_info=True,
                )
                await self.db.update_document_status(
                    document_id, status="error", progress=100, error=str(exc)
                )
            finally:
                if embedder:
                    try:
                        await embedder.close()
                    except Exception:
                        logger.warning("Embedder cleanup failed after ingestion", exc_info=True)
        except IndexLeaseUnavailableError:
            raise
        except Exception as exc:
            if isinstance(exc, _Bm25V2WriteDisabled):
                logger.warning(
                    "BM25 v2 ingest refused before mutation for document %s: %s",
                    document_id,
                    exc,
                )
                return None
            # Best-effort: keep document status in a terminal state.
            logger.error(
                f"Ingest failed for document {document_id}: {exc}",
                exc_info=True,
            )
            with contextlib.suppress(Exception):
                await self._mark_document_failed_if_writable(
                    dataset_id,
                    document_id,
                    str(exc),
                )
            return None

    async def reembed_document(
        self, dataset_id: str, document_id: str
    ) -> list[str] | None:
        """PRD T1 item 3 reembed verb: in-place vector repair.

        Re-embeds already-persisted text chunks at their EXISTING segment and
        point identity — no re-parse, no re-split, no delete-first window, so
        the serving generation never goes dark. Serving rows are repaired in
        place; rows a crashed generation left staged (status='indexing'; their
        vectors were persisted atomically with the row) are re-embedded and
        then promoted by the same staging flip the full pipeline uses.
        Operator-disabled rows keep their points deleted — re-enabling them is
        not this verb's job. When nothing persisted exists yet the verb
        degrades to the full pipeline so it never strands an unindexed
        document.
        """

        try:
            dataset = await self._ks._get_dataset_or_404(dataset_id)
            _require_dataset_index_writable(dataset)
            ingestion_identity = _ingestion_dataset_identity(dataset)
            try:
                lexical_config = LexicalConfig.from_index_config(
                    _ensure_dict(dataset.get("index_config"))
                )
            except LexicalConfigError as exc:
                raise ValidationFailedError(str(exc)) from exc
            if lexical_config.reads_bm25_v2 and not self.vector_store.bm25_v2_enabled:
                raise _Bm25V2WriteDisabled(
                    "bm25_v2 active writes are unavailable while the service kill "
                    "switch is off"
                )
            dataset_tenant_id = str(dataset.get("tenant_id") or "").strip()
            if not dataset_tenant_id:
                raise ValidationFailedError(
                    "dataset tenant_id is required for re-embedding"
                )
            doc = await self.db.get_document(document_id)
            if not doc or str(doc.get("dataset_id")) != dataset_id:
                raise ValidationFailedError("document not found")

            # Load the persisted text generation in position order: serving
            # rows plus staged rows left by a crashed generation.
            repair_rows: list[dict[str, Any]] = []
            offset = 0
            page_size = 500
            while True:
                page = await self.db.list_segments(
                    dataset_id,
                    document_id=document_id,
                    limit=page_size,
                    offset=offset,
                )
                if not page:
                    break
                for row in page:
                    if str(row.get("content_type") or "text") != "text":
                        continue
                    row_status = str(row.get("status") or "")
                    if row_status == "indexing" or (
                        row_status == "completed"
                        and row.get("enabled", True) is True
                    ):
                        repair_rows.append(row)
                if len(page) < page_size:
                    break
                offset += page_size

            if not repair_rows:
                logger.info(
                    f"Reembed of document {document_id} found no persisted chunks; "
                    "falling back to the full pipeline"
                )
                return await self.ingest_document(dataset_id, document_id)

            embedding_config = _ensure_dict(dataset.get("embedding_config"))
            is_multimodal = self._ks._is_multimodal_dataset(dataset)
            embedder: BaseEmbedding | None = None
            try:
                if is_multimodal:
                    embedder = await maybe_await(
                        self._ks._get_unified_multimodal_embedder(
                            dataset, embedding_config
                        )
                    )
                else:
                    embedder = await maybe_await(
                        self._ks._get_text_embedder(dataset, embedding_config)
                    )
                if embedder._dimension is None:
                    await embedder.embed_query("test")
                dim = embedder._dimension or 1024
                await self._require_ingestion_identity(
                    dataset_id, ingestion_identity
                )
                collection = await self.vector_store.ensure_collection(
                    dataset_id=dataset_id,
                    dimension=dim,
                    collection_name=str(dataset.get("collection_name") or "") or None,
                    tenant_id=dataset_tenant_id,
                    **(
                        {"lexical_config": lexical_config}
                        if lexical_config.configured
                        else {}
                    ),
                )

                await self.db.update_document_status(
                    document_id, status="indexing", progress=35
                )

                from qdrant_client.http import models as qmodels  # type: ignore

                batch_size = (
                    10
                    if is_multimodal
                    else self.settings.knowledge.text_embedding_batch_size
                )
                max_concurrent = (
                    self.settings.knowledge.multimodal_embedding_max_concurrent
                    if is_multimodal
                    else self.settings.knowledge.text_embedding_max_concurrent
                )
                total = len(repair_rows)
                batches = [
                    repair_rows[i : i + batch_size]
                    for i in range(0, total, batch_size)
                ]
                semaphore = asyncio.Semaphore(
                    max(1, min(max_concurrent, len(batches)))
                )

                def _reembed_input(row: dict[str, Any]) -> str:
                    # Vector parity contract: the engine embeds the
                    # prefix-augmented chunk text while the row stores the
                    # display text (contextual_prefix column carries the
                    # prefix). Reconstruct exactly what the engine embedded so
                    # a repair never rewrites vectors under different
                    # semantics; the "\n\n" join is the canonical composition
                    # any prefix producer must emit.
                    text = str(row.get("text") or "")
                    prefix = str(row.get("contextual_prefix") or "")
                    return f"{prefix}\n\n{text}" if prefix else text

                async def embed_indexed_batch(
                    batch_idx: int, batch: list[dict[str, Any]]
                ) -> tuple[int, list[Any], list[dict[str, Any]]]:
                    texts = [_reembed_input(row) for row in batch]
                    async with semaphore:
                        try:
                            vectors = await embedder.embed_documents(texts)
                            return batch_idx, vectors, batch
                        except Exception:
                            logger.exception(
                                f"Reembed embedding failed for batch {batch_idx + 1} "
                                f"of document {document_id}"
                            )
                            return batch_idx, [None] * len(batch), batch

                tasks = [
                    embed_indexed_batch(idx, batch)
                    for idx, batch in enumerate(batches)
                ]

                points: list[Any] = []
                repaired_ids: list[str] = []
                failed_chunks = 0
                embedded = 0
                for coro in asyncio.as_completed(tasks):
                    _, vectors, batch = await coro
                    await asyncio.sleep(0)
                    for row, vector in zip(batch, vectors, strict=True):
                        if vector is None:
                            failed_chunks += 1
                            continue
                        segment_id = str(row.get("segment_id") or "").strip()
                        if not segment_id:
                            failed_chunks += 1
                            continue
                        vector_id = (
                            str(row.get("vector_id") or "").strip() or segment_id
                        )
                        seg_metadata = row.get("metadata")
                        seg_metadata = (
                            seg_metadata if isinstance(seg_metadata, dict) else {}
                        )
                        payload_meta = {
                            key: seg_metadata.get(key)
                            for key in (
                                "source_type",
                                "citation_text",
                                "source_reference",
                                "section_title",
                                "section_full_path",
                                "page_number",
                                "chunk_index",
                                "paragraph_index",
                                "source_document",
                                "document_title",
                                "madhab",
                                "language",
                            )
                            if seg_metadata.get(key) is not None
                        }
                        payload = {
                            "tenant_id": dataset_tenant_id,
                            "dataset_id": dataset_id,
                            "document_id": document_id,
                            "segment_id": segment_id,
                            "position": row.get("position"),
                            # Same parity contract as the embedding input: the
                            # engine's payload carries the prefix-augmented text.
                            "text": _reembed_input(row),
                            "token_count": row.get("token_count"),
                            "source_type": payload_meta.get(
                                "source_type", "unknown"
                            ),
                            "language": payload_meta.get("language", "en"),
                            "metadata": payload_meta,
                            "citation_text": payload_meta.get("citation_text"),
                            "source_reference": payload_meta.get(
                                "source_reference"
                            ),
                        }
                        points.append(
                            qmodels.PointStruct(
                                id=vector_id, vector=vector, payload=payload
                            )
                        )
                        repaired_ids.append(segment_id)
                    embedded += len(batch)
                    progress = 35 + (embedded / max(total, 1)) * 55
                    await self.db.update_document_status(
                        document_id, status="indexing", progress=min(progress, 95)
                    )

                if failed_chunks > 0:
                    raise RuntimeError(
                        f"Reembed failed for {failed_chunks} chunks; refusing a "
                        "partial vector repair"
                    )

                staged_ids = [
                    str(row.get("segment_id") or "").strip()
                    for row in repair_rows
                    if str(row.get("status") or "") == "indexing"
                    and str(row.get("segment_id") or "").strip()
                ]
                upserted, promoted = await self._publish_reembed_generation(
                    collection=collection,
                    points=points,
                    staged_segment_ids=staged_ids,
                    tenant_id=dataset_tenant_id,
                    dataset_id=dataset_id,
                    document_id=document_id,
                    expected_ingestion_identity=ingestion_identity,
                )
                if staged_ids:
                    logger.info(
                        f"Reembed promoted {promoted}/{len(staged_ids)} staged "
                        f"segments for document {document_id}"
                    )

                await self.db.refresh_document_segment_count(document_id)
                await self._complete_document_generation(
                    collection=collection,
                    tenant_id=dataset_tenant_id,
                    dataset_id=dataset_id,
                    document_id=document_id,
                    expected_ingestion_identity=ingestion_identity,
                    lexical_config=lexical_config,
                )
                logger.info(
                    f"Reembed repaired {upserted} vectors for document {document_id}"
                )
                return repaired_ids
            finally:
                if embedder:
                    await embedder.close()
        except IndexLeaseUnavailableError:
            raise
        except Exception as exc:
            if isinstance(exc, _Bm25V2WriteDisabled):
                logger.warning(
                    "BM25 v2 reembed refused before mutation for document %s: %s",
                    document_id,
                    exc,
                )
                return None
            logger.error(
                f"Reembed failed for document {document_id}: {exc}",
                exc_info=True,
            )
            with contextlib.suppress(Exception):
                await self._mark_document_failed_if_writable(
                    dataset_id,
                    document_id,
                    str(exc),
                )
            return None

    async def _snapshot_points_for_rollback(
        self,
        *,
        collection: str,
        point_ids: list[str],
        tenant_id: str,
        dataset_id: str,
    ) -> dict[str, Any]:
        snapshot = getattr(self.vector_store, "snapshot_points", None)
        if not callable(snapshot):
            raise RuntimeError(
                "vector rollback snapshots are unavailable; refusing index publication"
            )
        result = await snapshot(
            collection,
            point_ids,
            tenant_id=tenant_id,
            dataset_id=dataset_id,
        )
        if not isinstance(result, dict):
            raise RuntimeError("vector rollback snapshot returned an invalid receipt")
        return {str(point_id): point for point_id, point in result.items()}

    async def _complete_document_generation(
        self,
        *,
        collection: str,
        tenant_id: str,
        dataset_id: str,
        document_id: str,
        expected_ingestion_identity: str,
        lexical_config: LexicalConfig,
    ) -> None:
        """Publish the terminal document authority with an active-v2 receipt.

        ``documents.status`` participates in content_revision and the BM25
        authority predicate. Active mode therefore closes the status change
        under the same negative seqlock and full-scroll certification as point
        writes. Shadow/legacy mode keeps the existing direct terminal update.
        """

        if not lexical_config.reads_bm25_v2:
            await self.db.update_document_status(
                document_id,
                status="completed",
                progress=100,
            )
            return

        async def commit(
            connection: Any,
            *,
            finish_publication: bool = True,
        ) -> None:
            if finish_publication:
                raise RuntimeError(
                    "active document completion requires deferred publication finalization"
                )
            async with connection.transaction():
                await self.db.update_document_status(
                    document_id,
                    status="completed",
                    progress=100,
                    connection=connection,
                )

        await self._publish_points_atomically(
            collection=collection,
            points=[],
            delete_point_ids=[],
            rollback_point_ids=[],
            tenant_id=tenant_id,
            dataset_id=dataset_id,
            expected_ingestion_identity=expected_ingestion_identity,
            commit=commit,
        )

    async def _prepare_durable_point_backups(
        self,
        *,
        collection: str,
        rollback_point_ids: list[str],
        publication_revision: int,
        recovered: bool,
        tenant_id: str,
        dataset_id: str,
        expected_ingestion_identity: str,
        lifecycle_lease_held: bool = False,
    ) -> tuple[dict[str, Any], list[str]]:
        """Create/recover disabled Qdrant backups before serving IDs mutate."""

        from qdrant_client.http import models as qmodels  # type: ignore

        from .vector_store import VectorStoreConfig

        original_ids = list(dict.fromkeys(rollback_point_ids))
        backup_ids = {
            point_id: _rollback_backup_point_id(
                dataset_id,
                point_id,
            )
            for point_id in original_ids
        }
        observed_backup_points = await self._snapshot_points_for_rollback(
            collection=collection,
            point_ids=list(backup_ids.values()),
            tenant_id=tenant_id,
            dataset_id=dataset_id,
        )
        # A fresh publication owns the deterministic backup slots and replaces
        # disabled cleanup orphans with a snapshot of the current serving
        # points.  Only a recovered negative revision consumes old receipts.
        backup_points = observed_backup_points if recovered else {}

        snapshots: dict[str, Any] = {}
        for original_id, backup_id in backup_ids.items():
            backup = backup_points.get(backup_id)
            if backup is None:
                continue
            backup_payload = dict(getattr(backup, "payload", None) or {})
            receipt = backup_payload.get(_INDEX_ROLLBACK_PAYLOAD_KEY)
            if not isinstance(receipt, dict):
                raise RuntimeError("vector rollback backup receipt is malformed")
            if str(receipt.get("original_point_id") or "") != original_id:
                raise RuntimeError("vector rollback backup receipt has conflicting identity")
            receipt_revision = int(receipt.get("publication_revision") or 0)
            current_revision = abs(publication_revision)
            if (
                receipt_revision < current_revision
                or receipt_revision - current_revision
                > INDEX_PUBLICATION_REVISION_RESERVE
            ):
                raise RuntimeError("vector rollback backup belongs to another publication")
            original_payload = receipt.get("payload")
            if not isinstance(original_payload, dict):
                raise RuntimeError("vector rollback backup payload is malformed")
            snapshots[original_id] = qmodels.PointStruct(
                id=receipt["original_point_id"],
                vector=backup.vector,
                payload=original_payload,
            )

        # No durable backup means the prior publisher never reached serving-ID
        # mutation (backups are fully written first).  Snapshot the current old
        # values and create the durable receipts.  If any backup exists, a
        # missing sibling was originally absent and must be treated as new.
        if not snapshots and original_ids:
            snapshots = await self._snapshot_points_for_rollback(
                collection=collection,
                point_ids=original_ids,
                tenant_id=tenant_id,
                dataset_id=dataset_id,
            )

        missing_backup_points: list[Any] = []
        for original_id, original in snapshots.items():
            backup_id = backup_ids[original_id]
            if backup_id in backup_points:
                continue
            original_payload = dict(getattr(original, "payload", None) or {})
            document_id = str(original_payload.get("document_id") or "").strip()
            if not document_id:
                raise RuntimeError(
                    "serving point lacks document identity required for durable rollback"
                )
            missing_backup_points.append(
                qmodels.PointStruct(
                    id=backup_id,
                    vector=original.vector,
                    payload={
                        "tenant_id": tenant_id,
                        "dataset_id": dataset_id,
                        "document_id": document_id,
                        "segment_id": original_payload.get("segment_id"),
                        "enabled": False,
                        _INDEX_ROLLBACK_PAYLOAD_KEY: {
                            "publication_revision": abs(publication_revision),
                            "original_point_id": original.id,
                            "payload": original_payload,
                        },
                    },
                )
            )

        batch_size = VectorStoreConfig.get_batch_size(len(missing_backup_points))
        for start in range(0, len(missing_backup_points), batch_size):
            await self._upsert_with_ingestion_identity(
                collection=collection,
                points=missing_backup_points[start : start + batch_size],
                dataset_id=dataset_id,
                expected_ingestion_identity=expected_ingestion_identity,
                lifecycle_lease_held=lifecycle_lease_held,
            )
        return snapshots, list(backup_ids.values())

    async def _restore_point_snapshot(
        self,
        *,
        collection: str,
        snapshots: dict[str, Any],
        intended_point_ids: list[str],
        tenant_id: str,
        dataset_id: str,
        expected_ingestion_identity: str,
        lifecycle_lease_held: bool = False,
        affects_bm25_scope: bool = True,
    ) -> None:
        """Restore overwritten points and delete only IDs that were truly new."""

        from .vector_store import VectorStoreConfig

        old_points = list(snapshots.values())
        batch_size = VectorStoreConfig.get_batch_size(len(old_points))
        for start in range(0, len(old_points), batch_size):
            await self._upsert_with_ingestion_identity(
                collection=collection,
                points=old_points[start : start + batch_size],
                dataset_id=dataset_id,
                expected_ingestion_identity=expected_ingestion_identity,
                lifecycle_lease_held=lifecycle_lease_held,
            )

        new_point_ids = sorted(set(intended_point_ids) - set(snapshots))
        if new_point_ids:
            delete_kwargs: dict[str, Any] = {}
            if lifecycle_lease_held:
                delete_kwargs["lifecycle_lease_held"] = True
            if not affects_bm25_scope:
                delete_kwargs["affects_bm25_scope"] = False
            await self.vector_store.delete_points(
                collection,
                new_point_ids,
                tenant_id=tenant_id,
                dataset_id=dataset_id,
                **delete_kwargs,
            )

    async def _publish_points_atomically(
        self,
        *,
        collection: str,
        points: list[Any],
        delete_point_ids: list[str],
        rollback_point_ids: list[str],
        tenant_id: str,
        dataset_id: str,
        expected_ingestion_identity: str,
        commit,
        affects_bm25_scope: bool = True,
    ) -> Any:
        """Publish Qdrant writes behind a PG seqlock with exact rollback."""

        from .vector_store import VectorStoreConfig

        lease = getattr(self.db, "dataset_index_publication_lease", None)
        abort = getattr(self.db, "abort_index_publication", None)
        if not callable(lease) or not callable(abort):
            raise RuntimeError("dataset index publication protocol is unavailable")

        point_ids = [
            str(point.id)
            for point in points
            if getattr(point, "id", None) is not None
        ]
        affected_point_ids = list(
            dict.fromkeys(point_ids + [str(point_id) for point_id in delete_point_ids])
        )
        lifecycle_service = (
            getattr(self._ks, "bm25_v2_lifecycle_service", None)
            if self._ks is not None
            else None
        )
        if lifecycle_service is not None:
            # Preflight before the publication lease changes content_revision:
            # an active dataset with the kill switch off must be a zero-side-
            # effect refusal. The check is repeated under the shared lease.
            await lifecycle_service.active_publication_context(dataset_id)
        else:
            get_live_profile = getattr(
                self.vector_store,
                "get_live_lexical_profile",
                None,
            )
            if callable(get_live_profile):
                live_profile, _receipt = await get_live_profile(collection)
                if live_profile is not None and live_profile.reads_bm25_v2:
                    raise RuntimeError(
                        "active bm25_v2 publication requires PostgreSQL lifecycle authority"
                    )

        async def publish() -> Any:
            async with lease(
                dataset_id,
                expected_ingestion_identity=expected_ingestion_identity,
            ) as publication:
                publication_connection = publication.connection
                publication_revision = int(publication.revision)
                backup_point_ids = [
                    _rollback_backup_point_id(
                        dataset_id,
                        point_id,
                    )
                    for point_id in dict.fromkeys(rollback_point_ids)
                ]
                snapshots: dict[str, Any] = {}
                backups_ready = False
                mutation_started = False
                active_context: dict[str, Any] | None = None
                if lifecycle_service is not None:
                    active_context = await lifecycle_service.active_publication_context(
                        dataset_id
                    )
                else:
                    get_live_profile = getattr(
                        self.vector_store,
                        "get_live_lexical_profile",
                        None,
                    )
                    if callable(get_live_profile):
                        live_profile, _receipt = await get_live_profile(collection)
                        if live_profile is not None and live_profile.reads_bm25_v2:
                            raise RuntimeError(
                                "active bm25_v2 publication requires PostgreSQL "
                                "lifecycle authority"
                            )
                try:
                    snapshots, backup_point_ids = await self._prepare_durable_point_backups(
                        collection=collection,
                        rollback_point_ids=rollback_point_ids,
                        publication_revision=publication_revision,
                        recovered=bool(publication.recovered),
                        tenant_id=tenant_id,
                        dataset_id=dataset_id,
                        expected_ingestion_identity=expected_ingestion_identity,
                        lifecycle_lease_held=True,
                    )
                    backups_ready = True
                    qdrant_batch_size = VectorStoreConfig.get_batch_size(len(points))
                    for start in range(0, len(points), qdrant_batch_size):
                        mutation_started = True
                        await self._upsert_with_ingestion_identity(
                            collection=collection,
                            points=points[start : start + qdrant_batch_size],
                            dataset_id=dataset_id,
                            expected_ingestion_identity=expected_ingestion_identity,
                            lifecycle_lease_held=True,
                        )
                    if delete_point_ids:
                        mutation_started = True
                        await self.vector_store.delete_points(
                            collection,
                            delete_point_ids,
                            tenant_id=tenant_id,
                            dataset_id=dataset_id,
                            lifecycle_lease_held=True,
                            affects_bm25_scope=affects_bm25_scope,
                        )
                    if active_context is None:
                        result = await commit(
                            publication_connection,
                            finish_publication=True,
                        )
                    else:
                        finish_publication = getattr(
                            self.db,
                            "finish_index_publication",
                            None,
                        )
                        if not callable(finish_publication):
                            raise RuntimeError(
                                "deferred index publication finalization is unavailable"
                            )
                        async with publication_connection.transaction():
                            result = await commit(
                                publication_connection,
                                finish_publication=False,
                            )
                            certification = await lifecycle_service.recertify_active_publication(
                                active_context,
                                publication_revision=publication_revision,
                                connection=publication_connection,
                            )
                            final_revision = await finish_publication(
                                dataset_id,
                                connection=publication_connection,
                            )
                            if int(final_revision) != int(
                                certification["target_revision"]
                            ):
                                raise RuntimeError(
                                    "index publication revision disagrees with the "
                                    "BM25 v2 receipt"
                                )
                            await lifecycle_service.settle_active_publication(
                                active_context,
                                certification,
                                connection=publication_connection,
                            )
                except BaseException:
                    if not backups_ready and publication.recovered:
                        raise RuntimeError(
                            "unfinished index publication could not recover its durable backups; "
                            "retrieval remains fenced"
                        )
                    if backups_ready and mutation_started:
                        try:
                            await self._restore_point_snapshot(
                                collection=collection,
                                snapshots=snapshots,
                                intended_point_ids=affected_point_ids,
                                tenant_id=tenant_id,
                                dataset_id=dataset_id,
                                expected_ingestion_identity=expected_ingestion_identity,
                                lifecycle_lease_held=True,
                                affects_bm25_scope=affects_bm25_scope,
                            )
                        except BaseException as rollback_error:
                            raise RuntimeError(
                                "index publication rollback was incomplete; retrieval remains "
                                "fenced"
                            ) from rollback_error
                    try:
                        if active_context is None:
                            await abort(
                                dataset_id,
                                connection=publication_connection,
                            )
                        else:
                            finish_publication = getattr(
                                self.db, "finish_index_publication", None,
                            )
                            if not callable(finish_publication):
                                raise RuntimeError("active publication finalizer is unavailable")
                            async with publication_connection.transaction():
                                restored = await lifecycle_service.recertify_active_publication(
                                    active_context,
                                    publication_revision=publication_revision,
                                    connection=publication_connection,
                                )
                                final_revision = await finish_publication(
                                    dataset_id, connection=publication_connection,
                                )
                                if int(final_revision) != int(restored["target_revision"]):
                                    raise RuntimeError("restored BM25 v2 revision disagrees")
                                await lifecycle_service.settle_active_publication(
                                    active_context, restored,
                                    connection=publication_connection,
                                )
                    except BaseException as rollback_error:
                        raise RuntimeError(
                            "index publication rollback was incomplete; retrieval remains fenced"
                        ) from rollback_error
                    if backup_point_ids:
                        try:
                            await self.vector_store.delete_points(
                                collection, backup_point_ids,
                                tenant_id=tenant_id, dataset_id=dataset_id,
                                lifecycle_lease_held=True, affects_bm25_scope=False,
                            )
                        except Exception:
                            logger.warning(
                                "Coherent rollback retained disabled point backups",
                                exc_info=True,
                            )
                    raise

                if backup_point_ids:
                    try:
                        await self.vector_store.delete_points(
                            collection,
                            backup_point_ids,
                            tenant_id=tenant_id,
                            dataset_id=dataset_id,
                            lifecycle_lease_held=True,
                            affects_bm25_scope=False,
                        )
                    except Exception:
                        # Backups are explicitly disabled and PostgreSQL has
                        # already committed the new revision.  A cleanup miss is
                        # storage debt, not a serving-consistency failure.
                        logger.warning(
                            "Published index revision retained disabled rollback backups",
                            exc_info=True,
                        )
                return result

        # Propagate worker cancellation into the publisher, whose BaseException
        # branch proves rollback (or leaves an already-committed active publish
        # behind its negative fail-closed fence) before the document lease can
        # be released.
        task = asyncio.create_task(publish())
        try:
            return await asyncio.shield(task)
        except asyncio.CancelledError:
            task.cancel()
            try:
                await task
            except asyncio.CancelledError:
                pass
            except BaseException:
                logger.exception("Index publication failed while worker cancellation was pending")
            raise

    async def _publish_text_generation(
        self,
        *,
        collection: str,
        points: list[Any],
        segment_rows: list[dict[str, Any]],
        excess_vector_ids: list[str],
        overwritten_point_ids: list[str],
        keep_segment_ids: list[str],
        staged_segment_ids: list[str],
        delete_excess: bool,
        tenant_id: str,
        dataset_id: str,
        document_id: str,
        expected_ingestion_identity: str,
        candidate_content: str | None = None,
        expected_content: str | None = None,
        candidate_metadata_patch: dict[str, Any] | None = None,
        candidate_word_count: int | None = None,
        candidate_version_number: int | None = None,
        previous_version_number: int | None = None,
        finalize_document: bool = False,
    ) -> tuple[int, int]:
        commit_publication = getattr(self.db, "commit_text_segment_publication", None)
        if not callable(commit_publication):
            raise RuntimeError("text segment publication protocol is unavailable")

        async def commit(
            connection: Any,
            *,
            finish_publication: bool = True,
        ) -> tuple[int, int]:
            kwargs: dict[str, Any] = {}
            if not finish_publication:
                kwargs["finish_publication"] = False
            if candidate_content is not None:
                kwargs["candidate_content"] = candidate_content
                kwargs["expected_content"] = expected_content
                if candidate_metadata_patch is not None:
                    kwargs["candidate_metadata_patch"] = candidate_metadata_patch
                if candidate_word_count is not None:
                    kwargs["candidate_word_count"] = candidate_word_count
                if candidate_version_number is not None:
                    kwargs["candidate_version_number"] = candidate_version_number
                    kwargs["previous_version_number"] = previous_version_number
            if finalize_document:
                kwargs["finalize_document"] = True
            return await commit_publication(
                dataset_id=dataset_id,
                document_id=document_id,
                segment_rows=segment_rows,
                keep_segment_ids=keep_segment_ids,
                staged_segment_ids=staged_segment_ids,
                delete_excess=delete_excess,
                expected_ingestion_identity=expected_ingestion_identity,
                connection=connection,
                **kwargs,
            )

        return await self._publish_points_atomically(
            collection=collection,
            points=points,
            delete_point_ids=excess_vector_ids,
            rollback_point_ids=[
                *overwritten_point_ids,
                *excess_vector_ids,
            ],
            tenant_id=tenant_id,
            dataset_id=dataset_id,
            expected_ingestion_identity=expected_ingestion_identity,
            commit=commit,
        )

    async def _publish_reembed_generation(
        self,
        *,
        collection: str,
        points: list[Any],
        staged_segment_ids: list[str],
        tenant_id: str,
        dataset_id: str,
        document_id: str,
        expected_ingestion_identity: str,
    ) -> tuple[int, int]:
        commit_publication = getattr(self.db, "commit_reembed_publication", None)
        if not callable(commit_publication):
            raise RuntimeError("reembed publication protocol is unavailable")

        async def commit(
            connection: Any,
            *,
            finish_publication: bool = True,
        ) -> int:
            kwargs: dict[str, Any] = {}
            if not finish_publication:
                kwargs["finish_publication"] = False
            return await commit_publication(
                dataset_id=dataset_id,
                document_id=document_id,
                staged_segment_ids=staged_segment_ids,
                expected_ingestion_identity=expected_ingestion_identity,
                connection=connection,
                **kwargs,
            )

        promoted = await self._publish_points_atomically(
            collection=collection,
            points=points,
            delete_point_ids=[],
            rollback_point_ids=[
                str(point.id)
                for point in points
                if getattr(point, "id", None) is not None
            ],
            tenant_id=tenant_id,
            dataset_id=dataset_id,
            expected_ingestion_identity=expected_ingestion_identity,
            commit=commit,
        )
        return len(points), int(promoted)

    async def _cleanup_stale_image_generation(
        self,
        *,
        collection: str,
        stale_image_segments: list[dict[str, Any]],
        tenant_id: str,
        dataset_id: str,
        document_id: str,
        expected_ingestion_identity: str,
    ) -> int:
        """Remove stale image rows/points with exact Qdrant rollback."""

        stale_segment_ids = [
            str(segment.get("segment_id") or "").strip()
            for segment in stale_image_segments
            if str(segment.get("segment_id") or "").strip()
        ]
        if not stale_segment_ids:
            return 0
        stale_point_ids = [
            str(segment.get("vector_id") or segment.get("segment_id") or "").strip()
            for segment in stale_image_segments
            if str(
                segment.get("vector_id") or segment.get("segment_id") or ""
            ).strip()
        ]
        commit_cleanup = getattr(self.db, "commit_image_segment_cleanup", None)
        if not callable(commit_cleanup):
            raise RuntimeError("image generation cleanup protocol is unavailable")

        async def commit(
            connection: Any,
            *,
            finish_publication: bool = True,
        ) -> int:
            kwargs: dict[str, Any] = {}
            if not finish_publication:
                kwargs["finish_publication"] = False
            return await commit_cleanup(
                dataset_id=dataset_id,
                document_id=document_id,
                stale_segment_ids=stale_segment_ids,
                expected_ingestion_identity=expected_ingestion_identity,
                connection=connection,
                **kwargs,
            )

        return int(
            await self._publish_points_atomically(
                collection=collection,
                points=[],
                delete_point_ids=stale_point_ids,
                rollback_point_ids=stale_point_ids,
                tenant_id=tenant_id,
                dataset_id=dataset_id,
                expected_ingestion_identity=expected_ingestion_identity,
                commit=commit,
                affects_bm25_scope=False,
            )
        )

    async def _persist_segment_batch(
        self,
        *,
        collection: str,
        points: list[Any],
        segment_rows: list[dict[str, Any]],
        tenant_id: str,
        dataset_id: str,
        expected_ingestion_identity: str,
    ) -> None:
        """Persist one batch while preserving overwritten points on DB failure."""

        point_ids = [str(point.id) for point in points if getattr(point, "id", None)]
        snapshots = await self._snapshot_points_for_rollback(
            collection=collection,
            point_ids=point_ids,
            tenant_id=tenant_id,
            dataset_id=dataset_id,
        )
        await self._upsert_with_ingestion_identity(
            collection=collection,
            points=points,
            dataset_id=dataset_id,
            expected_ingestion_identity=expected_ingestion_identity,
        )
        try:
            await self.db.insert_segments(segment_rows)
        except Exception:
            try:
                await self._restore_point_snapshot(
                    collection=collection,
                    snapshots=snapshots,
                    intended_point_ids=point_ids,
                    tenant_id=tenant_id,
                    dataset_id=dataset_id,
                    expected_ingestion_identity=expected_ingestion_identity,
                )
            except Exception:
                logger.warning(
                    "Failed to restore Qdrant points after segment DB failure",
                    exc_info=True,
                )
            raise

    async def _persist_image_segment_batch(
        self,
        *,
        collection: str,
        points: list[Any],
        image_segments: list[dict[str, Any]],
        tenant_id: str,
        dataset_id: str,
        expected_ingestion_identity: str,
    ) -> None:
        """Persist image vectors and rows as one compensating receipt.

        Qdrant and PostgreSQL cannot share a transaction.  A failed image-row
        write therefore rejects the whole new batch, removes every new point,
        and removes any rows already committed earlier in the batch.  Callers
        must not publish an ``images_embedded`` receipt unless this method
        returns successfully.
        """

        point_ids = [str(point.id) for point in points if getattr(point, "id", None)]
        snapshot_points = getattr(self.vector_store, "snapshot_points", None)
        if callable(snapshot_points):
            snapshots = await self._snapshot_points_for_rollback(
                collection=collection,
                point_ids=point_ids,
                tenant_id=tenant_id,
                dataset_id=dataset_id,
            )
        else:
            get_existing = getattr(
                self.db,
                "get_image_segments_by_document",
                None,
            )
            if callable(get_existing) and image_segments:
                existing_rows = await get_existing(
                    str(image_segments[0].get("document_id") or "")
                )
            else:
                existing_rows = []
            existing_point_ids = {
                str(row.get("vector_id") or row.get("segment_id") or "").strip()
                for row in existing_rows
                if str(
                    row.get("vector_id") or row.get("segment_id") or ""
                ).strip()
            }
            if existing_point_ids.intersection(point_ids):
                raise RuntimeError(
                    "vector rollback snapshots are unavailable for an image replacement"
                )
            snapshots = {}
        await self._upsert_with_ingestion_identity(
            collection=collection,
            points=points,
            dataset_id=dataset_id,
            expected_ingestion_identity=expected_ingestion_identity,
        )
        fallback_stored_ids: list[str] = []
        try:
            store_batch = getattr(self.db, "store_image_segments", None)
            if callable(store_batch):
                await store_batch(image_segments)
            else:
                store_one = getattr(self.db, "save_image_segment", None)
                if not callable(store_one):
                    raise RuntimeError("image segment persistence is unavailable")
                for segment in image_segments:
                    fallback_stored_ids.append(
                        str(segment.get("segment_id") or "").strip()
                    )
                    await store_one(segment)
        except Exception as exc:
            cleanup_failures: list[str] = []
            try:
                await self._restore_point_snapshot(
                    collection=collection,
                    snapshots=snapshots,
                    intended_point_ids=point_ids,
                    tenant_id=tenant_id,
                    dataset_id=dataset_id,
                    expected_ingestion_identity=expected_ingestion_identity,
                    affects_bm25_scope=False,
                )
            except Exception:
                cleanup_failures.append("qdrant")
                logger.warning(
                    "Failed to compensate image Qdrant points after DB failure",
                    exc_info=True,
                )

            if fallback_stored_ids:
                delete_segment = getattr(self.db, "delete_segment", None)
                if not callable(delete_segment):
                    cleanup_failures.append("postgres")
                else:
                    for segment_id in fallback_stored_ids:
                        try:
                            await delete_segment(segment_id)
                        except Exception:
                            cleanup_failures.append("postgres")
                            logger.warning(
                                "Failed to compensate a fallback image segment row",
                                exc_info=True,
                            )

            suffix = (
                f"; incomplete compensation: {','.join(sorted(set(cleanup_failures)))}"
                if cleanup_failures
                else ""
            )
            raise RuntimeError(
                "image segment persistence failed; the new image generation was rejected"
                f"{suffix}"
            ) from exc

    async def _upsert_with_ingestion_identity(
        self,
        *,
        collection: str,
        points: list[Any],
        dataset_id: str,
        expected_ingestion_identity: str,
        lifecycle_lease_held: bool = False,
    ) -> None:
        """Publish Qdrant points only while the captured generation is current."""

        normalized_dataset = str(dataset_id or "").strip()
        point_dataset_ids = {
            str((getattr(point, "payload", None) or {}).get("dataset_id") or "").strip()
            for point in points
        }
        if not normalized_dataset or point_dataset_ids != {normalized_dataset}:
            raise ValidationFailedError(
                "dataset_id is required and must match every fenced vector point"
            )
        document_ids = sorted(
            {
                str((getattr(point, "payload", None) or {}).get("document_id") or "").strip()
                for point in points
                if str(
                    (getattr(point, "payload", None) or {}).get("document_id") or ""
                ).strip()
            }
        )
        if not document_ids:
            raise ValidationFailedError(
                "document_id is required for a fenced vector write"
            )
        lease_factory = getattr(self.db, "dataset_index_write_lease", None)
        if not callable(lease_factory):
            raise ValidationFailedError(
                "vector writes are unavailable without the dataset identity fence"
            )
        upsert_kwargs: dict[str, Any] = {}
        if lifecycle_lease_held:
            upsert_kwargs["lifecycle_lease_held"] = True
        await self.vector_store.upsert(
            collection_name=collection,
            points=points,
            expected_ingestion_identity=expected_ingestion_identity,
            **upsert_kwargs,
        )

    async def _resolve_ingestion_identity(
        self,
        dataset_id: str,
        expected_ingestion_identity: str | None,
    ) -> str:
        if expected_ingestion_identity:
            return expected_ingestion_identity
        current = await self.db.get_dataset(dataset_id)
        if not current:
            raise ValidationFailedError("dataset not found for vector write")
        return _ingestion_dataset_identity(current)

    async def _require_ingestion_identity(
        self,
        dataset_id: str,
        expected_identity: str,
    ) -> None:
        current = await self.db.get_dataset(dataset_id)
        if not current:
            raise ValidationFailedError(
                "dataset ingestion identity changed; refusing a mixed index generation"
            )
        _require_dataset_index_writable(current)
        if _ingestion_dataset_identity(current) != expected_identity:
            raise ValidationFailedError(
                "dataset ingestion identity changed; refusing a mixed index generation"
            )

    async def _mark_document_failed_if_writable(
        self,
        dataset_id: str,
        document_id: str,
        error: str,
    ) -> None:
        current = await self.db.get_dataset(dataset_id)
        if not current:
            return
        try:
            _require_dataset_index_writable(current)
        except ValidationFailedError:
            logger.warning(
                "Skipping failed-status write while dataset index deletion is pending",
                extra={"dataset_id": dataset_id, "document_id": document_id},
            )
            return
        await self.db.update_document_status(
            document_id,
            status="error",
            progress=100,
            error=error,
        )

    # ========================================================================
    # Image Processing
    # ========================================================================

    async def _process_document_images_with_embedder(
        self,
        embedder: Any,  # Multimodal embedder
        dataset_id: str,
        document_id: str,
        image_metadata_list: list[dict[str, Any]],
        collection: str,
        base_position: int = 0,
        tenant_id: str = "default",
        expected_ingestion_identity: str | None = None,
    ) -> int:
        """
        Process and embed images loaded from persistent storage.

        Optimized for parallel processing:
        1. Parallel image loading (concurrent downloads)
        2. Batch embedding (single API call for multiple images)
        3. Batch database/vector store operations

        Args:
            dataset_id: Dataset ID
            document_id: Document ID
            image_metadata_list: List of image metadata dicts from document.metadata
            collection: Qdrant collection name
            base_position: Starting position for image segments
            tenant_id: Tenant ID for storage path

        Returns:
            Number of successfully processed images
        """
        if not embedder or not self._ks.image_storage_service or not image_metadata_list:
            return 0
        expected_ingestion_identity = await self._resolve_ingestion_identity(
            dataset_id,
            expected_ingestion_identity,
        )
        get_existing_images = getattr(
            self.db,
            "get_image_segments_by_document",
            None,
        )
        existing_image_segments = (
            await get_existing_images(document_id)
            if callable(get_existing_images)
            else []
        )
        existing_ids_by_position = {
            int(segment.get("position") or 0): str(
                segment.get("segment_id") or ""
            ).strip()
            for segment in existing_image_segments
            if str(segment.get("segment_id") or "").strip()
        }

        from qdrant_client.http import models as qmodels

        total_images = len(image_metadata_list)
        logger.info(f"Processing {total_images} images in parallel batches...")

        # Step 1: Load images from storage (parallel download with retry)
        MAX_DOWNLOAD_RETRIES = 3

        async def load_image(
            idx: int, img_meta: dict[str, Any]
        ) -> tuple[int, dict[str, Any], bytes | None]:
            """Load a single image from storage with retry, return (idx, metadata, bytes or None)."""
            storage_url = str(img_meta.get("storage_url") or "").strip()
            storage_key = str(img_meta.get("storage_key") or "").strip()
            if not storage_url or not storage_key:
                return (idx, img_meta, None)

            for retry in range(MAX_DOWNLOAD_RETRIES):
                try:
                    image_bytes = await self._ks.image_storage_service.download_document_image(
                        tenant_id=tenant_id,
                        document_id=document_id,
                        storage_key=storage_key,
                    )
                    return (idx, img_meta, image_bytes)
                except ValueError:
                    # Receipt scope violations are permanent authority failures,
                    # not transient storage errors.
                    raise
                except Exception as e:
                    if retry < MAX_DOWNLOAD_RETRIES - 1:
                        wait_time = 2**retry  # 1s, 2s
                        logger.debug(
                            f"Download retry {retry + 1} for image {idx} in {wait_time}s: {e}"
                        )
                        await asyncio.sleep(wait_time)
                    else:
                        logger.warning(
                            f"Failed to load image {img_meta.get('image_id')} after {MAX_DOWNLOAD_RETRIES} retries: {e}"
                        )
            return (idx, img_meta, None)

        # Load all images in parallel (limit concurrency to avoid S3 socket timeouts)
        load_semaphore = asyncio.Semaphore(5)  # Reduced from 20 to 5

        async def load_with_semaphore(idx: int, meta: dict) -> tuple[int, dict, bytes | None]:
            async with load_semaphore:
                return await load_image(idx, meta)

        load_tasks = [load_with_semaphore(i, m) for i, m in enumerate(image_metadata_list)]
        load_results = await asyncio.gather(*load_tasks, return_exceptions=True)

        # Filter successful loads
        loaded_images: list[tuple[int, dict[str, Any], bytes]] = []
        for result in load_results:
            if isinstance(result, Exception):
                continue
            idx, meta, img_bytes = result
            if img_bytes:
                loaded_images.append((idx, meta, img_bytes))

        logger.info(f"Loaded {len(loaded_images)}/{total_images} images from storage")

        if len(loaded_images) != total_images:
            raise RuntimeError(
                "durable image receipt could not be fully loaded; refusing a partial "
                "image generation"
            )

        # Step 2: Batch embed images (DashScope supports batch)
        # Process in batches to avoid API limits
        EMBED_BATCH_SIZE = 8  # Reduced batch size for stability
        MAX_RETRIES = 3

        image_points = []
        image_segments = []
        processed = 0

        for batch_start in range(0, len(loaded_images), EMBED_BATCH_SIZE):
            batch = loaded_images[batch_start : batch_start + EMBED_BATCH_SIZE]
            batch_bytes = [img_bytes for _, _, img_bytes in batch]

            # Retry with exponential backoff
            vectors = None
            for retry in range(MAX_RETRIES):
                try:
                    # Single API call for batch embedding
                    batch_num = batch_start // EMBED_BATCH_SIZE + 1
                    logger.info(
                        f"Embedding batch {batch_num}: {len(batch)} images... (attempt {retry + 1}/{MAX_RETRIES})"
                    )
                    vectors = await embedder.embed_images(batch_bytes)

                    if vectors and len(vectors) == len(batch):
                        break  # Success
                    else:
                        logger.warning(
                            f"Embedding returned {len(vectors) if vectors else 0} vectors for {len(batch)} images"
                        )
                        vectors = None
                except Exception as e:
                    logger.warning(f"Batch {batch_num} embedding attempt {retry + 1} failed: {e}")
                    if retry < MAX_RETRIES - 1:
                        wait_time = 2**retry  # 1s, 2s, 4s
                        logger.info(f"Retrying in {wait_time}s...")
                        await asyncio.sleep(wait_time)
                    else:
                        logger.error(
                            f"Batch {batch_num} failed after {MAX_RETRIES} attempts, skipping"
                        )

            if not vectors:
                continue  # Skip this batch if all retries failed

            # Create points and segments for this batch
            for i, (idx, img_meta, _img_bytes) in enumerate(batch):
                vector = vectors[i]
                if not vector:
                    continue

                position = base_position + idx
                storage_url = img_meta.get("storage_url", "")

                # Use context text as image description (skip slow VLM calls)
                image_text = (
                    img_meta.get("vlm_description", "")
                    or img_meta.get("context_text", "")
                    or f"[Image: page {img_meta.get('page_number', 'unknown')}]"
                )
                attachment_id = (
                    img_meta.get("confluence_attachment_id")
                    or img_meta.get("image_id")
                )
                if not str(attachment_id or "").strip():
                    raise RuntimeError("durable image receipt has no attachment identity")
                seg_id = existing_ids_by_position.get(position) or _stable_segment_id(
                    document_id,
                    "image",
                    position,
                )
                image_filename = (
                    img_meta.get("filename")
                    or (
                        img_meta.get("storage_key", "").split("/")[-1]
                        if img_meta.get("storage_key")
                        else f"image_{idx}"
                    )
                )

                payload = {
                    "tenant_id": tenant_id,
                    "dataset_id": dataset_id,
                    "document_id": document_id,
                    "segment_id": seg_id,
                    "position": position,
                    "text": image_text,
                    "content_type": "image",
                    "image_id": img_meta.get("image_id"),
                    "image_mime_type": img_meta.get("mime_type"),
                    "image_width": img_meta.get("width"),
                    "image_height": img_meta.get("height"),
                    "image_page": img_meta.get("page_number"),
                    "source_location": img_meta.get("source_location"),
                    "confluence_attachment_id": img_meta.get(
                        "confluence_attachment_id"
                    ),
                    "attachment_updated_at": img_meta.get("attachment_updated_at"),
                }

                image_points.append(qmodels.PointStruct(id=seg_id, vector=vector, payload=payload))

                image_segments.append(
                    {
                        "segment_id": seg_id,
                        "dataset_id": dataset_id,
                        "document_id": document_id,
                        "position": position,
                        "text": image_text,
                        "token_count": 0,
                        "vector_id": seg_id,
                        "content_type": "image",
                        "image_url": storage_url,
                        "image_attachment_id": attachment_id,
                        "image_filename": image_filename,
                        "image_media_type": img_meta.get("mime_type"),
                        "image_file_size": img_meta.get("size_bytes", 0),
                        "metadata": {
                            "width": img_meta.get("width"),
                            "height": img_meta.get("height"),
                            "page_number": img_meta.get("page_number"),
                            "source_location": img_meta.get("source_location"),
                            "source_position": idx,
                            "storage_key": img_meta.get("storage_key"),
                            "image_id": img_meta.get("image_id"),
                            "filename": img_meta.get("filename"),
                            "context_text": img_meta.get("context_text"),
                            "vlm_description": img_meta.get("vlm_description"),
                            "confluence_attachment_id": img_meta.get(
                                "confluence_attachment_id"
                            ),
                            "attachment_updated_at": img_meta.get(
                                "attachment_updated_at"
                            ),
                        },
                    }
                )
                processed += 1

            # Update progress after each batch
            progress = 85 + (batch_start + len(batch)) / len(loaded_images) * 10  # 85% -> 95%
            await self.db.update_document_status(
                document_id, status="indexing", progress=progress
            )
            logger.info(
                f"Batch complete: {processed}/{len(loaded_images)} images embedded, progress={progress:.1f}%"
            )

        if processed != len(loaded_images):
            raise RuntimeError(
                "image embedding did not produce every required vector; refusing a "
                "partial image generation"
            )

        # Step 3: Persist one compensating Qdrant/PostgreSQL receipt.
        if image_points:
            try:
                # Debug: validate vectors before upserting
                sample_point = image_points[0]
                sample_vector = sample_point.vector if sample_point else []

                # Check for NaN/Infinity in vectors
                import math

                has_invalid = False
                for i, pt in enumerate(image_points):
                    vec = pt.vector
                    if any(math.isnan(v) or math.isinf(v) for v in vec):
                        logger.error(f"Point {i} has invalid vector values (NaN/Inf)")
                        has_invalid = True
                        break

                logger.info(
                    f"Upserting {len(image_points)} image vectors to collection={collection}, "
                    f"vector_dim={len(sample_vector)}, sample_id={sample_point.id}, "
                    f"has_invalid={has_invalid}"
                )

                if has_invalid:
                    raise ValueError("Vectors contain NaN or Infinity values")

                await self._persist_image_segment_batch(
                    collection=collection,
                    points=image_points,
                    image_segments=image_segments,
                    tenant_id=tenant_id,
                    dataset_id=dataset_id,
                    expected_ingestion_identity=expected_ingestion_identity,
                )
                logger.info(
                    "Persisted %d image vectors and rows to collection %s",
                    len(image_points),
                    collection,
                )
            except Exception as e:
                logger.error(f"Failed to persist image batch to collection={collection}: {e}")
                # Log more details for debugging
                if image_points:
                    pt = image_points[0]
                    logger.error(
                        f"Sample point: id={pt.id}, vector_len={len(pt.vector) if pt.vector else 0}, payload_keys={list(pt.payload.keys()) if pt.payload else []}"
                    )
                raise

        logger.info(f"Image processing complete: {processed}/{total_images} images processed")
        return processed

    async def _embed_images_in_memory(
        self,
        embedder: Any,  # Multimodal embedder
        dataset_id: str,
        document_id: str,
        images: list[IngestionExtractedImage],
        collection: str,
        tenant_id: str,
        base_position: int = 0,
        expected_ingestion_identity: str | None = None,
    ) -> tuple[int, list[dict[str, Any]]]:
        """
        Embed images directly from memory (no S3 download needed).

        This is the "in-memory direct embedding" mode for maximum efficiency.
        Images are embedded while still in memory after extraction, before S3 upload.

        Args:
            embedder: Multimodal embedder instance
            dataset_id: Dataset ID
            document_id: Document ID
            images: List of IngestionExtractedImage objects (with content bytes)
            collection: Qdrant collection name
            base_position: Starting position for image segments

        Returns:
            Tuple of (processed_count, list of image metadata with embedding info)
        """
        if not embedder or not images:
            return 0, []
        expected_ingestion_identity = await self._resolve_ingestion_identity(
            dataset_id,
            expected_ingestion_identity,
        )

        import math

        from qdrant_client.http import models as qmodels

        total_images = len(images)
        logger.info(f"[MemoryEmbed] Embedding {total_images} images directly from memory...")

        # Prepare image data
        image_data: list[tuple[int, IngestionExtractedImage, bytes]] = []
        for idx, img in enumerate(images):
            if img.content and len(img.content) > 0:
                image_data.append((idx, img, img.content))
            else:
                logger.debug(f"Skipping image {idx} with no content")

        if not image_data:
            logger.warning("[MemoryEmbed] No valid images with content to embed")
            return 0, []

        if len(image_data) != total_images:
            raise RuntimeError(
                "in-memory image receipt is incomplete; refusing a partial image "
                "generation"
            )

        logger.info(f"[MemoryEmbed] {len(image_data)}/{total_images} images have valid content")

        # Batch embed images
        EMBED_BATCH_SIZE = 8
        MAX_RETRIES = 3

        image_points = []
        image_segments = []
        embedded_metadata = []  # Track which images were embedded
        processed = 0

        for batch_start in range(0, len(image_data), EMBED_BATCH_SIZE):
            batch = image_data[batch_start : batch_start + EMBED_BATCH_SIZE]
            batch_bytes = [img_bytes for _, _, img_bytes in batch]

            # Retry with exponential backoff
            vectors = None
            for retry in range(MAX_RETRIES):
                try:
                    batch_num = batch_start // EMBED_BATCH_SIZE + 1
                    total_batches = (len(image_data) + EMBED_BATCH_SIZE - 1) // EMBED_BATCH_SIZE
                    logger.info(
                        f"[MemoryEmbed] Batch {batch_num}/{total_batches}: {len(batch)} images (attempt {retry + 1})"
                    )

                    vectors = await embedder.embed_images(batch_bytes)

                    if vectors and len(vectors) == len(batch):
                        break
                    else:
                        logger.warning(
                            f"[MemoryEmbed] Got {len(vectors) if vectors else 0} vectors for {len(batch)} images"
                        )
                        vectors = None
                except Exception as e:
                    logger.warning(
                        f"[MemoryEmbed] Batch {batch_num} attempt {retry + 1} failed: {e}"
                    )
                    if retry < MAX_RETRIES - 1:
                        wait_time = 2**retry
                        await asyncio.sleep(wait_time)
                    else:
                        logger.error(
                            f"[MemoryEmbed] Batch {batch_num} failed after {MAX_RETRIES} attempts"
                        )

            if not vectors:
                continue

            # Create points and segments for this batch
            for i, (idx, img, _img_bytes) in enumerate(batch):
                vector = vectors[i]
                if not vector:
                    continue

                position = base_position + idx
                seg_id = _stable_segment_id(document_id, "image", position)

                # Use context text as image description
                image_text = (
                    img.context_text[:200]
                    if img.context_text
                    else f"[Image: page {getattr(img, 'page_number', idx)}]"
                )
                page_number = getattr(img, "page_number", None) or (
                    img.metadata.get("page_number") if hasattr(img, "metadata") else None
                )

                payload = {
                    "tenant_id": tenant_id,
                    "dataset_id": dataset_id,
                    "document_id": document_id,
                    "segment_id": seg_id,
                    "position": position,
                    "text": image_text,
                    "content_type": "image",
                    "image_id": img.image_id,
                    "image_mime_type": img.mime_type,
                    "image_width": img.width,
                    "image_height": img.height,
                    "image_page": page_number,
                }

                image_points.append(qmodels.PointStruct(id=seg_id, vector=vector, payload=payload))

                image_segments.append(
                    {
                        "segment_id": seg_id,
                        "dataset_id": dataset_id,
                        "document_id": document_id,
                        "position": position,
                        "text": image_text,
                        "token_count": 0,
                        "vector_id": seg_id,
                        "content_type": "image",
                        "image_attachment_id": img.image_id,
                        "image_filename": img.filename
                        or f"image_{idx}.{img.mime_type.split('/')[-1]}",
                        "image_media_type": img.mime_type,
                        "image_file_size": img.size_bytes,
                        "metadata": {
                            "width": img.width,
                            "height": img.height,
                            "page_number": page_number,
                            "source_location": img.source_location,
                            "source_position": idx,
                        },
                    }
                )

                # Track this image as embedded
                embedded_metadata.append(
                    {
                        "idx": idx,
                        "image_id": img.image_id,
                        "segment_id": seg_id,
                        "vector_id": seg_id,
                    }
                )
                processed += 1

            # Update progress
            progress = 10 + (batch_start + len(batch)) / len(image_data) * 50  # 10% -> 60%
            await self.db.update_document_status(
                document_id, status="uploading_images", progress=progress
            )
            logger.info(
                f"[MemoryEmbed] Progress: {processed}/{len(image_data)} images, {progress:.1f}%"
            )

        if processed != len(image_data):
            raise RuntimeError(
                "in-memory embedding did not produce every required vector; refusing "
                "a partial image generation"
            )

        # Persist one compensating Qdrant/PostgreSQL receipt.  The caller may
        # publish ``images_embedded`` only after this succeeds.
        if image_points:
            try:
                # Validate vectors
                has_invalid = False
                for i, pt in enumerate(image_points):
                    if any(math.isnan(v) or math.isinf(v) for v in pt.vector):
                        logger.error(f"Point {i} has invalid vector values")
                        has_invalid = True
                        break

                if has_invalid:
                    raise ValueError("Vectors contain NaN or Infinity values")

                logger.info(
                    f"[MemoryEmbed] Upserting {len(image_points)} vectors to collection={collection}"
                )
                await self._persist_image_segment_batch(
                    collection=collection,
                    points=image_points,
                    image_segments=image_segments,
                    tenant_id=tenant_id,
                    dataset_id=dataset_id,
                    expected_ingestion_identity=expected_ingestion_identity,
                )
                logger.info(
                    f"[MemoryEmbed] Persisted {len(image_points)} image vectors and rows"
                )
            except Exception as e:
                logger.error(f"[MemoryEmbed] Failed to persist image batch: {e}")
                raise

        logger.info(f"[MemoryEmbed] Complete: {processed}/{total_images} images embedded")
        return processed, embedded_metadata

    async def _process_document_images(
        self,
        dataset_id: str,
        document_id: str,
        images: list[ExtractedImage],
        collection: str,
        base_position: int = 0,
        tenant_id: str = "default",
        expected_ingestion_identity: str | None = None,
    ) -> int:
        """
        Process and embed images from a document.

        Args:
            dataset_id: Dataset ID
            document_id: Document ID
            images: List of extracted images
            collection: Qdrant collection name
            base_position: Starting position for image segments
            tenant_id: Tenant ID for storage path

        Returns:
            Number of successfully processed images
        """

        if not self._ks.multimodal_embedding or not images:
            return 0
        expected_ingestion_identity = await self._resolve_ingestion_identity(
            dataset_id,
            expected_ingestion_identity,
        )

        from qdrant_client.http import models as qmodels

        processed = 0
        image_points = []
        image_segments = []

        for idx, img in enumerate(images):
            try:
                if not img.is_embeddable:
                    logger.debug(f"Skipping non-embeddable image: {img.image_id}")
                    continue

                # Embed the image
                logger.debug(f"Embedding image {img.image_id} ({img.width}x{img.height})")
                embeddings = await self._ks.multimodal_embedding.embed_images([img.content])

                if not embeddings or not embeddings[0]:
                    logger.warning(f"No embedding returned for image {img.image_id}")
                    continue

                vector = embeddings[0]
                position = base_position + idx
                seg_id = _stable_segment_id(document_id, "image", position)

                # Prepare payload for Qdrant
                payload = {
                    "tenant_id": tenant_id,
                    "dataset_id": dataset_id,
                    "document_id": document_id,
                    "segment_id": seg_id,
                    "position": position,
                    "text": img.context_text or f"[Image: Page {img.page_number}]",
                    "content_type": "image",
                    "image_id": img.image_id,
                    "image_mime_type": img.mime_type,
                    "image_width": img.width,
                    "image_height": img.height,
                    "image_page": img.page_number,
                }

                image_points.append(
                    qmodels.PointStruct(
                        id=seg_id,
                        vector=vector,
                        payload=payload,
                    )
                )

                # Prepare segment for database
                image_segments.append(
                    {
                        "segment_id": seg_id,
                        "dataset_id": dataset_id,
                        "document_id": document_id,
                        "position": position,
                        "text": img.context_text or "",
                        "token_count": 0,
                        "vector_id": seg_id,
                        "content_type": "image",
                        "image_url": "",  # Will be set if stored to S3/OSS
                        "image_attachment_id": img.image_id,
                        "image_filename": f"page{img.page_number}_{img.image_id}.{img.mime_type.split('/')[-1]}",
                        "image_media_type": img.mime_type,
                        "image_file_size": img.size_bytes,
                        "metadata": {
                            "width": img.width,
                            "height": img.height,
                            "page_number": img.page_number,
                            "source_position": idx,
                        },
                    }
                )

                processed += 1

                # Optionally upload to S3/OSS storage
                if self._ks.image_storage_service:
                    try:
                        filename = image_segments[-1].get(
                            "image_filename", f"{img.image_id}.{img.mime_type.split('/')[-1]}"
                        )
                        storage_url = await self._ks.image_storage_service.upload_image(
                            tenant_id=tenant_id,
                            document_id=document_id,
                            attachment_id=f"pdf_{idx}",
                            filename=filename,
                            content=img.content,
                            content_type=img.mime_type,
                        )
                        image_segments[-1]["image_url"] = storage_url
                        logger.debug(f"Uploaded image to storage: {storage_url}")
                    except Exception as store_err:
                        logger.warning(f"Failed to upload image to storage: {store_err}")

            except Exception as e:
                logger.warning(f"Failed to process image {img.image_id}: {e}")
                continue

        # Upsert image vectors to Qdrant
        if image_points:
            try:
                await self._upsert_with_ingestion_identity(
                    collection=collection,
                    points=image_points,
                    dataset_id=dataset_id,
                    expected_ingestion_identity=expected_ingestion_identity,
                )
                logger.info(
                    f"Upserted {len(image_points)} image vectors to collection {collection}"
                )
            except Exception as e:
                logger.error(f"Failed to upsert image vectors: {e}")
                raise

        # Save image segments to database
        for seg in image_segments:
            try:
                await self.db.save_image_segment(seg)
            except Exception as e:
                logger.warning(f"Failed to save image segment {seg['segment_id']}: {e}")

        return processed
