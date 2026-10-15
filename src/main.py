"""
AI Service Gateway - Application Entry Point

Unified AI service gateway with multi-protocol adapters, rate limiting,
circuit breaker, and session management.
"""

from __future__ import annotations

import asyncio
import os

# Load environment variables from .env file BEFORE importing other modules
# This ensures env vars are available for module-level configurations
from pathlib import Path

from dotenv import load_dotenv

# Load .env from project root (one level up from src/)
_env_file = Path(__file__).parent.parent / ".env"
load_dotenv(_env_file, override=False)

from fastapi import Depends, FastAPI, Request
from fastapi.middleware.cors import CORSMiddleware
from fastapi.responses import JSONResponse, PlainTextResponse

from .api.deps import AuthContext, get_auth_context, require_gateway_capability
from .adapters.registry import auto_register_builtin_adapters
from .core.auth.permissions import Capability
from .api.router import api_router
from .config.settings import Settings
from .container import Container, create_container
from .core.errors import setup_exception_handlers
from .core.openapi import stable_openapi_operation_id

# 兼容旧的异常导入（向后兼容）
from ai_gateway_core.exceptions import (
    AuthError,
    CircuitBreakerOpenError,
    GatewayError,
    PermissionDeniedError,
    RateLimitExceededError,
    ServiceNotFoundError,
)
from .core.file_cleanup import get_cleanup_service

# 使用流式友好的纯 ASGI 中间件（替换 BaseHTTPMiddleware）
from .core.middleware.streaming import (
    RequestContextBridgeMiddleware,
    SecurityHeadersMiddleware,
    StreamingAnonymousConfig,
    StreamingAnonymousMiddleware,
    StreamingAuthConfig,
    StreamingAuthMiddleware,
    StreamingLogConfig,
    StreamingLoggingMiddleware,
    StreamingRateLimitMiddleware,
    StreamingTracingConfig,
    StreamingTracingMiddleware,
)
from .core.middleware.request_body_limit import RequestBodyLimitMiddleware
from ai_gateway_core.logging import configure_structured_logging, get_logger
from .core.observability.metrics import get_metrics
from ai_gateway_core.comm.client import configure_service_metrics
from .services.metrics.collector import get_service_metrics
from .services.metrics.metrics_recorder import init_metrics_recorder
from .services.metrics.realtime_metrics import init_realtime_metrics
from .services.health_contract import (
    gateway_readiness_snapshot as _gateway_readiness_snapshot,
    public_gateway_readiness as _public_gateway_readiness,
)

logger = get_logger(__name__)

OPENAPI_TAGS = [
    {
        "name": "Health",
        "description": "Service health checks, readiness state, and provider connectivity.",
    },
    {
        "name": "Auth",
        "description": (
            "User authentication — login, logout, password management, token validation."
        ),
    },
    {
        "name": "Sessions",
        "description": (
            "Conversation session management — CRUD, message history, per-user isolation."
        ),
    },
    {
        "name": "LangGraph",
        "description": (
            "LangGraph Platform proxy — assistants, threads, runs (streaming/sync), "
            "and key-value store."
        ),
    },
    {
        "name": "Knowledge",
        "description": (
            "Knowledge base proxy — dataset management, document upload, and RAG retrieval."
        ),
    },
    {
        "name": "Quiz",
        "description": "Quiz share and grading shims. Generation is the in-chat generate_quiz tool.",
    },
    {
        "name": "Dashboard",
        "description": "Real-time metrics, usage timeseries, and operational dashboard data.",
    },
]


def create_app() -> FastAPI:
    """
    创建 FastAPI 应用

    使用依赖注入容器管理所有组件。
    """
    # 加载配置
    settings = Settings()

    # 配置结构化日志
    # PR-3: env-driven format. Prod (ENVIRONMENT=production) → JSON so
    # request_id / trace_id stamping flows to log aggregators; dev →
    # human-readable "simple". Override either with LOG_FORMAT.
    _log_format = os.environ.get("LOG_FORMAT")
    if not _log_format:
        _log_format = (
            "json" if os.environ.get("ENVIRONMENT", "").lower() == "production" else "simple"
        )
    configure_structured_logging(
        level="INFO",
        format_type=_log_format,
        service="ai-gateway",
    )

    # 创建依赖注入容器
    container = create_container(settings)

    # 自动注册内置适配器
    auto_register_builtin_adapters()

    # 创建 FastAPI 应用
    app = FastAPI(
        title="AI Service Gateway",
        version="2.0.0",
        description=(
            "Unified AI service gateway with multi-protocol adapters, rate limiting, "
            "circuit breaking, and session management.\n\n"
            "**Core features:**\n"
            "- JWT authentication with RBAC and multi-tenant isolation\n"
            "- LangGraph Platform proxy with load balancing and streaming SSE\n"
            "- Knowledge base management with hybrid RAG retrieval\n"
            "- AI quiz/exam system with scoring and analytics\n"
            "- Real-time usage metrics and billing\n\n"
            "**Auth:** All endpoints (except `/health` and `/api/v1/auth/login`) "
            "require `Authorization: Bearer <token>`."
        ),
        openapi_tags=OPENAPI_TAGS,
        generate_unique_id_function=stable_openapi_operation_id,
        contact={"name": "AI Gateway Maintainers", "email": "maintainers@example.com"},
        license_info={"name": "MIT"},
    )
    # The Gateway-owned Confluence broker is opt-in at runtime.  Startup
    # enables it only after the database and connector authority are ready.
    # Grant and catalog credential backends still fail independently closed.
    app.state.confluence_capability_enabled = False
    app.state.mcp_secret_resolver = None

    # ========== 中间件配置 ==========
    # 注意：中间件执行顺序与添加顺序相反（后添加的先执行）
    # 执行顺序：Tracing -> CORS -> RequestLogging -> Auth -> RateLimit -> AnonymousIdentity
    #
    # 重要：使用纯 ASGI 中间件替换 BaseHTTPMiddleware，避免缓冲 StreamingResponse
    # 这是解决首 token 延迟问题的关键

    # Stable anonymous identity for guest users (cookie/header) - 纯 ASGI
    from ai_gateway_core.proxy.version_middleware import APIVersionMiddleware

    app.add_middleware(APIVersionMiddleware)

    # Stable anonymous identity for guest users (cookie/header) - 纯 ASGI
    anon_config = StreamingAnonymousConfig(
        enabled=getattr(settings.anonymous, "enabled", True),
        header_name=getattr(settings.anonymous, "header_name", "X-AG-Anonymous-Id"),
        cookie_name=getattr(settings.anonymous, "cookie_name", "ag_anon_id"),
        ttl_days=getattr(settings.anonymous, "ttl_days", 365),
        same_site=getattr(settings.anonymous, "same_site", "lax"),
    )
    app.add_middleware(StreamingAnonymousMiddleware, config=anon_config)

    # HTTP 级别限流中间件 - 纯 ASGI
    # Note: Limits should be generous enough for frontend usage patterns
    # (multiple concurrent API calls on page load)
    # SPO-02: the middleware is the single authoritative counter for
    # global/user/guest/ip/tenant; the route-level multi-dimension limiter
    # skips those dimensions (deps.enforce_rate_limit).
    from .core.gateway.multi_dimension_rate_limiter import create_rate_limit_config
    from .core.middleware._streaming.rate_limit import (
        streaming_rate_limit_config_from_policy,
    )

    md_rate_config = create_rate_limit_config()
    rate_limit_config = streaming_rate_limit_config_from_policy(
        md_rate_config,
        whitelist_paths=[
            "/health",
            "/health/live",
            "/health/ready",
            "/docs",
            "/openapi.json",
            "/version",
            # Frontend metadata endpoints (high frequency, low cost)
            "/api/v1/services",
            "/api/v1/assistant/config",
            "/api/v1/assistant/models",
            "/api/v1/assistant/datasets",
            # Private Runtime path performs its own constant-time service auth
            # and lease/budget admission; public rate buckets must not count it.
            "/internal/v1/agent-model-plane/responses",
        ],
    )
    app.add_middleware(StreamingRateLimitMiddleware, config=rate_limit_config)

    app.add_middleware(
        RequestBodyLimitMiddleware,
        max_bytes=8 * 1024 * 1024,
        paths={
            "/api/v1/assistant/chat",
            "/api/v1/assistant/chat/stream",
            "/internal/v1/agent-model-plane/responses",
        },
        path_prefixes=("/api/v1/public/agents/",),
    )

    # 统一鉴权中间件（支持 JWT、API Key、游客会话）- 纯 ASGI
    auth_config = StreamingAuthConfig(
        jwt_enabled=settings.authentication.jwt.enabled
        if hasattr(settings, "authentication")
        else False,
        jwt_secret=settings.authentication.jwt.secret
        if hasattr(settings, "authentication")
        else "",
        jwt_algorithms=settings.authentication.jwt.algorithms
        if hasattr(settings, "authentication")
        else ["HS256"],
        jwt_audience=getattr(settings.authentication.jwt, "audience", None)
        if hasattr(settings, "authentication")
        else None,
        jwt_issuer=getattr(settings.authentication.jwt, "issuer", None)
        if hasattr(settings, "authentication")
        else None,
        api_key_enabled=settings.authentication.api_key.enabled
        if hasattr(settings, "authentication")
        else False,
        api_keys=settings.authentication.api_key.keys
        if hasattr(settings, "authentication")
        else [],
        guest_session_enabled=settings.authentication.guest_session_enabled
        if hasattr(settings, "authentication")
        else True,
        anonymous_enabled=True,
        whitelist_paths=[
            "/health",
            "/health/live",
            "/health/ready",
            "/docs",
            "/openapi.json",
            "/version",
            # This exact path is not public: the route performs independent
            # service-token and signed Runtime lease verification.
            "/internal/v1/agent-model-plane/responses",
        ],
    )
    app.add_middleware(StreamingAuthMiddleware, config=auth_config)
    app.add_middleware(RequestContextBridgeMiddleware)
    # 请求日志中间件 - 纯 ASGI
    request_log_config = StreamingLogConfig(
        enabled=True,
        log_request_body=False,
        log_response_body=False,
        exclude_paths=["/health", "/health/live", "/health/ready", "/metrics"],
    )
    app.add_middleware(StreamingLoggingMiddleware, config=request_log_config)
    # CORS 中间件（Starlette 内置，已经是纯 ASGI）
    cors = getattr(settings, "cors", None)
    allow_origins = cors.allow_origins if cors else ["*"]
    allow_credentials = cors.allow_credentials if cors else True
    if allow_credentials and "*" in allow_origins:
        logger.warning(
            "CORS allow_origins includes '*' while allow_credentials is enabled; "
            "disabling credentials to avoid unsafe wildcard usage."
        )
        allow_credentials = False
    app.add_middleware(
        CORSMiddleware,
        allow_origins=allow_origins,
        allow_credentials=allow_credentials,
        allow_methods=cors.allow_methods if cors else ["*"],
        allow_headers=cors.allow_headers if cors else ["*"],
        expose_headers=cors.expose_headers if cors else None,
        max_age=cors.max_age if cors else 600,
        allow_origin_regex=cors.allow_origin_regex if cors else None,
    )

    # Graceful drain — flips a process-singleton flag on SIGTERM/SIGINT
    # (handlers installed in the startup event below) so new requests are
    # rejected with 503 + Retry-After while in-flight requests get to finish.
    # The shutdown event awaits ``DRAIN.wait_drained`` before tearing down
    # container singletons (DB pool, Redis, billing interceptor). Health-probe
    # paths bypass DRAIN middleware so probes answer; readiness itself reports
    # core unavailable while draining so the load balancer removes this pod.
    #
    # The Knowledge Service uses the same signal/drain contract.
    from ai_gateway_core.proxy import DrainMiddleware
    from ai_gateway_core.proxy.drain import DRAIN

    app.add_middleware(DrainMiddleware)

    # OpenTelemetry inbound server-span middleware. Sits AFTER CORS (added
    # below; remember Starlette executes last-added FIRST, so CORS still
    # wraps OTel). Reads W3C ``traceparent`` from inbound headers, opens
    # a server span, and exposes the active context on ``request.state``
    # so downstream internal service calls can forward it intact.
    # ``init_tracing`` is called in the startup handler below so the
    # middleware's tracer reference is valid by request time.
    from ai_gateway_core.tracing import OTelInboundMiddleware

    app.add_middleware(OTelInboundMiddleware)

    # Security response headers — pure ASGI so SSE is never wrapped in call_next.
    app.add_middleware(SecurityHeadersMiddleware)

    # 追踪中间件 - 纯 ASGI
    tracing_config = StreamingTracingConfig(
        service_name="gateway",
        log_requests=True,
        log_responses=True,
        exclude_paths={"/health", "/health/live", "/health/ready", "/metrics"},
    )
    app.add_middleware(StreamingTracingMiddleware, config=tracing_config)

    # ========== 存储组件引用到 app.state（向后兼容）==========

    # 注意：这些属性会在 startup 事件中设置，因为需要先初始化容器
    app.state.settings = settings
    app.state.container = container

    # ========== 路由 ==========

    from .api.version import router as version_router

    app.include_router(version_router)
    app.include_router(api_router, prefix="/api/v1")
    from .api.v1.local_nodes import router as local_node_router
    from .api.v1.agent_local_nodes import router as internal_local_node_router

    # The public Local Node compatibility alias is backed by the same
    # Gateway-owned control plane; no Assistant implementation is mounted.
    app.include_router(local_node_router, prefix="/api/v1")
    app.include_router(local_node_router, prefix="/api/v1/assistant")
    app.include_router(internal_local_node_router)
    # Image generation is Gateway-owned.  Mount this router exactly once,
    # outside the compatibility Assistant router, so every public image path
    # has one implementation and one OpenAPI operation.
    from .api.v1.agent_images import router as agent_images_router

    app.include_router(agent_images_router, prefix="/api/v1")
    # Native Agent Thread/Turn/Item API. V1 remains the compatibility surface
    # for one complete release window; V2 is the native Agent Runtime contract.
    from .api.v2.agent import router as agent_v2_router

    app.include_router(agent_v2_router, prefix="/api/v2")
    from .api.internal.agent_model_plane import router as agent_model_plane_router
    from .api.internal.agent_capabilities import router as agent_capabilities_router
    from .api.internal.confluence_capabilities import router as confluence_capabilities_router
    from .api.internal.image_capabilities import router as image_capabilities_router
    from .api.internal.office_artifacts import router as office_artifacts_router
    from .api.internal_mcp_broker import router as internal_mcp_broker_router

    app.include_router(agent_model_plane_router)
    app.include_router(agent_capabilities_router)
    from .api.internal.attachment_capabilities import router as attachment_capabilities_router
    from .api.internal.python_code_capabilities import router as python_code_capabilities_router

    app.include_router(attachment_capabilities_router)
    app.include_router(python_code_capabilities_router)
    app.include_router(confluence_capabilities_router)
    app.include_router(image_capabilities_router)
    app.include_router(office_artifacts_router)
    app.include_router(internal_mcp_broker_router)
    from .api.v1.agent_public import document_router as agent_embed_document_router

    app.include_router(agent_embed_document_router)
    # Compatibility alias for agents expecting `/v1/...`
    from .api.v1.knowledge import router as knowledge_router

    app.include_router(knowledge_router, prefix="/v1")
    from .api.v1.responses import router as responses_router

    app.include_router(responses_router, prefix="/v1")

    # ========== Scalar API 文档（现代化 UI + 可调用）==========
    try:
        from scalar_fastapi import Layout, get_scalar_api_reference
    except ModuleNotFoundError:
        Layout = None
        get_scalar_api_reference = None

    @app.get("/scalar", include_in_schema=False)
    async def scalar_html():
        if get_scalar_api_reference is None or Layout is None:
            return PlainTextResponse(
                "Scalar UI is unavailable because scalar-fastapi is not installed. "
                "Use /docs or install project dependencies.",
                status_code=503,
            )
        return get_scalar_api_reference(
            openapi_url=app.openapi_url,
            title=f"{app.title} — API Reference",
            layout=Layout.MODERN,
            dark_mode=True,
        )

    # ========== 健康检查和指标端点 ==========

    @app.get("/health", tags=["Health"])
    async def health_check():
        """健康检查端点"""
        return {"status": "healthy", "version": "2.0.0"}

    @app.get("/health/live", tags=["Health"])
    async def liveness_check():
        """存活检查端点（K8s liveness probe）"""
        return {"status": "alive"}

    @app.get("/health/ready", tags=["Health"])
    async def readiness_check():
        """Public core-readiness only; dependency detail is admin-authorized."""

        snapshot = await _gateway_readiness_snapshot(
            app,
            settings,
            container,
            draining=DRAIN.draining,
        )
        payload = _public_gateway_readiness(snapshot)
        status_code = 200 if snapshot["core_ready"] is True else 503
        return JSONResponse(
            status_code=status_code,
            content=payload,
        )

    async def refresh_gateway_health() -> dict[str, object]:
        return await _gateway_readiness_snapshot(
            app,
            settings,
            container,
            draining=DRAIN.draining,
        )

    # Existing authenticated /api/v1/health/services endpoints call this
    # seam. Public probes never expose the returned dependency map.
    app.state.gateway_health_probe = refresh_gateway_health

    @app.get("/metrics", tags=["Observability"])
    async def metrics(
        request: Request,
        auth: AuthContext = Depends(get_auth_context),
    ):
        """Prometheus 指标端点"""
        require_gateway_capability(request, auth, Capability.GATEWAY_METRICS_READ)
        metrics_collector = get_metrics()
        return PlainTextResponse(
            content=metrics_collector.to_prometheus(),
            media_type="text/plain",
        )

    # ========== 生命周期事件 ==========

    @app.on_event("startup")
    async def startup():
        """应用启动"""
        configure_service_metrics(get_service_metrics)
        logger.info("正在启动 AI Gateway...")

        # OpenTelemetry SDK bootstrap — must run BEFORE the DB/Redis init
        # below so AsyncPGInstrumentor can patch asyncpg before any pool
        # is created. Idempotent: calling init_tracing twice is a no-op.
        # OTLP endpoint resolved from OTEL_EXPORTER_OTLP_ENDPOINT env;
        # unset → no-op exporter (spans recorded in-process, dropped).
        from ai_gateway_core.tracing import init_tracing

        init_tracing("gateway")

        # Graceful drain — install SIGTERM/SIGINT handlers so the orchestrator's
        # "please stop" signal flips DRAIN. The matching DrainMiddleware was
        # registered above (just after CORSMiddleware); the shutdown handler
        # below awaits ``DRAIN.wait_drained`` before tearing down DB/Redis pools
        # so in-flight chat streams + tool calls get to finish.
        import asyncio as _asyncio_drain
        from ai_gateway_core.proxy.drain import install_signal_handlers

        install_signal_handlers(_asyncio_drain.get_running_loop())

        # 初始化容器（连接数据库、Redis 等）
        await container.initialize()

        # 设置 app.state 属性（向后兼容）
        _setup_app_state(app, container)

        # 从数据库加载服务
        await _load_services_from_database(container, settings)

        # 启动后台任务
        await container.health_monitor.start()
        await container.task_worker.start(settings.task_worker_concurrency)

        # 启动计费拦截器（如果启用）
        if settings.proxy.enabled and settings.proxy.billing_enabled:
            billing_interceptor = container.billing_interceptor
            if billing_interceptor:
                await billing_interceptor.start()
                logger.info("计费拦截器已启动")

        # ========== 初始化存储服务 ==========
        # 存储服务独立于知识库服务，用于文档生成、图片生成、代码执行等功能
        # 即使没有启用知识库，也需要存储服务来保存生成的文件
        image_storage_service = None
        storage_config = None
        try:
            from ai_gateway_core.storage.image_storage import (
                ImageStorageService,
                StorageBackend,
                StorageConfig,
            )

            # 确定存储后端：优先使用配置的，否则使用本地存储
            storage_backend = StorageBackend.LOCAL
            if hasattr(settings, "storage"):
                backend_str = getattr(settings.storage, "backend", "local")
                try:
                    storage_backend = StorageBackend(backend_str)
                except ValueError:
                    storage_backend = StorageBackend.LOCAL

            storage_config = StorageConfig(
                backend=storage_backend,
                s3_bucket=getattr(getattr(settings, "storage", None), "s3", None)
                and getattr(settings.storage.s3, "bucket", "")
                or "",
                s3_region=getattr(getattr(settings, "storage", None), "s3", None)
                and getattr(settings.storage.s3, "region", "")
                or "us-east-1",
                s3_access_key=getattr(getattr(settings, "storage", None), "s3", None)
                and getattr(settings.storage.s3, "access_key", "")
                or "",
                s3_secret_key=getattr(getattr(settings, "storage", None), "s3", None)
                and getattr(settings.storage.s3, "secret_key", "")
                or "",
                s3_endpoint_url=getattr(getattr(settings, "storage", None), "s3", None)
                and getattr(settings.storage.s3, "endpoint_url", None)
                or None,
                oss_bucket=getattr(getattr(settings, "storage", None), "oss", None)
                and getattr(settings.storage.oss, "bucket", "")
                or "",
                oss_endpoint=getattr(getattr(settings, "storage", None), "oss", None)
                and getattr(settings.storage.oss, "endpoint", "")
                or "",
                oss_access_key=getattr(getattr(settings, "storage", None), "oss", None)
                and getattr(settings.storage.oss, "access_key", "")
                or "",
                oss_secret_key=getattr(getattr(settings, "storage", None), "oss", None)
                and getattr(settings.storage.oss, "secret_key", "")
                or "",
                local_base_path=getattr(getattr(settings, "storage", None), "local_base_path", None)
                or "./data/artifacts",
                url_expiry_seconds=getattr(
                    getattr(settings, "storage", None), "url_expiry_seconds", None
                )
                or 3600,
                key_prefix=getattr(getattr(settings, "storage", None), "key_prefix", None) or "",
            )
            # Get signing key for local file URLs (security)
            signing_key = (
                getattr(getattr(settings, "image_signing", None), "encryption_key", "") or ""
            )
            image_storage_service = ImageStorageService(storage_config, signing_key=signing_key)
            app.state.image_storage_service = image_storage_service
            url_signing = "enabled" if signing_key else "disabled"
            logger.info(
                f"图片存储服务已初始化 (backend={storage_backend.value}, url_signing={url_signing})"
            )

            # Initialize artifact storage service (for document/image generation, code execution)
            from ai_gateway_core.storage import init_artifact_storage

            artifact_storage = init_artifact_storage(storage_config, container.database)
            app.state.artifact_storage = artifact_storage
            logger.info(f"Artifact 存储服务已初始化 (backend={storage_backend.value})")

            # Initialize file storage service for user uploads
            from ai_gateway_core.storage import init_file_storage

            file_storage = init_file_storage(storage_config)
            app.state.file_storage = file_storage
            logger.info(f"文件存储服务已初始化 (backend={storage_backend.value})")
        except Exception as e:
            logger.warning(f"存储服务初始化失败: {e}")

        from ai_gateway_core.media import ImageGenerationProvider

        app.state.image_generation_service = ImageGenerationProvider()

        from types import SimpleNamespace

        from .services.images.service import ImageGenerationService
        from .services.images.worker import ImageTaskWorker, decode_image_task

        image_worker_service = ImageGenerationService(SimpleNamespace(app=app), None)
        app.state.image_task_worker = ImageTaskWorker(
            image_worker_service,
            decode_image_task,
        )
        await app.state.image_task_worker.start()

        # ========== Knowledge Base ==========
        # KB now runs as an independent microservice on :8092. The gateway
        # only holds a thin HTTP client (`KBProxyClient`, initialised below).
        # All chunking / embedding / vector-store
        # / worker / sync logic lives in `apps/knowledge-service/`.
        #
        # The pre-K5b in-process initialisation (KnowledgeService, KnowledgeWorker,
        # VisionPDFProcessor, HierarchicalIndexer, VLMOCRService …) was gated
        # behind `if False:` and was removed in Phase K5b — see
        # plans/kb-fork-merge-report.md for the deletion rationale.
        logger.info(
            "Knowledge Base runs as a microservice (:8092); gateway uses KBProxyClient only."
        )

        # 启动文件清理服务
        file_cleanup_service = get_cleanup_service()
        await file_cleanup_service.start()
        app.state.file_cleanup_service = file_cleanup_service

        # 启动使用量调度器（聚合任务、配额重置）
        if settings.database.enabled:
            from .services.billing import init_usage_scheduler

            usage_scheduler = init_usage_scheduler(
                container.database,
                retention_days=30,  # 详细记录保留30天
                aggregation_hour=0,
                aggregation_minute=30,
                cleanup_hour=1,
                cleanup_minute=0,
                quota_check_interval=3600,  # 每小时检查配额
            )
            await usage_scheduler.start()
            app.state.usage_scheduler = usage_scheduler
            logger.info("使用量调度器已启动")

            from .services.eval import (
                init_eval_outbox_worker,
                init_trace_retention_scheduler,
            )

            eval_outbox_worker = init_eval_outbox_worker(container.database, admission_controller=container.admission_controller, capacity_resolver=container.capacity_resolver)
            if eval_outbox_worker is not None:
                await eval_outbox_worker.start(concurrency=2)
                app.state.eval_outbox_worker = eval_outbox_worker
                logger.info("Eval outbox worker 已启动")

            trace_retention_scheduler = init_trace_retention_scheduler(container.database)
            if trace_retention_scheduler is not None:
                await trace_retention_scheduler.start()
                app.state.trace_retention_scheduler = trace_retention_scheduler
                logger.info("Agent trace retention scheduler 已启动")

            # Recover terminal Runtime turns whose in-process SSE trace writer
            # was lost with an earlier Gateway process. This reads durable
            # rows only and never starts or resumes a Runtime turn.
            from .services.eval.assistant_trace_reconciler import run_assistant_trace_reconciler

            app.state.assistant_trace_reconciler_task = asyncio.create_task(
                run_assistant_trace_reconciler(
                    container.database,
                    retention_days=(
                        trace_retention_scheduler.retention_days
                        if trace_retention_scheduler is not None else 90
                    ),
                )
            )

            # 启动使用量记录器后台任务
            from .services.metrics import get_usage_recorder

            usage_recorder = get_usage_recorder()
            if usage_recorder:
                await usage_recorder.start()
                app.state.usage_recorder = usage_recorder
                logger.info("使用量记录器已启动")

        # Initialize the Gateway-owned Knowledge proxy client.
        from ai_gateway_core.knowledge import KBProxyClient

        app.state.kb_proxy = KBProxyClient()

        # Initialize the Gateway control plane used by the Agent Runtime.
        await _init_agent_control_plane(app, settings)

        # Initialize Assistant TaskManager lifecycle explicitly
        from ai_gateway_core.tasks import init_task_manager

        app.state.assistant_task_manager = await init_task_manager()

        # ── Phase 6: usage event-bus consumer (opt-in via EVENT_BUS_REDIS_URL) ──
        # When the recorder is dual-writing events (commit 6f1b974), this
        # consumer reads them off ``events:usage:recorded:v1`` and logs.
        # Today the body is log-only — observable proof that the bus path
        # works in prod. The next iteration replaces the handler with a
        # real downstream sink (analytics warehouse, secondary aggregator,
        # quota service) without changing any other plumbing.
        bus_url = os.environ.get("EVENT_BUS_REDIS_URL", "").strip()
        if bus_url:
            try:
                from ai_gateway_contracts.event_envelope import UsageRecordedV1
                from ai_gateway_core.events.consumer import EventConsumer
                from ai_gateway_core.events.registry import get_stream

                async def _log_usage_event(envelope) -> None:
                    p = envelope.payload
                    logger.info(
                        "usage_event tenant=%s user=%s model=%s tokens=%d/%d "
                        "lat=%dms status=%s req=%s event_id=%s",
                        envelope.tenant_id,
                        getattr(p, "user_id", ""),
                        getattr(p, "model", ""),
                        getattr(p, "input_tokens", 0),
                        getattr(p, "output_tokens", 0),
                        getattr(p, "latency_ms", 0),
                        getattr(p, "status", ""),
                        envelope.request_id,
                        envelope.event_id,
                    )

                _consumer = EventConsumer(
                    redis_url=bus_url,
                    stream=get_stream(UsageRecordedV1.EVENT_TYPE),
                    group="gateway-usage-logger",
                    consumer_name=f"gateway-{os.getpid()}",
                    handler=_log_usage_event,
                )
                _consumer_task = asyncio.create_task(_consumer.start())
                app.state.usage_event_consumer = _consumer
                app.state.usage_event_consumer_task = _consumer_task
                logger.info("usage event consumer started → %s", bus_url.rsplit("@", 1)[-1])
            except Exception as exc:  # noqa: BLE001
                logger.warning("usage event consumer skipped: %s: %s", type(exc).__name__, exc)
                app.state.usage_event_consumer = None

        # 打印启动信息
        _print_startup_info(settings)

    @app.on_event("shutdown")
    async def shutdown():
        """应用关闭"""
        logger.info("正在关闭 AI Gateway...")

        # Wait for in-flight requests (chat streams, tool calls, KB proxy hops)
        # to finish before tearing down container singletons. ``DrainMiddleware``
        # is already 503-ing fresh traffic; this bound stops a hung handler from
        # blocking container exit indefinitely.
        from ai_gateway_core.proxy.drain import DRAIN

        if not await DRAIN.wait_drained(timeout=30.0):
            logger.warning(f"drain timeout — {DRAIN.inflight} request(s) still in flight")

        # Phase K5c: KB worker + Confluence scheduler no longer run in the
        # gateway process — they're owned by knowledge-service. Nothing to
        # stop here; the attributes are never set.

        # Phase 6: stop the usage event consumer (if running). The consumer
        # is started lazily when EVENT_BUS_REDIS_URL is set; we need to
        # signal stop + await its background task so the redis client closes
        # cleanly and we don't leak the asyncio task on container exit.
        _consumer = getattr(app.state, "usage_event_consumer", None)
        _consumer_task = getattr(app.state, "usage_event_consumer_task", None)
        if _consumer is not None:
            try:
                await _consumer.stop()
            except Exception:
                logger.exception("usage event consumer stop() failed")
        if _consumer_task is not None and not _consumer_task.done():
            try:
                await asyncio.wait_for(_consumer_task, timeout=10.0)
            except (asyncio.TimeoutError, Exception):
                _consumer_task.cancel()

        # Stop file cleanup service
        file_cleanup_service = getattr(app.state, "file_cleanup_service", None)
        if file_cleanup_service is not None:
            await file_cleanup_service.stop()

        eval_outbox_worker = getattr(app.state, "eval_outbox_worker", None)
        if eval_outbox_worker is not None:
            await eval_outbox_worker.stop()

        trace_retention_scheduler = getattr(app.state, "trace_retention_scheduler", None)
        if trace_retention_scheduler is not None:
            await trace_retention_scheduler.stop()

        assistant_trace_reconciler_task = getattr(app.state, "assistant_trace_reconciler_task", None)
        if assistant_trace_reconciler_task is not None:
            assistant_trace_reconciler_task.cancel()
            await asyncio.gather(assistant_trace_reconciler_task, return_exceptions=True)

        # Stop usage scheduler
        usage_scheduler = getattr(app.state, "usage_scheduler", None)
        if usage_scheduler is not None:
            await usage_scheduler.stop()

        # Stop billing interceptor BEFORE usage_recorder — it routes records
        # through the recorder, so inflight flushes must drain first.
        # Phase 0 hotfix: without this, fire-and-forget _flush_buffer tasks
        # spawned by StreamProcessor get GC'd mid-write on shutdown.
        billing_interceptor = getattr(app.state, "billing_interceptor", None)
        if billing_interceptor is not None:
            try:
                await billing_interceptor.stop()
                logger.info("billing_interceptor stopped cleanly")
            except Exception:
                logger.exception("billing_interceptor.stop() failed")

        # Stop usage recorder (flush remaining records)
        usage_recorder = getattr(app.state, "usage_recorder", None)
        if usage_recorder is not None:
            await usage_recorder.stop()

        image_task_worker = getattr(app.state, "image_task_worker", None)
        if image_task_worker is not None:
            await image_task_worker.shutdown()

        agent_model_plane = getattr(app.state, "agent_model_plane", None)
        if agent_model_plane is not None:
            await agent_model_plane.close()
        mcp_gateway_broker = getattr(app.state, "mcp_gateway_broker", None)
        if mcp_gateway_broker is not None:
            await mcp_gateway_broker.close()
        image_generation_service = getattr(app.state, "image_generation_service", None)
        if image_generation_service is not None:
            await image_generation_service.close()
        agent_runtime_control = getattr(app.state, "agent_runtime_control", None)
        if agent_runtime_control is not None:
            await agent_runtime_control.close()
        agent_capability_catalog_service = getattr(
            app.state,
            "agent_capability_catalog_service",
            None,
        )
        if agent_capability_catalog_service is not None:
            await agent_capability_catalog_service.close()
        agent_knowledge_resolver = getattr(
            app.state,
            "agent_runtime_knowledge_resolver",
            None,
        )
        if agent_knowledge_resolver is not None:
            await agent_knowledge_resolver.close()

        # Stop Assistant TaskManager lifecycle
        from ai_gateway_core.tasks import shutdown_task_manager

        await shutdown_task_manager()

        # Close file storage service
        file_storage = getattr(app.state, "file_storage", None)
        if file_storage is not None:
            await file_storage.close()

        await container.shutdown()
        logger.info("AI Gateway 已关闭")

    # ========== 异常处理 ==========

    # 设置新的异常处理器
    setup_exception_handlers(app, debug=False, include_trace_id=True)

    # 向后兼容：处理旧的 GatewayError 异常
    @app.exception_handler(GatewayError)
    async def handle_legacy_gateway_error(request: Request, exc: GatewayError):
        """处理旧版 GatewayError（向后兼容）"""
        status = 400
        if isinstance(exc, AuthError):
            status = 401
        elif isinstance(exc, PermissionDeniedError):
            status = 403
        elif isinstance(exc, RateLimitExceededError):
            status = 429
        elif isinstance(exc, ServiceNotFoundError):
            status = 404
        elif isinstance(exc, CircuitBreakerOpenError):
            status = 503

        return JSONResponse(
            status_code=status,
            content={"error": str(exc)},
        )

    return app


def _setup_app_state(app: FastAPI, container: Container) -> None:
    """
    设置 app.state 属性

    为向后兼容，将容器中的组件暴露到 app.state。
    """
    app.state.registry = container.service_registry
    app.state.load_balancer = container.load_balancer
    app.state.dispatcher = container.dispatcher
    app.state.task_manager = container.task_manager
    app.state.session_manager = container.session_manager
    app.state.health_monitor = container.health_monitor
    app.state.task_worker = container.task_worker
    app.state.database = container.database
    app.state.redis = container.redis
    app.state.memory_service = container.memory_service

    # Gateway-owned Local Node authority.  Public reads remain available when
    # PostgreSQL is ready; without a real device adapter the internal action
    # route deliberately returns 503 instead of claiming dispatch success.
    local_node_pool = getattr(container.database, "_pool", None)
    if local_node_pool is not None:
        from .services.local_node import (
            build_local_node_control_plane,
            build_local_node_execution_repository,
        )

        app.state.local_node_control_plane = build_local_node_control_plane(local_node_pool)
        app.state.local_node_execution_repository = build_local_node_execution_repository(
            local_node_pool
        )
    else:
        app.state.local_node_control_plane = None
        app.state.local_node_execution_repository = None
    app.state.local_node_device_adapter = None
    app.state.local_node_channel_verifier = None
    app.state.local_node_device_channel_verifier = None

    from .services.assistant_runtime_assignment import (
        AssistantRuntimeAssignmentStore,
        RuntimeAssignmentPolicy,
        runtime_assignment_policy_from_env,
    )

    runtime_owner, kernel_revision = runtime_assignment_policy_from_env()
    app.state.assistant_runtime_default_owner = runtime_owner
    app.state.assistant_runtime_kernel_revision = kernel_revision
    app.state.assistant_runtime_assignment_policy = RuntimeAssignmentPolicy.from_env()
    app.state.assistant_runtime_assignments = (
        AssistantRuntimeAssignmentStore(container.database)
        if container.settings.database.enabled
        else None
    )

    # Gateway owns tenant MCP CRUD, discovery, credentials and remote protocol
    # clients. The Rust Runtime receives only authorized tool descriptors and
    # invokes this broker; no MCP credential crosses that boundary.
    from ai_gateway_core.persistence.repositories.mcp_repository import (
        DatabaseMCPAgentCapabilityResolver,
        DatabaseMCPRepository,
    )
    from .services.knowledge_authz import KnowledgeServiceAgentKnowledgeResolver
    from ai_gateway_core.skills import DatabaseSkillArtifactRepository
    from .api.internal.confluence_capabilities import ConfiguredEnvironmentSecretResolver

    mcp_enabled = os.getenv("AGENT_STUDIO_MCP_ENABLED", "true").strip().lower() in {
        "1",
        "true",
        "yes",
        "on",
    }
    app.state.agent_studio_mcp_enabled = mcp_enabled
    app.state.mcp_repository = DatabaseMCPRepository(container.database)
    confluence_secret_resolver = ConfiguredEnvironmentSecretResolver.from_env()
    app.state.mcp_secret_resolver = (
        confluence_secret_resolver
        if (
            container.settings.database.enabled
            and mcp_enabled
            and confluence_secret_resolver.ready
        )
        else None
    )
    app.state.confluence_capability_enabled = container.settings.database.enabled and mcp_enabled
    if container.settings.database.enabled and mcp_enabled:
        from .services.agent_runtime.mcp_gateway_broker import MCPGatewayBroker

        app.state.mcp_gateway_broker = MCPGatewayBroker(
            repository=app.state.mcp_repository,
            secret_resolver=confluence_secret_resolver,
            ttl_seconds=float(os.getenv("ASSISTANT_MCP_CLIENT_CACHE_TTL_SECONDS", "60")),
            max_entries=int(os.getenv("ASSISTANT_MCP_CLIENT_CACHE_MAX_ENTRIES", "100")),
        )
        app.state.mcp_discovery_service = app.state.mcp_gateway_broker
    else:
        app.state.mcp_gateway_broker = None
        app.state.mcp_discovery_service = None
    app.state.skill_artifact_repository = DatabaseSkillArtifactRepository(container.database)
    app.state.agent_runtime_capability_resolver = DatabaseMCPAgentCapabilityResolver(
        app.state.mcp_repository,
        mcp_enabled=mcp_enabled,
        skill_repository=app.state.skill_artifact_repository,
    )
    # Dataset ACL authority lives in knowledge-service (PRD T8.2): the gateway
    # never reads KB tables to authorize a run.
    app.state.agent_runtime_knowledge_resolver = KnowledgeServiceAgentKnowledgeResolver()

    # LangGraph 相关
    app.state.langgraph_proxy = container.langgraph_proxy
    app.state.multi_rate_limiter = container.multi_rate_limiter
    app.state.user_resolver = container.user_resolver

    # 透明代理相关
    app.state.transparent_proxy = container.transparent_proxy
    app.state.proxy_config_loader = container.proxy_config_loader
    app.state.billing_interceptor = container.billing_interceptor
    app.state.context_injector = container.context_injector

    # Knowledge Base (KBMS) — microservice mode
    app.state.knowledge_service = None
    app.state.knowledge_worker = None
    # kb_proxy already initialized before assistant service

    # 游客会话管理器
    from .services.session.guest_session_manager import GuestSessionConfig, GuestSessionManager

    app.state.guest_session_manager = GuestSessionManager(
        config=GuestSessionConfig(),
        redis_client=container.redis,
    )

    # Initialize metrics recorder with Redis for dashboard
    init_metrics_recorder(
        container.redis,
        latency_sample_cap=container.settings.metrics.latency_sample_cap,
    )

    # Initialize realtime metrics service for LangSmith-style dashboard
    init_realtime_metrics(container.redis)

    # Initialize usage recorder for persistent usage metrics
    from .services.metrics import init_usage_recorder

    init_usage_recorder(container.database)

    # Initialize security event recorder for auth/rate limit aggregates
    from .services.metrics import init_security_event_recorder

    init_security_event_recorder(container.database)

    # Initialize billing services
    from .services.billing import init_pricing_service, init_quota_service

    init_quota_service(container.database)
    init_pricing_service(container.database)

    # Initialize LLM provider and model services
    from .services.llm import ModelService, ProviderService

    encryption_key = os.environ.get("GATEWAY_ENCRYPTION_KEY", "")
    app.state.provider_service = ProviderService(container.database, encryption_key)
    app.state.model_service = ModelService(container.database)

    from ai_gateway_contracts.agent_runtime_lease import RuntimeModelLeaseSigner

    from .services.agent_runtime import AgentModelPlane, AgentRuntimeControlPlane
    from .services.agent_runtime.capability_catalog import (
        CapabilityCatalogService,
        LocalCapabilityCatalogClient,
    )

    model_plane_token = os.environ.get(
        "AI_PLATFORM_AGENT_RUNTIME_MODEL_PLANE_INTERNAL_TOKEN", ""
    ).strip()
    lease_secret = os.environ.get("AI_PLATFORM_CAPABILITY_LEASE_SIGNING_SECRET", "").strip()
    app.state.agent_model_plane_internal_token = model_plane_token
    from .services.assistant_entry.source_access import runtime_source_access_checker
    app.state.agent_model_plane = (
        AgentModelPlane(
            admission_controller=container.admission_controller,
            capacity_resolver=container.capacity_resolver,
            database=container.database,
            provider_service=app.state.provider_service,
            lease_signer=RuntimeModelLeaseSigner(lease_secret),
            source_access_checker=runtime_source_access_checker(app),
        )
        if container.settings.database.enabled and model_plane_token and lease_secret
        else None
    )
    runtime_internal_token = os.environ.get("AI_PLATFORM_INTERNAL_TOKEN", "").strip()
    runtime_url = os.environ.get(
        "AI_PLATFORM_AGENT_RUNTIME_URL", "http://agent-runtime:8094"
    ).strip()
    capability_worker_url = os.environ.get(
        "AI_PLATFORM_CAPABILITY_WORKER_URL", "http://agent-capability-worker:8095"
    ).strip()
    app.state.agent_capability_catalog_service = (
        CapabilityCatalogService(
            database=container.database,
            worker_url=capability_worker_url,
            internal_token=runtime_internal_token,
            web_search_configured=bool(os.environ.get("TAVILY_API_KEY", "").strip()),
        )
        if container.settings.database.enabled
        else None
    )
    capability_catalog_client = (
        LocalCapabilityCatalogClient(app.state.agent_capability_catalog_service)
        if app.state.agent_capability_catalog_service is not None
        else None
    )
    model_plane_runtime_base_url = os.environ.get(
        "AI_PLATFORM_AGENT_RUNTIME_MODEL_PLANE_RUNTIME_BASE_URL",
        "http://gateway:8080/internal/v1/agent-model-plane",
    ).strip()
    app.state.agent_runtime_control = (
        AgentRuntimeControlPlane(
            admission_controller=container.admission_controller,
            capacity_resolver=container.capacity_resolver,
            database=container.database,
            model_service=app.state.model_service,
            provider_service=app.state.provider_service,
            assignment_store=app.state.assistant_runtime_assignments,
            lease_signer=RuntimeModelLeaseSigner(lease_secret),
            runtime_url=runtime_url,
            runtime_internal_token=runtime_internal_token,
            model_plane_base_url=model_plane_runtime_base_url,
            kernel_revision=str(app.state.assistant_runtime_kernel_revision or ""),
            memory_service=app.state.memory_service,
            capability_catalog_client=capability_catalog_client,
        )
        if (
            container.settings.database.enabled
            and app.state.assistant_runtime_assignments is not None
            and runtime_internal_token
            and runtime_url
            and model_plane_runtime_base_url
            and app.state.assistant_runtime_kernel_revision
            and lease_secret
        )
        else None
    )

    from .adapters.langgraph import LangGraphAdapter

    LangGraphAdapter.configure_model_control_plane(
        app.state.provider_service,
        app.state.model_service,
    )


async def _load_services_from_database(container: Container, settings: Settings) -> None:
    """从数据库加载服务"""
    if not settings.database.enabled:
        return

    database = container.database
    if not database._pool:
        return

    try:
        from .services.registry.database_storage import DatabaseRegistryStorage

        db_services = await database.list_services()
        registry = container.service_registry
        registry_storage = registry.storage

        loaded_count = 0
        for svc in db_services:
            try:
                if isinstance(registry_storage, DatabaseRegistryStorage):
                    service = registry_storage._dict_to_service(svc)
                    registry._cache[service.service_id] = service
                    loaded_count += 1
            except Exception as e:
                logger.warning(f"加载服务 {svc.get('service_id')} 失败: {e}")

        if loaded_count > 0:
            logger.info(f"从数据库加载了 {loaded_count} 个服务")

        # Auto-sync LangGraph service URLs from environment config
        # This ensures DB services always point to the correct upstream
        # (e.g., localhost:2024 in dev or a LangGraph service URL in Docker)
        if settings.langgraph.enabled and settings.langgraph.instance_urls:
            env_url = settings.langgraph.instance_urls[0]
            updated = 0
            for svc in db_services:
                if svc.get("service_type") == "langgraph" and svc.get("status") == "active":
                    cc = svc.get("connector_config") or {}
                    db_url = cc.get("upstream_url") or cc.get("base_url") or ""
                    if db_url and db_url != env_url:
                        new_cc = dict(cc, base_url=env_url, upstream_url=env_url)
                        await database.execute(
                            "UPDATE services SET connector_config = $1::jsonb "
                            "WHERE service_id = $2",
                            [__import__("json").dumps(new_cc), svc["service_id"]],
                        )
                        logger.info(
                            f"Auto-synced service '{svc['service_id']}' URL: {db_url} -> {env_url}"
                        )
                        updated += 1
            if updated:
                logger.info(f"Synced {updated} LangGraph service URL(s) to match environment")
    except Exception as e:
        logger.warning(f"从数据库加载服务失败: {e}")


async def _init_agent_control_plane(app: FastAPI, settings: Settings) -> None:
    """Initialize provider metadata for the Gateway-owned Agent control plane.

    从环境变量读取各 LLM 提供商的 API Key：
    - OPENAI_API_KEY
    - ANTHROPIC_API_KEY
    - DEEPSEEK_API_KEY
    - DASHSCOPE_API_KEY (或复用 knowledge.dashscope.api_key)
    - GOOGLE_API_KEY

    DashScope + Google chat routing (free/paid swap) is handled by
    ``ai_gateway_core.config.endpoints.resolve_dashscope("chat")`` and
    ``resolve_google("chat")`` — same helpers are used for image and
    embedding elsewhere, so chat / image / embedding can be flipped
    between free and paid endpoints independently.

    同时将配置同步到数据库，确保前端可以管理。
    """
    from .services.llm.startup_seeder import (
        seed_startup_providers,
        sync_startup_model_catalog,
    )

    knowledge_dashscope = getattr(getattr(settings, "knowledge", None), "dashscope", None)
    legacy_dashscope_api_key = (
        getattr(knowledge_dashscope, "api_key", "") if knowledge_dashscope else ""
    )

    # Phase 5e: gateway no longer builds an in-process ``ModelRegistry``.
    # Provider-config sync (env → DB seeding) still runs because the admin
    # UI expects to see the env-derived providers in ``llm_providers``.
    provider_service = getattr(app.state, "provider_service", None)
    seed_result = await seed_startup_providers(
        provider_service=provider_service,
        legacy_dashscope_api_key=legacy_dashscope_api_key,
        tenant_id="default",
        log=logger,
    )
    configured_providers = list(seed_result.configured_providers)
    assistant_runtime_providers = set(seed_result.runtime_configured_providers)

    model_service = getattr(app.state, "model_service", None)
    await sync_startup_model_catalog(
        provider_service=provider_service,
        model_service=model_service,
        configured_providers=configured_providers,
        tenant_id="default",
        log=logger,
    )

    # Get KB service if available
    kb_service = getattr(app.state, "knowledge_service", None)

    # Get VLM service from KB service for assistant (if KB is enabled)
    knowledge_vlm_service = getattr(kb_service, "vlm_service", None) if kb_service else None

    # Get session manager for conversation persistence
    session_manager = getattr(app.state, "session_manager", None)

    # Get Tavily API key for web search
    tavily_api_key = os.environ.get("TAVILY_API_KEY", "")

    # The Gateway owns model metadata, session state, MCP, image and capability
    # boundaries. Runtime execution is delegated only to the Rust Agent Runtime.
    _ = session_manager, tavily_api_key, knowledge_vlm_service  # kept for sibling code paths

    # Phase 5e: gateway uses a narrow ``GatewayModelMeta`` facade over
    # ModelService + ProviderService for the 3 routes that still need LLM
    # metadata (chat-stream permission check, /health/providers, model
    # CRUD). The full model authority remains in Gateway/Postgres.
    provider_service = getattr(app.state, "provider_service", None)
    if model_service and provider_service:
        from .services.llm.gateway_model_meta import GatewayModelMeta

        app.state.model_meta = GatewayModelMeta(
            model_service,
            provider_service,
            runtime_configured_providers=assistant_runtime_providers,
        )
    else:
        app.state.model_meta = None
        logger.warning(
            "GatewayModelMeta not initialised — /chat/stream permission check "
            "will be bypassed and /health/providers will return {}"
        )

    # Sync model pricing from DB; no second runtime registry is maintained.
    if model_service:
        try:
            synced = await model_service.sync_pricing_from_llm_models(
                tenant_id="default",
                include_disabled=True,
            )
            logger.info(f"Synchronized {synced} model pricing records from llm_models")
        except Exception as e:
            logger.warning(f"Failed to sync llm_models pricing to model_pricing: {e}")

    features = []
    if configured_providers:
        features.append(f"providers: {', '.join(configured_providers)}")
    if session_manager:
        features.append("session persistence")
    if kb_service:
        features.append("KB tools")
    features.append("agent-runtime + capability-worker")

    if configured_providers:
        logger.info(f"Gateway启动完成: ({', '.join(features)})")
    else:
        logger.warning("Gateway 启动，但没有配置任何 LLM 提供商 API Key")

    # Startup diagnostics — log which provider keys are present so ops can
    # tell at a glance whether the gateway can proxy-sign KB calls + which
    # providers model_registry can route to.
    dashscope_key_present = bool(os.environ.get("DASHSCOPE_API_KEY"))
    if not dashscope_key_present:
        knowledge_dashscope = getattr(getattr(settings, "knowledge", None), "dashscope", None)
        dashscope_key_present = (
            bool(getattr(knowledge_dashscope, "api_key", "")) if knowledge_dashscope else False
        )

    google_key_present = bool(os.environ.get("GEMINI_API_KEY") or os.environ.get("GOOGLE_API_KEY"))

    logger.info(
        "[Gateway Startup] role=proxy keys={google:%s,dashscope:%s}",
        google_key_present,
        dashscope_key_present,
    )


def _print_startup_info(settings: Settings) -> None:
    """打印启动信息"""
    print("\n" + "=" * 50)
    print("AI Gateway 已启动")
    print(f"   地址: http://{settings.host}:{settings.port}")
    print(f"   API: http://{settings.host}:{settings.port}/api/v1")
    print(f"   数据库: {'已启用' if settings.database.enabled else '未启用'}")
    print(f"   Redis: {'已启用' if settings.redis.enabled else '未启用'}")
    print("=" * 50 + "\n")


# 创建应用实例
app = create_app()
