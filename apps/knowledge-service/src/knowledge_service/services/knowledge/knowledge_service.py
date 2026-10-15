from __future__ import annotations

import asyncio
import math
import re
from typing import TYPE_CHECKING, Any

from ...config.settings import Settings
from ...core.auth.user_resolver import UserContext
from ...core.exceptions import ValidationFailedError
from ...core.observability.logging import get_logger
from ...persistence.database import (
    DOCUMENT_UPLOAD_FAILED_KEY,
    DatabaseStorage,
    IndexLeaseUnavailableError,
    dataset_ingestion_identity,
)
from .chunking import (
    AssociatedImage,
    Chunk,
    ChunkingConfig,
    ContentType,
    merge_small_chunks,
)
from .common import ensure_dict as _ensure_dict
from .embedding import (
    BaseEmbedding,
    DashScopeMultimodalEmbedding,
    EmbeddingConfig,
)
from .ingestion import DocumentImageExtractor
from .ingestion import ExtractedImage as IngestionExtractedImage
from .lexical_config import LexicalConfig, LexicalConfigError
from .lifecycle_errors import Bm25V2LifecycleError
from .pdf_image_processor import ExtractedImage, PDFImageProcessor
from .retrieval_service import RetrieveResult
from .structured_document_parser import StructuredDocumentParser

logger = get_logger(__name__)

if TYPE_CHECKING:
    from ..storage.image_storage import ImageStorageService
    from .embedding import UnifiedMultimodalEmbedding
    from .worker import KnowledgeWorker

# Global VLM semaphore for rate limiting across all concurrent document processing
# This prevents overwhelming the VLM API when multiple documents are processed simultaneously
_global_vlm_semaphore: asyncio.Semaphore | None = None
_global_vlm_max_concurrent: int = 10  # Default, updated from settings on first use

class KnowledgeService:
    """Main knowledge base service - coordinates specialized sub-services.

    This service acts as a facade for:
    - DatasetService: Dataset management
    - DocumentService: Document CRUD operations
    - IngestionService: Document processing and embedding
    - RetrievalService: Search and retrieval

    Note: This class is being refactored. New code should use the sub-services directly.
    """

    def __init__(
        self,
        settings: Settings,
        database: DatabaseStorage,
        multimodal_embedding: DashScopeMultimodalEmbedding | None = None,
        image_storage_service: ImageStorageService | None = None,
        vlm_service: Any | None = None,
        # New: allow injecting sub-services (for testing and gradual migration)
        dataset_service: Any | None = None,
        document_service: Any | None = None,
        ingestion_service: Any | None = None,
        retrieval_service: Any | None = None,
        tenant_embedding_credential_resolver: Any | None = None,
    ):
        self.settings = settings
        self.db = database
        self.multimodal_embedding = multimodal_embedding
        self.image_storage_service = image_storage_service
        self.vlm_service = vlm_service
        self.pdf_image_processor = PDFImageProcessor()
        self.document_image_extractor = DocumentImageExtractor()

        # Initialize sub-services (composition over inheritance)
        # These can be injected for testing or created internally
        from .dataset_service import DatasetService
        from .document_service import DocumentService as DocService
        from .ingestion_service import IngestionService
        from .retrieval_service import RetrievalService
        from .vector_store import VectorStore

        self.dataset_service = dataset_service or DatasetService(settings, database)
        self.document_service = document_service or DocService(
            settings, database, self.dataset_service
        )
        self.retrieval_service = retrieval_service or RetrievalService(settings, database)

        # Initialize structured document parser for enhanced multimodal parsing
        self.structured_parser = None
        try:
            self.structured_parser = StructuredDocumentParser(
                vlm_service=vlm_service,
                enable_vlm_description=False,  # Disabled by default, enable per-dataset config
                max_vlm_images=10,
            )
            logger.info("Structured document parser initialized")
        except Exception as e:
            logger.warning(f"Structured parser initialization failed: {e}")

        if not getattr(settings, "knowledge", None) or not settings.knowledge.enabled:
            raise RuntimeError("Knowledge service is disabled (GATEWAY_KNOWLEDGE__ENABLED=false)")

        if not settings.knowledge.qdrant.enabled:
            raise RuntimeError("Qdrant is disabled (GATEWAY_KNOWLEDGE__QDRANT__ENABLED=false)")

        self.vector_store = VectorStore(
            url=settings.knowledge.qdrant.url,
            api_key=settings.knowledge.qdrant.api_key,
            timeout_seconds=settings.knowledge.qdrant.timeout_seconds,
            interactive_deadline_seconds=getattr(
                settings.knowledge.qdrant,
                "interactive_deadline_seconds",
                3.0,
            ),
            prefer_grpc=settings.knowledge.qdrant.prefer_grpc,
            max_retries=getattr(settings.knowledge.qdrant, "max_retries", 3),
            retry_base_delay=getattr(settings.knowledge.qdrant, "retry_base_delay", 0.5),
            bm25_v2_enabled=getattr(
                settings.knowledge.qdrant, "bm25_v2_enabled", False
            ),
            bm25_v2_capability_ttl_seconds=getattr(
                settings.knowledge.qdrant,
                "bm25_v2_capability_ttl_seconds",
                300.0,
            ),
            bm25_v2_readiness_ttl_seconds=getattr(
                settings.knowledge.qdrant,
                "bm25_v2_readiness_ttl_seconds",
                5.0,
            ),
            dataset_write_lease=database.dataset_index_write_lease,
        )

        # Update sub-services with shared resources (post-init injection)
        self.retrieval_service.vector_store = self.vector_store
        self.retrieval_service._ks = self
        self.dataset_service._ks = self
        self.document_service._ks = self

        # Initialize ingestion service with vector store
        self.ingestion_service = ingestion_service or IngestionService(
            settings, database, self.vector_store
        )
        self.ingestion_service._ks = self

        # T3 embedding versioning / blue-green migration. Both are resolved
        # lazily so a not-yet-connected pool (or degraded mode) never breaks
        # service construction; they activate once the PG pool is live.
        self._embedding_version_store: Any | None = None
        self._embedding_migration_service: Any | None = None

        # T6 BM25 v2 cross-storage lifecycle protocol. Same lazy pattern as
        # T3: no pool, no transition machinery (and no accidental writes to
        # kb_bm25_v2_lifecycle from a degraded-mode boot).
        self._bm25_v2_lifecycle_store: Any | None = None
        self._bm25_v2_lifecycle_service: Any | None = None

        # Extracted sub-components (Phase 2 refactoring)
        from .cache_manager import CacheManager
        from .chunking_manager import ChunkingManager
        from .document_processor import DocumentProcessor
        from .embedding_manager import EmbeddingManager

        cache_ttl = max(
            int(getattr(settings.knowledge, "retrieval_cache_ttl_seconds", 0) or 0), 0
        )
        self.cache_manager = CacheManager(ttl_seconds=cache_ttl)
        self.document_processor = DocumentProcessor(settings, vlm_service)
        self.embedding_manager = EmbeddingManager(
            settings,
            credential_resolver=tenant_embedding_credential_resolver,
        )
        self.chunking_manager = ChunkingManager(
            settings, database, self.vector_store, knowledge_service=self,
        )

        # Backward-compat aliases for internal callers
        self._retrieval_cache_ttl_seconds = cache_ttl
        self._retrieval_cache = self.cache_manager._cache
        self._retrieval_cache_lock = self.cache_manager._lock

    async def close(self) -> None:
        close_ingestion = getattr(self.ingestion_service, "close", None)
        if callable(close_ingestion):
            await close_ingestion()
        await self.vector_store.close()

    # -- Cache delegation (implementation in CacheManager) --

    @staticmethod
    def _clone_retrieve_results(results: list[RetrieveResult]) -> list[RetrieveResult]:
        from .cache_manager import CacheManager
        return CacheManager.clone_results(results)

    @staticmethod
    def _compute_retrieval_query_fingerprint(payload: dict[str, Any]) -> str:
        from .cache_manager import CacheManager
        return CacheManager.compute_fingerprint(payload)

    async def _get_cached_retrieval(
        self, cache_key: str
    ) -> tuple[list[RetrieveResult], dict[str, Any]] | None:
        return await self.cache_manager.get(cache_key)

    async def _set_cached_retrieval(
        self, cache_key: str, results: list[RetrieveResult], meta: dict[str, Any],
    ) -> None:
        await self.cache_manager.set(cache_key, results, meta)

    # -- Delegations to DocumentProcessor / EmbeddingManager --

    def _create_vlm_callback(self):
        """Delegate to DocumentProcessor."""
        return self.document_processor.create_vlm_callback()

    def _is_multimodal_dataset(self, dataset: dict[str, Any]) -> bool:
        """Delegate to EmbeddingManager."""
        return self.embedding_manager.is_multimodal_dataset(dataset)

    async def _get_unified_multimodal_embedder(
        self, dataset: dict[str, Any], embedding_config: dict[str, Any] | None = None,
    ) -> UnifiedMultimodalEmbedding:
        """Delegate to EmbeddingManager."""
        return await self.embedding_manager.get_unified_multimodal_embedder(
            dataset, embedding_config
        )

    async def _get_text_embedder(
        self, dataset: dict[str, Any], embedding_config: dict[str, Any] | None = None,
    ) -> BaseEmbedding:
        """Delegate to EmbeddingManager."""
        return await self.embedding_manager.get_text_embedder(dataset, embedding_config)

    def _convert_structured_chunks(
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
        from .chunking import MAX_CHUNK_OUTPUTS, Chunk, ContentType

        if len(structured_chunks) > MAX_CHUNK_OUTPUTS:
            raise ValidationFailedError(
                f"structured parsing exceeds the {MAX_CHUNK_OUTPUTS} chunk limit"
            )

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

            if len(flat_chunks) >= MAX_CHUNK_OUTPUTS:
                raise ValidationFailedError(
                    f"structured parsing exceeds the {MAX_CHUNK_OUTPUTS} chunk limit"
                )
            flat_chunks.append(chunk)

        return flat_chunks

    def _normalize_structured_chunks(
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

        # Merge tiny fragments for non-fixed mode
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

    def _resolve_fusion_config(
        self,
        *,
        retrieval_defaults: dict[str, Any],
        fusion_method: str | None,
        fusion: str | None,
        alpha: float | None,
        dense_weight: float | None,
        bm25_weight: float | None,
        rrf_k: int | None,
        rrf_weights: dict[str, float] | None = None,
    ) -> dict[str, Any]:
        fusion_cfg = (
            retrieval_defaults.get("fusion") if isinstance(retrieval_defaults, dict) else None
        )
        fusion_strategy = None
        fusion_alpha = None
        fusion_rrf_k = None
        fusion_rrf_weights = None
        fusion_dense_weight = None
        fusion_bm25_weight = None
        if isinstance(fusion_cfg, dict):
            fusion_strategy = fusion_cfg.get("strategy") or fusion_cfg.get("method")
            fusion_alpha = fusion_cfg.get("alpha")
            fusion_rrf_k = fusion_cfg.get("rrf_k")
            fusion_rrf_weights = fusion_cfg.get("rrf_weights")
            fusion_dense_weight = fusion_cfg.get("dense_weight")
            fusion_bm25_weight = fusion_cfg.get("bm25_weight")

        effective_fusion_method = str(
            (
                fusion_method
                if fusion_method is not None
                else fusion
                if fusion is not None
                else fusion_strategy
                or retrieval_defaults.get("fusion_method")
                or retrieval_defaults.get("fusion")
            )
            or "rrf"
        ).lower()
        if effective_fusion_method == "alpha":
            effective_fusion_method = "weighted"
        if effective_fusion_method not in {"weighted", "rrf"}:
            effective_fusion_method = "rrf"

        def coerce_weight(raw_value: Any) -> float:
            try:
                return float(raw_value)
            except (TypeError, ValueError) as exc:
                raise ValidationFailedError(
                    "fusion weights must be finite and non-negative"
                ) from exc

        default_dense_weight = retrieval_defaults.get("dense_weight")
        default_bm25_weight = retrieval_defaults.get("bm25_weight")
        effective_dense_weight = coerce_weight(
            0.5 if default_dense_weight is None else default_dense_weight
        )
        effective_bm25_weight = coerce_weight(
            0.5 if default_bm25_weight is None else default_bm25_weight
        )

        # Resolve dataset defaults first. Canonical nested ``fusion`` values
        # override legacy root values, then explicit request values override the
        # dataset. This keeps native Qdrant and Python fallback on one contract.
        if fusion_alpha is not None:
            effective_dense_weight = coerce_weight(fusion_alpha)
            effective_bm25_weight = 1.0 - coerce_weight(fusion_alpha)
        else:
            if fusion_dense_weight is not None:
                effective_dense_weight = coerce_weight(fusion_dense_weight)
            if fusion_bm25_weight is not None:
                effective_bm25_weight = coerce_weight(fusion_bm25_weight)

        def apply_rrf_weights(weights: Any) -> None:
            nonlocal effective_dense_weight, effective_bm25_weight
            if not isinstance(weights, dict):
                return
            for alias in ("vector", "dense"):
                if weights.get(alias) is not None:
                    effective_dense_weight = coerce_weight(weights[alias])
                    break
            for alias in ("keyword", "bm25"):
                if weights.get(alias) is not None:
                    effective_bm25_weight = coerce_weight(weights[alias])
                    break

        if effective_fusion_method == "rrf":
            apply_rrf_weights(retrieval_defaults.get("rrf_weights"))
            apply_rrf_weights(fusion_rrf_weights)

        if alpha is not None:
            effective_dense_weight = coerce_weight(alpha)
            effective_bm25_weight = 1.0 - coerce_weight(alpha)
        else:
            if dense_weight is not None:
                effective_dense_weight = coerce_weight(dense_weight)
            if bm25_weight is not None:
                effective_bm25_weight = coerce_weight(bm25_weight)
        if effective_fusion_method == "rrf":
            apply_rrf_weights(rrf_weights)

        effective_weights = (effective_dense_weight, effective_bm25_weight)
        if not all(math.isfinite(weight) and weight >= 0.0 for weight in effective_weights):
            raise ValidationFailedError("fusion weights must be finite and non-negative")
        if not any(weight > 0.0 for weight in effective_weights):
            raise ValidationFailedError("at least one fusion weight must be positive")

        raw_rrf_k = (
            rrf_k
            if rrf_k is not None
            else fusion_rrf_k
            if fusion_rrf_k is not None
            else retrieval_defaults.get("rrf_k")
        )
        raw_rrf_k = 60 if raw_rrf_k is None else raw_rrf_k
        try:
            numeric_rrf_k = float(raw_rrf_k)
        except (TypeError, ValueError) as exc:
            raise ValidationFailedError("rrf_k must be an integer between 1 and 10000") from exc
        if (
            isinstance(raw_rrf_k, bool)
            or not math.isfinite(numeric_rrf_k)
            or not numeric_rrf_k.is_integer()
            or not 1 <= numeric_rrf_k <= 10_000
        ):
            raise ValidationFailedError("rrf_k must be an integer between 1 and 10000")
        rrf_k_value = int(numeric_rrf_k)

        return {
            "method": effective_fusion_method,
            "dense_weight": effective_dense_weight,
            "bm25_weight": effective_bm25_weight,
            "rrf_k": rrf_k_value,
        }

    def _filter_candidates_by_metadata(
        self,
        candidates: list[dict[str, Any]],
        source_type: str | None,
        language: str | None,
        metadata_filter: dict[str, Any] | None = None,
    ) -> list[dict[str, Any]]:
        if not source_type and not language and not metadata_filter:
            return candidates
        filtered: list[dict[str, Any]] = []
        for c in candidates:
            outer_meta = _ensure_dict(c.get("metadata"))
            meta = {
                **_ensure_dict(outer_meta.get("metadata")),
                **outer_meta,
            }
            if source_type and str(meta.get("source_type")) != str(source_type):
                continue
            if language and str(meta.get("language")) != str(language):
                continue
            if metadata_filter:
                matched = True
                for key, expected in metadata_filter.items():
                    if meta.get(key) != expected:
                        matched = False
                        break
                if not matched:
                    continue
            filtered.append(c)
        return filtered

    def _should_apply_score_threshold(self, mode: str | None) -> bool:
        return str(mode or "").lower() == "dense"

    async def _get_dataset_or_404(self, dataset_id: str) -> dict[str, Any]:
        return await self.dataset_service._get_dataset_or_404(dataset_id)

    async def _effective_dataset_permission(
        self, dataset: dict[str, Any], user: UserContext
    ) -> str | None:
        return await self.dataset_service._effective_dataset_permission(dataset, user)

    async def require_dataset_access(
        self, user: UserContext, dataset_id: str, required: str = "viewer"
    ) -> dict[str, Any]:
        return await self.dataset_service.require_dataset_access(user, dataset_id, required)

    def _redact_dataset_secrets(self, dataset: dict[str, Any]) -> dict[str, Any]:
        return self.dataset_service._redact_dataset_secrets(dataset)

    def sanitize_dataset_for_response(self, dataset: dict[str, Any]) -> dict[str, Any]:
        """Public wrapper to redact sensitive config fields for API responses."""
        return self.dataset_service._redact_dataset_secrets(dataset)

    # ========================= Dataset (delegated to DatasetService) =========================

    async def list_datasets(self, user: UserContext) -> list[dict[str, Any]]:
        return await self.dataset_service.list_datasets(user)

    async def list_datasets_page(
        self, user: UserContext, *, limit: int = 200, cursor: str | None = None,
    ) -> dict[str, Any]:
        return await self.dataset_service.list_datasets_page(user, limit=limit, cursor=cursor)

    async def preview_chunking(
        self, user: UserContext, dataset_id: str, text: str, config: dict[str, Any] | None = None
    ) -> list[dict[str, Any]]:
        return await self.dataset_service.preview_chunking(user, dataset_id, text, config)

    async def create_dataset(self, user: UserContext, data: dict[str, Any]) -> dict[str, Any]:
        return await self.dataset_service.create_dataset(user, data)

    async def update_dataset(
        self, user: UserContext, dataset_id: str, patch: dict[str, Any]
    ) -> dict[str, Any]:
        return await self.dataset_service.update_dataset(user, dataset_id, patch)

    async def delete_dataset(
        self, user: UserContext, dataset_id: str, *, deletion_confirmed: bool = False, reason: str | None = None,
    ) -> bool:
        return await self.dataset_service.delete_dataset(
            user, dataset_id, deletion_confirmed=deletion_confirmed, reason=reason
        )

    async def list_dataset_permissions(
        self, user: UserContext, dataset_id: str
    ) -> list[dict[str, Any]]:
        return await self.dataset_service.list_dataset_permissions(user, dataset_id)

    async def grant_dataset_permission(
        self, user: UserContext, dataset_id: str,
        subject_type: str, subject_id: str, permission: str,
    ) -> None:
        return await self.dataset_service.grant_dataset_permission(
            user, dataset_id, subject_type, subject_id, permission
        )

    async def revoke_dataset_permission(
        self, user: UserContext, dataset_id: str, subject_type: str, subject_id: str
    ) -> bool:
        return await self.dataset_service.revoke_dataset_permission(
            user, dataset_id, subject_type, subject_id
        )

    async def list_query_history(
        self, user: UserContext, dataset_id: str, **kwargs: Any
    ) -> dict[str, Any]:
        return await self.dataset_service.list_query_history(
            user, dataset_id, **kwargs
        )

    async def upsert_query_feedback(
        self, user: UserContext, dataset_id: str, **kwargs: Any
    ) -> dict[str, Any]:
        return await self.dataset_service.upsert_query_feedback(
            user, dataset_id, **kwargs
        )

    async def list_query_feedback(
        self, user: UserContext, dataset_id: str, **kwargs: Any
    ) -> dict[str, Any]:
        return await self.dataset_service.list_query_feedback(
            user, dataset_id, **kwargs
        )

    # ========================= Document (delegated to DocumentService) =========================

    async def create_document_from_text(
        self, user: UserContext, dataset_id: str, title: str, content: str,
        metadata: dict[str, Any] | None = None,
    ) -> dict[str, Any]:
        return await self.document_service.create_document_from_text(
            user, dataset_id, title, content, metadata
        )

    async def create_document_from_upload(
        self, user: UserContext, dataset_id: str, filename: str, content_bytes: bytes,
        mime_type: str | None = None, metadata: dict[str, Any] | None = None,
        processing_mode: str = "text_only",
    ) -> dict[str, Any]:
        return await self.document_service.create_document_from_upload(
            user, dataset_id, filename, content_bytes, mime_type, metadata, processing_mode
        )

    async def create_document_from_url(
        self, user: UserContext, dataset_id: str, url: str,
        title: str | None = None, metadata: dict[str, Any] | None = None,
    ) -> dict[str, Any]:
        return await self.document_service.create_document_from_url(
            user, dataset_id, url, title, metadata
        )

    async def list_documents(
        self,
        user: UserContext,
        dataset_id: str,
        *,
        limit: int | None = None,
        offset: int = 0,
    ) -> list[dict[str, Any]]:
        kwargs: dict[str, Any] = {}
        if limit is not None:
            kwargs["limit"] = limit
        return await self.document_service.list_documents(
            user, dataset_id, offset=offset, **kwargs
        )

    async def count_documents(self, user: UserContext, dataset_id: str) -> int:
        return await self.document_service.count_documents(user, dataset_id)

    async def list_documents_page(
        self,
        user: UserContext,
        dataset_id: str,
        *,
        limit: int,
        offset: int,
    ) -> dict[str, Any]:
        return await self.document_service.list_documents_page(
            user,
            dataset_id,
            limit=limit,
            offset=offset,
        )

    async def list_all_document_ids(self, user: UserContext, dataset_id: str) -> list[str]:
        return await self.document_service.list_all_document_ids(user, dataset_id)

    async def get_document(
        self, user: UserContext, dataset_id: str, document_id: str
    ) -> dict[str, Any]:
        return await self.document_service.get_document(user, dataset_id, document_id)

    async def enqueue_ingest(self, dataset_id: str, document_id: str) -> None:
        return await self.document_service.enqueue_ingest(dataset_id, document_id)

    async def delete_document(self, user: UserContext, dataset_id: str, document_id: str) -> bool:
        return await self.document_service.delete_document(user, dataset_id, document_id)

    async def detach_confluence_source(
        self, user: UserContext, binding: dict[str, Any], page: dict[str, Any],
    ) -> bool:
        """Remove one source reference; delete its document only on last reference."""
        return await self.document_service.delete_document(
            user, str(binding.get("dataset_id") or ""),
            str(page.get("document_id") or ""),
            _confluence_source=(binding, page),
        )

    # ========================= Segment (delegated to DocumentService) =========================

    async def list_segments(
        self, user: UserContext, dataset_id: str,
        document_id: str | None = None, q: str | None = None,
        *,
        limit: int | None = None,
        offset: int = 0,
    ) -> list[dict[str, Any]]:
        kwargs: dict[str, Any] = {}
        if limit is not None:
            kwargs["limit"] = limit
        return await self.document_service.list_segments(
            user, dataset_id, document_id, q, offset=offset, **kwargs
        )

    async def count_segments(
        self, user: UserContext, dataset_id: str,
        document_id: str | None = None, q: str | None = None,
    ) -> int:
        return await self.document_service.count_segments(user, dataset_id, document_id, q)

    async def list_segments_page(
        self,
        user: UserContext,
        dataset_id: str,
        document_id: str | None = None,
        q: str | None = None,
        *,
        limit: int,
        offset: int,
    ) -> dict[str, Any]:
        return await self.document_service.list_segments_page(
            user,
            dataset_id,
            document_id,
            q,
            limit=limit,
            offset=offset,
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
        return await self._run_segment_mutation_with_bm25_publication(
            user,
            dataset_id,
            lambda: self.document_service.update_segment(
                user,
                dataset_id,
                segment_id,
                new_text,
                new_answer=new_answer,
                new_keywords=new_keywords,
            ),
        )

    async def delete_segment(self, user: UserContext, dataset_id: str, segment_id: str) -> bool:
        return await self._run_segment_mutation_with_bm25_publication(
            user,
            dataset_id,
            lambda: self.document_service.delete_segment(
                user,
                dataset_id,
                segment_id,
            ),
        )

    async def _run_segment_mutation_with_bm25_publication(
        self,
        user: UserContext,
        dataset_id: str,
        operation: Any,
    ) -> Any:
        """Fence active-v2 manual segment writes behind one publication.

        Legacy/shadow datasets keep their existing short segment lease. In
        active mode the outer shared publication lock makes the whole remote +
        PostgreSQL change retryable to readers, then full-scroll evidence and
        the positive revision are certified atomically. Failed but coherent
        no-op/idempotent operations can close the fence; mismatches leave its
        negative revision fail-closed for repair.
        """

        dataset = await self.require_dataset_access(user, dataset_id, required="editor")
        try:
            lexical = LexicalConfig.from_index_config(dataset.get("index_config"))
        except LexicalConfigError as exc:
            raise ValidationFailedError(str(exc)) from exc
        if not lexical.reads_bm25_v2:
            return await operation()
        if not self.vector_store.bm25_v2_enabled:
            raise Bm25V2LifecycleError(
                "bm25_v2 active writes are unavailable while the service kill switch is off",
                code="bm25_v2_disabled",
                http_status=503,
            )

        lifecycle = self.bm25_v2_lifecycle_service
        lease = getattr(self.db, "dataset_index_publication_lease", None)
        finish = getattr(self.db, "finish_index_publication", None)
        abort = getattr(self.db, "abort_index_publication", None)
        if (
            lifecycle is None
            or not callable(lease)
            or not callable(finish)
            or not callable(abort)
        ):
            raise Bm25V2LifecycleError(
                "BM25 v2 publication authority is unavailable",
                code="bm25_v2_authority_unavailable",
                http_status=503,
            )

        # Zero-side-effect active/kill-switch/state preflight before the lease
        # writes the negative revision; repeat under the shared lock below.
        await lifecycle.active_publication_context(dataset_id)
        async with lease(
            dataset_id,
            expected_ingestion_identity=dataset_ingestion_identity(dataset),
        ) as publication:
            context = await lifecycle.active_publication_context(dataset_id)
            if context is None:
                # A rollback won between the initial authorization read and
                # shared-lock acquisition. The negative seqlock still safely
                # wraps this legacy publication; no active receipt CAS applies.
                try:
                    result = await operation()
                finally:
                    await abort(
                        dataset_id,
                        connection=publication.connection,
                    )
                return result

            async def finalize() -> None:
                certification = await lifecycle.recertify_active_publication(
                    context,
                    publication_revision=int(publication.revision),
                    connection=publication.connection,
                )
                async with publication.connection.transaction():
                    revision = await finish(
                        dataset_id,
                        connection=publication.connection,
                    )
                    if int(revision) != int(certification["target_revision"]):
                        raise RuntimeError(
                            "segment publication revision disagrees with BM25 v2 receipt"
                        )
                    await lifecycle.settle_active_publication(
                        context,
                        certification,
                        connection=publication.connection,
                    )

            try:
                result = await operation()
            except BaseException as operation_error:
                try:
                    finalize_task = asyncio.create_task(finalize())
                    await asyncio.shield(finalize_task)
                except BaseException as certification_error:
                    logger.error(
                        "Active BM25 v2 segment mutation failed and remains fenced: %s",
                        certification_error,
                    )
                    if isinstance(operation_error, asyncio.CancelledError):
                        raise operation_error
                    raise RuntimeError(
                        "segment mutation failed with cross-authority mismatch; "
                        "the dataset remains fail-closed"
                    ) from operation_error
                raise
            await finalize()
            return result

    # ========================= Ingest pipeline (delegated to IngestionService) =========================

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
        return await self.ingestion_service.ingest_document(
            dataset_id,
            document_id,
            chunking_config_override=chunking_config_override,
            index_config_override=index_config_override,
            source_text_override=source_text_override,
            candidate_metadata_patch=candidate_metadata_patch,
        )

    async def reembed_document(
        self, dataset_id: str, document_id: str
    ) -> list[str] | None:
        """PRD T1 item 3 reembed verb: re-embed persisted chunks in place.

        No re-parse, no re-split — vector repair under unchanged model and
        parameters. Chunks keep their segment/point identity. Falls back to
        the full pipeline when nothing serving is persisted yet.
        """
        return await self.ingestion_service.reembed_document(dataset_id, document_id)

    async def _process_document_images_with_embedder(
        self,
        embedder: Any,
        dataset_id: str,
        document_id: str,
        image_metadata_list: list[dict[str, Any]],
        collection: str,
        base_position: int = 0,
        tenant_id: str = "default",
    ) -> int:
        return await self.ingestion_service._process_document_images_with_embedder(
            embedder=embedder,
            dataset_id=dataset_id,
            document_id=document_id,
            image_metadata_list=image_metadata_list,
            collection=collection,
            base_position=base_position,
            tenant_id=tenant_id,
        )

    async def _embed_images_in_memory(
        self,
        embedder: Any,
        dataset_id: str,
        document_id: str,
        images: list[IngestionExtractedImage],
        collection: str,
        tenant_id: str,
        base_position: int = 0,
    ) -> tuple[int, list[dict[str, Any]]]:
        return await self.ingestion_service._embed_images_in_memory(
            embedder=embedder,
            dataset_id=dataset_id,
            document_id=document_id,
            images=images,
            collection=collection,
            tenant_id=tenant_id,
            base_position=base_position,
        )

    async def _process_document_images(
        self,
        dataset_id: str,
        document_id: str,
        images: list[ExtractedImage],
        collection: str,
        base_position: int = 0,
        tenant_id: str = "default",
    ) -> int:
        return await self.ingestion_service._process_document_images(
            dataset_id=dataset_id,
            document_id=document_id,
            images=images,
            collection=collection,
            base_position=base_position,
            tenant_id=tenant_id,
        )

    # ========================= Retrieval (delegated to RetrievalService) =========================

    async def retrieve(
        self,
        user: UserContext,
        dataset_id: str,
        query: str,
        top_k: int = 5,
        mode: str = "hybrid",
        document_id: str | None = None,
        dense_weight: float | None = None,
        bm25_weight: float | None = None,
        fusion_method: str | None = None,
        rrf_k: int | None = None,
        alpha: float | None = None,
        score_threshold: float | None = None,
        vector_top_k: int | None = None,
        keyword_top_k: int | None = None,
        candidate_top_k: int | None = None,
        keyword_candidate_k: int | None = None,
        fusion: str | None = None,
        rrf_weights: dict[str, float] | None = None,
        rerank: bool | None = None,
        rerank_model: str | None = None,
        rerank_top_n: int | None = None,
        mmr: bool | None = None,
        mmr_lambda: float | None = None,
        mmr_threshold: float | None = None,
        source_type_filter: str | None = None,
        language_filter: str | None = None,
        metadata_filter: dict[str, Any] | None = None,
        telemetry_source: str = "api",
    ) -> tuple[list[RetrieveResult], dict[str, Any]]:
        return await self.retrieval_service.retrieve(
            user=user, dataset_id=dataset_id, query=query, top_k=top_k,
            mode=mode, document_id=document_id,
            dense_weight=dense_weight, bm25_weight=bm25_weight,
            fusion_method=fusion_method, rrf_k=rrf_k, alpha=alpha,
            score_threshold=score_threshold,
            vector_top_k=vector_top_k, keyword_top_k=keyword_top_k,
            candidate_top_k=candidate_top_k, keyword_candidate_k=keyword_candidate_k,
            fusion=fusion, rrf_weights=rrf_weights,
            rerank=rerank, rerank_model=rerank_model, rerank_top_n=rerank_top_n,
            mmr=mmr, mmr_lambda=mmr_lambda, mmr_threshold=mmr_threshold,
            source_type_filter=source_type_filter, language_filter=language_filter,
            metadata_filter=metadata_filter,
            telemetry_source=telemetry_source,
        )

    async def retrieve_with_images(
        self,
        user: UserContext,
        dataset_id: str,
        query: str,
        top_k: int = 5,
        include_images: bool = True,
        content_type_filter: str | None = None,
        multimodal_rerank: bool = False,
        image_search_enabled: bool = True,
        vlm_rerank_weight: float | None = None,
        image_boost: float | None = None,
        image_score_threshold: float | None = None,
        use_separate_thresholds: bool = False,
        **kwargs: Any,
    ) -> tuple[list[RetrieveResult], dict[str, Any]]:
        return await self.retrieval_service.retrieve_with_images(
            user=user, dataset_id=dataset_id, query=query, top_k=top_k,
            include_images=include_images, content_type_filter=content_type_filter,
            multimodal_rerank=multimodal_rerank,
            image_search_enabled=image_search_enabled,
            vlm_rerank_weight=vlm_rerank_weight, image_boost=image_boost,
            image_score_threshold=image_score_threshold,
            use_separate_thresholds=use_separate_thresholds,
            **kwargs,
        )

    def record_external_retrieval_observation(self, **kwargs: Any) -> dict[str, Any]:
        return self.retrieval_service.record_external_retrieval_observation(**kwargs)

    async def retrieve_with_images_v2(
        self,
        user: UserContext,
        dataset_id: str,
        query: str,
        top_k: int = 5,
        intent: str = "general",
        vlm_rerank: bool = True,
        include_images: bool = True,
        **kwargs: Any,
    ) -> tuple[list[RetrieveResult], dict[str, Any]]:
        return await self.retrieval_service.retrieve_with_images_v2(
            user=user, dataset_id=dataset_id, query=query, top_k=top_k,
            intent=intent, vlm_rerank=vlm_rerank, include_images=include_images,
            **kwargs,
        )

    async def retrieve_batch(
        self,
        user: UserContext,
        dataset_id: str,
        queries: list[Any],
        top_k: int | None = None,
        mode: str = "hybrid",
        document_id: str | None = None,
        dense_weight: float | None = None,
        bm25_weight: float | None = None,
        fusion_method: str | None = None,
        alpha: float | None = None,
        score_threshold: float | None = None,
        source_type_filter: str | None = None,
        language_filter: str | None = None,
        vector_top_k: int | None = None,
        keyword_top_k: int | None = None,
        candidate_top_k: int | None = None,
        keyword_candidate_k: int | None = None,
        fusion: str | None = None,
        rrf_k: int | None = None,
        rrf_weights: dict[str, float] | None = None,
        rerank: bool | None = None,
        rerank_model: str | None = None,
        rerank_top_n: int | None = None,
        mmr: bool | None = None,
        mmr_lambda: float | None = None,
        mmr_threshold: float | None = None,
        include_images: bool = True,
        include_associated_images: bool = True,
        max_parallel: int = 10,
        dedupe_results: bool = False,
    ) -> tuple[list[dict[str, Any]], dict[str, Any]]:
        return await self.retrieval_service.retrieve_batch(
            user=user, dataset_id=dataset_id, queries=queries, top_k=top_k,
            mode=mode, document_id=document_id,
            dense_weight=dense_weight, bm25_weight=bm25_weight,
            fusion_method=fusion_method, alpha=alpha,
            score_threshold=score_threshold,
            source_type_filter=source_type_filter, language_filter=language_filter,
            vector_top_k=vector_top_k, keyword_top_k=keyword_top_k,
            candidate_top_k=candidate_top_k, keyword_candidate_k=keyword_candidate_k,
            fusion=fusion, rrf_k=rrf_k, rrf_weights=rrf_weights,
            rerank=rerank, rerank_model=rerank_model, rerank_top_n=rerank_top_n,
            mmr=mmr, mmr_lambda=mmr_lambda, mmr_threshold=mmr_threshold,
            include_images=include_images, include_associated_images=include_associated_images,
            max_parallel=max_parallel, dedupe_results=dedupe_results,
        )


    # -- Delegations to DocumentProcessor / EmbeddingManager (text extraction + embedding config) --

    def _sanitize_text_for_db(self, text: str) -> str:
        return self.document_processor._sanitize_text_for_db(text)

    def _decode_text_bytes(self, content: bytes) -> str:
        return self.document_processor.decode_text_bytes(content)

    def _clean_pdf_content(self, text: str) -> str:
        return self.document_processor.clean_pdf_content(text)

    def _extract_text_from_pdf_bytes(self, content: bytes) -> str:
        return self.document_processor.extract_text_from_pdf_bytes(content)

    def _ocr_pdf_bytes(self, content: bytes) -> str:
        return self.document_processor.ocr_pdf_bytes(content)

    def _extract_pdf_with_pdfplumber(self, pdf_stream) -> str:
        return self.document_processor.extract_pdf_with_pdfplumber(pdf_stream)

    def _pdf_table_to_markdown(self, table: list[list]) -> str:
        return self.document_processor.pdf_table_to_markdown(table)

    def _extract_text_from_docx_bytes(self, content: bytes) -> str:
        return self.document_processor.extract_text_from_docx_bytes(content)

    def _table_to_markdown(self, table) -> str:
        return self.document_processor.table_to_markdown(table)

    def _parse_table_row(self, row, total_cols: int) -> list[str]:
        return self.document_processor.parse_table_row(row, total_cols)

    def _extract_text_from_doc_bytes(self, content: bytes) -> str:
        return self.document_processor.extract_text_from_doc_bytes(content)

    def _extract_text_from_html(self, html: str) -> str:
        return self.document_processor.extract_text_from_html(html)

    def _extract_text_from_bytes(
        self, content: bytes, filename: str | None = None, mime_type: str | None = None,
    ) -> tuple[str, str]:
        return self.document_processor.extract_text_from_bytes(content, filename, mime_type)

    async def _resolve_embedding_config(
        self,
        provider: str,
        model: str,
        embedding_config: dict[str, Any],
        *,
        tenant_id: str = "",
    ) -> EmbeddingConfig:
        return await self.embedding_manager.resolve_embedding_config(
            provider,
            model,
            embedding_config,
            tenant_id=tenant_id,
        )

    # ---------------- T6 BM25 v2 cutover/rollback lifecycle -----------------

    @property
    def bm25_v2_lifecycle_store(self) -> Any | None:
        """Lazily bound Bm25V2LifecycleStore; None until the PG pool is up."""
        if self._bm25_v2_lifecycle_store is None:
            from ...persistence.bm25_v2_lifecycle import Bm25V2LifecycleStore

            pool = getattr(self.db, "_pool", None)
            if pool is not None:
                self._bm25_v2_lifecycle_store = Bm25V2LifecycleStore(pool)
        return self._bm25_v2_lifecycle_store

    @property
    def bm25_v2_lifecycle_service(self) -> Any | None:
        """Lazily bound T6 protocol service over the lifecycle store.

        None in degraded mode (no pool). Route integrators must surface that
        as "lifecycle tables unavailable", never attempt a transition.
        """
        if self._bm25_v2_lifecycle_service is None:
            store = self.bm25_v2_lifecycle_store
            if store is not None:
                from .bm25_v2_lifecycle import Bm25V2LifecycleService

                self._bm25_v2_lifecycle_service = Bm25V2LifecycleService(
                    vector_store=self.vector_store,
                    lifecycle_store=store,
                )
        return self._bm25_v2_lifecycle_service

    # --------------------- T3 embedding versioning / blue-green ------------

    @property
    def embedding_version_store(self) -> Any | None:
        """Lazily bound EmbeddingVersionStore; None until the PG pool is up.

        Callers must treat None as "T3 tables unavailable" and degrade, not
        crash: legacy datasets predate the binding tables entirely.
        """
        if self._embedding_version_store is None:
            from ...persistence.embedding_version_store import EmbeddingVersionStore

            pool = getattr(self.db, "_pool", None)
            if pool is not None:
                self._embedding_version_store = EmbeddingVersionStore(pool)
        return self._embedding_version_store

    @property
    def embedding_migration_service(self) -> Any | None:
        """Lazily bound blue-green orchestrator over the version store.

        ``embedder_factory`` resolves the target-generation embedder through
        the same server-owned config path dataset creation uses, so a backfill
        never needs caller-supplied credentials.
        """
        store = self.embedding_version_store
        if store is None:
            return None
        if self._embedding_migration_service is None:
            from .embedding_migration import EmbeddingMigrationService

            async def _embedder_for(identity: dict[str, Any]) -> Any:
                from .embedding import create_embedding

                econf = await self._resolve_embedding_config(
                    provider=str(identity.get("embedding_provider") or ""),
                    model=str(identity.get("embedding_model") or ""),
                    embedding_config={},
                    tenant_id=str(identity.get("tenant_id") or ""),
                )
                return create_embedding(
                    econf,
                    dimension=int(identity.get("embedding_dimension") or 0) or None,
                )

            self._embedding_migration_service = EmbeddingMigrationService(
                store=store,
                vector_store=self.vector_store,
                embedder_factory=_embedder_for,
            )
        return self._embedding_migration_service

    # ========================= Document Enable/Disable/Archive (delegated to DocumentService) =====

    async def set_document_enabled(
        self, user: UserContext, dataset_id: str, document_id: str, enabled: bool
    ) -> dict[str, Any]:
        return await self.document_service.set_document_enabled(user, dataset_id, document_id, enabled)

    async def set_document_archived(
        self, user: UserContext, dataset_id: str, document_id: str,
        archived: bool, reason: str | None = None,
    ) -> dict[str, Any]:
        return await self.document_service.set_document_archived(
            user, dataset_id, document_id, archived, reason
        )

    async def update_document(
        self, user: UserContext, dataset_id: str, document_id: str,
        update_data: dict[str, Any],
    ) -> dict[str, Any]:
        return await self.document_service.update_document(
            user, dataset_id, document_id, update_data
        )

    # ========================= Batch Operations (delegated to DocumentService) ===================

    async def batch_create_documents(
        self, user: UserContext, dataset_id: str, documents: list[Any],
        process_rule: dict[str, Any] | None = None, batch_name: str | None = None,
    ) -> dict[str, Any]:
        return await self.document_service.batch_create_documents(
            user, dataset_id, documents, process_rule, batch_name
        )

    async def batch_delete_documents(
        self, user: UserContext, dataset_id: str, document_ids: list[str]
    ) -> dict[str, Any]:
        return await self.document_service.batch_delete_documents(user, dataset_id, document_ids)

    # ========================= Segment Enable/Disable (delegated to DocumentService) =============

    async def set_segment_enabled(
        self, user: UserContext, dataset_id: str, segment_id: str, enabled: bool
    ) -> dict[str, Any]:
        return await self._run_segment_mutation_with_bm25_publication(
            user,
            dataset_id,
            lambda: self.document_service.set_segment_enabled(
                user,
                dataset_id,
                segment_id,
                enabled,
            ),
        )

    async def set_segments_enabled_batch(
        self,
        user: UserContext,
        dataset_id: str,
        segment_ids: Any,
        enabled: Any,
    ) -> dict[str, Any]:
        return await self._run_segment_mutation_with_bm25_publication(
            user,
            dataset_id,
            lambda: self.document_service.set_segments_enabled_batch(
                user,
                dataset_id,
                segment_ids,
                enabled,
            ),
        )

    async def create_segment(
        self, user: UserContext, dataset_id: str, document_id: str, content: str,
        answer: str | None = None, keywords: list[str] | None = None,
    ) -> dict[str, Any]:
        return await self._run_segment_mutation_with_bm25_publication(
            user,
            dataset_id,
            lambda: self.document_service.create_segment(
                user,
                dataset_id,
                document_id,
                content,
                answer,
                keywords,
            ),
        )

    # ========================= Statistics =========================

    async def get_dataset_statistics(self, user: UserContext, dataset_id: str) -> dict[str, Any]:
        """Get dataset statistics."""
        from .document_service import (
            _dataset_content_generation,
            _require_unchanged_dataset_content,
        )

        dataset = await self.require_dataset_access(user, dataset_id, required="viewer")
        generation = _dataset_content_generation(dataset)

        stats = (await self.db.get_datasets_statistics_batch([dataset_id])).get(
            dataset_id, {}
        )
        result = {
            "dataset_id": dataset_id,
            "document_count": stats.get("document_count", 0),
            "available_document_count": stats.get("available_document_count", 0),
            "segment_count": stats.get("segment_count", 0),
            "available_segment_count": stats.get("available_segment_count", 0),
            "word_count": stats.get("word_count", 0),
            "hit_count": stats.get("hit_count", 0),
        }
        await _require_unchanged_dataset_content(
            self,
            user,
            dataset_id,
            generation,
        )
        return result

    async def get_document_statistics(
        self, user: UserContext, dataset_id: str, document_id: str
    ) -> dict[str, Any]:
        """Get document statistics."""
        from .document_service import (
            _dataset_content_generation,
            _require_unchanged_dataset_content,
        )

        dataset = await self.require_dataset_access(user, dataset_id, required="viewer")
        generation = _dataset_content_generation(dataset)
        stats = await self.db.get_document_statistics_aggregate(dataset_id, document_id)
        if not stats:
            raise ValidationFailedError("document not found")
        result = {
            "document_id": document_id,
            "segment_count": stats.get("segment_count", 0),
            "word_count": stats.get("word_count", 0) or 0,
            "hit_count": stats.get("hit_count", 0),
            "status": stats.get("status", "unknown"),
            "enabled": stats.get("enabled", True),
            "archived": stats.get("archived", False),
        }
        await _require_unchanged_dataset_content(
            self,
            user,
            dataset_id,
            generation,
        )
        return result

    def _normalize_local_image_url(
        self,
        image_url: str | None,
        segment_id: str | None,
    ) -> str | None:
        if not image_url or not segment_id:
            return image_url
        if isinstance(image_url, str) and image_url.startswith("file://"):
            return f"/api/v1/knowledge/images/{segment_id}"
        return image_url

    async def _get_presigned_image_url(
        self,
        image_url: str | None,
        segment_id: str | None,
        expiry_seconds: int = 3600,
    ) -> str | None:
        """
        Get presigned URL for an image.

        Text-First RAG: Generate presigned URLs for image results so LLMs can access them.

        Args:
            image_url: The stored image URL (S3/OSS/file://)
            segment_id: Segment ID (for local storage API fallback)
            expiry_seconds: URL expiry time

        Returns:
            Presigned URL or API endpoint for accessing the image
        """
        if not image_url:
            return None

        # Handle local file:// URLs - use API endpoint
        if isinstance(image_url, str) and image_url.startswith("file://"):
            if segment_id:
                return f"/api/v1/knowledge/images/{segment_id}"
            return None

        # For S3/OSS URLs, generate presigned URL if storage service is available
        if self.image_storage_service:
            try:
                presigned = await self.image_storage_service.get_presigned_url(
                    storage_url=image_url,
                    expiry_seconds=expiry_seconds,
                    segment_id=segment_id,
                )
                if presigned:
                    return presigned
            except Exception as e:
                logger.warning(f"Failed to generate presigned URL: {e}")

        # Fall back to original URL
        return image_url

    # ========================= P3: Multimodal Image-Chunk Association =========================

    async def associate_images_to_chunks(
        self,
        document_id: str,
        max_images_per_chunk: int = 10,
        proximity_threshold: float = 0.3,
        image_segment_ids: set[str] | None = None,
    ) -> dict[str, Any]:
        """
        Associate image segments with text segments based on proximity.

        This implements Dify-style Smart Attachment Handling where each text
        chunk can have up to max_images_per_chunk associated images.

        Association Strategy:
        1. Get all text segments and image segments for the document
        2. For each text segment, find nearby image segments
        3. Compute proximity score based on:
           - Same page: 0.7-1.0
           - Adjacent position: 0.5-0.7
           - Same document section: 0.3-0.5
        4. Associate top-k images (by proximity) to each text chunk

        Args:
            document_id: The document to process
            max_images_per_chunk: Maximum images per text chunk (default 10, Dify pattern)
            proximity_threshold: Minimum proximity score to create association

        Returns:
            Statistics about associations created
        """

        # Get all segments for the document
        doc = await self.db.get_document(document_id)
        if not doc:
            raise ValidationFailedError("document not found")

        dataset_id = str(doc.get("dataset_id"))
        dataset = await self.db.get_dataset(dataset_id)
        tenant_id = str((dataset or {}).get("tenant_id") or "").strip()
        if not tenant_id:
            raise ValidationFailedError("document dataset tenant scope is unavailable")

        # Get text and image segments separately
        all_segments = await self.db.list_segments(
            dataset_id=dataset_id, document_id=document_id, limit=10000, offset=0
        )

        text_segments = [
            s for s in all_segments if str(s.get("content_type", "text")).lower() == "text"
        ]
        image_segments = [
            s
            for s in all_segments
            if str(s.get("content_type", "text")).lower() == "image"
            and (
                image_segment_ids is None
                or str(s.get("segment_id") or "") in image_segment_ids
            )
        ]

        if not text_segments or not image_segments:
            logger.info(
                f"Document {document_id}: {len(text_segments)} text, {len(image_segments)} image segments - skipping association"
            )
            replace_associations = getattr(
                self.db,
                "replace_document_image_associations",
                None,
            )
            if (
                text_segments
                and image_segment_ids is not None
                and callable(replace_associations)
            ):
                await replace_associations(
                    document_id,
                    dataset_id,
                    tenant_id,
                    [],
                )
            return {
                "document_id": document_id,
                "text_segments": len(text_segments),
                "image_segments": len(image_segments),
                "associations_created": 0,
            }

        logger.info(
            f"Associating images for document {document_id}: {len(text_segments)} text, {len(image_segments)} image segments"
        )

        def _normalize_for_match(value: str) -> str:
            return re.sub(r"\s+", " ", (value or "").strip()).lower()

        placeholder_pattern = re.compile(r"\[Image\]|\[图片\]")
        placeholder_map: dict[int, dict[str, Any]] = {}
        text_norm_cache: dict[str, str] = {}

        text_segments_sorted = sorted(
            text_segments,
            key=lambda s: int(s.get("position", 0) or 0),
        )
        placeholder_index = 0
        for seg in text_segments_sorted:
            seg_id = str(seg.get("segment_id"))
            text_value = str(seg.get("text") or "")
            text_norm_cache[seg_id] = _normalize_for_match(text_value)
            matches = placeholder_pattern.findall(text_value)
            if matches:
                for _ in range(len(matches)):
                    if placeholder_index not in placeholder_map:
                        placeholder_map[placeholder_index] = seg
                    placeholder_index += 1

        image_infos: list[dict[str, Any]] = []
        for img_seg in image_segments:
            img_metadata = _ensure_dict(img_seg.get("metadata"))
            context_index = img_metadata.get("context_index")
            if context_index is None:
                context_index = img_metadata.get("source_position")
            if context_index is not None:
                try:
                    context_index = int(context_index)
                except (TypeError, ValueError):
                    context_index = None

            image_position = int(img_seg.get("position", 0) or 0)
            image_page = img_metadata.get("page") or img_metadata.get("page_number")
            if context_index is not None:
                if context_index in placeholder_map:
                    mapped_seg = placeholder_map[context_index]
                    image_position = int(
                        mapped_seg.get("position", image_position) or image_position
                    )
                    if image_page is None:
                        mapped_meta = _ensure_dict(mapped_seg.get("metadata"))
                        image_page = mapped_meta.get("page") or mapped_meta.get("page_number")
                else:
                    image_position = int(context_index)

            context_text = str(img_metadata.get("context_text") or "")
            context_norm = _normalize_for_match(context_text)
            if len(context_norm) < 12:
                context_norm = ""

            image_infos.append(
                {
                    "segment": img_seg,
                    "position": image_position,
                    "page": image_page,
                    "context_norm": context_norm,
                }
            )

        # Build associations
        associations: list[dict[str, Any]] = []
        segments_with_images = 0

        for text_seg in text_segments_sorted:
            text_seg_id = str(text_seg.get("segment_id"))
            text_position = int(text_seg.get("position", 0))
            text_metadata = _ensure_dict(text_seg.get("metadata"))
            text_page = text_metadata.get("page") or text_metadata.get("page_number")
            text_norm = text_norm_cache.get(text_seg_id, "")

            # Find candidate images and compute proximity scores
            candidates: list[tuple[dict[str, Any], float]] = []

            for img_info in image_infos:
                img_seg = img_info["segment"]
                img_position = int(img_info["position"])
                img_page = img_info["page"]

                # Compute proximity score
                score = self._compute_image_proximity_score(
                    text_position=text_position,
                    text_page=text_page,
                    image_position=img_position,
                    image_page=img_page,
                    total_segments=len(all_segments),
                )

                if img_info["context_norm"] and img_info["context_norm"] in text_norm:
                    score = max(score, 0.95)

                if score >= proximity_threshold:
                    candidates.append((img_seg, score))

            # Sort by score and take top-k
            candidates.sort(key=lambda x: x[1], reverse=True)
            top_candidates = candidates[:max_images_per_chunk]

            if top_candidates:
                segments_with_images += 1

            for position, (img_seg, score) in enumerate(top_candidates):
                associations.append(
                    {
                        "segment_id": text_seg_id,
                        "image_segment_id": str(img_seg.get("segment_id")),
                        "position": position,
                        "proximity_score": score,
                        "char_offset": int(img_seg.get("position", 0)),
                        "page_number": img_seg.get("metadata", {}).get("page"),
                    }
                )

        # Replace the complete set in one transaction.  Reprocess must remove
        # stale pairs just as it creates new ones; append-only association
        # writes leave bindings and image points reachable forever.
        replace_associations = getattr(
            self.db,
            "replace_document_image_associations",
            None,
        )
        if callable(replace_associations):
            count = await replace_associations(
                document_id,
                dataset_id,
                tenant_id,
                associations,
            )
        elif associations:
            count = await self.db.add_segment_image_associations_batch(
                associations,
                dataset_id=dataset_id,
                tenant_id=tenant_id,
            )
        else:
            count = 0

        if associations:
            logger.info(f"Created {count} image associations for document {document_id}")

            # Compatibility for narrow fakes without the atomic replacement
            # method; production updates flags inside the same transaction.
            affected_segment_ids = list({a["segment_id"] for a in associations})
            if not callable(replace_associations):
                for seg_id in affected_segment_ids:
                    await self.db.update_segment_image_flags(seg_id)

        return {
            "document_id": document_id,
            "text_segments": len(text_segments),
            "image_segments": len(image_segments),
            "associations_created": len(associations),
            "segments_with_images": segments_with_images,
        }

    def _compute_image_proximity_score(
        self,
        text_position: int,
        text_page: int | None,
        image_position: int,
        image_page: int | None,
        total_segments: int,
    ) -> float:
        """
        Compute proximity score between a text segment and an image segment.

        Scoring strategy:
        - Same page (if pages are tracked): base score 0.7
        - Position distance: closer = higher score
        - Bonus for adjacent positions

        Args:
            text_position: Position index of text segment
            text_page: Page number of text segment (if available)
            image_position: Position index of image segment
            image_page: Page number of image segment (if available)
            total_segments: Total segments in document (for normalization)

        Returns:
            Proximity score [0.0, 1.0]
        """
        score = 0.0

        # Page-based scoring (if pages are available)
        if text_page is not None and image_page is not None:
            if text_page == image_page:
                score = 0.7  # Same page is strong signal
            else:
                page_distance = abs(text_page - image_page)
                if page_distance == 1:
                    score = 0.4  # Adjacent page
                elif page_distance <= 3:
                    score = 0.2  # Within 3 pages
                else:
                    return 0.0  # Too far

        # Position-based scoring
        position_distance = abs(text_position - image_position)

        if position_distance == 0:
            # Same position (unlikely but possible in some chunking strategies)
            position_score = 1.0
        elif position_distance == 1:
            # Adjacent positions - very close
            position_score = 0.9
        elif position_distance <= 3:
            # Within 3 positions
            position_score = 0.7
        elif position_distance <= 10:
            # Within 10 positions
            position_score = 0.5 - (position_distance - 3) * 0.05
        else:
            # Normalize based on total segments
            normalized_distance = position_distance / max(total_segments, 1)
            position_score = max(0.0, 0.3 - normalized_distance)

        # If page information exists, weight it more heavily; otherwise use
        # the bounded position-only score.
        final_score = 0.6 * score + 0.4 * position_score if score > 0 else position_score

        return min(1.0, max(0.0, final_score))

    async def get_segments_with_images(
        self,
        user: UserContext,
        dataset_id: str,
        document_id: str | None = None,
        include_images: bool = True,
    ) -> list[dict[str, Any]]:
        """
        Get segments with their associated images attached.

        This is used for multimodal retrieval where text segments need
        their associated images for context.

        Args:
            user: User context for permission check
            dataset_id: Dataset ID
            document_id: Optional document ID to filter
            include_images: Whether to include associated image details

        Returns:
            List of segments with associated_images field populated
        """
        dataset = await self.require_dataset_access(user, dataset_id, required="viewer")
        tenant_id = str(dataset.get("tenant_id") or "").strip()
        if not tenant_id:
            raise ValidationFailedError("dataset tenant scope is unavailable")

        segments = await self.db.list_segments(
            dataset_id=dataset_id, document_id=document_id, limit=5000, offset=0
        )

        if not include_images:
            return segments

        # Get text segments that have images
        text_segment_ids = [
            s.get("segment_id")
            for s in segments
            if str(s.get("content_type", "text")).lower() == "text" and s.get("has_images", False)
        ]

        if not text_segment_ids:
            return segments

        # Batch fetch associated images
        associations = await self.db.get_segment_associations_batch(
            text_segment_ids,
            dataset_id=dataset_id,
            tenant_id=tenant_id,
        )

        # Attach images to segments
        result = []
        for seg in segments:
            seg_dict = dict(seg)
            seg_id = seg_dict.get("segment_id")
            if seg_dict.get("image_url"):
                seg_dict["image_url"] = self._normalize_local_image_url(
                    seg_dict.get("image_url"),
                    seg_id,
                )

            if seg_id in associations:
                seg_dict["associated_images"] = [
                    AssociatedImage(
                        image_segment_id=img["image_segment_id"],
                        storage_url=self._normalize_local_image_url(
                            img.get("storage_url", ""),
                            img.get("image_segment_id"),
                        ),
                        filename=img.get("filename", ""),
                        vlm_description=img.get("vlm_description"),
                        proximity_score=float(img.get("proximity_score", 1.0)),
                        char_offset=int(img.get("char_offset", 0)),
                        page_number=img.get("page_number"),
                        media_type=img.get("media_type", "image/png"),
                    ).to_dict()
                    for img in associations[seg_id]
                ]
            else:
                seg_dict["associated_images"] = []

            result.append(seg_dict)

        return result

    async def clear_image_associations(
        self,
        document_id: str,
    ) -> int:
        """
        Clear all image associations for a document.

        This should be called before re-processing a document's
        image associations.

        Args:
            document_id: Document ID to clear associations for

        Returns:
            Number of associations deleted
        """
        return await self.db.delete_image_associations_by_document(document_id)

    async def recover_stuck_documents(
        self,
        stuck_threshold_minutes: int = 15,
        worker: KnowledgeWorker | None = None,
    ) -> dict[str, Any]:
        """
        Recover documents stuck in processing state.

        This method detects documents that have been stuck in 'parsing',
        'splitting', or 'indexing' status for longer than the threshold and
        resets them to 'waiting' status so they can be re-queued for
        processing.

        Args:
            stuck_threshold_minutes: Minutes after which a document is considered stuck
            worker: Optional KnowledgeWorker to re-enqueue recovered documents

        Returns:
            Dict with recovery statistics:
            - recovered_count: Number of documents recovered
            - recovered_documents: List of recovered document IDs and titles
            - requeued_count: Number of documents re-added to processing queue
        """
        result = {
            "recovered_count": 0,
            "recovered_documents": [],
            "requeued_count": 0,
            "abandoned_upload_count": 0,
            "upload_cleanup_count": 0,
        }

        try:
            # Upload owners are never ordinary ingest candidates. Crash-stale
            # owners first become a durable cleanup receipt; storage cleanup is
            # retried under the same document owner lease until it commits.
            fail_stale_uploads = getattr(self.db, "fail_stale_document_uploads", None)
            list_upload_cleanups = getattr(
                self.db,
                "list_pending_document_upload_cleanups",
                None,
            )
            complete_upload_cleanup = getattr(
                self.db,
                "complete_document_upload_cleanup",
                None,
            )
            document_lease = getattr(self.db, "document_index_update_lease", None)
            delete_assets = getattr(
                getattr(self, "image_storage_service", None),
                "delete_document_assets",
                None,
            )
            if callable(fail_stale_uploads):
                abandoned = await fail_stale_uploads(
                    max(int(stuck_threshold_minutes) * 4, 60)
                )
                result["abandoned_upload_count"] = len(abandoned)
            if all(
                callable(value)
                for value in (
                    list_upload_cleanups,
                    complete_upload_cleanup,
                    document_lease,
                    delete_assets,
                )
            ):
                pending_cleanups = await list_upload_cleanups(limit=100)
                for receipt in pending_cleanups:
                    document_id = str(receipt.get("document_id") or "").strip()
                    dataset_id = str(receipt.get("dataset_id") or "").strip()
                    if not document_id or not dataset_id:
                        continue
                    try:
                        async with document_lease(
                            dataset_id,
                            document_id,
                        ) as lease_connection:
                            document = await self.db.get_document(
                                document_id,
                                connection=lease_connection,
                            )
                            upload_failure = _ensure_dict(
                                _ensure_dict((document or {}).get("metadata")).get(
                                    DOCUMENT_UPLOAD_FAILED_KEY
                                )
                            )
                            if upload_failure.get("status") != "cleanup_pending":
                                continue
                            dataset = await self.db.get_dataset(
                                dataset_id,
                                connection=lease_connection,
                            )
                            tenant_id = str((dataset or {}).get("tenant_id") or "").strip()
                            if not tenant_id:
                                continue
                            await delete_assets(
                                tenant_id=tenant_id,
                                document_id=document_id,
                            )
                            if await complete_upload_cleanup(
                                document_id,
                                dataset_id,
                                connection=lease_connection,
                            ):
                                result["upload_cleanup_count"] += 1
                    except IndexLeaseUnavailableError:
                        logger.info(
                            "Deferred stale upload cleanup during dataset lifecycle work",
                            extra={
                                "dataset_id": dataset_id,
                                "document_id": document_id,
                            },
                        )
                    except Exception:
                        logger.exception(
                            "Stale upload storage cleanup failed and remains retryable",
                            extra={
                                "dataset_id": dataset_id,
                                "document_id": document_id,
                            },
                        )

            # Claim before enqueue so concurrent replicas cannot recover the
            # same durable generation. The claim refreshes updated_at; if this
            # process dies before enqueue, the pending lifecycle marker becomes
            # claimable again after the same TTL.
            claim_stuck = getattr(self.db, "claim_stuck_documents", None)
            claimed_atomically = callable(claim_stuck)
            allow_active_bm25_v2 = bool(
                getattr(
                    getattr(self, "vector_store", None),
                    "bm25_v2_enabled",
                    False,
                )
            )
            stuck_documents = (
                await claim_stuck(
                    stuck_threshold_minutes,
                    **({"allow_active_bm25_v2": True} if allow_active_bm25_v2 else {}),
                )
                if claimed_atomically
                else await self.db.find_stuck_documents(stuck_threshold_minutes)
            )

            if not stuck_documents:
                logger.info("No stuck documents found")
                return result

            logger.warning(f"Found {len(stuck_documents)} stuck documents, recovering...")

            for doc in stuck_documents:
                doc_id = doc.get("document_id")
                dataset_id = doc.get("dataset_id")
                title = doc.get("title", "Unknown")
                old_status = doc.get("old_status") or doc.get("status")

                try:
                    if not claimed_atomically:
                        # Compatibility fallback for non-PostgreSQL test stores.
                        await self.db.update_document_status(
                            doc_id,
                            status="waiting",
                            progress=0,
                            error=None,
                        )

                    result["recovered_count"] += 1
                    result["recovered_documents"].append(
                        {
                            "document_id": doc_id,
                            "title": title,
                            "old_status": old_status,
                        }
                    )

                    logger.info(f"Recovered stuck document: {title} ({doc_id}) from {old_status}")

                    # Re-enqueue for processing if worker is available
                    if worker and dataset_id:
                        enqueue_claimed = getattr(worker, "enqueue_claimed", None)
                        if claimed_atomically and callable(enqueue_claimed):
                            await enqueue_claimed(dataset_id, doc_id)
                        else:
                            await worker.enqueue(dataset_id, doc_id)
                        result["requeued_count"] += 1

                except Exception as e:
                    logger.error(f"Failed to recover document {doc_id}: {e}")

        except Exception as e:
            logger.error(f"Error during stuck document recovery: {e}")
            raise

        return result
