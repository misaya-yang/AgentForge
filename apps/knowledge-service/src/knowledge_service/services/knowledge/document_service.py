"""Document management service for knowledge base.

Handles document CRUD, text extraction, segment management, and ingestion queuing.
Migrated from KnowledgeService as part of Phase 2 refactoring (Step 3).
"""

from __future__ import annotations

import asyncio
import hashlib
import re
import uuid
from datetime import datetime, timedelta, timezone
from typing import TYPE_CHECKING, Any
from urllib.parse import parse_qsl, urlencode, urlsplit, urlunsplit

from ...config.settings import Settings
from ...core.auth.user_resolver import UserContext
from ...core.exceptions import ValidationFailedError
from ...core.observability.logging import get_logger
from ...persistence.database import (
    CONFLUENCE_SYNC_GENERATION_KEY,
    DOCUMENT_LIFECYCLE_REINDEX_KEY,
    DOCUMENT_UPLOAD_FAILED_KEY,
    DOCUMENT_UPLOAD_GENERATION_KEY,
    SOURCE_OWNED_DOCUMENT_METADATA_KEYS,
    DatabaseStorage,
    dataset_index_deletion_fence,
    dataset_ingestion_identity,
    make_dataset_index_deletion_fence,
)
from .chunking import validate_persisted_chunking_config
from .common import SPECIALIZED_REBUILD_UNAVAILABLE, maybe_await
from .common import ensure_dict as _ensure_dict
from .embedding import BaseEmbedding, create_embedding
from .lexical_config import LexicalConfig

if TYPE_CHECKING:
    from .knowledge_service import KnowledgeService

logger = get_logger(__name__)

_SENSITIVE_URL_QUERY_KEY = re.compile(
    r"(?:token|secret|password|passwd|credential|api[_-]?key|signature|sig|auth|code)",
    re.IGNORECASE,
)


def _redacted_source_url(raw_url: str) -> str:
    """Return a display/persistence URL without reusable credential material."""

    parsed = urlsplit(raw_url)
    if parsed.username or parsed.password:
        raise ValidationFailedError("URL userinfo is not allowed")
    query = [
        (key, "***" if _SENSITIVE_URL_QUERY_KEY.search(key) else value)
        for key, value in parse_qsl(parsed.query, keep_blank_values=True)
    ]
    return urlunsplit(
        (
            parsed.scheme,
            parsed.netloc,
            parsed.path,
            urlencode(query, doseq=True),
            "",
        )
    )


def _lexical_ensure_kwargs(index_config: Any) -> dict[str, Any]:
    lexical = LexicalConfig.from_index_config(index_config)
    return {"lexical_config": lexical} if lexical.configured else {}


def _require_dataset_index_writable(
    dataset: dict[str, Any],
    *,
    allowed_deletion: tuple[str, str] | None = None,
) -> None:
    try:
        deletion_fence = dataset_index_deletion_fence(dataset)
    except RuntimeError as exc:
        raise ValidationFailedError(str(exc)) from exc
    if deletion_fence is not None:
        allowed_marker = (
            make_dataset_index_deletion_fence(*allowed_deletion)
            if allowed_deletion is not None
            else None
        )
        if deletion_fence != allowed_marker:
            raise ValidationFailedError(
                "dataset index deletion is pending; indexed content is unavailable"
            )
    index_config = dataset.get("index_config") or {}
    if not isinstance(index_config, dict):
        raise ValidationFailedError("dataset index_config is invalid")
    validate_persisted_chunking_config(index_config.get("chunking", {}))
    LexicalConfig.from_index_config(index_config)


def _validate_segment_batch_enable_request(
    segment_ids: Any,
    enabled: Any,
) -> tuple[list[str], bool]:
    """Defend direct service callers that bypass the public Pydantic schema."""

    if not isinstance(segment_ids, list) or not 1 <= len(segment_ids) <= 500:
        raise ValidationFailedError("segment_ids must contain between 1 and 500 IDs")
    normalized: list[str] = []
    for raw_segment_id in segment_ids:
        if not isinstance(raw_segment_id, str):
            raise ValidationFailedError("segment IDs must contain 1-256 characters")
        segment_id = raw_segment_id.strip()
        if not segment_id or len(segment_id) > 256:
            raise ValidationFailedError("segment IDs must contain 1-256 characters")
        normalized.append(segment_id)
    if not isinstance(enabled, bool):
        raise ValidationFailedError("enabled must be a boolean")
    return normalized, enabled


def _require_dataset_index_readable(dataset: dict[str, Any]) -> None:
    """Hide content while a multi-store deletion is incomplete."""

    try:
        deletion_fence = dataset_index_deletion_fence(dataset)
    except RuntimeError as exc:
        raise ValidationFailedError(str(exc)) from exc
    if deletion_fence is not None:
        raise ValidationFailedError(
            "dataset index deletion is pending; indexed content is unavailable"
        )


def _dataset_content_generation(dataset: dict[str, Any]) -> tuple[str, Any]:
    _require_dataset_index_readable(dataset)
    return (
        str(dataset.get("tenant_id") or "").strip(),
        dataset.get("content_revision"),
    )


def _require_document_active_for_manual_index_write(document: dict[str, Any]) -> None:
    """Reject manual segment writes while their owning document is hidden."""

    metadata = _ensure_dict(document.get("metadata"))
    if (
        not bool(document.get("enabled", True))
        or bool(document.get("archived", False))
        or str(document.get("status") or "") != "completed"
        or DOCUMENT_LIFECYCLE_REINDEX_KEY in metadata
        or DOCUMENT_UPLOAD_GENERATION_KEY in metadata
        or DOCUMENT_UPLOAD_FAILED_KEY in metadata
        or CONFLUENCE_SYNC_GENERATION_KEY in metadata
    ):
        raise ValidationFailedError(
            "manual segment indexing requires an active completed document"
        )


def _require_no_reserved_document_metadata(metadata: Any) -> None:
    supplied = _ensure_dict(metadata)
    for key in SOURCE_OWNED_DOCUMENT_METADATA_KEYS:
        if key in supplied:
            raise ValidationFailedError(f"metadata key '{key}' is reserved")


def _segment_vector_payload(
    *,
    dataset: dict[str, Any],
    segment: dict[str, Any],
    text: str,
) -> dict[str, Any]:
    """Rebuild the serving payload without dropping authoritative fields."""

    metadata = _ensure_dict(segment.get("metadata"))
    source_type = str(
        segment.get("source_type") or metadata.get("source_type") or "unknown"
    )
    language = str(segment.get("language") or metadata.get("language") or "en")
    content_type = str(
        segment.get("content_type") or metadata.get("content_type") or "text"
    )
    return {
        "tenant_id": str(dataset.get("tenant_id") or ""),
        "dataset_id": str(segment.get("dataset_id") or ""),
        "document_id": str(segment.get("document_id") or ""),
        "segment_id": str(segment.get("segment_id") or ""),
        "position": int(segment.get("position") or 0),
        "text": text,
        "enabled": bool(segment.get("enabled", True)),
        "status": "completed",
        "level": int(segment.get("level") or 3),
        "content_type": content_type,
        "source_type": source_type,
        "language": language,
        "metadata": metadata,
        "parent_segment_id": segment.get("parent_segment_id"),
        "token_count": int(segment.get("token_count") or 0),
        "source_reference": segment.get("source_reference")
        or metadata.get("source_reference"),
        "citation_text": segment.get("citation_text")
        or metadata.get("citation_text"),
        "page_number": segment.get("page_number") or metadata.get("page_number"),
        "section_header": segment.get("section_header")
        or metadata.get("section_header"),
    }


# PRD T1.1: the API never leaks internal lifecycle states. Every document
# payload returned to callers carries a derived ``display_status`` from this
# fixed vocabulary (Dify-parity contract):
#   queuing / indexing / paused / error / available / disabled / archived
DOCUMENT_DISPLAY_STATUS_VOCABULARY = (
    "queuing",
    "indexing",
    "paused",
    "error",
    "available",
    "disabled",
    "archived",
)

# Internal document states that mean "accepted, not yet being indexed".
_DISPLAY_QUEUING_STATES = frozenset({"waiting"})
# Internal states that mean "actively moving through the pipeline". This is
# the catch-all bucket on purpose: parsing/splitting/indexing, the legacy
# Confluence 'syncing', upload-phase 'uploading'/'uploading_images', and any
# unknown in-flight value must never surface verbatim.
_DISPLAY_ACTIVE_STATES = frozenset(
    {
        "parsing",
        "splitting",
        "indexing",
        "syncing",
        "uploading",
        "uploading_images",
    }
)

# D4 (frontend handoff): list pagination. A single page is capped; callers
# walk further pages via offset. Caps keep the documented pre-pagination
# behaviour as the default page (200 documents / 500 segments).
DOCUMENT_LIST_PAGE_CAP = 200
SEGMENT_LIST_PAGE_CAP = 500


def _clamp_page_limit(limit: int, cap: int) -> int:
    """Clamp a requested page size into [1, cap]."""
    try:
        value = int(limit)
    except (TypeError, ValueError):
        return cap
    return max(1, min(value, cap))


def derive_document_display_status(document: Any) -> str:
    """Derive the display-safe document status (PRD T1.1).

    Collapses the internal machine vocabulary
    (waiting/parsing/splitting/indexing/completed/error plus legacy
    'syncing' and upload-phase states) into the display vocabulary.
    Archived wins over every other state; then error; then the
    completed-state enabled/disabled split; then queued vs actively indexing.
    Unknown states fail closed into 'indexing' rather than leaking raw text.
    """

    doc = document if isinstance(document, dict) else {}
    if bool(doc.get("archived", False)):
        return "archived"
    status = str(doc.get("status") or "").strip().lower()
    if status == "error" or status == "failed":
        return "error"
    if status == "paused":
        return "paused"
    if status == "completed":
        return "disabled" if doc.get("enabled", True) is False else "available"
    if status in _DISPLAY_QUEUING_STATES:
        return "queuing"
    # parsing/splitting/indexing/syncing/uploading*/unknown in-flight states.
    return "indexing"


def _with_display_status(document: dict[str, Any]) -> dict[str, Any]:
    """Stamp the derived display_status onto an API-bound document payload."""

    if not isinstance(document, dict):
        return document
    document["display_status"] = derive_document_display_status(document)
    if document.get("error") == SPECIALIZED_REBUILD_UNAVAILABLE:
        document["failure_code"] = SPECIALIZED_REBUILD_UNAVAILABLE
    # Worker failures may contain provider responses, storage paths or credentials.
    # The raw value stays in the internal record/logs; public document reads
    # expose a stable instruction and the existing document_id for diagnosis.
    if document.get("error"):
        document["error"] = (
            "This processing path may have partial results and cannot be replayed "
            "safely. Ask an administrator to inspect it before retrying."
            if document.get("failure_code") == SPECIALIZED_REBUILD_UNAVAILABLE
            else "Document processing failed. Check the source file and retry manually."
        )
    return document


async def _require_unchanged_dataset_content(
    knowledge_service: Any,
    user: UserContext,
    dataset_id: str,
    expected: tuple[str, Any],
    *,
    required: str = "viewer",
) -> None:
    authoritative = await knowledge_service.require_dataset_access(
        user,
        dataset_id,
        required=required,
    )
    if _dataset_content_generation(authoritative) != expected:
        raise ValidationFailedError(
            "dataset content generation changed during read; retry the request"
        )


class DocumentService:
    """Service for managing knowledge base documents.

    Accepts a ``_ks`` (parent KnowledgeService) reference for shared resources
    like ``vector_store``, ``structured_parser``, ``multimodal_embedding``, etc.
    Set post-init by the parent because those are created after sub-service construction.
    """

    _ks: KnowledgeService | None

    def __init__(
        self,
        settings: Settings,
        database: DatabaseStorage,
        dataset_service: Any | None = None,
    ):
        self.settings = settings
        self.db = database
        self.dataset_service = dataset_service
        self._ks = None  # Set post-init by KnowledgeService

    async def _save_document_for_dataset(
        self,
        document: dict[str, Any],
        dataset: dict[str, Any],
    ) -> None:
        await self.db.insert_document(
            document,
            expected_ingestion_identity=dataset_ingestion_identity(dataset),
        )

    async def _finalize_upload_for_dataset(
        self,
        document: dict[str, Any],
        dataset: dict[str, Any],
        *,
        upload_generation: str,
        connection: Any | None = None,
    ) -> None:
        try:
            finalized = await self.db.finalize_document_upload(
                document,
                upload_generation=upload_generation,
                expected_ingestion_identity=dataset_ingestion_identity(dataset),
                connection=connection,
            )
        except RuntimeError as exc:
            raise ValidationFailedError(
                "document upload lost ownership while processing"
            ) from exc
        if not finalized:
            raise ValidationFailedError(
                "document upload was deleted or superseded while processing"
            )

    async def _upsert_for_dataset_identity(
        self,
        *,
        dataset: dict[str, Any],
        collection: str,
        points: list[Any],
        lifecycle_lease_held: bool = False,
    ) -> None:
        dataset_id = str(dataset.get("dataset_id") or "").strip()
        document_ids = sorted(
            {
                str((getattr(point, "payload", None) or {}).get("document_id") or "").strip()
                for point in points
                if str(
                    (getattr(point, "payload", None) or {}).get("document_id") or ""
                ).strip()
            }
        )
        lease_factory = getattr(self.db, "dataset_index_write_lease", None)
        if not dataset_id or not document_ids or not callable(lease_factory):
            raise ValidationFailedError(
                "vector writes are unavailable without the dataset identity fence"
            )
        upsert_kwargs: dict[str, Any] = {}
        if lifecycle_lease_held:
            upsert_kwargs["lifecycle_lease_held"] = True
        await self._ks.vector_store.upsert(
            collection_name=collection,
            points=points,
            expected_ingestion_identity=dataset_ingestion_identity(dataset),
            **upsert_kwargs,
        )

    # ========================================================================
    # Document CRUD Operations
    # ========================================================================

    async def create_document_from_text(
        self,
        user: UserContext,
        dataset_id: str,
        title: str,
        content: str,
        metadata: dict[str, Any] | None = None,
    ) -> dict[str, Any]:
        _require_no_reserved_document_metadata(metadata)
        dataset = await self._ks.require_dataset_access(user, dataset_id, required="editor")
        _require_dataset_index_writable(dataset)
        doc_id = str(uuid.uuid4())
        # Sanitize content for PostgreSQL
        clean_content = self._ks._sanitize_text_for_db(content or "")
        doc = {
            "document_id": doc_id,
            "dataset_id": dataset_id,
            "title": title or doc_id,
            "source_type": "text",
            "mime_type": "text/plain",
            "size_bytes": len(clean_content.encode("utf-8")),
            "status": "waiting",
            "progress": 0,
            "content": clean_content,
            "metadata": metadata or {},
        }
        await self._save_document_for_dataset(doc, dataset)
        return _with_display_status(await self.db.get_document(doc_id) or doc)

    async def create_document_from_upload(
        self,
        user: UserContext,
        dataset_id: str,
        filename: str,
        content_bytes: bytes,
        mime_type: str | None = None,
        metadata: dict[str, Any] | None = None,
        processing_mode: str = "text_only",  # auto | text_only | scanned | multimodal
    ) -> dict[str, Any]:
        """
        Create a document from file upload.

        Args:
            user: User context
            dataset_id: Target dataset ID
            filename: Original filename
            content_bytes: File content bytes
            mime_type: MIME type
            metadata: Optional metadata
            processing_mode: Processing mode - auto, text_only, scanned, or multimodal
        """
        from .processing_mode import parse_processing_mode

        _require_no_reserved_document_metadata(metadata)
        dataset = await self._ks.require_dataset_access(user, dataset_id, required="editor")
        _require_dataset_index_writable(dataset)
        storage = getattr(self._ks, "image_storage_service", None)
        upload_original = getattr(storage, "upload_original_file", None)
        delete_assets = getattr(storage, "delete_document_assets", None)
        lease_factory = getattr(self.db, "document_index_update_lease", None)
        if not all(callable(value) for value in (upload_original, delete_assets, lease_factory)):
            raise ValidationFailedError(
                "document upload requires durable storage and the document owner fence"
            )
        tenant_id = str(dataset.get("tenant_id") or user.tenant_id or "").strip()
        if not tenant_id:
            raise ValidationFailedError("dataset tenant identity is required for document upload")
        doc_id = str(uuid.uuid4())

        # Parse and validate processing mode
        mode = parse_processing_mode(processing_mode)
        logger.info(f"Creating document with processing_mode={mode.value}")

        # Prepare initial metadata with processing mode
        doc_metadata = dict(metadata or {})
        doc_metadata["processing_mode"] = mode.value
        upload_generation = str(uuid.uuid4())
        doc_metadata[DOCUMENT_UPLOAD_GENERATION_KEY] = upload_generation

        # Save document record immediately so frontend can show it while processing
        initial_doc = {
            "document_id": doc_id,
            "dataset_id": dataset_id,
            "title": filename or doc_id,
            "source_type": "upload",
            "mime_type": mime_type or "application/octet-stream",
            "size_bytes": len(content_bytes),
            "status": "uploading",
            "progress": 0,
            "content": "",
            "metadata": doc_metadata,
        }
        await self._save_document_for_dataset(initial_doc, dataset)

        finalized = False
        try:
            async with lease_factory(dataset_id, doc_id) as lease_connection:
                current = await self.db.get_document(
                    doc_id,
                    connection=lease_connection,
                )
                current_metadata = _ensure_dict((current or {}).get("metadata"))
                if (
                    not current
                    or str(current.get("dataset_id") or "") != dataset_id
                    or str(current.get("status") or "") != "uploading"
                    or current_metadata.get(DOCUMENT_UPLOAD_GENERATION_KEY)
                    != upload_generation
                ):
                    raise ValidationFailedError(
                        "document upload lost ownership before storage publication"
                    )

                original_key = await upload_original(
                    tenant_id=tenant_id,
                    document_id=doc_id,
                    filename=filename,
                    content=content_bytes,
                    content_type=mime_type or "application/octet-stream",
                )
                doc_metadata.update(
                    {
                        "original_file_key": original_key,
                        "original_filename": filename,
                        "original_mime_type": mime_type or "application/octet-stream",
                        "mime_type": mime_type or "application/octet-stream",
                    }
                )
                doc = {
                    "document_id": doc_id,
                    "dataset_id": dataset_id,
                    "title": filename or doc_id,
                    "source_type": "upload",
                    "mime_type": mime_type or "application/octet-stream",
                    "size_bytes": len(content_bytes),
                    "status": "waiting",
                    "progress": 0,
                    "content": "",
                    "metadata": doc_metadata,
                }
                await self._finalize_upload_for_dataset(
                    doc,
                    dataset,
                    upload_generation=upload_generation,
                    connection=lease_connection,
                )
                finalized = True
                persisted = await self.db.get_document(
                    doc_id,
                    connection=lease_connection,
                )
                if not persisted:
                    raise RuntimeError("finalized document upload is not readable")
                return _with_display_status(persisted)
        except BaseException:
            if not finalized:
                cleanup = asyncio.create_task(
                    delete_assets(
                        tenant_id=tenant_id,
                        document_id=doc_id,
                    )
                )
                try:
                    await asyncio.shield(cleanup)
                except asyncio.CancelledError:
                    await cleanup
                except Exception:
                    logger.exception(
                        "Failed to compensate document upload storage",
                        extra={"dataset_id": dataset_id, "document_id": doc_id},
                    )
            raise

    async def create_document_from_url(
        self,
        user: UserContext,
        dataset_id: str,
        url: str,
        title: str | None = None,
        metadata: dict[str, Any] | None = None,
    ) -> dict[str, Any]:
        _require_no_reserved_document_metadata(metadata)
        dataset = await self._ks.require_dataset_access(user, dataset_id, required="editor")
        _require_dataset_index_writable(dataset)

        raw_url = (url or "").strip()
        if not raw_url:
            raise ValidationFailedError("url is required")
        source_url = _redacted_source_url(raw_url)

        max_bytes = 10 * 1024 * 1024  # 10MB safety limit

        # SSRF-safe fetch — DNS pinning, private/loopback rejection,
        # streaming with hard byte cap, urljoin-based redirect.
        # Same primitive used by AS image route to keep semantics aligned.
        from ai_gateway_core.security import SafeFetchError, safe_fetch

        try:
            content_bytes = await safe_fetch(
                raw_url,
                max_bytes=max_bytes,
                max_redirects=5,
                timeout=20.0,
            )
        except SafeFetchError as exc:
            raise ValidationFailedError(f"Failed to fetch url: {exc}") from exc

        # ``safe_fetch`` doesn't expose response headers; sniff content-type
        # later from the bytes via the existing extractor pipeline. The
        # earlier code used Content-Type from the response, but downstream
        # ``extract_text_from_url_content`` already does its own detection
        # so the field is informational.
        content_type: str | None = None

        text, detected_mime = await asyncio.to_thread(
            self._ks._extract_text_from_bytes, content_bytes, raw_url, content_type
        )

        doc_id = str(uuid.uuid4())
        doc = {
            "document_id": doc_id,
            "dataset_id": dataset_id,
            "title": (title or "").strip() or source_url,
            "source_type": "url",
            "source_uri": source_url,
            "mime_type": detected_mime or content_type or "text/html",
            "size_bytes": len(content_bytes),
            "status": "waiting",
            "progress": 0,
            "content": text,
            "metadata": metadata or {},
        }
        await self._save_document_for_dataset(doc, dataset)
        return _with_display_status(await self.db.get_document(doc_id) or doc)

    async def list_documents(
        self,
        user: UserContext,
        dataset_id: str,
        *,
        limit: int = DOCUMENT_LIST_PAGE_CAP,
        offset: int = 0,
    ) -> list[dict[str, Any]]:
        dataset = await self._ks.require_dataset_access(user, dataset_id, required="viewer")
        generation = _dataset_content_generation(dataset)
        active_only = await self._ks._effective_dataset_permission(dataset, user) not in {"owner", "editor"}
        documents = await self.db.list_documents(
            dataset_id=dataset_id,
            limit=_clamp_page_limit(limit, DOCUMENT_LIST_PAGE_CAP),
            offset=max(0, offset),
            active_only=active_only,
            tenant_id=str(dataset.get("tenant_id") or ""),
        )
        await _require_unchanged_dataset_content(
            self._ks,
            user,
            dataset_id,
            generation,
        )
        return [_with_display_status(document) for document in documents]

    async def list_documents_page(
        self,
        user: UserContext,
        dataset_id: str,
        *,
        limit: int = DOCUMENT_LIST_PAGE_CAP,
        offset: int = 0,
    ) -> dict[str, Any]:
        """Return one page and its total under one content-generation fence."""

        dataset = await self._ks.require_dataset_access(user, dataset_id, required="viewer")
        generation = _dataset_content_generation(dataset)
        active_only = await self._ks._effective_dataset_permission(dataset, user) not in {"owner", "editor"}
        page_limit = _clamp_page_limit(limit, DOCUMENT_LIST_PAGE_CAP)
        page_offset = max(0, offset)
        total = int(await self.db.count_documents(
            dataset_id=dataset_id, active_only=active_only,
            tenant_id=str(dataset.get("tenant_id") or ""),
        ) or 0)
        documents = await self.db.list_documents(
            dataset_id=dataset_id,
            limit=page_limit,
            offset=page_offset,
            active_only=active_only,
            tenant_id=str(dataset.get("tenant_id") or ""),
        )
        await _require_unchanged_dataset_content(
            self._ks,
            user,
            dataset_id,
            generation,
        )
        return {
            "items": [_with_display_status(document) for document in documents],
            "total": total,
            "limit": page_limit,
            "offset": page_offset,
        }

    async def count_documents(self, user: UserContext, dataset_id: str) -> int:
        """Total document rows the caller may see (pagination total)."""
        dataset = await self._ks.require_dataset_access(user, dataset_id, required="viewer")
        active_only = await self._ks._effective_dataset_permission(dataset, user) not in {"owner", "editor"}
        return await self.db.count_documents(
            dataset_id=dataset_id, active_only=active_only,
            tenant_id=str(dataset.get("tenant_id") or ""),
        )

    async def list_all_document_ids(
        self, user: UserContext, dataset_id: str
    ) -> list[str]:
        """Enumerate every document ID in the dataset without a page cap.

        ``list_documents`` intentionally caps a single page (used by the list
        UI); bulk operations such as batch-reindex must see all documents or
        they silently skip everything past the cap.
        """
        dataset = await self._ks.require_dataset_access(user, dataset_id, required="viewer")
        generation = _dataset_content_generation(dataset)
        active_only = await self._ks._effective_dataset_permission(dataset, user) not in {"owner", "editor"}
        document_ids = await self.db.list_document_ids_by_dataset(
            dataset_id, active_only=active_only,
            tenant_id=str(dataset.get("tenant_id") or ""),
        )
        await _require_unchanged_dataset_content(
            self._ks,
            user,
            dataset_id,
            generation,
        )
        return document_ids

    async def get_document(
        self, user: UserContext, dataset_id: str, document_id: str
    ) -> dict[str, Any]:
        dataset = await self._ks.require_dataset_access(user, dataset_id, required="viewer")
        generation = _dataset_content_generation(dataset)
        doc = await self.db.get_document(document_id)
        if not doc or str(doc.get("dataset_id")) != dataset_id:
            raise ValidationFailedError("document not found")
        await _require_unchanged_dataset_content(
            self._ks,
            user,
            dataset_id,
            generation,
        )
        # Editors need the management view to repair disabled/archived rows.
        # Ordinary viewers may only read serving source bytes.
        effective_permission = await self._ks._effective_dataset_permission(dataset, user)
        if effective_permission not in {"owner", "editor"}:
            filter_active = getattr(self.db, "filter_active_document_ids", None)
            if not callable(filter_active):
                raise ValidationFailedError("Document active-state authority is unavailable")
            active = await filter_active(
                dataset_id, str(dataset.get("tenant_id") or ""), [document_id],
            )
            if document_id not in set(active or ()):
                raise ValidationFailedError("document not found")
        return _with_display_status(doc)

    async def enqueue_ingest(self, dataset_id: str, document_id: str) -> None:
        # Worker will be injected from app.state; this is a convenience for API.
        dataset = await self._ks._get_dataset_or_404(dataset_id)
        _require_dataset_index_writable(dataset)
        worker = getattr(self._ks, "_worker", None) if self._ks else None
        if worker is None:
            return
        await worker.enqueue(dataset_id, document_id)

    async def delete_document(
        self, user: UserContext, dataset_id: str, document_id: str,
        *, _confluence_source: tuple[dict[str, Any], dict[str, Any]] | None = None,
    ) -> bool:
        dataset = await self._ks.require_dataset_access(user, dataset_id, required="editor")
        deletion_target = ("document_delete", document_id)
        _require_dataset_index_writable(
            dataset,
            allowed_deletion=deletion_target,
        )
        lease_factory = getattr(self.db, "dataset_index_delete_lease", None)
        set_fence = getattr(self.db, "set_dataset_index_deletion_fence", None)
        clear_fence = getattr(self.db, "clear_dataset_index_deletion_fence", None)
        delete_vectors = getattr(
            self._ks.vector_store,
            "delete_document_points",
            None,
        )
        storage = getattr(self._ks, "image_storage_service", None)
        delete_assets = getattr(storage, "delete_document_assets", None)
        if not all(
            callable(value)
            for value in (lease_factory, set_fence, clear_fence, delete_vectors)
        ) or (storage is not None and not callable(delete_assets)):
            raise ValidationFailedError(
                "document deletion is unavailable without the index lifecycle fence"
            )

        async with lease_factory(dataset_id) as lease_connection:
            authoritative = await self.db.get_dataset(
                dataset_id,
                connection=lease_connection,
            )
            if not authoritative:
                raise ValidationFailedError("dataset not found")
            _require_dataset_index_writable(
                authoritative,
                allowed_deletion=deletion_target,
            )
            tenant_id = str(authoritative.get("tenant_id") or "").strip()
            if not tenant_id or tenant_id != str(dataset.get("tenant_id") or "").strip():
                raise ValidationFailedError(
                    "dataset tenant identity changed during document deletion"
                )
            document = await self.db.get_document(
                document_id,
                connection=lease_connection,
            )
            if document is not None and str(document.get("dataset_id") or "") != dataset_id:
                raise ValidationFailedError("document not found")
            if _confluence_source is not None:
                from ...persistence.confluence_source_detach import prepare_source_removal

                binding, page = _confluence_source
                async with lease_connection.transaction():
                    disposition = await prepare_source_removal(
                        connection=lease_connection, dataset_id=dataset_id,
                        tenant_id=tenant_id, document=document, binding=binding, page=page,
                    )
                if disposition in {"detached", "absent"}:
                    return False
            if document is None:
                existing_fence = dataset_index_deletion_fence(authoritative)
                if existing_fence != make_dataset_index_deletion_fence(
                    "document_delete",
                    document_id,
                ):
                    raise ValidationFailedError("document not found")
                cleared = await clear_fence(
                    dataset_id,
                    operation="document_delete",
                    target_id=document_id,
                    connection=lease_connection,
                )
                if not cleared:
                    raise ValidationFailedError(
                        "document deletion fence recovery could not be committed"
                    )
                # The row was committed before a prior attempt failed to clear
                # this exact marker. Clearing it completes that retry.
                return True

            try:
                authoritative, _marker_created = await set_fence(
                    dataset_id,
                    operation="document_delete",
                    target_id=document_id,
                    connection=lease_connection,
                )
            except RuntimeError as exc:
                raise ValidationFailedError(str(exc)) from exc
            if str(authoritative.get("tenant_id") or "").strip() != tenant_id:
                raise ValidationFailedError(
                    "dataset tenant identity changed during document deletion"
                )
            await delete_vectors(
                tenant_id=tenant_id,
                dataset_id=dataset_id,
                document_id=document_id,
                lifecycle_lease_held=True,
            )
            if callable(delete_assets):
                await delete_assets(
                    tenant_id=tenant_id,
                    document_id=document_id,
                )
            if _confluence_source is not None:
                from ...persistence.confluence_source_detach import delete_source_record

                # Keep the reference until external deletion succeeds. The last
                # reference, document and exact fence-clear then commit together,
                # preventing ON DELETE SET NULL from erasing retry ownership.
                async with lease_connection.transaction():
                    await delete_source_record(
                        connection=lease_connection, page=_confluence_source[1],
                    )
                    deleted = await self.db.delete_document(
                        document_id, connection=lease_connection,
                    )
                    if not deleted:
                        raise ValidationFailedError("Confluence document deletion failed")
                    cleared = await clear_fence(
                        dataset_id, operation="document_delete", target_id=document_id,
                        connection=lease_connection,
                    )
                    if not cleared:
                        raise ValidationFailedError("Confluence deletion fence could not be cleared")
                return True
            deleted = await self.db.delete_document(
                document_id,
                connection=lease_connection,
            )
            if not deleted:
                raise ValidationFailedError(
                    "document database deletion failed; index deletion fence remains"
                )
            cleared = await clear_fence(
                dataset_id,
                operation="document_delete",
                target_id=document_id,
                connection=lease_connection,
            )
            if not cleared:
                raise ValidationFailedError(
                    "document deletion committed but its index fence remains pending"
                )
            return True

    # ========================================================================
    # Segment Operations
    # ========================================================================

    async def list_segments(
        self,
        user: UserContext,
        dataset_id: str,
        document_id: str | None = None,
        q: str | None = None,
        *,
        limit: int = SEGMENT_LIST_PAGE_CAP,
        offset: int = 0,
    ) -> list[dict[str, Any]]:
        dataset = await self._ks.require_dataset_access(user, dataset_id, required="viewer")
        generation = _dataset_content_generation(dataset)
        active_only = await self._ks._effective_dataset_permission(dataset, user) not in {"owner", "editor"}
        segments = await self.db.list_segments(
            dataset_id=dataset_id,
            document_id=document_id,
            query_text=q,
            limit=_clamp_page_limit(limit, SEGMENT_LIST_PAGE_CAP),
            offset=max(0, offset),
            active_only=active_only,
            tenant_id=str(dataset.get("tenant_id") or ""),
        )
        await _require_unchanged_dataset_content(
            self._ks,
            user,
            dataset_id,
            generation,
        )
        return segments

    async def list_segments_page(
        self,
        user: UserContext,
        dataset_id: str,
        document_id: str | None = None,
        q: str | None = None,
        *,
        limit: int = SEGMENT_LIST_PAGE_CAP,
        offset: int = 0,
    ) -> dict[str, Any]:
        """Return a filtered segment page and exact total under one fence."""

        dataset = await self._ks.require_dataset_access(user, dataset_id, required="viewer")
        generation = _dataset_content_generation(dataset)
        active_only = await self._ks._effective_dataset_permission(dataset, user) not in {"owner", "editor"}
        page_limit = _clamp_page_limit(limit, SEGMENT_LIST_PAGE_CAP)
        page_offset = max(0, offset)
        total = int(
            await self.db.count_segments(
                dataset_id=dataset_id,
                document_id=document_id,
                query_text=q,
                active_only=active_only,
                tenant_id=str(dataset.get("tenant_id") or ""),
            )
            or 0
        )
        segments = await self.db.list_segments(
            dataset_id=dataset_id,
            document_id=document_id,
            query_text=q,
            limit=page_limit,
            offset=page_offset,
            active_only=active_only,
            tenant_id=str(dataset.get("tenant_id") or ""),
        )
        await _require_unchanged_dataset_content(
            self._ks,
            user,
            dataset_id,
            generation,
        )
        return {
            "items": segments,
            "total": total,
            "limit": page_limit,
            "offset": page_offset,
        }

    async def count_segments(
        self,
        user: UserContext,
        dataset_id: str,
        document_id: str | None = None,
        q: str | None = None,
    ) -> int:
        """Total segment rows for the filtered list (pagination total)."""
        dataset = await self._ks.require_dataset_access(user, dataset_id, required="viewer")
        active_only = await self._ks._effective_dataset_permission(dataset, user) not in {"owner", "editor"}
        return await self.db.count_segments(
            dataset_id=dataset_id, document_id=document_id, query_text=q,
            active_only=active_only, tenant_id=str(dataset.get("tenant_id") or ""),
        )

    async def update_segment(
        self,
        user: UserContext,
        dataset_id: str,
        segment_id: str,
        new_text: str,
        new_answer: str | None = None,
        new_keywords: list[str] | None = None,
    ) -> dict[str, Any]:
        """Hot-update one segment under its stable point ID.

        ``new_answer`` / ``new_keywords`` follow the PUT contract: ``None``
        leaves the stored value untouched, ``""`` / ``[]`` clears it. The
        content hash is refreshed alongside the text so incremental re-ingest
        skip logic never sees a stale hash for an edited segment.
        """
        dataset = await self._ks.require_dataset_access(user, dataset_id, required="editor")
        _require_dataset_index_writable(dataset)
        seg = await self.db.get_segment(segment_id)
        if not seg or str(seg.get("dataset_id")) != dataset_id:
            raise ValidationFailedError("segment not found")

        # Sanitize text for PostgreSQL
        clean_text = self._ks._sanitize_text_for_db(new_text)
        clean_answer = (
            self._ks._sanitize_text_for_db(new_answer)
            if new_answer is not None
            else None
        )
        content_hash = hashlib.sha256(clean_text.encode("utf-8")).hexdigest()

        # Re-embed and upsert under one cross-replica segment generation.
        embedding_provider = str(dataset.get("embedding_provider") or "local")
        embedding_model = str(dataset.get("embedding_model") or "hash-384")
        embedding_config = _ensure_dict(dataset.get("embedding_config"))
        dim = int(dataset.get("embedding_dimension") or 0) or None
        econf = await maybe_await(
            self._ks._resolve_embedding_config(
                provider=embedding_provider,
                model=embedding_model,
                embedding_config=embedding_config,
                tenant_id=str(dataset.get("tenant_id") or ""),
            )
        )

        set_index_state = getattr(self.db, "set_segment_index_state", None)
        lease_factory = getattr(self.db, "segment_index_update_lease", None)
        if not callable(set_index_state) or not callable(lease_factory):
            raise ValidationFailedError(
                "segment editing requires the fail-closed index state contract"
            )

        initial_document_id = str(seg.get("document_id") or "").strip()
        if not initial_document_id:
            raise ValidationFailedError("segment document identity is unavailable")
        async with lease_factory(
            dataset_id,
            initial_document_id,
            segment_id,
        ) as lease_connection:
            authoritative_dataset = await self.db.get_dataset(
                dataset_id,
                connection=lease_connection,
            )
            if not authoritative_dataset:
                raise ValidationFailedError("dataset not found")
            _require_dataset_index_writable(authoritative_dataset)
            if dataset_ingestion_identity(authoritative_dataset) != dataset_ingestion_identity(
                dataset
            ):
                raise ValidationFailedError(
                    "dataset ingestion identity changed during segment update; retry"
                )
            current = await self.db.get_segment(
                segment_id,
                connection=lease_connection,
            )
            if not current or str(current.get("dataset_id") or "") != dataset_id:
                raise ValidationFailedError("segment not found")
            seg = current
            document_id = str(seg.get("document_id") or "").strip()
            document = await self.db.get_document(
                document_id,
                connection=lease_connection,
            )
            if not document or str(document.get("dataset_id") or "") != dataset_id:
                raise ValidationFailedError("segment document not found")
            _require_document_active_for_manual_index_write(document)
            segment_metadata = _ensure_dict(seg.get("metadata"))
            segment_metadata.pop("source_version", None)
            segment_metadata.pop("source_hash", None)
            segment_metadata["manual_source_override"] = True
            seg = {**seg, "metadata": segment_metadata}

            # Autocommit the hidden state before either authoritative text or
            # Qdrant changes. A crash or stale payload remains non-retrievable.
            await set_index_state(
                segment_id,
                "pending",
                connection=lease_connection,
            )
            try:
                await self.db.update_segment(
                    segment_id,
                    text=clean_text,
                    answer=clean_answer,
                    keywords=new_keywords,
                    content_hash=content_hash,
                    metadata=segment_metadata,
                    connection=lease_connection,
                )
                embedder: BaseEmbedding | None = None
                try:
                    embedder = create_embedding(econf, dimension=dim)
                    vec = (
                        await asyncio.wait_for(
                            embedder.embed_documents([clean_text]),
                            timeout=float(econf.timeout_seconds) + 10.0,
                        )
                    )[0]
                    collection = await self._ks.vector_store.ensure_collection(
                        dataset_id=dataset_id,
                        dimension=embedder.dimension,
                        collection_name=str(dataset.get("collection_name") or "") or None,
                        tenant_id=str(dataset.get("tenant_id") or ""),
                        lifecycle_lease_held=True,
                        **_lexical_ensure_kwargs(dataset.get("index_config")),
                    )
                finally:
                    if embedder:
                        await embedder.close()

                from qdrant_client.http import models as qmodels  # type: ignore

                pid = str(seg.get("vector_id") or seg.get("segment_id") or "")
                payload = _segment_vector_payload(
                    dataset=authoritative_dataset,
                    segment=seg,
                    text=clean_text,
                )
                if not pid or not collection:
                    raise RuntimeError("segment vector identity is unavailable")
                await self._upsert_for_dataset_identity(
                    dataset=dataset,
                    collection=collection,
                    points=[qmodels.PointStruct(id=pid, vector=vec, payload=payload)],
                    lifecycle_lease_held=True,
                )
                await set_index_state(
                    segment_id,
                    "completed",
                    connection=lease_connection,
                )
            except Exception as exc:
                try:
                    await set_index_state(
                        segment_id,
                        "error",
                        error="vector update failed",
                        connection=lease_connection,
                    )
                except Exception as state_exc:
                    raise ValidationFailedError(
                        "segment vector update failed and its hidden error state "
                        "could not be confirmed"
                    ) from state_exc
                raise ValidationFailedError(
                    "segment vector update failed; the segment remains hidden until retry"
                ) from exc

        return await self.db.get_segment(segment_id) or {
            **seg,
            "text": clean_text,
            "status": "completed",
        }

    async def delete_segment(self, user: UserContext, dataset_id: str, segment_id: str) -> bool:
        dataset = await self._ks.require_dataset_access(user, dataset_id, required="editor")
        deletion_target = ("segment_delete", segment_id)
        _require_dataset_index_writable(
            dataset,
            allowed_deletion=deletion_target,
        )
        lease_factory = getattr(self.db, "dataset_index_delete_lease", None)
        set_fence = getattr(self.db, "set_dataset_index_deletion_fence", None)
        clear_fence = getattr(self.db, "clear_dataset_index_deletion_fence", None)
        delete_segment_points = getattr(
            self._ks.vector_store,
            "delete_segment_points",
            None,
        )
        if not all(
            callable(value)
            for value in (
                lease_factory,
                set_fence,
                clear_fence,
                delete_segment_points,
            )
        ):
            raise ValidationFailedError(
                "segment deletion is unavailable without the index lifecycle fence"
            )

        document_id = ""
        async with lease_factory(dataset_id) as lease_connection:
            authoritative = await self.db.get_dataset(
                dataset_id,
                connection=lease_connection,
            )
            if not authoritative:
                raise ValidationFailedError("dataset not found")
            _require_dataset_index_writable(
                authoritative,
                allowed_deletion=deletion_target,
            )
            tenant_id = str(authoritative.get("tenant_id") or "").strip()
            if not tenant_id or tenant_id != str(dataset.get("tenant_id") or "").strip():
                raise ValidationFailedError(
                    "dataset tenant identity changed during segment deletion"
                )
            seg = await self.db.get_segment(
                segment_id,
                connection=lease_connection,
            )
            if seg is not None and str(seg.get("dataset_id") or "") != dataset_id:
                raise ValidationFailedError(
                    "segment dataset identity changed; index deletion fence remains"
                )
            if seg is None:
                existing_fence = dataset_index_deletion_fence(authoritative)
                if existing_fence == make_dataset_index_deletion_fence(
                    "segment_delete",
                    segment_id,
                ):
                    cleared = await clear_fence(
                        dataset_id,
                        operation="segment_delete",
                        target_id=segment_id,
                        connection=lease_connection,
                    )
                    if not cleared:
                        raise ValidationFailedError(
                            "segment deletion fence recovery could not be committed"
                        )
                    return True
                raise ValidationFailedError("segment not found")

            document_id = str(seg.get("document_id") or "").strip()
            point_id = str(seg.get("vector_id") or seg.get("segment_id") or "").strip()
            if not point_id or not document_id:
                raise ValidationFailedError(
                    "segment ownership identity is incomplete"
                )
            document = await self.db.get_document(
                document_id,
                connection=lease_connection,
            )
            if not document or str(document.get("dataset_id") or "") != dataset_id:
                raise ValidationFailedError("segment document not found")
            _require_document_active_for_manual_index_write(document)

            content_type = str(seg.get("content_type") or "text").strip().lower()
            if content_type == "image":
                raise ValidationFailedError(
                    "image segment deletion is disabled until the durable image "
                    "receipt and object can be retired atomically"
                )

            try:
                authoritative, _marker_created = await set_fence(
                    dataset_id,
                    operation="segment_delete",
                    target_id=segment_id,
                    connection=lease_connection,
                )
            except RuntimeError as exc:
                raise ValidationFailedError(str(exc)) from exc
            if str(authoritative.get("tenant_id") or "").strip() != tenant_id:
                raise ValidationFailedError(
                    "dataset tenant identity changed during segment deletion"
                )

            await delete_segment_points(
                tenant_id=tenant_id,
                dataset_id=dataset_id,
                document_id=document_id,
                segment_id=point_id,
                lifecycle_lease_held=True,
            )
            deleted = await self.db.delete_segment(
                segment_id,
                connection=lease_connection,
            )
            if not deleted:
                raise ValidationFailedError(
                    "segment database deletion failed; index deletion fence remains"
                )
            cleared = await clear_fence(
                dataset_id,
                operation="segment_delete",
                target_id=segment_id,
                connection=lease_connection,
            )
            if not cleared:
                raise ValidationFailedError(
                    "segment deletion committed but its index fence remains pending"
                )

        # Update document segment_count after deletion
        if document_id:
            try:
                await self.db.refresh_document_segment_count(document_id)
            except Exception:
                logger.warning(
                    "Failed to refresh segment count after deleting %s",
                    segment_id,
                    exc_info=True,
                )
        return True

    # ========================================================================
    # Document Enable/Disable/Archive (Dify-style)
    # ========================================================================

    def _lifecycle_reindex_is_stale(self, document: dict[str, Any]) -> bool:
        updated_at = document.get("updated_at")
        if isinstance(updated_at, str):
            try:
                updated_at = datetime.fromisoformat(updated_at.replace("Z", "+00:00"))
            except ValueError:
                return False
        if not isinstance(updated_at, datetime):
            return False
        knowledge_settings = getattr(self.settings, "knowledge", None)
        threshold_minutes = max(
            int(
                getattr(
                    knowledge_settings,
                    "lifecycle_reindex_stale_minutes",
                    15,
                )
            ),
            1,
        )
        now = datetime.now(updated_at.tzinfo or timezone.utc)
        comparable = updated_at
        if comparable.tzinfo is None:
            comparable = comparable.replace(tzinfo=timezone.utc)
        return now - comparable >= timedelta(minutes=threshold_minutes)

    async def _transition_document_lifecycle(
        self,
        *,
        user: UserContext,
        dataset: dict[str, Any],
        dataset_id: str,
        document_id: str,
        desired_enabled: bool,
        desired_archived: bool,
        state_updates: dict[str, Any],
        requested_action: str,
    ) -> dict[str, Any]:
        """Apply a durable, replayable document lifecycle transition."""

        lease_factory = getattr(self.db, "dataset_index_delete_lease", None)
        delete_vectors = getattr(
            self._ks.vector_store,
            "delete_document_points",
            None,
        )
        clear_marker = getattr(self.db, "clear_document_lifecycle_marker", None)
        bump_revision = getattr(self.db, "bump_dataset_content_revision", None)
        if not all(
            callable(value)
            for value in (lease_factory, delete_vectors, clear_marker, bump_revision)
        ):
            raise ValidationFailedError(
                "document lifecycle changes require the index lifecycle fence"
            )

        async with lease_factory(dataset_id) as lease_connection:
            authoritative_dataset = await self.db.get_dataset(
                dataset_id,
                connection=lease_connection,
            )
            if not authoritative_dataset:
                raise ValidationFailedError("dataset not found")
            _require_dataset_index_writable(authoritative_dataset)
            tenant_id = str(authoritative_dataset.get("tenant_id") or "").strip()
            if not tenant_id or tenant_id != str(dataset.get("tenant_id") or "").strip():
                raise ValidationFailedError(
                    "dataset tenant identity changed during document lifecycle update"
                )

            document = await self.db.get_document(
                document_id,
                connection=lease_connection,
            )
            if not document or str(document.get("dataset_id") or "") != dataset_id:
                raise ValidationFailedError("document not found")

            metadata = _ensure_dict(document.get("metadata"))
            pending = _ensure_dict(metadata.get(DOCUMENT_LIFECYCLE_REINDEX_KEY))
            current_enabled = bool(document.get("enabled", True))
            current_archived = bool(document.get("archived", False))
            desired_active = desired_enabled and not desired_archived
            current_active = current_enabled and not current_archived

            marker_status = str(pending.get("status") or "").strip()
            if pending:
                if marker_status not in {"deactivating", "pending"}:
                    raise ValidationFailedError(
                        "document lifecycle marker is malformed; refusing an unsafe transition"
                    )
                if "desired_enabled" in pending and "desired_archived" in pending:
                    marker_target = (
                        bool(pending.get("desired_enabled")),
                        bool(pending.get("desired_archived")),
                    )
                    if marker_target != (desired_enabled, desired_archived):
                        raise ValidationFailedError(
                            "a different document lifecycle transition is pending; retry "
                            "the original target first"
                        )
                elif (marker_status == "pending") != desired_active:
                    # Legacy markers did not persist the target fields. Their
                    # status still unambiguously distinguishes restore from
                    # inactive cleanup.
                    raise ValidationFailedError(
                        "a different document lifecycle transition is pending"
                    )

            if desired_active and current_active and not pending:
                # Idempotent active request: no vectors were removed, so a
                # synthetic reindex would only create churn.
                await self.db.update_document_fields(
                    document_id,
                    state_updates,
                    connection=lease_connection,
                )
                return _with_display_status(
                    await self.db.get_document(
                        document_id,
                        connection=lease_connection,
                    )
                    or document
                )

            if not desired_active:
                # Persist the inactive state and durable marker first. Every DB
                # authority path rejects any document carrying this marker, so
                # a crash before/during the multi-collection sweep stays hidden
                # and a same-target request can safely replay the cleanup.
                if not pending:
                    pending = {
                        "status": "deactivating",
                        "desired_enabled": desired_enabled,
                        "desired_archived": desired_archived,
                        "requested_action": requested_action,
                        "requested_at": datetime.now(timezone.utc).isoformat(),
                        "requested_by": user.user_id,
                    }
                metadata[DOCUMENT_LIFECYCLE_REINDEX_KEY] = pending
                inactive_updates = dict(state_updates)
                inactive_updates["metadata"] = metadata
                await self.db.update_document_fields(
                    document_id,
                    inactive_updates,
                    connection=lease_connection,
                    allow_lifecycle_marker_update=True,
                )
                try:
                    await delete_vectors(
                        tenant_id=tenant_id,
                        dataset_id=dataset_id,
                        document_id=document_id,
                        lifecycle_lease_held=True,
                    )
                except Exception as exc:
                    raise ValidationFailedError(
                        "document vectors could not be fully deactivated; the document "
                        "remains hidden and the same transition is retryable"
                    ) from exc
                # PRD T1 unified lifecycle contract (§885-886): deactivation
                # changes which content is visible for retrieval, so the
                # dataset's authoritative revision advances with it — the
                # retrieval cache is keyed on the revision fingerprint, and a
                # result cached before the document went hidden must not
                # outlive the transition (the restore direction bumps
                # atomically inside the activation status write). A crash
                # before the marker clear leaves the document hidden under
                # its marker, and a replay simply bumps once more.
                await bump_revision(
                    dataset_id,
                    connection=lease_connection,
                )
                cleared = await clear_marker(
                    document_id,
                    expected_status="deactivating",
                    connection=lease_connection,
                )
                if not cleared:
                    raise ValidationFailedError(
                        "document vectors were deactivated but lifecycle finalization failed"
                    )
                metadata.pop(DOCUMENT_LIFECYCLE_REINDEX_KEY, None)
                return _with_display_status(
                    await self.db.get_document(
                        document_id,
                        connection=lease_connection,
                    )
                    or {**document, **inactive_updates}
                )

            worker = getattr(self._ks, "_worker", None)
            enqueue_claimed = getattr(worker, "enqueue_claimed", None)
            if not callable(enqueue_claimed):
                raise ValidationFailedError(
                    "document restore requires an available ingestion worker"
                )

            document_status = str(document.get("status") or "")
            stale_processing = bool(pending) and document_status not in {
                "error",
                "completed",
            } and self._lifecycle_reindex_is_stale(document)
            if pending and document_status not in {"error", "completed"}:
                if not stale_processing:
                    # A fresh worker owns this durable generation. Do not
                    # duplicate its work or destroy partial progress.
                    return _with_display_status(document)
            else:
                if not pending:
                    pending = {
                        "status": "pending",
                        "desired_enabled": True,
                        "desired_archived": False,
                        "requested_action": requested_action,
                        "requested_at": datetime.now(timezone.utc).isoformat(),
                        "requested_by": user.user_id,
                    }
                    metadata[DOCUMENT_LIFECYCLE_REINDEX_KEY] = pending
                    await self.db.update_document_fields(
                        document_id,
                        {"metadata": metadata},
                        connection=lease_connection,
                        allow_lifecycle_marker_update=True,
                    )

                # Hot restore (PRD T1 item 6): a text-only generation keeps
                # its persisted rows and rebuilds every point row-by-row
                # through the reembed engine at existing segment/point
                # identity, instead of the legacy clear-rows full re-ingest.
                # Image-capable generations cannot hot-restore because the
                # reembed engine repairs text rows only; image points would
                # stay deleted, so they keep the clean rebuild.
                hot_restore = (
                    str(metadata.get("processing_mode") or "text_only")
                    not in {"multimodal", "scanned"}
                    and "images_embedded" not in metadata
                    and "embedded_image_count" not in metadata
                    and int(metadata.get("image_count") or 0) <= 0
                )
                if not hot_restore:
                    # Initial restore and explicit failed retry rebuild from a
                    # clean generation. A stale-processing replay deliberately
                    # skips this branch so useful partial progress is retained.
                    receipt_changed = False
                    for receipt_key in ("images_embedded", "embedded_image_count"):
                        if receipt_key in metadata:
                            metadata.pop(receipt_key, None)
                            receipt_changed = True
                    if receipt_changed:
                        # The old receipt only proves persistence in the generation
                        # that is about to be swept. Keeping it would make retry
                        # skip image embedding and falsely complete without images.
                        cleared_receipts = await self.db.clear_document_legacy_image_receipts(
                            document_id,
                            dataset_id,
                            connection=lease_connection,
                        )
                        if not cleared_receipts:
                            raise ValidationFailedError(
                                "document restore lost image-receipt generation authority"
                            )
                try:
                    await delete_vectors(
                        tenant_id=tenant_id,
                        dataset_id=dataset_id,
                        document_id=document_id,
                        lifecycle_lease_held=True,
                    )
                except Exception as exc:
                    raise ValidationFailedError(
                        "document restore cleanup failed; the document remains hidden"
                    ) from exc
                if hot_restore:
                    pin_ingest_action = getattr(
                        self.db, "pin_document_ingest_action", None
                    )
                    if not callable(pin_ingest_action):
                        raise ValidationFailedError(
                            "document hot restore requires the ingest-action pin"
                        )
                    pinned = await pin_ingest_action(
                        dataset_id,
                        document_id,
                        "reembed",
                        connection=lease_connection,
                    )
                    if not pinned:
                        raise ValidationFailedError(
                            "document restore could not pin the rebuild verb"
                        )
                else:
                    await self.db.delete_segments_by_document(
                        document_id,
                        connection=lease_connection,
                    )
                    await self.db.update_document_fields(
                        document_id,
                        {"segment_count": 0},
                        connection=lease_connection,
                    )

            # The dataset-exclusive lifecycle lease is stronger than the
            # normal dataset/document enqueue claim. Persist the outbox state
            # on this connection, then publish to memory only after release.
            await self.db.update_document_status(
                document_id,
                status="waiting",
                progress=0,
                error="",
                connection=lease_connection,
            )

        # Publish only after releasing the dataset-exclusive cleanup lease.
        # A process crash or queue failure leaves ``waiting`` durable and the
        # periodic SKIP LOCKED recovery pass will replay it after the TTL.
        try:
            await enqueue_claimed(dataset_id, document_id)
        except Exception as exc:
            raise ValidationFailedError(
                "document restore reindex could not be queued; durable recovery "
                "will retry the pending lifecycle generation"
            ) from exc

        result = await self.db.get_document(document_id)
        return _with_display_status(
            result
            or {
                **document,
                "metadata": metadata,
                "segment_count": document.get("segment_count", 0)
                if stale_processing
                else 0,
                "status": "waiting",
                "progress": 0,
            }
        )

    async def set_document_enabled(
        self, user: UserContext, dataset_id: str, document_id: str, enabled: bool
    ) -> dict[str, Any]:
        """Enable or disable a document."""
        dataset = await self._ks.require_dataset_access(user, dataset_id, required="editor")
        _require_dataset_index_writable(dataset)
        doc = await self.db.get_document(document_id)
        if not doc or str(doc.get("dataset_id")) != dataset_id:
            raise ValidationFailedError("document not found")

        update_data: dict[str, Any] = {"enabled": enabled}
        if not enabled:
            update_data["disabled_at"] = datetime.utcnow()
            update_data["disabled_by"] = user.user_id
        else:
            update_data["disabled_at"] = None
            update_data["disabled_by"] = None

        return await self._transition_document_lifecycle(
            user=user,
            dataset=dataset,
            dataset_id=dataset_id,
            document_id=document_id,
            desired_enabled=enabled,
            desired_archived=bool(doc.get("archived", False)),
            state_updates=update_data,
            requested_action="enable" if enabled else "disable",
        )

    async def set_document_archived(
        self,
        user: UserContext,
        dataset_id: str,
        document_id: str,
        archived: bool,
        reason: str | None = None,
    ) -> dict[str, Any]:
        """Archive or unarchive a document."""
        dataset = await self._ks.require_dataset_access(user, dataset_id, required="editor")
        _require_dataset_index_writable(dataset)
        doc = await self.db.get_document(document_id)
        if not doc or str(doc.get("dataset_id")) != dataset_id:
            raise ValidationFailedError("document not found")

        update_data: dict[str, Any] = {"archived": archived}
        if archived:
            update_data["archived_at"] = datetime.utcnow()
            update_data["archived_by"] = user.user_id
            update_data["archived_reason"] = reason
        else:
            update_data["archived_at"] = None
            update_data["archived_by"] = None
            update_data["archived_reason"] = None

        return await self._transition_document_lifecycle(
            user=user,
            dataset=dataset,
            dataset_id=dataset_id,
            document_id=document_id,
            desired_enabled=bool(doc.get("enabled", True)),
            desired_archived=archived,
            state_updates=update_data,
            requested_action="archive" if archived else "unarchive",
        )

    async def update_document(
        self,
        user: UserContext,
        dataset_id: str,
        document_id: str,
        update_data: dict[str, Any],
    ) -> dict[str, Any]:
        """Update document metadata."""
        dataset = await self._ks.require_dataset_access(user, dataset_id, required="editor")
        _require_dataset_index_writable(dataset)
        doc = await self.db.get_document(document_id)
        if not doc or str(doc.get("dataset_id")) != dataset_id:
            raise ValidationFailedError("document not found")

        allowed_fields = {"title", "metadata", "doc_type", "doc_language"}
        filtered = {k: v for k, v in update_data.items() if k in allowed_fields}
        if "metadata" in filtered:
            _require_no_reserved_document_metadata(filtered["metadata"])
        if filtered:
            await self.db.update_document_fields(document_id, filtered)
        return _with_display_status(await self.db.get_document(document_id) or doc)

    # ========================================================================
    # Batch Operations
    # ========================================================================

    async def batch_create_documents(
        self,
        user: UserContext,
        dataset_id: str,
        documents: list[Any],
        process_rule: dict[str, Any] | None = None,
        batch_name: str | None = None,
    ) -> dict[str, Any]:
        """Batch create documents from text."""
        # Documented gap (PRD T1 item 7): the Dify-shaped ProcessRuleSchema
        # payload stays ACCEPTED for wire compatibility but is intentionally
        # UNWIRED here. The authoritative rule snapshots are recorded at
        # generation-open from the dataset's live chunking config (worker
        # _record_generation_process_rule / route _record_ingest_execution,
        # canonical dialect {"chunking", "processing_mode"}). Mapping the
        # Dify dialect (pre_processing_rules/segmentation/parent_mode) into
        # that config belongs to a future rule-cascade theme, not to this
        # upgrade — wiring it ad hoc would diverge generation behaviour from
        # the pinned snapshots.
        _ = process_rule
        await self._ks.require_dataset_access(user, dataset_id, required="editor")

        batch_id = batch_name or f"batch_{uuid.uuid4().hex[:8]}"
        created_docs: list[dict[str, Any]] = []
        errors: dict[str, str] = {}

        for i, doc_data in enumerate(documents):
            try:
                title = (
                    doc_data.title
                    if hasattr(doc_data, "title")
                    else doc_data.get("title", f"doc_{i}")
                )
                content = (
                    doc_data.content
                    if hasattr(doc_data, "content")
                    else doc_data.get("content", "")
                )
                metadata = (
                    doc_data.metadata
                    if hasattr(doc_data, "metadata")
                    else doc_data.get("metadata", {})
                )

                doc = await self.create_document_from_text(
                    user, dataset_id, title=title, content=content, metadata=metadata
                )
                # Set batch ID
                await self.db.update_document_fields(doc["document_id"], {"batch": batch_id})
                created_docs.append(doc)
            except Exception as e:
                errors[f"doc_{i}"] = str(e)

        return {
            "batch": batch_id,
            "documents": created_docs,
            "success_count": len(created_docs),
            "failed_count": len(errors),
            "errors": errors,
        }

    async def batch_delete_documents(
        self, user: UserContext, dataset_id: str, document_ids: list[str]
    ) -> dict[str, Any]:
        """Batch delete documents."""
        await self._ks.require_dataset_access(user, dataset_id, required="editor")

        success_count = 0
        failed_ids: list[str] = []
        errors: dict[str, str] = {}

        for doc_id in document_ids:
            try:
                ok = await self.delete_document(user, dataset_id, doc_id)
                if ok:
                    success_count += 1
                else:
                    failed_ids.append(doc_id)
                    errors[doc_id] = "not found"
            except Exception as e:
                failed_ids.append(doc_id)
                errors[doc_id] = str(e)

        return {
            "success_count": success_count,
            "failed_count": len(failed_ids),
            "failed_ids": failed_ids,
            "errors": errors,
        }

    # ========================================================================
    # Segment Enable/Disable
    # ========================================================================

    async def set_segment_enabled(
        self,
        user: UserContext,
        dataset_id: str,
        segment_id: str,
        enabled: bool,
        *,
        _authorized_dataset: dict[str, Any] | None = None,
    ) -> dict[str, Any]:
        """Synchronize reversible segment visibility across DB and Qdrant."""
        dataset = _authorized_dataset
        if dataset is None:
            dataset = await self._ks.require_dataset_access(
                user,
                dataset_id,
                required="editor",
            )
            _require_dataset_index_writable(dataset)
        seg = await self.db.get_segment(segment_id)
        if not seg or str(seg.get("dataset_id")) != dataset_id:
            raise ValidationFailedError("segment not found")
        lease_factory = getattr(self.db, "segment_index_update_lease", None)
        set_payload_enabled = getattr(
            self._ks.vector_store,
            "set_segment_payload_enabled",
            None,
        )
        if not callable(lease_factory) or not callable(set_payload_enabled):
            raise ValidationFailedError(
                "segment visibility changes require the serialized index contract"
            )

        initial_document_id = str(seg.get("document_id") or "").strip()
        if not initial_document_id:
            raise ValidationFailedError("segment document identity is unavailable")
        async with lease_factory(
            dataset_id,
            initial_document_id,
            segment_id,
        ) as lease_connection:
            authoritative_dataset = await self.db.get_dataset(
                dataset_id,
                connection=lease_connection,
            )
            current = await self.db.get_segment(
                segment_id,
                connection=lease_connection,
            )
            if (
                not authoritative_dataset
                or dataset_ingestion_identity(authoritative_dataset)
                != dataset_ingestion_identity(dataset)
                or not current
                or str(current.get("dataset_id") or "") != dataset_id
            ):
                raise ValidationFailedError(
                    "segment visibility identity changed; retry the request"
                )
            _require_dataset_index_writable(authoritative_dataset)
            document_id = str(current.get("document_id") or "").strip()
            document = await self.db.get_document(
                document_id,
                connection=lease_connection,
            )
            if not document or str(document.get("dataset_id") or "") != dataset_id:
                raise ValidationFailedError("segment document not found")
            _require_document_active_for_manual_index_write(document)

            update_data: dict[str, Any] = {"enabled": enabled}
            if not enabled:
                update_data["disabled_at"] = datetime.utcnow()
                update_data["disabled_by"] = user.user_id
                # DB authority hides the point before any multi-collection
                # remote mutation. A partial Qdrant failure remains safe and a
                # same-value retry repairs all payloads.
                await self.db.update_segment_fields(
                    segment_id,
                    update_data,
                    connection=lease_connection,
                )
                try:
                    await set_payload_enabled(
                        tenant_id=str(authoritative_dataset.get("tenant_id") or ""),
                        dataset_id=dataset_id,
                        document_id=document_id,
                        segment_id=segment_id,
                        enabled=False,
                        lifecycle_lease_held=True,
                    )
                except Exception as exc:
                    raise ValidationFailedError(
                        "segment remains disabled but Qdrant visibility sync failed; "
                        "retry the same request"
                    ) from exc
            else:
                update_data["disabled_at"] = None
                update_data["disabled_by"] = None
                # Qdrant first, DB authority last: a failed re-enable leaves the
                # segment disabled and retryable. Same-value calls intentionally
                # resynchronize a prior partial update.
                try:
                    await set_payload_enabled(
                        tenant_id=str(authoritative_dataset.get("tenant_id") or ""),
                        dataset_id=dataset_id,
                        document_id=document_id,
                        segment_id=segment_id,
                        enabled=True,
                        lifecycle_lease_held=True,
                    )
                except Exception as exc:
                    raise ValidationFailedError(
                        "segment Qdrant visibility could not be enabled; retry the request"
                    ) from exc
                await self.db.update_segment_fields(
                    segment_id,
                    update_data,
                    connection=lease_connection,
                )

        return await self.db.get_segment(segment_id) or {**seg, **update_data}

    async def set_segments_enabled_batch(
        self,
        user: UserContext,
        dataset_id: str,
        segment_ids: Any,
        enabled: Any,
    ) -> dict[str, Any]:
        """Bound and authorize one batch before any per-segment mutation."""

        normalized_ids, normalized_enabled = _validate_segment_batch_enable_request(
            segment_ids,
            enabled,
        )
        dataset = await self._ks.require_dataset_access(
            user,
            dataset_id,
            required="editor",
        )
        _require_dataset_index_writable(dataset)

        updated = 0
        for segment_id in normalized_ids:
            try:
                await self.set_segment_enabled(
                    user,
                    dataset_id,
                    segment_id,
                    normalized_enabled,
                    _authorized_dataset=dataset,
                )
                updated += 1
            except Exception:
                # Preserve the compatibility endpoint's partial-success
                # contract while keeping authorization outside the loop.
                continue
        return {
            "success": True,
            "updated": updated,
            "total": len(normalized_ids),
        }

    async def create_segment(
        self,
        user: UserContext,
        dataset_id: str,
        document_id: str,
        content: str,
        answer: str | None = None,
        keywords: list[str] | None = None,
    ) -> dict[str, Any]:
        """Create a new segment manually."""
        dataset = await self._ks.require_dataset_access(user, dataset_id, required="editor")
        _require_dataset_index_writable(dataset)
        doc = await self.db.get_document(document_id)
        if not doc or str(doc.get("dataset_id")) != dataset_id:
            raise ValidationFailedError("document not found")
        _require_document_active_for_manual_index_write(doc)
        set_index_state = getattr(self.db, "set_segment_index_state", None)
        lease_factory = getattr(self.db, "document_segment_create_lease", None)
        next_position = getattr(self.db, "next_segment_position", None)
        if not all(callable(value) for value in (set_index_state, lease_factory, next_position)):
            raise ValidationFailedError(
                "segment creation requires the fail-closed index state contract"
            )

        seg_id = str(uuid.uuid4())
        clean_content = self._ks._sanitize_text_for_db(content)
        seg: dict[str, Any] = {}

        # Embed and index the new row under the same cross-replica generation
        # contract as edits. Validate the owning document under the dataset
        # barrier before the first DB mutation, then insert through the held
        # connection so a one-connection pool cannot deadlock.
        async with lease_factory(dataset_id, document_id) as lease_connection:
            authoritative_dataset = await self.db.get_dataset(
                dataset_id,
                connection=lease_connection,
            )
            stored_document = await self.db.get_document(
                document_id,
                connection=lease_connection,
            )
            if authoritative_dataset:
                _require_dataset_index_writable(authoritative_dataset)
            if (
                not authoritative_dataset
                or dataset_ingestion_identity(authoritative_dataset)
                != dataset_ingestion_identity(dataset)
                or not stored_document
                or str(stored_document.get("dataset_id") or "") != dataset_id
            ):
                raise ValidationFailedError(
                    "segment creation identity changed before vector indexing"
                )
            _require_document_active_for_manual_index_write(stored_document)
            position = await next_position(
                dataset_id,
                document_id,
                connection=lease_connection,
            )
            seg = {
                "segment_id": seg_id,
                "dataset_id": dataset_id,
                "document_id": document_id,
                "position": position,
                "level": 3,
                "text": clean_content,
                "token_count": len(clean_content) // 4,
                "word_count": len(clean_content.split()),
                "answer": answer,
                "keywords": keywords or [],
                "created_by": user.user_id,
                "enabled": True,
                "status": "waiting",
                "source_type": "manual",
                "language": "en",
                "metadata": {"content_type": "text"},
            }
            await self.db.insert_segments([seg], connection=lease_connection)
            stored_segment = await self.db.get_segment(
                seg_id,
                connection=lease_connection,
            )
            if not stored_segment or str(stored_segment.get("dataset_id") or "") != dataset_id:
                raise ValidationFailedError("segment creation did not persist its identity")
            await set_index_state(
                seg_id,
                "pending",
                connection=lease_connection,
            )
            try:
                embedding_provider = str(dataset.get("embedding_provider") or "local")
                embedding_model = str(dataset.get("embedding_model") or "hash-384")
                embedding_config = _ensure_dict(dataset.get("embedding_config"))
                dim = int(dataset.get("embedding_dimension") or 0) or None

                econf = await maybe_await(
                    self._ks._resolve_embedding_config(
                        provider=embedding_provider,
                        model=embedding_model,
                        embedding_config=embedding_config,
                        tenant_id=str(dataset.get("tenant_id") or ""),
                    )
                )

                embedder: BaseEmbedding | None = None
                try:
                    embedder = create_embedding(econf, dimension=dim)
                    vec = (
                        await asyncio.wait_for(
                            embedder.embed_documents([clean_content]),
                            timeout=float(econf.timeout_seconds) + 10.0,
                        )
                    )[0]

                    collection = await self._ks.vector_store.ensure_collection(
                        dataset_id=dataset_id,
                        dimension=embedder.dimension,
                        collection_name=str(dataset.get("collection_name") or "") or None,
                        tenant_id=str(dataset.get("tenant_id") or ""),
                        lifecycle_lease_held=True,
                        **_lexical_ensure_kwargs(dataset.get("index_config")),
                    )
                finally:
                    if embedder:
                        await embedder.close()

                from qdrant_client.http import models as qmodels

                payload = _segment_vector_payload(
                    dataset=authoritative_dataset,
                    segment=stored_segment,
                    text=clean_content,
                )
                await self._upsert_for_dataset_identity(
                    dataset=authoritative_dataset,
                    collection=collection,
                    points=[qmodels.PointStruct(id=seg_id, vector=vec, payload=payload)],
                    lifecycle_lease_held=True,
                )
                await self.db.update_segment_fields(
                    seg_id,
                    {"vector_id": seg_id},
                    connection=lease_connection,
                )
                await set_index_state(
                    seg_id,
                    "completed",
                    connection=lease_connection,
                )
            except Exception as exc:
                try:
                    await set_index_state(
                        seg_id,
                        "error",
                        error="vector creation failed",
                        connection=lease_connection,
                    )
                except Exception as state_exc:
                    raise ValidationFailedError(
                        "segment vector creation failed and its hidden error state "
                        "could not be confirmed"
                    ) from state_exc
                raise ValidationFailedError(
                    "segment vector creation failed; the segment remains hidden until retry"
                ) from exc

        # Update document segment_count after creating a new segment
        await self.db.refresh_document_segment_count(document_id)

        return await self.db.get_segment(seg_id) or seg
