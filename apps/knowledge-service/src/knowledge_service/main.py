"""Knowledge Base Microservice entry-point.

Boots a FastAPI application with async lifespan management for the database
pool, Qdrant client, and background worker.
"""

from __future__ import annotations

import asyncio
import os
from contextlib import asynccontextmanager, suppress
from typing import Any

import structlog
import uvicorn
from ai_gateway_core.logging import configure_structured_logging
from fastapi import FastAPI, Request
from fastapi.middleware.cors import CORSMiddleware
from fastapi.responses import ORJSONResponse

from .config import Settings, get_settings
from .health import (
    public_readiness as _public_knowledge_readiness,
)
from .health import (
    readiness_snapshot as _knowledge_readiness_snapshot,
)

# ---------------------------------------------------------------------------
# Structured logging
# ---------------------------------------------------------------------------
# PR-3: All four services now share the ai_gateway_core.logging machinery
# so every record (incl. structlog calls below) flows through ContextFilter
# and carries request_id / trace_id / service. structlog stays as a
# dependency for callers that already use its kw-args API; we just stop
# using it for service-level config and route its emissions through stdlib
# via PrintLoggerFactory's default behavior (each structlog log call ends
# up calling print() — which we redirect via stdlib by reconfiguring it
# to use the standard logger factory).


def configure_logging(level: str = "INFO") -> None:
    """Configure JSON/simple logging via the shared core machinery.

    Format toggle order: LOG_FORMAT env > ENVIRONMENT=production → json,
    else simple. structlog is reconfigured to dispatch through stdlib so
    its kw-args still surface but ContextFilter can stamp request_id.
    """
    env = get_settings()
    log_format = env.log_format
    if not log_format:
        log_format = "json" if env.environment.lower() == "production" else "simple"

    configure_structured_logging(
        level=level,
        format_type=log_format,
        service="knowledge-service",
        log_to_file=False,
    )

    # Wire structlog through stdlib so ``structlog.get_logger().info(...)``
    # ends up as a stdlib LogRecord ContextFilter can stamp. JSONRenderer
    # would otherwise serialize at the structlog layer and bypass our
    # filter / formatter.
    structlog.configure(
        processors=[
            structlog.contextvars.merge_contextvars,
            structlog.stdlib.add_log_level,
            structlog.processors.StackInfoRenderer(),
            structlog.dev.set_exc_info,
            structlog.stdlib.render_to_log_kwargs,
        ],
        logger_factory=structlog.stdlib.LoggerFactory(),
        wrapper_class=structlog.stdlib.BoundLogger,
        context_class=dict,
        cache_logger_on_first_use=True,
    )


logger = structlog.get_logger()

# ---------------------------------------------------------------------------
# Qdrant client helper
# ---------------------------------------------------------------------------


async def _init_qdrant(settings: Settings) -> Any:
    """Create and verify Qdrant async client connection."""
    from qdrant_client import AsyncQdrantClient

    qdrant = AsyncQdrantClient(
        url=settings.qdrant.url,
        api_key=settings.qdrant.api_key,
        prefer_grpc=settings.qdrant.prefer_grpc,
        timeout=settings.qdrant.timeout_seconds,
    )
    try:
        collections = await qdrant.get_collections()
        logger.info(
            "qdrant_connected",
            url=settings.qdrant.url,
            collections=len(collections.collections),
        )
    except Exception:
        logger.exception("qdrant_probe_failed", url=settings.qdrant.url)
        # Qdrant is a core retrieval dependency.  Returning a client object
        # after a failed probe would let readiness report a false healthy
        # instance, so close the partial client and abort startup.
        await qdrant.close()
        raise
    return qdrant


_LOCAL_IDEMPOTENCY_ENVIRONMENTS = frozenset(
    {"local", "dev", "development", "test", "testing"}
)


def _idempotency_store_from_settings(settings: Settings) -> tuple[Any, Any | None]:
    """Build the configured store without silently weakening persistence."""
    from ai_gateway_core.comm import (
        InMemoryIdempotencyStore,
        RedisIdempotencyStore,
    )

    backend = settings.internal_idempotency_backend.strip().lower()
    environment = settings.environment.strip().lower()
    if backend == "memory":
        if environment not in _LOCAL_IDEMPOTENCY_ENVIRONMENTS:
            raise RuntimeError(
                "INTERNAL_IDEMPOTENCY_BACKEND=memory is allowed only when "
                "ENVIRONMENT explicitly identifies local development or tests"
            )
        return InMemoryIdempotencyStore(), None

    if backend != "redis":
        raise RuntimeError("INTERNAL_IDEMPOTENCY_BACKEND must be redis or memory")

    redis_url = (settings.internal_comm_redis_url or settings.redis_url).strip()
    if not redis_url:
        raise RuntimeError(
            "Redis idempotency is configured but neither INTERNAL_COMM_REDIS_URL "
            "nor REDIS_URL is set"
        )

    import redis.asyncio as aioredis

    redis_client = aioredis.from_url(redis_url, decode_responses=False)
    return RedisIdempotencyStore(redis_client), redis_client


async def _probe_idempotency_redis(redis_client: Any) -> None:
    """Refuse readiness when the configured durable store is unreachable."""
    try:
        await asyncio.wait_for(redis_client.ping(), timeout=5.0)
    except Exception as exc:
        raise RuntimeError("Redis idempotency backend is unavailable") from exc


async def _close_idempotency_redis(redis_client: Any) -> None:
    close = getattr(redis_client, "aclose", None)
    if close is None:
        close = getattr(redis_client, "close", None)
    if callable(close):
        await close()


# ---------------------------------------------------------------------------
# FastAPI app factory
# ---------------------------------------------------------------------------

OPENAPI_TAGS = [
    {"name": "Health", "description": "Liveness and readiness probes for container orchestration."},
    {
        "name": "Datasets",
        "description": "Dataset CRUD — create, list, update, delete knowledge bases with configurable embedding and chunking.",
    },
    {
        "name": "Documents",
        "description": "Document management — upload (PDF/DOCX/TXT/HTML/images), text creation, batch operations, versioning, and status control.",
    },
    {
        "name": "Segments",
        "description": "Segment (chunk) management — list, create, update, enable/disable individual text segments within documents.",
    },
    {
        "name": "Retrieval",
        "description": "Vector similarity search and hybrid RAG retrieval — dense, BM25, hybrid (RRF/weighted), with optional reranking and MMR diversity.",
    },
    {
        "name": "QA",
        "description": "Question answering — RAG retrieval + LLM generation, with streaming and batch evaluation support.",
    },
    {
        "name": "Configuration",
        "description": "Dataset configuration — chunking strategy, retrieval parameters, embedding settings, and statistics.",
    },
    {
        "name": "Maintenance",
        "description": "Maintenance operations — deduplication, force-complete stuck documents, worker status monitoring.",
    },
    {"name": "Worker", "description": "Background ingestion worker status and queue monitoring."},
]


def create_app(settings: Settings | None = None) -> FastAPI:
    """Build the FastAPI application with lifespan hooks."""
    resolved = settings or Settings()
    idempotency_store, idempotency_redis_client = _idempotency_store_from_settings(
        resolved
    )

    @asynccontextmanager
    async def lifespan(app: FastAPI):
        configure_logging(resolved.app.log_level)
        logger.info(
            "knowledge_service_starting",
            port=resolved.app.port,
            qdrant_url=resolved.qdrant.url,
        )

        # OpenTelemetry SDK bootstrap — must run BEFORE the DB pool init
        # below so AsyncPGInstrumentor patches asyncpg before the first
        # connection is acquired. Idempotent across restarts. Endpoint
        # resolved from OTEL_EXPORTER_OTLP_ENDPOINT; unset → no-op spans.
        from ai_gateway_core.tracing import init_tracing

        init_tracing("knowledge-service")

        # Graceful drain — flip ``DRAIN`` on SIGTERM/SIGINT so DrainMiddleware
        # short-circuits new requests with 503 + Retry-After. Below (after
        # ``yield``) we await ``DRAIN.wait_drained`` so in-flight retrieval /
        # ingestion requests get to finish before the worker + DB pool close.
        from ai_gateway_core.proxy.drain import DRAIN, install_signal_handlers

        install_signal_handlers(asyncio.get_running_loop())

        # --- startup ---
        app.state.settings = resolved

        # --- Initialize KnowledgeService + Worker ---
        knowledge_service = None
        knowledge_worker = None
        confluence_manual_sync = None
        embedding_migration_worker = None
        db_storage = None
        qdrant = None
        vlm_ocr_service = None
        try:
            if idempotency_redis_client is not None:
                await _probe_idempotency_redis(idempotency_redis_client)

            from .persistence.database import DatabaseStorage as FullDatabaseStorage
            from .services.knowledge.knowledge_service import KnowledgeService
            from .services.knowledge.tenant_provider import (
                TenantEmbeddingCredentialResolver,
            )
            from .services.knowledge.worker import DurableEnqueueProxy, KnowledgeWorker

            db_storage = FullDatabaseStorage(
                dsn=resolved.database.dsn,
                enabled=True,
                auto_init=False,  # Schema already exists
                pool_min_size=resolved.database.pool_min_size,
                pool_max_size=resolved.database.pool_max_size,
            )
            await db_storage.connect()
            app.state.db = db_storage

            qdrant = await _init_qdrant(resolved)
            app.state.qdrant = qdrant

            # KnowledgeService expects gateway-style settings with settings.knowledge.*
            # Create a compatibility wrapper that maps KB Service flat config
            class _SettingsCompat:
                """Adapts KB Service Settings to gateway Settings shape."""

                def __init__(self, s):
                    self._s = s
                    embed = s.embeddings
                    self.knowledge = type(
                        "K",
                        (),
                        {
                            "enabled": True,
                            "qdrant": type(
                                "Q",
                                (),
                                {
                                    "enabled": True,
                                    "url": s.qdrant.url,
                                    "api_key": s.qdrant.api_key,
                                    "timeout_seconds": s.qdrant.timeout_seconds,
                                    "interactive_deadline_seconds": (
                                        s.qdrant_interactive_deadline_seconds
                                    ),
                                    "prefer_grpc": s.qdrant.prefer_grpc,
                                    "max_retries": getattr(s.qdrant, "max_retries", 3),
                                    "retry_base_delay": getattr(s.qdrant, "retry_base_delay", 1.0),
                                    "bm25_v2_enabled": getattr(s.qdrant, "bm25_v2_enabled", False),
                                    "bm25_v2_capability_ttl_seconds": getattr(
                                        s.qdrant, "bm25_v2_capability_ttl_seconds", 300.0
                                    ),
                                    "bm25_v2_readiness_ttl_seconds": getattr(
                                        s.qdrant, "bm25_v2_readiness_ttl_seconds", 5.0
                                    ),
                                    "bm25_v2_cutover_test_tenants": getattr(
                                        s.qdrant, "bm25_v2_cutover_test_tenants", ""
                                    ),
                                },
                            )(),
                            "dashscope": type(
                                "D",
                                (),
                                {
                                    "api_key": embed.dashscope_api_key
                                    or (embed.api_key if embed.provider == "dashscope" else ""),
                                    "model_name": embed.model
                                    if embed.provider == "dashscope"
                                    else "",
                                },
                            )(),
                            "gemini": type(
                                "G",
                                (),
                                {
                                    "api_key": embed.google_api_key
                                    or (embed.api_key if embed.provider == "gemini" else ""),
                                    "model_name": embed.model if embed.provider == "gemini" else "",
                                },
                            )(),
                            "siliconflow": type(
                                "SF",
                                (),
                                {
                                    "api_key": embed.siliconflow_api_key
                                    or (embed.api_key if embed.provider == "siliconflow" else ""),
                                    "base_url": embed.siliconflow_base_url or embed.base_url or "",
                                    "model_name": embed.model
                                    if embed.provider == "siliconflow"
                                    else "",
                                },
                            )(),
                            "ocr_enabled": s.ocr.enabled,
                            "ocr_strategy": s.ocr.strategy,
                            "worker_concurrency": s.processing.worker_concurrency,
                            "document_worker_concurrency": s.processing.document_worker_concurrency,
                            "retrieval_query_max_concurrency": s.processing.retrieval_query_max_concurrency,
                            "retrieval_cache_ttl_seconds": s.processing.retrieval_cache_ttl_seconds,
                            # Embedding config
                            "text_embedding_dimension": embed.dimension,
                            "text_embedding_batch_size": embed.batch_size,
                            "text_embedding_max_concurrent": embed.max_concurrent,
                            "text_embedding_config": {
                                "provider": embed.provider,
                                "model": embed.model,
                                "api_key": embed.api_key,
                                "dimension": embed.dimension,
                                "base_url": embed.base_url or "",
                            },
                            "default_embedding_model": embed.model,
                            "default_embedding_provider": embed.provider,
                            # Multimodal (optional)
                            "multimodal_embedding_model": getattr(
                                s, "multimodal", type("M", (), {"model": ""})()
                            ).model,
                            "multimodal_embedding_max_concurrent": 5,
                            # VLM
                            "vlm_max_concurrent": getattr(s.ocr, "vlm_concurrency", 4),
                        },
                    )()

                def __getattr__(self, name):
                    return getattr(self._s, name)

            compat_settings = _SettingsCompat(resolved)
            logger.info(
                "knowledge_retrieval_config "
                f"query_concurrency={compat_settings.knowledge.retrieval_query_max_concurrency} "
                f"cache_ttl={compat_settings.knowledge.retrieval_cache_ttl_seconds}s"
            )

            # Initialize S3 ImageStorageService for file persistence
            image_storage = None
            try:
                from ai_gateway_core.storage.image_storage import (
                    ImageStorageService,
                    StorageBackend,
                    StorageConfig,
                )

                storage_cfg = resolved.storage
                if storage_cfg.backend == "s3" and storage_cfg.s3.bucket:
                    sc = StorageConfig(
                        backend=StorageBackend.S3,
                        s3_bucket=storage_cfg.s3.bucket,
                        s3_region=storage_cfg.s3.region,
                        s3_access_key=storage_cfg.s3.access_key,
                        s3_secret_key=storage_cfg.s3.secret_key,
                        s3_endpoint_url=storage_cfg.s3.endpoint_url or None,
                        key_prefix=storage_cfg.key_prefix,
                        url_expiry_seconds=storage_cfg.url_expiry_seconds,
                    )
                    image_storage = ImageStorageService(
                        sc,
                        signing_key=storage_cfg.signing_key.get_secret_value(),
                    )
                    logger.info("s3_storage_initialized", bucket=storage_cfg.s3.bucket)
                else:
                    sc = StorageConfig(
                        backend=StorageBackend.LOCAL,
                        local_base_path=storage_cfg.local_base_path,
                        key_prefix=storage_cfg.key_prefix,
                    )
                    image_storage = ImageStorageService(
                        sc,
                        signing_key=storage_cfg.signing_key.get_secret_value(),
                    )
                    logger.info("local_storage_initialized", path=storage_cfg.local_base_path)
            except Exception as e:
                logger.warning("storage_init_failed", error=str(e))

            knowledge_service = KnowledgeService(
                settings=compat_settings,
                database=db_storage,
                image_storage_service=image_storage,
                tenant_embedding_credential_resolver=TenantEmbeddingCredentialResolver(
                    db_storage,
                    encryption_key=get_settings().gateway_encryption_key,
                ),
            )
            app.state.knowledge_service = knowledge_service

            # --- Initialize VLM OCR Service (if configured) ---
            ocr = resolved.ocr
            if ocr.enabled and ocr.strategy in ("vlm", "hybrid"):
                try:
                    from ai_gateway_core.config.endpoints import resolve_dashscope

                    from .services.knowledge.vlm_ocr_service import VLMOCRService

                    ocr_provider = str(ocr.vlm_provider or "dashscope").strip().lower()
                    ocr_model = str(ocr.vlm_model or "qwen-vl-ocr").strip()
                    if ocr_provider == "auto":
                        if ocr_model.startswith("gemini-"):
                            ocr_provider = "gemini"
                        elif "deepseek" in ocr_model.lower():
                            ocr_provider = "siliconflow"
                        else:
                            ocr_provider = "dashscope"
                    if ocr_provider == "siliconflow" or "deepseek" in ocr_model.lower():
                        keys = [k.strip() for k in ocr.vlm_api_keys.split(",") if k.strip()]
                        if keys:
                            vlm_ocr_service = VLMOCRService(
                                api_keys=keys,
                                model=ocr_model or "deepseek-ai/DeepSeek-OCR",
                                provider="siliconflow",
                                base_url=ocr.vlm_base_url,
                                max_retries=3,
                            )
                    else:
                        ocr_api_keys = [
                            key.strip()
                            for key in str(ocr.vlm_api_keys or "").split(",")
                            if key.strip()
                        ]
                        if ocr_provider == "gemini":
                            ocr_api_key = (
                                resolved.gemini_api_key
                                or resolved.google_api_key
                                or resolved.embeddings.google_api_key
                                or (
                                    resolved.embeddings.api_key
                                    if resolved.embeddings.provider == "gemini"
                                    else ""
                                )
                            )
                            ocr_base_url = ocr.vlm_base_url
                        else:
                            dashscope_key, dashscope_base_url = resolve_dashscope("ocr")
                            ocr_api_key = (
                                ocr_api_keys[0]
                                if ocr_api_keys
                                else dashscope_key
                                or resolved.embeddings.dashscope_api_key
                                or (
                                    resolved.embeddings.api_key
                                    if resolved.embeddings.provider == "dashscope"
                                    else ""
                                )
                            )
                            ocr_base_url = ocr.vlm_base_url or dashscope_base_url
                        if ocr_api_key:
                            vlm_ocr_service = VLMOCRService(
                                api_key=ocr_api_key,
                                api_keys=ocr_api_keys or None,
                                model=ocr_model,
                                provider=ocr_provider,
                                base_url=ocr_base_url,
                                concurrency=ocr.vlm_concurrency,
                                timeout_seconds=ocr.vlm_timeout_seconds,
                                task=ocr.vlm_task,
                                min_pixels=ocr.vlm_min_pixels,
                                max_pixels=ocr.vlm_max_pixels,
                                max_tokens=ocr.vlm_max_tokens,
                                enable_rotate=ocr.vlm_enable_rotate,
                            )
                    if vlm_ocr_service:
                        logger.info(
                            "vlm_ocr_initialized",
                            provider=vlm_ocr_service.provider,
                            model=vlm_ocr_service.model,
                        )
                except Exception as e:
                    logger.warning("vlm_ocr_init_failed", error=str(e))

            # Initialize document type detector for auto-mode processing
            doc_detector = None
            try:
                from .services.knowledge.document_detector import DocumentTypeDetector

                doc_detector = DocumentTypeDetector()
                logger.info("document_detector_initialized")
            except Exception as e:
                logger.warning("document_detector_init_failed", error=str(e))

            # PRD T9 item 1: wire the hierarchical indexer into the default
            # worker. Every construction site used to omit it, so the L1/L2/L3
            # branches in worker.py were dead code. Embedding provider/model/
            # dimension are dataset-level facts, so the indexer resolves its
            # embedder per dataset through the embedding manager instead of a
            # startup-time global. Grayscale stays on the worker side: the
            # hierarchical plane only runs for datasets whose chunking config
            # explicitly selects mode="hierarchical".
            hierarchical_indexer = None
            try:
                from .services.knowledge.hierarchical_indexer import HierarchicalIndexer

                async def _resolve_hierarchical_embedder(dataset_id: str):
                    dataset = await db_storage.get_dataset(dataset_id)
                    if not dataset:
                        raise ValueError(
                            f"hierarchical indexing: dataset {dataset_id} not found"
                        )
                    return await knowledge_service.embedding_manager.get_text_embedder(dataset)

                hierarchical_indexer = HierarchicalIndexer(
                    vector_store=knowledge_service.vector_store,
                    database=db_storage,
                    embedding_resolver=_resolve_hierarchical_embedder,
                    knowledge_settings=compat_settings,
                )
                logger.info("hierarchical_indexer_initialized")
            except Exception as e:
                logger.warning("hierarchical_indexer_init_failed", error=str(e))

            if resolved.runtime_role in {"all", "worker"}:
                knowledge_worker = KnowledgeWorker(
                    knowledge_service,
                    detector=doc_detector,
                    vlm_ocr_service=vlm_ocr_service,
                    hierarchical_indexer=hierarchical_indexer,
                )
                await knowledge_worker.start()
                from .services.knowledge.embedding_migration_worker import (
                    EmbeddingMigrationJobWorker,
                )

                embedding_migration_worker = EmbeddingMigrationJobWorker(
                    knowledge_service
                )
                await embedding_migration_worker.start()
            else:
                knowledge_worker = DurableEnqueueProxy(knowledge_service)
            # Lifecycle restore is orchestrated inside DocumentService, while
            # normal create/reprocess routes receive the producer through
            # FastAPI dependency injection. Keep both paths on the exact same
            # durable producer; otherwise API-role deployments can ingest new
            # documents but falsely reject enable/unarchive as "no worker".
            knowledge_service._worker = knowledge_worker
            app.state.knowledge_worker = knowledge_worker
            app.state.embedding_migration_worker = embedding_migration_worker
            from .services.knowledge.confluence.manual_sync import build_manual_sync

            confluence_manual_sync = build_manual_sync(
                settings=compat_settings, database=db_storage,
                knowledge_service=knowledge_service, knowledge_worker=knowledge_worker,
                image_storage_service=image_storage,
            )
            app.state.confluence_sync_service = confluence_manual_sync

            logger.info(
                "knowledge_service_initialized",
                runtime_role=resolved.runtime_role,
                worker_running=bool(getattr(knowledge_worker, "_running", False)),
            )
        except Exception as e:
            app.state._ready = False
            logger.exception("knowledge_service_init_failed", error=str(e))

            # Lifespan cleanup after a pre-yield failure is not automatic.  Release
            # every core resource that may already have started, but never mask the
            # startup exception that must keep this instance out of rotation.
            if confluence_manual_sync:
                with suppress(Exception):
                    await confluence_manual_sync.close()
            if knowledge_worker:
                try:
                    await knowledge_worker.stop()
                except Exception as cleanup_error:
                    logger.warning(
                        "knowledge_worker_startup_cleanup_failed",
                        error=str(cleanup_error),
                    )
            if embedding_migration_worker:
                try:
                    await embedding_migration_worker.stop()
                except Exception as cleanup_error:
                    logger.warning(
                        "embedding_migration_worker_startup_cleanup_failed",
                        error=str(cleanup_error),
                    )
            if vlm_ocr_service:
                try:
                    await vlm_ocr_service.close()
                except Exception as cleanup_error:
                    logger.warning(
                        "vlm_ocr_startup_cleanup_failed",
                        error=str(cleanup_error),
                    )
            if knowledge_service:
                try:
                    await knowledge_service.close()
                except Exception as cleanup_error:
                    logger.warning(
                        "knowledge_service_startup_cleanup_failed",
                        error=str(cleanup_error),
                    )
            if db_storage:
                try:
                    await db_storage.close()
                except Exception as cleanup_error:
                    logger.warning(
                        "knowledge_database_startup_cleanup_failed",
                        error=str(cleanup_error),
                    )
            if qdrant is not None and hasattr(qdrant, "close"):
                try:
                    await qdrant.close()
                except Exception as cleanup_error:
                    logger.warning(
                        "qdrant_startup_cleanup_failed",
                        error=str(cleanup_error),
                    )
            if idempotency_redis_client is not None:
                with suppress(Exception):
                    await _close_idempotency_redis(idempotency_redis_client)
            raise

        app.state._ready = True
        logger.info("knowledge_service_ready")
        yield

        # --- shutdown ---
        app.state._ready = False
        logger.info("knowledge_service_shutting_down")
        # Wait for in-flight requests (retrieval, QA, document upload) to
        # finish before stopping the worker / closing pools. ``DrainMiddleware``
        # has already started rejecting fresh traffic with 503.
        if not await DRAIN.wait_drained(timeout=30.0):
            logger.warning(
                "drain_timeout",
                inflight=DRAIN.inflight,
                hint="forcing shutdown with requests still in flight",
            )
        if confluence_manual_sync:
            await confluence_manual_sync.close()
        if knowledge_worker:
            with suppress(Exception):
                stop_worker = getattr(knowledge_worker, "stop", None)
                if callable(stop_worker):
                    await stop_worker()
        if embedding_migration_worker:
            with suppress(Exception):
                await embedding_migration_worker.stop()
        if vlm_ocr_service:
            with suppress(Exception):
                await vlm_ocr_service.close()
        if knowledge_service:
            with suppress(Exception):
                await knowledge_service.close()
        if qdrant is not None and hasattr(qdrant, "close"):
            await qdrant.close()
        if db_storage:
            await db_storage.close()
        if idempotency_redis_client is not None:
            await _close_idempotency_redis(idempotency_redis_client)
        logger.info("knowledge_service_stopped")

    app = FastAPI(
        title="Knowledge Base Service",
        version="1.0.0",
        description=(
            "Independent knowledge base microservice for document ingestion, vector indexing, and RAG retrieval.\n\n"
            "**Capabilities:**\n"
            "- Document upload (PDF, DOCX, TXT, HTML, images) with auto-chunking and embedding\n"
            "- Hybrid search: dense vector + BM25 keyword, with RRF fusion and optional reranking\n"
            "- Multi-tenant dataset isolation with RBAC permissions\n"
            "- Document versioning, batch operations, and background worker queue\n\n"
            "**Stack:** PostgreSQL + Qdrant (vector DB) + Gemini/DashScope embeddings"
        ),
        openapi_tags=OPENAPI_TAGS,
        default_response_class=ORJSONResponse,
        lifespan=lifespan,
        contact={"name": "AI Gateway Maintainers", "email": "maintainers@example.com"},
        license_info={"name": "MIT"},
    )

    from .services.knowledge.bm25_v2_lifecycle import Bm25V2LifecycleError

    @app.exception_handler(Bm25V2LifecycleError)
    async def bm25_v2_lifecycle_error_handler(
        _request: Request,
        exc: Bm25V2LifecycleError,
    ) -> ORJSONResponse:
        return ORJSONResponse(
            status_code=exc.http_status,
            content={"detail": {"code": exc.code, "message": str(exc)}},
        )

    # --- CORS ---
    _origins = resolved.cors.allow_origins
    _credentials = "*" not in _origins
    if not _credentials:
        logger.warning(
            "cors_wildcard_with_credentials_disabled",
            hint="Set KNOWLEDGE_CORS__ALLOW_ORIGINS to explicit origins",
        )
    app.add_middleware(
        CORSMiddleware,
        allow_origins=_origins,
        allow_credentials=_credentials,
        allow_methods=["*"],
        allow_headers=["*"],
    )

    # OTel inbound server-span middleware. Placed AFTER CORS, BEFORE
    # DrainMiddleware in source order so its execution wraps the inner
    # stack (Drain → RequestID → routes) but is wrapped by CORS. The
    # span sees ``request.state.request_id`` because RequestIDMiddleware
    # sits inside it (added after, executes first).
    from ai_gateway_core.tracing import OTelInboundMiddleware

    app.add_middleware(OTelInboundMiddleware)

    # Graceful drain — placed alongside ``RequestIDMiddleware`` (immediately
    # below) for symmetry with the Gateway. ``DrainMiddleware`` excludes
    # ``/health*`` + ``/metrics`` so probes still answer during drain (the LB
    # uses readiness flips to stop routing traffic). Starlette stacks
    # last-added → outermost; this position keeps drain checks inside the
    # CORS handler so OPTIONS preflight stays cheap.
    from ai_gateway_core.proxy import DrainMiddleware

    app.add_middleware(DrainMiddleware)

    # X-Request-Id middleware — bind incoming gateway request_id to
    # request.state + REQUEST_ID_CTX contextvar so log lines can include it.
    from ai_gateway_core.proxy import RequestIDMiddleware

    app.add_middleware(RequestIDMiddleware)

    from .api.connection_idempotency import ConnectionIdempotencyMiddleware as IdempotencyMiddleware

    app.add_middleware(
        IdempotencyMiddleware,
        store=idempotency_store,
        ttl_seconds=resolved.internal_idempotency_ttl_seconds,
    )

    # --- Phase K5c: Gateway HMAC verification (closes Polaris #6-KB) ---
    # Reject requests that didn't pass through the gateway signer. Sibling
    # containers on the Docker bridge network previously could craft
    # ``X-User-Id``/``X-Tenant-Id`` headers and impersonate users by calling
    # knowledge-service directly. With this middleware enabled (and
    # ``allow_anonymous=false``) knowledge-service refuses such traffic.
    #
    # Use the platform-wide internal token as the HMAC key for this private
    # hop. The old Assistant-specific alias was removed with that service.
    _internal_env = get_settings()
    _gateway_secret_env = _internal_env.ai_platform_internal_token.strip()
    from ai_gateway_core.auth.gateway_secret_middleware import (
        validate_gateway_auth_configuration,
    )

    validate_gateway_auth_configuration(
        secret=_gateway_secret_env,
        allow_anonymous=resolved.app.allow_anonymous,
        allow_anonymous_setting="KNOWLEDGE_APP__ALLOW_ANONYMOUS",
        environment=_internal_env.environment,
    )
    if _gateway_secret_env:
        from ai_gateway_contracts.replay import InMemoryReplayStore
        from ai_gateway_core.auth.gateway_secret import (
            GatewaySecret,
            RedisReplayStore,
        )

        from .auth import GatewaySecretAuthMiddleware

        auth_version = _internal_env.internal_auth_version.strip().lower()
        if auth_version != "v2":
            raise RuntimeError("INTERNAL_AUTH_VERSION must be v2 for private service traffic")
        environment = _internal_env.environment.strip().lower()
        replay_backend = _internal_env.internal_comm_state_backend.strip().lower()
        if (
            environment not in {"local", "dev", "development", "test", "testing"}
            and replay_backend != "redis"
        ):
            raise RuntimeError(
                "INTERNAL_COMM_STATE_BACKEND must be redis outside local development/test"
            )
        replay_store = InMemoryReplayStore()
        if replay_backend == "redis":
            redis_url = _internal_env.internal_comm_redis_url.strip()
            if not redis_url:
                if not os.environ.get("PYTEST_CURRENT_TEST") and environment not in {
                    "local",
                    "dev",
                    "development",
                    "test",
                    "testing",
                }:
                    raise RuntimeError("Redis replay protection is configured without a URL")
            else:
                replay_store = RedisReplayStore.from_url(redis_url)

        app.add_middleware(
            GatewaySecretAuthMiddleware,
            gateway_secret=GatewaySecret(
                secret=_gateway_secret_env,
                version=auth_version,
                replay_store=replay_store,
                caller_service="gateway",
                audience="knowledge-service",
                allowed_path_prefixes=("/api/v1",),
            ),
            allow_anonymous=resolved.app.allow_anonymous,
            separately_authenticated_paths=frozenset({"/version"}),
            separately_authenticated_prefixes=frozenset({"/internal/v2/capabilities/"}),
        )
        logger.info(
            "gateway_secret_middleware_active",
            allow_anonymous=resolved.app.allow_anonymous,
        )
    elif not resolved.app.allow_anonymous:
        # Fail hard. ``get_user_context`` trusts X-User-* headers
        # verbatim when present; no middleware + no anonymous means
        # a sibling container can impersonate any user. Refuse to
        # start in that configuration.
        raise RuntimeError(
            "AI_PLATFORM_INTERNAL_TOKEN is unset AND "
            "KNOWLEDGE_APP__ALLOW_ANONYMOUS=false. This combination is a "
            "security hole: get_user_context trusts X-User-* headers with "
            "no HMAC check, so any sibling container can impersonate any "
            "user. Either set the secret (production) or enable "
            "allow_anonymous=true (dev only)."
        )

    # --- Health ---
    from .api.version import register_version_route

    # Workers expose probes only; their release identity comes from OCI labels.
    if resolved.runtime_role != "worker":
        register_version_route(app, "knowledge-service")

    @app.middleware("http")
    async def security_headers(request, call_next):
        response = await call_next(request)
        response.headers["X-Content-Type-Options"] = "nosniff"
        response.headers["X-Frame-Options"] = "DENY"
        response.headers["Strict-Transport-Security"] = "max-age=31536000; includeSubDomains"
        return response

    @app.get("/health", tags=["Health"])
    async def health(request: Request) -> dict[str, str]:
        return {"status": "ok", "service": "knowledge-service"}

    @app.get("/health/live", tags=["Health"])
    async def health_live() -> dict[str, str]:
        return {"status": "alive", "service": "knowledge-service"}

    @app.get("/health/ready", tags=["Health"])
    async def health_ready(request: Request):
        from ai_gateway_core.proxy.drain import DRAIN

        snapshot = await _knowledge_readiness_snapshot(
            request.app,
            runtime_role=resolved.runtime_role,
            draining=DRAIN.draining,
            timeout_seconds=1.0,
        )
        return ORJSONResponse(
            status_code=200 if snapshot["core_ready"] is True else 503,
            content=_public_knowledge_readiness(snapshot),
        )

    # --- Metrics (PRD §7 Phase 0; drained /health-style path, no auth) ---
    # DrainMiddleware already excludes "/metrics" so scrapes keep working
    # while the instance stops accepting traffic. The scrape must never
    # fail the request path: queue-depth refresh is best-effort.
    @app.get("/metrics", tags=["Health"])
    async def metrics(request: Request):
        from fastapi.responses import Response
        from prometheus_client import CONTENT_TYPE_LATEST, generate_latest

        from .core.observability import metrics as kb_metrics

        db = getattr(request.app.state, "db", None)
        if db is not None:
            with suppress(Exception):
                kb_metrics.set_ingestion_queue_depth(
                    await db.count_queued_documents()
                )
        return Response(
            content=generate_latest(),
            media_type=CONTENT_TYPE_LATEST,
        )

    # --- API routes ---
    # These routers are part of the service's required API surface.  Import
    # failures must abort application construction so the process cannot
    # become ready while serving only a reduced placeholder API.
    if resolved.runtime_role != "worker":
        from .api.route_registry import include_required_api_routes

        include_required_api_routes(app)
        logger.info("knowledge_routes_loaded", mode="full", endpoints=64)
    else:
        logger.info("knowledge_routes_loaded", mode="worker", endpoints=0)

    app.state.settings = resolved
    return app


app = create_app()


def main() -> None:
    """CLI entry-point (``knowledge-service`` console script)."""
    settings = Settings()
    configure_logging(settings.app.log_level)
    uvicorn.run(
        "knowledge_service.main:app",
        host=settings.app.host,
        port=settings.app.port,
        reload=False,
    )


if __name__ == "__main__":
    main()
