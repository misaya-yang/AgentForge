"""Document chunk and segment management."""
from __future__ import annotations

import asyncio
import re
import uuid
from datetime import datetime
from typing import Any

from ...core.auth.user_resolver import UserContext
from ...core.exceptions import ValidationFailedError
from ...core.observability.logging import get_logger
from ...persistence.database import (
    DOCUMENT_LIFECYCLE_REINDEX_KEY,
    DatabaseStorage,
    dataset_index_deletion_fence,
    dataset_ingestion_identity,
)
from .chunking import (
    Chunk,
    ChunkingConfig,
    ContentType,
    flatten_chunks,
    merge_small_chunks,
    process_document,
    validate_persisted_chunking_config,
)
from .common import ensure_dict as _ensure_dict
from .common import maybe_await
from .embedding import BaseEmbedding, EmbeddingConfig, create_embedding
from .lexical_config import LexicalConfig
from .vector_store import VectorStore

logger = get_logger(__name__)


def _lexical_ensure_kwargs(index_config: Any) -> dict[str, Any]:
    lexical = LexicalConfig.from_index_config(index_config)
    return {"lexical_config": lexical} if lexical.configured else {}


def _require_dataset_index_writable(dataset: dict[str, Any]) -> None:
    try:
        deletion_fence = dataset_index_deletion_fence(dataset)
    except RuntimeError as exc:
        raise ValidationFailedError(str(exc)) from exc
    if deletion_fence is not None:
        raise ValidationFailedError(
            "dataset index deletion is pending; indexed content is unavailable"
        )
    LexicalConfig.from_index_config(dataset.get("index_config") or {})


def _require_active_segment_document(
    document: dict[str, Any] | None,
    *,
    dataset_id: str,
) -> dict[str, Any]:
    """Reject manual index writes unless their document is authoritatively active."""

    if not document or str(document.get("dataset_id") or "") != dataset_id:
        raise ValidationFailedError("segment document not found")
    metadata = _ensure_dict(document.get("metadata"))
    if (
        not bool(document.get("enabled", True))
        or bool(document.get("archived", False))
        or str(document.get("status") or "") != "completed"
        or DOCUMENT_LIFECYCLE_REINDEX_KEY in metadata
    ):
        raise ValidationFailedError(
            "segment document is inactive or has a pending lifecycle transition"
        )
    return document


def _segment_vector_payload(
    *,
    dataset: dict[str, Any],
    segment: dict[str, Any],
    text: str,
) -> dict[str, Any]:
    """Project authoritative segment fields into a lossless serving payload."""

    metadata = _ensure_dict(segment.get("metadata"))
    payload = {
        "tenant_id": str(dataset.get("tenant_id") or ""),
        "dataset_id": str(segment.get("dataset_id") or ""),
        "document_id": str(segment.get("document_id") or ""),
        "segment_id": str(segment.get("segment_id") or ""),
        "position": int(segment.get("position") or 0),
        "text": text,
        "enabled": bool(segment.get("enabled", True)),
        # The point is published immediately before the authoritative DB row
        # becomes completed. PostgreSQL remains the visibility authority during
        # that small interval.
        "status": "completed",
        "level": int(segment.get("level") or 3),
        "content_type": str(segment.get("content_type") or "text"),
        "source_type": str(
            segment.get("source_type") or metadata.get("source_type") or "unknown"
        ),
        "language": str(segment.get("language") or metadata.get("language") or "en"),
        "metadata": metadata,
        "parent_segment_id": segment.get("parent_segment_id"),
    }
    for field in (
        "summary",
        "page_start",
        "page_end",
        "source_reference",
        "citation_text",
        "page_number",
        "section_header",
        "contextual_prefix",
        "answer",
        "keywords",
    ):
        payload[field] = segment.get(field)
    return payload


class ChunkingManager:
    """Manages document chunking, segment CRUD, and chunk preview."""

    def __init__(
        self,
        settings: Any,
        db: DatabaseStorage,
        vector_store: VectorStore,
        knowledge_service: Any = None,
    ):
        self.settings = settings
        self.db = db
        self.vector_store = vector_store
        self._ks = knowledge_service  # back-reference for shared helpers

    # ---- helpers proxied from KnowledgeService ----

    def _sanitize_text_for_db(self, text: str) -> str:
        """Remove NULL bytes and other characters that PostgreSQL cannot handle."""
        if not text:
            return ""
        # Remove NULL bytes (0x00) which PostgreSQL rejects
        text = text.replace("\x00", "")
        # Remove other control characters except common whitespace
        cleaned = []
        for char in text:
            # Keep printable chars, newlines, tabs, carriage returns
            if char.isprintable() or char in "\n\r\t" or ord(char) > 31:
                cleaned.append(char)
        return "".join(cleaned)

    async def _resolve_embedding_config(self, **kwargs: Any) -> EmbeddingConfig:
        return await maybe_await(self._ks._resolve_embedding_config(**kwargs))

    # ========================= Chunk conversion =========================

    def convert_structured_chunks(
        self,
        structured_chunks: list[dict[str, Any]],
        document_id: str,
        doc_name: str,
        dataset_id: str,
    ) -> list[Any]:
        """
        Convert structured parsing chunks to Chunk objects for embedding.

        This preserves the document structure (headings, images, tables)
        for better multimodal retrieval.
        """
        from .chunking import Chunk, ContentType

        flat_chunks = []

        for i, sc in enumerate(structured_chunks):
            chunk_type = sc.get("type", "text")
            content = sc.get("content", "")
            text = sc.get("text", content)  # Use text if available, else content

            # Determine content type based on chunk type
            content_type = ContentType.TEXT
            if chunk_type in ("image", "mixed"):
                content_type = ContentType.MIXED

            # Create Chunk object
            chunk = Chunk(
                text=text,
                index=i,  # Position in sequence
                metadata={
                    **sc.get("metadata", {}),
                    "source_document": doc_name,
                    "source_document_id": document_id,
                    "source_dataset_id": dataset_id,
                    "chunk_index": i,
                    "paragraph_index": i,
                    "structured_chunk_type": chunk_type,
                    "page_number": sc.get("page_number", 0),
                    "section_title": sc.get("section_title"),
                    "section_level": sc.get("section_level", 0),
                },
                content_type=content_type,
            )

            # Mark if this chunk has associated images
            if sc.get("has_images") or sc.get("images"):
                chunk.metadata["has_images"] = True
                chunk.metadata["image_count"] = len(sc.get("images", []))
                # Store image metadata for later retrieval
                chunk.metadata["associated_images"] = sc.get("images", [])

            flat_chunks.append(chunk)

        return flat_chunks

    # ========================= Chunk normalization =========================

    def normalize_structured_chunks(
        self,
        chunks: list[Chunk],
        config: ChunkingConfig,
    ) -> list[Chunk]:
        """Normalize structured chunks to enforce token limits and merge tiny fragments."""
        if not chunks:
            return chunks

        from .chunking import Chunk, ChunkingMode, FixedSizeChunker, count_tokens

        splitter = FixedSizeChunker(config)
        normalized: list[Chunk] = []
        separator = "\n\n"
        substantive_pattern = re.compile(r"[\w\u0600-\u06ff]", re.UNICODE)

        pending_text = ""
        pending_meta: dict[str, Any] = {}
        pending_images = []
        pending_base: Chunk | None = None

        def emit_chunk(
            text: str,
            base_chunk: Chunk,
            metadata: dict[str, Any],
            images: list[Any],
        ) -> None:
            if not text.strip():
                return
            # Drop punctuation-only fragments that add noise to retrieval
            if not substantive_pattern.search(text):
                return

            # Split only if chunk exceeds limits in fixed-size mode
            token_limit = config.token_limit if config.use_token_count else None
            if (
                config.mode == ChunkingMode.FIXED_SIZE
                and config.use_token_count
                and token_limit
                and count_tokens(text) > token_limit
            ):
                sub_chunks = splitter.chunk(text)
                for sc in sub_chunks:
                    sc.metadata = {**metadata, **(sc.metadata or {})}
                    sc.metadata.setdefault("structured_parent_id", base_chunk.hash_id)
                    sc.associated_images = images + sc.associated_images
                    normalized.append(sc)
                return
            if (
                config.mode == ChunkingMode.FIXED_SIZE
                and (not config.use_token_count)
                and len(text) > config.max_chunk_size
            ):
                sub_chunks = splitter.chunk(text)
                for sc in sub_chunks:
                    sc.metadata = {**metadata, **(sc.metadata or {})}
                    sc.metadata.setdefault("structured_parent_id", base_chunk.hash_id)
                    sc.associated_images = images + sc.associated_images
                    normalized.append(sc)
                return

            normalized.append(
                Chunk(
                    text=text,
                    index=base_chunk.index,
                    metadata=metadata,
                    parent_id=base_chunk.parent_id,
                    content_type=ContentType.TEXT,
                    associated_images=images,
                )
            )

        for c in chunks:
            # Preserve non-text or mixed content as-is to keep image context intact
            if c.content_type != ContentType.TEXT:
                if pending_text:
                    emit_chunk(
                        pending_text,
                        pending_base or c,
                        pending_meta,
                        pending_images,
                    )
                    pending_text = ""
                    pending_meta = {}
                    pending_images = []
                    pending_base = None
                normalized.append(c)
                continue

            text = c.text.strip()
            if not text:
                continue

            text_tokens = count_tokens(text)
            min_tokens = (
                config.min_chunk_tokens
                if config.use_token_count and config.mode == ChunkingMode.FIXED_SIZE
                else None
            )
            is_too_small = (len(text) < config.min_chunk_size) or (
                min_tokens is not None and text_tokens < min_tokens
            )

            if is_too_small:
                pending_text = f"{pending_text}{separator}{text}" if pending_text else text
                pending_meta = {**pending_meta, **(c.metadata or {})}
                pending_images.extend(c.associated_images)
                pending_base = pending_base or c

                if (c.metadata or {}).get("structured_chunk_type") == "heading":
                    pending_meta.setdefault("section_title", text)
                continue

            combined_text = text
            combined_meta = dict(c.metadata or {})
            combined_images = list(c.associated_images)
            base_chunk = c

            if pending_text:
                combined_text = f"{pending_text}{separator}{text}"
                combined_meta = {**pending_meta, **(c.metadata or {})}
                combined_images = pending_images + list(c.associated_images)
                pending_text = ""
                pending_meta = {}
                pending_images = []
                pending_base = None

            emit_chunk(combined_text, base_chunk, combined_meta, combined_images)

        if pending_text:
            if normalized and normalized[-1].content_type == ContentType.TEXT:
                last = normalized.pop()
                combined_text = f"{last.text}{separator}{pending_text}"
                combined_meta = {**(last.metadata or {}), **pending_meta}
                combined_images = list(last.associated_images) + pending_images
                emit_chunk(combined_text, last, combined_meta, combined_images)
            else:
                emit_chunk(
                    pending_text,
                    pending_base or Chunk(text=pending_text),
                    pending_meta,
                    pending_images,
                )

        # Merge tiny fragments for non-fixed mode.
        if config.mode != ChunkingMode.FIXED_SIZE:
            normalized = merge_small_chunks(
                normalized,
                min_size=config.min_chunk_size,
                max_size=config.max_chunk_size,
                min_tokens=None,
                max_tokens=None,
            )

        # Re-index after normalization
        for i, c in enumerate(normalized):
            c.index = i
            c.metadata["chunk_index"] = i
            c.metadata["paragraph_index"] = i

        return normalized

    # ========================= Chunk preview =========================

    async def preview_chunking(
        self, user: UserContext, dataset_id: str, text: str, config: dict[str, Any] | None = None
    ) -> list[dict[str, Any]]:
        if config is not None:
            validate_persisted_chunking_config(config)
        # Verify dataset access (viewer is enough for preview, though ideally check if member)
        if dataset_id != "temp_preview":
            await self._ks.require_dataset_access(user, dataset_id, required="viewer")

        # Parse config or use dataset default
        chunking_config: ChunkingConfig
        if config:
            chunking_config = ChunkingConfig.from_dict(config)
        else:
            if dataset_id == "temp_preview":
                chunking_config = ChunkingConfig()  # Default config
            else:
                # Fallback to dataset default if no config provided
                dataset = await self._ks._get_dataset_or_404(dataset_id)
                index_config = _ensure_dict(dataset.get("index_config"))
                chunking_config = ChunkingConfig.from_dict(index_config.get("chunking", {}))

        # Process text
        # Use a dummy document_id for preview
        doc_id = f"preview_{uuid.uuid4().hex[:8]}"

        # We need to run this in a thread pool as it might be CPU intensive
        chunks = await asyncio.to_thread(process_document, text, chunking_config, doc_id)

        # Flatten and format
        flat_chunks = flatten_chunks(chunks)

        return [
            {
                "content": c.text,
                "token_count": c.token_count,
                "char_count": c.char_count,
                "metadata": c.metadata,
            }
            for c in flat_chunks
        ]

    # ========================= Segment CRUD =========================

    async def list_segments(
        self,
        user: UserContext,
        dataset_id: str,
        document_id: str | None = None,
        q: str | None = None,
    ) -> list[dict[str, Any]]:
        await self._ks.require_dataset_access(user, dataset_id, required="viewer")
        return await self.db.list_segments(
            dataset_id=dataset_id, document_id=document_id, query_text=q, limit=500, offset=0
        )

    async def update_segment(
        self,
        user: UserContext,
        dataset_id: str,
        segment_id: str,
        new_text: str,
    ) -> dict[str, Any]:
        dataset = await self._ks.require_dataset_access(user, dataset_id, required="editor")
        _require_dataset_index_writable(dataset)
        clean_text = self._sanitize_text_for_db(new_text)
        embedding_provider = str(dataset.get("embedding_provider") or "local")
        embedding_model = str(dataset.get("embedding_model") or "hash-384")
        embedding_config = _ensure_dict(dataset.get("embedding_config"))
        dim = int(dataset.get("embedding_dimension") or 0) or None
        econf = await self._resolve_embedding_config(
            provider=embedding_provider,
            model=embedding_model,
            embedding_config=embedding_config,
            tenant_id=str(dataset.get("tenant_id") or ""),
        )
        lease_factory = getattr(self.db, "segment_index_update_lease", None)
        set_index_state = getattr(self.db, "set_segment_index_state", None)
        if not callable(lease_factory) or not callable(set_index_state):
            raise ValidationFailedError(
                "segment editing requires the serialized index state contract"
            )
        segment_hint = await self.db.get_segment(segment_id)
        if not segment_hint or str(segment_hint.get("dataset_id") or "") != dataset_id:
            raise ValidationFailedError("segment not found")
        hinted_document_id = str(segment_hint.get("document_id") or "").strip()
        if not hinted_document_id:
            raise ValidationFailedError("segment document identity is incomplete")

        vector_error: str | None = None
        async with lease_factory(
            dataset_id,
            hinted_document_id,
            segment_id,
        ) as lease_connection:
            authoritative_dataset = await self.db.get_dataset(
                dataset_id,
                connection=lease_connection,
            )
            if not authoritative_dataset:
                raise ValidationFailedError("dataset not found")
            _require_dataset_index_writable(authoritative_dataset)
            if dataset_ingestion_identity(
                authoritative_dataset
            ) != dataset_ingestion_identity(dataset):
                raise ValidationFailedError(
                    "dataset ingestion identity changed during segment update; retry"
                )
            seg = await self.db.get_segment(
                segment_id,
                connection=lease_connection,
            )
            if not seg or str(seg.get("dataset_id")) != dataset_id:
                raise ValidationFailedError("segment not found")
            document_id = str(seg.get("document_id") or "").strip()
            if document_id != hinted_document_id:
                raise ValidationFailedError(
                    "segment document identity changed during update; retry"
                )
            document = await self.db.get_document(
                document_id,
                connection=lease_connection,
            )
            _require_active_segment_document(document, dataset_id=dataset_id)

            # Hide stale Qdrant payloads throughout the serialized replacement.
            # The active-segment authority requires status=completed and also
            # preserves the user's independent enabled toggle.
            await set_index_state(
                segment_id,
                "pending",
                connection=lease_connection,
            )
            try:
                await self.db.update_segment(
                    segment_id,
                    text=clean_text,
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
                    collection = await self.vector_store.ensure_collection(
                        dataset_id=dataset_id,
                        dimension=embedder.dimension,
                        collection_name=str(
                            authoritative_dataset.get("collection_name") or ""
                        )
                        or None,
                        tenant_id=str(authoritative_dataset.get("tenant_id") or ""),
                        lifecycle_lease_held=True,
                        **_lexical_ensure_kwargs(
                            authoritative_dataset.get("index_config")
                        ),
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
                    raise ValidationFailedError(
                        "segment vector identity is incomplete"
                    )
                await self.vector_store.upsert(
                    collection_name=collection,
                    points=[qmodels.PointStruct(id=pid, vector=vec, payload=payload)],
                    lifecycle_lease_held=True,
                )
                await set_index_state(
                    segment_id,
                    "completed",
                    connection=lease_connection,
                )
            except Exception as exc:
                vector_error = str(exc)
                await set_index_state(
                    segment_id,
                    "error",
                    error=vector_error,
                    connection=lease_connection,
                )

        out = await self.db.get_segment(segment_id) or seg
        if vector_error:
            out = dict(out)
            out["_vector_error"] = vector_error
        return out

    async def delete_segment(self, user: UserContext, dataset_id: str, segment_id: str) -> bool:
        dataset = await self._ks.require_dataset_access(user, dataset_id, required="editor")
        _require_dataset_index_writable(dataset)
        segment_hint = await self.db.get_segment(segment_id)
        if not segment_hint or str(segment_hint.get("dataset_id")) != dataset_id:
            raise ValidationFailedError("segment not found")
        document_id = str(segment_hint.get("document_id") or "").strip()
        lease_factory = getattr(self.db, "segment_index_update_lease", None)
        set_index_state = getattr(self.db, "set_segment_index_state", None)
        delete_segment_points = getattr(
            self.vector_store,
            "delete_segment_points",
            None,
        )
        if not document_id or not all(
            callable(value)
            for value in (lease_factory, set_index_state, delete_segment_points)
        ):
            raise ValidationFailedError(
                "segment vector deletion is unavailable without owned-collection cleanup"
            )
        async with lease_factory(
            dataset_id,
            document_id,
            segment_id,
        ) as lease_connection:
            authoritative_dataset = await self.db.get_dataset(
                dataset_id,
                connection=lease_connection,
            )
            if not authoritative_dataset:
                raise ValidationFailedError("dataset not found")
            _require_dataset_index_writable(authoritative_dataset)
            if dataset_ingestion_identity(
                authoritative_dataset
            ) != dataset_ingestion_identity(dataset):
                raise ValidationFailedError(
                    "dataset ingestion identity changed during segment deletion; retry"
                )
            seg = await self.db.get_segment(
                segment_id,
                connection=lease_connection,
            )
            if (
                not seg
                or str(seg.get("dataset_id") or "") != dataset_id
                or str(seg.get("document_id") or "") != document_id
            ):
                raise ValidationFailedError("segment not found")
            document = await self.db.get_document(
                document_id,
                connection=lease_connection,
            )
            _require_active_segment_document(document, dataset_id=dataset_id)
            await set_index_state(
                segment_id,
                "pending",
                connection=lease_connection,
            )
            try:
                await delete_segment_points(
                    tenant_id=str(authoritative_dataset.get("tenant_id") or ""),
                    dataset_id=dataset_id,
                    document_id=document_id,
                    segment_id=segment_id,
                    lifecycle_lease_held=True,
                )
            except Exception as exc:
                await set_index_state(
                    segment_id,
                    "error",
                    error=str(exc),
                    connection=lease_connection,
                )
                raise ValidationFailedError(
                    f"segment vector deletion failed; database row was retained: {exc}"
                ) from exc
            result = await self.db.delete_segment(
                segment_id,
                connection=lease_connection,
            )
            if not result:
                raise ValidationFailedError(
                    "segment database deletion failed after vector cleanup"
                )
        # Update document segment_count after deletion
        if result and document_id:
            await self.db.refresh_document_segment_count(document_id)
        return result

    # ========================= Segment enable/disable =========================

    async def set_segment_enabled(
        self, user: UserContext, dataset_id: str, segment_id: str, enabled: bool
    ) -> dict[str, Any]:
        """Enable or disable a segment."""
        dataset = await self._ks.require_dataset_access(user, dataset_id, required="editor")
        _require_dataset_index_writable(dataset)
        lease_factory = getattr(self.db, "segment_index_update_lease", None)
        set_payload_enabled = getattr(
            self.vector_store,
            "set_segment_payload_enabled",
            None,
        )
        if not callable(lease_factory) or not callable(set_payload_enabled):
            raise ValidationFailedError(
                "segment enablement requires the serialized payload-state contract"
            )
        segment_hint = await self.db.get_segment(segment_id)
        if not segment_hint or str(segment_hint.get("dataset_id") or "") != dataset_id:
            raise ValidationFailedError("segment not found")
        hinted_document_id = str(segment_hint.get("document_id") or "").strip()
        if not hinted_document_id:
            raise ValidationFailedError("segment document identity is incomplete")

        async with lease_factory(
            dataset_id,
            hinted_document_id,
            segment_id,
        ) as lease_connection:
            seg = await self.db.get_segment(
                segment_id,
                connection=lease_connection,
            )
            if not seg or str(seg.get("dataset_id")) != dataset_id:
                raise ValidationFailedError("segment not found")
            document_id = str(seg.get("document_id") or "").strip()
            if document_id != hinted_document_id:
                raise ValidationFailedError(
                    "segment document identity changed during enablement; retry"
                )
            document = await self.db.get_document(
                document_id,
                connection=lease_connection,
            )
            _require_active_segment_document(document, dataset_id=dataset_id)
            tenant_id = str(dataset.get("tenant_id") or "").strip()
            if not tenant_id or not document_id:
                raise ValidationFailedError("segment ownership identity is incomplete")

            update_data: dict[str, Any] = {"enabled": enabled}
            if not enabled:
                update_data["disabled_at"] = datetime.utcnow()
                update_data["disabled_by"] = user.user_id
                # Database authority hides the segment before a possibly
                # partial cross-generation payload update. Same-value retries
                # still run the idempotent Qdrant repair.
                if bool(seg.get("enabled", True)):
                    await self.db.update_segment_fields(
                        segment_id,
                        update_data,
                        connection=lease_connection,
                    )
                await set_payload_enabled(
                    tenant_id=tenant_id,
                    dataset_id=dataset_id,
                    document_id=document_id,
                    segment_id=segment_id,
                    enabled=False,
                    lifecycle_lease_held=True,
                )
            else:
                # Restore every owned payload first; PostgreSQL becomes active
                # only after Qdrant acknowledges all generations.
                await set_payload_enabled(
                    tenant_id=tenant_id,
                    dataset_id=dataset_id,
                    document_id=document_id,
                    segment_id=segment_id,
                    enabled=True,
                    lifecycle_lease_held=True,
                )
                update_data["disabled_at"] = None
                update_data["disabled_by"] = None
                await self.db.update_segment_fields(
                    segment_id,
                    update_data,
                    connection=lease_connection,
                )

        return await self.db.get_segment(segment_id) or seg

    # ========================= Segment creation =========================

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
        lease_factory = getattr(self.db, "document_segment_create_lease", None)
        set_index_state = getattr(self.db, "set_segment_index_state", None)
        next_position = getattr(self.db, "next_segment_position", None)
        if not all(
            callable(value)
            for value in (lease_factory, set_index_state, next_position)
        ):
            raise ValidationFailedError(
                "segment creation requires the serialized index state contract"
            )

        seg_id = str(uuid.uuid4())
        clean_content = self._sanitize_text_for_db(content)

        async with lease_factory(dataset_id, document_id) as lease_connection:
            authoritative_dataset = await self.db.get_dataset(
                dataset_id,
                connection=lease_connection,
            )
            if not authoritative_dataset:
                raise ValidationFailedError("dataset not found")
            _require_dataset_index_writable(authoritative_dataset)
            if dataset_ingestion_identity(
                authoritative_dataset
            ) != dataset_ingestion_identity(dataset):
                raise ValidationFailedError(
                    "dataset ingestion identity changed during segment creation; retry"
                )
            authoritative_document = await self.db.get_document(
                document_id,
                connection=lease_connection,
            )
            _require_active_segment_document(
                authoritative_document,
                dataset_id=dataset_id,
            )
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
                "parent_segment_id": None,
                "text": clean_content,
                "token_count": len(clean_content) // 4,
                "word_count": len(clean_content.split()),
                "answer": answer,
                "keywords": keywords or [],
                "created_by": user.user_id,
                "enabled": True,
                "status": "indexing",
                "content_type": "text",
                "source_type": str(
                    authoritative_document.get("source_type") or "manual"
                ),
                "language": str(
                    authoritative_document.get("doc_language") or "en"
                ),
                "metadata": {},
            }
            await self.db.insert_segments(
                [seg],
                connection=lease_connection,
            )
            stored_segment = await self.db.get_segment(
                seg_id,
                connection=lease_connection,
            )
            if not stored_segment or str(stored_segment.get("dataset_id") or "") != dataset_id:
                raise ValidationFailedError(
                    "segment creation failed before vector indexing"
                )
            seg = stored_segment
            await set_index_state(
                seg_id,
                "pending",
                connection=lease_connection,
            )

            # Embed and index the segment
            try:
                embedding_provider = str(
                    authoritative_dataset.get("embedding_provider") or "local"
                )
                embedding_model = str(
                    authoritative_dataset.get("embedding_model") or "hash-384"
                )
                embedding_config = _ensure_dict(
                    authoritative_dataset.get("embedding_config")
                )
                dim = int(authoritative_dataset.get("embedding_dimension") or 0) or None

                econf = await self._resolve_embedding_config(
                    provider=embedding_provider,
                    model=embedding_model,
                    embedding_config=embedding_config,
                    tenant_id=str(authoritative_dataset.get("tenant_id") or ""),
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

                    collection = await self.vector_store.ensure_collection(
                        dataset_id=dataset_id,
                        dimension=embedder.dimension,
                        collection_name=str(
                            authoritative_dataset.get("collection_name") or ""
                        )
                        or None,
                        tenant_id=str(authoritative_dataset.get("tenant_id") or ""),
                        lifecycle_lease_held=True,
                        **_lexical_ensure_kwargs(
                            authoritative_dataset.get("index_config")
                        ),
                    )
                finally:
                    if embedder:
                        await embedder.close()

                from qdrant_client.http import models as qmodels

                payload = _segment_vector_payload(
                    dataset=authoritative_dataset,
                    segment=seg,
                    text=clean_content,
                )
                await self.vector_store.upsert(
                    collection_name=collection,
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
                await set_index_state(
                    seg_id,
                    "error",
                    error=str(exc),
                    connection=lease_connection,
                )

        # Update document segment_count after creating a new segment
        await self.db.refresh_document_segment_count(document_id)

        return await self.db.get_segment(seg_id) or seg
