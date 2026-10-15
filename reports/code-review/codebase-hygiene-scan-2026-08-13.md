# 代码库卫生扫描报告(只读全库扫描)

- **日期**: 2026-08-13
- **分支**: `codex/hermes-openclaw-parity-hardening`(基于 main 干净工作区)
- **扫描方式**: 14 个只读代理并行,每个覆盖一个模块;全部发现经全库 `rg` 交叉验证;共 1576 次工具调用
- **扫描性质**: 严格只读,未修改任何文件
- **范围**: 排除 `.venv/`、`node_modules/`、`dist/`、`__pycache__/`、`.mypy_cache/`、`.ruff_cache/` 等生成物

## 一、执行摘要

- **死代码 240 处**(含 60+ 处整文件/整包级高置信判定,预计可删除 ≥ 3 万行)
- **过时注释 57 处**、**错误注释 25 处**
- **超大文件 105 个**(Python ≥1000 行 119 个全库中报告前 105;Web ≥500 行 66 个中报告前 30+)
- **跨模块备注 117 条**(重复实现、提取半途、版本漂移等)

**最重要的十个发现**:

1. **Knowledge 提取半途而废**: `src/services/knowledge/confluence/` 栈(≈7900 行)同时存在于 gateway 与 `apps/knowledge-service`,两侧都未被运行时接线 —— gateway 副本仅被 503 桩路由 `src/api/v1/confluence.py` 导入 2 个异常类,`main.py:647-655` 的注释却声称迁移已完成(与事实矛盾)。
2. **docgen 三份拷贝**: `apps/assistant-service/.../core/docgen`(~8685 行)是 `packages/mcp-docgen-server/src/docgen`(~8513 行)的全包分叉,零生产引用,已在 runbook 中计划退役;`_skills_data` 中 docx/pptx/xlsx 各自内嵌一份 md5 相同的 `scripts/office/`(~4700 行重复),第三份变体在 assistant skills 目录。
3. **死中间件链**: `src/core/middleware/{base,auth,request_logging,validation,retry,session,circuit_breaker,concurrency,logging,rate_limit}.py`(~1800 行)被纯 ASGI 的 `streaming.py` 取代,零生产引用,仅测试依赖。
4. **死限流包**: `src/core/ratelimit/`(855 行)零生产引用;活跃限流走 `core/gateway/rate_limiter.py` 等三处。
5. **assistant-service 内部死子系统**: RAG Manus 风格分析集群(~3400 行)被构造但从不调用;`AGUIEventEmitter` 34 个公开方法中 22 个无调用方;local-node 控制面从未接线(所有 `/local-nodes` 路由永久 503);`config/settings.py` 被 `startup_fingerprint.py` 完全遮蔽。
6. **gateway-core 死仓库链**: `DatabaseStorage.repos` 字典 + 6 个 repository 模块(~2074 行) + barrel 构成完全断开的引用链;`MULTIMODAL_EMBEDDING_MODELS` 在 4 处重复定义。
7. **web 死组件群**: `pages/Dashboard.tsx → ServiceCostAnalysis/UserServiceUsageAnalytics/SecurityEventCharts → api/metrics.ts` 互死链;`Playground.legacy.tsx`(2244 行)无路由;`VITE_ASSISTANT_UI_V2` 标志的 V1 分支引用已不存在的组件;`api/*.ts` 约 30 个导出函数无调用方。
8. **四个 SDK 默认模型漂移**: 全部默认 `qwen3.6-plus`,而服务端默认 `qwen3.7-plus`(`src/api/schemas/assistant.py:139`、`chat.py:65`)—— SDK 请求静默降级模型。
9. **超大文件**: 全库最大 `apps/knowledge-service/.../persistence/database.py` 9559 行;其次 `agent_repository.py` 7540、`database.py` 7322(ai-gateway-core);web 最大 `DatasetDetail.tsx` 4854 行。
10. **基础设施杂物**: 4 套并行的 DB 迁移 runner(两套 tracking 表)+ 手写 1962 行 `schema.sql` 聚合;`apps/islamic-content-service` 为空壳;根目录遗留截图/临时脚本(均未 git 跟踪)。

| 模块 | 死代码 | 过时注释 | 错误注释 | 超大文件 | 备注 |
|---|---|---|---|---|---|
| src-core | 23 | 4 | 2 | 2 | 9 |
| src-api | 25 | 2 | 2 | 12 | 6 |
| src-services | 8 | 2 | 1 | 5 | 8 |
| asst-core-a | 26 | 3 | 1 | 12 | 8 |
| asst-core-b | 22 | 5 | 2 | 3 | 8 |
| asst-api | 8 | 3 | 0 | 10 | 6 |
| knowledge | 20 | 1 | 3 | 12 | 8 |
| gateway-core | 13 | 7 | 2 | 7 | 7 |
| docgen (packages/mcp-docgen-server) | 20 | 8 | 5 | 0 | 10 |
| web | 30 | 2 | 1 | 12 | 9 |
| sdk | 13 | 9 | 2 | 1 | 8 |
| tests-services | 6 | 2 | 1 | 12 | 10 |
| tests-rest | 2 | 7 | 1 | 12 | 9 |
| misc | 24 | 2 | 2 | 5 | 10 |
| **合计** | **240** | **57** | **25** | **105** | **116** |

## 二、扫描方法与判定标准

### 阈值(仓库无 max-lines 规范,以下为本报告的约定)

| 语言 | LARGE(应拆) | CRITICAL(必须拆) |
|---|---|---|
| Python | ≥ 1000 行 | ≥ 1500 行 |
| TS/TSX | ≥ 500 行 | ≥ 1000 行 |
| CSS/SCSS | ≥ 1000 行 | — |
| Dart/Java | ≥ 500 行 | — |

### 死代码判定协议

- 符号级: 对符号名做全库 `rg`(排除定义文件与生成物),零引用才算死;同时排查字符串/动态引用(路由装饰器、`__init__.py` re-export、`getattr`/`importlib`、`pyproject [project.scripts]` 入口、Vite 路由、`index.ts` barrel、Dockerfile/Makefile/compose 挂载)。
- 文件级: 模块名全库无 import、无入口、无挂载,才算整文件死。
- 置信度: **高** = 全库 rg 验证且无动态引用风险;**中** = 有保留条件(仅自模块测试引用、字符串引用等);**低** = 仅存疑。
- 排除项: 被明确标记为 back-compat shim 且有文档依据的不算死代码(如 `src/persistence/database.py` 18 行 Phase 5f 转发 shim);vendored 第三方内容(skills/、agent-plugins/)默认不逐行审查。

## 三、分模块发现明细

### 3.1 src-core

死代码 23 | 过时注释 4 | 错误注释 2 | 超大文件 2

#### 死代码

| 位置 | 符号 | 类型 | 证据 | 置信度 |
|---|---|---|---|---|
| `src/core/ratelimit/limiter.py:148` | UnifiedRateLimiter (module: src/core/ratelimit/{limiter,storage,strategy}.py) | 未使用类 | Entire src/core/ratelimit package (~855 lines: limiter.py 393, storage.py 187, strategy.py 275) has zero production importers — rg across src/, tests/, apps/, packages/ finds only tests/core/test_ratelimit.py importin... | 高 |
| `src/core/middleware/base.py:11` | MiddlewareChain / InvocationMiddleware / whole chain middleware package | 未使用类 | The responsibility-chain middleware package (base.py 351, auth.py 433, request_logging.py 318, validation.py 56, retry.py 128, session.py 145, logging.py 137, circuit_breaker.py 85, concurrency.py 77, rate_limit.py 72... | 高 |
| `src/config/constants.py:14` | entire file (33 constants: TIMEOUT_*, BATCH_SIZE_*, EMBEDDING_DIM_*, CACHE_SIZE_*, RATE_LIMIT_*, CHUNK_*) | 未使用文件(整文件死) | Every one of the 33 constants has zero references outside constants.py — rg across src, tests, apps, packages (BATCH_SIZE_* hits in apps/knowledge-service/vector_store.py are class-local attributes, not imports). No m... | 高 |
| `src/models/knowledge.py:11` | entire file (ProcessRuleMode, Dataset, Document, Segment, ChildChunk, DatasetPermissionBinding, ...) | 未使用文件(整文件死) | 311-line KB domain model file with zero references anywhere in src/, tests/, apps/, packages/ (verified via 'models.knowledge' and class names Dataset/ChildChunk/DatasetPermissionBinding). KB moved to apps/knowledge-s... | 高 |
| `src/models/job.py:10` | entire file (JobStatus, JobType, Job) | 未使用文件(整文件死) | 131-line file with zero importers anywhere. The JobStatus used by tests/services/assistant/test_durable_subagent_protocol_oracle.py is a separate class defined in tests/services/assistant/durable_subagent_harness.py:5... | 高 |
| `src/config/logging.py:7` | configure_logging | 未使用函数 | Zero importers of src.config.logging across src/, tests/, apps/, packages/ (matches in apps/knowledge-service are that app's own configure_logging). main.py uses configure_structured_logging from ai_gateway_core.loggi... | 高 |
| `src/core/observability/tracing.py:354` | trace_langgraph_run / trace_llm_call / record_run_completion / inject_trace_headers / trace_span | 未使用函数 | Five tracing helpers with zero references in src/, tests/, apps/ (record_token_usage hits are realtime_metrics.record_token_usage on a different class). The module's TracingMiddleware is superseded by StreamingTracing... | 高 |
| `src/core/observability/tracing.py:197` | TracingMiddleware (BaseHTTPMiddleware variant) | 未使用类 | Only referenced by tests/core/test_observability.py and the never-imported barrel src/core/observability/__init__.py:16 — nothing imports src.core.observability as a package (all prod imports go to '.observability.met... | 中 |
| `src/core/observability/metrics.py:438` | MetricsCollector.collect_all | 未使用方法 | rg 'collect_all' across src/tests/apps: zero usages (metrics endpoint uses to_prometheus, main.py:477). Also register_histogram (line 420) has zero usages outside the file — no Histogram is ever registered. | 高 |
| `src/core/middleware/streaming.py:177` | StreamingAuthConfig.anonymous_enabled | 死配置开关/字段 | Field never read: StreamingAuthMiddleware._extract_user_info falls back to anonymous unconditionally (streaming.py:343-360); the flag is only ever set by main.py:251. Dead config flag. | 高 |
| `src/core/middleware/streaming.py:179` | StreamingAuthConfig.anonymous_header | 死配置开关/字段 | Field never read — _extract_user_info hardcodes headers.get(b'x-ag-anonymous-id', ...) (streaming.py:345) and _sanitize_anon_id path ignores config.anonymous_header. Only anonymous_cookie is actually consulted. | 高 |
| `src/core/middleware/streaming.py:800` | StreamingTracingConfig.service_name | 死配置开关/字段 | Field never read anywhere in StreamingTracingMiddleware.__call__ (only default 'gateway' passed at main.py:334). Dead config field. | 高 |
| `src/core/middleware/streaming.py:615` | StreamingLogConfig.log_request_body / log_response_body | 死配置开关/字段 | Both fields never read in StreamingLoggingMiddleware.__call__ — the middleware never logs bodies regardless of config (main.py:265-266 passes False). Dead config fields. | 高 |
| `src/core/middleware/streaming.py:447` | StreamingAdmissionConfig | 未使用类 | Never instantiated in production; only referenced by tests/security/test_release_secret_regressions.py:5,37 (docstring itself says 'used by release security regressions'). Per scan rules, referenced only from its own ... | 中 |
| `src/core/auth/password.py:130` | is_password_valid | 未使用函数 | rg across src/tests/apps: zero usages. Live password checks go through validate_password_strength (imported by src/api/v1/auth.py:25) and hash_password/verify_password. | 高 |
| `src/core/auth/password.py:345` | get_lockout_end_time | 未使用函数 | rg across src/tests/apps: zero usages. should_lock_account/is_account_locked are live (api/v1/auth.py:22-23), but get_lockout_end_time has no caller. | 高 |
| `src/core/gateway/capacity.py:299` | CapacityResolver.inventory_rows | 未使用方法 | rg 'inventory_rows' across src/tests/apps: zero usages (not even internally). Other capacity helpers (normalize_capacity_group, capacity_config_from_service, provider_budget_key) are used internally or via CapacityRes... | 高 |
| `src/core/middleware/rate_limit_http.py:199` | RateLimitMiddleware (BaseHTTPMiddleware) + RateLimitConfig (line 30) | 未使用类 | streaming.py:28 imports only RateLimitInfo and SlidingWindowRateLimiter from this module; the BaseHTTPMiddleware-based RateLimitMiddleware and RateLimitConfig have zero references anywhere (the RateLimitMiddleware imp... | 高 |
| `src/persistence/storage.py:4` | FileStorage | 未使用类 | No importer of src.persistence package or .persistence.storage anywhere (prod storage usage is ai_gateway_core.storage.FileStorageService, e.g. api/v1/files.py:34). src/persistence/__init__.py barrel (which re-exports... | 高 |
| `src/persistence/repositories/__init__.py:1` | back-compat shim re-exporting repositories | 未使用文件(整文件死) | Never imported: all 17 production import sites of repositories go directly to ai_gateway_core.persistence.repositories.* (main.py:915, api/v1/agents.py:20, api/v1/mcp.py:10, ...). The shim's docstring claims 'keeps an... | 中 |
| `src/main.py:110` | _make_process_file_handler | 未使用函数 | Only referenced by tests/unit/test_code_review_fixes.py:224,243. Production removed the process_file task handler (comment at main.py:529-533); the helper is never called from main.py itself. | 中 |
| `src/main.py:1344` | _ = session_manager, tavily_api_key, knowledge_vlm_service | 死配置开关/字段 | tavily_api_key and knowledge_vlm_service are computed (main.py:1325, 1331) then only kept alive by the discarding tuple assignment on line 1344; neither is read anywhere else (session_manager IS read at line 1394). De... | 中 |
| `src/config/settings.py:631` | Settings.services_path | 死配置开关/字段 | Declared field 'services: str = "services"' with zero reads anywhere in src/, tests/, apps/ (rg 'services_path' finds only the definition). Dead setting. | 高 |

#### 过时注释

| 位置 | 注释(摘录) | 问题 | 置信度 |
|---|---|---|---|
| `src/main.py:1339` | "The gateway keeps ``app.state.model_registry`` because /chat/stream runs an edge-side model-permission check, /generate-image resolves p... | Contradicted by the same function 30 lines below: main.py:1369 sets ``app.state.model_registry = None``. The registry was removed in Phase 5e and replaced by the GatewayModelMeta facade (comment at... | 高 |
| `src/main.py:1064` | Docstring: "初始化 Assistant Service (GPT-like 体验) ... 同时将配置同步到数据库，确保前端可以管理。" | Function no longer initializes an AssistantService — since Phase 5d it only seeds providers/models into the DB and builds the GatewayModelMeta facade; app.state.assistant_service is set to None at ... | 中 |
| `src/core/middleware/streaming.py:460` | Class docstring: "对于流式路径，跳过限流检查（或使用异步检查）。对于非流式路径，执行完整的限流检查。" (streaming paths skip rate limiting) | StreamingRateLimitMiddleware.__call__ (line 474-546) overrides the base and applies the global/user/guest/ip checks to every non-whitelisted path; there is no is_streaming_path() branch — streaming... | 中 |
| `src/core/middleware/streaming.py:192` | Class docstring: "对于非流式路径，执行完整的鉴权流程。" (full authentication flow for non-streaming paths) | process_request for non-streaming paths (line 215-232) only extracts/injects user info and returns True; it never rejects unauthenticated requests. Both streaming and non-streaming paths behave ide... | 中 |

#### 错误注释

| 位置 | 注释(摘录) | 错误之处 | 置信度 |
|---|---|---|---|
| `src/main.py:188` | "# Stable anonymous identity for guest users (cookie/header) - 纯 ASGI" | Comment is a copy-paste of line 193 but sits above the APIVersionMiddleware import + app.add_middleware(APIVersionMiddleware) (lines 189-191). The comment describes the anonymous-identity middlewar... | 高 |
| `src/core/errors/codes.py:4` | Module docstring: "3000-3999: 业务逻辑错误（限流、余额不足）" (rate limiting listed under business-logic errors) | Rate-limit codes live in the client range: "限流错误 (1400-1499)" with RATE_LIMIT_EXCEEDED = 1400 (codes.py:33-38), and RateLimitError extends ClientError (exceptions.py:121). Only quota/credits errors... | 中 |

#### 超大文件

| 文件 | 行数 | 问题 | 拆分建议 |
|---|---|---|---|
| `src/main.py` | 1436 | LARGE (>=1000). God-file app factory: middleware wiring (~100 lines), health/metrics endpoints, full startup/shutdown lifecycle (~180 lines each with per-com... | Extract _init_assistant_service/provider seeding (~360 lines) into src/services/llm/provider_seeder.py; extract the storage-init block (main.py:551-629) into... |
| `src/core/middleware/streaming.py` | 1010 | LARGE (>=1000). Single file containing 5 middleware classes (StreamingAuth/RateLimit/Logging/Tracing/Anonymous), 5 config dataclasses, 3 rate-limit/streaming... | Split per middleware into src/core/middleware/streaming/{__init__,paths,auth,rate_limit,logging,tracing,anonymous}.py; keep config dataclasses beside their m... |

#### 模块备注(交叉发现)

- Largest cleanup target in module: the responsibility-chain middleware package (src/core/middleware/base|auth|request_logging|validation|retry|session|circuit_breaker|concurrency|logging|rate_limit.py, ~1800 lines) is entirely superseded by src/core/middleware/streaming.py (pure ASGI) — zero production importers; only tests. Safe to delete after confirming no external/deploy wiring.
- Second largest: src/core/ratelimit/ package (limiter.py 393 + storage.py 187 + strategy.py 275, ~855 lines) has zero production importers; live rate limiting runs through src/core/gateway/rate_limiter.py, src/core/gateway/multi_dimension_rate_limiter.py, and src/core/middleware/rate_limit_http.py. The package is only exercised by tests/core/test_ratelimit.py.
- Redis TaskQueue (src/core/tasks/queue.py) is a no-op feature: main.py:534 calls start_worker() with zero registered handlers (register_handler/enqueue have no callers anywhere), so the worker never actually starts — main.py:536 even logs 'worker active, no handlers registered'. The whole process_file task pipeline was removed in Phase 5d; consider deleting the queue bootstrap too.
- settings.py KnowledgeSettings (~175 lines of qdrant/gemini/siliconflow/OCR/hierarchical-index config, lines 308-482) and ConfluenceSettings (lines 485-518) are near-dead config: KB and Confluence moved to apps/knowledge-service. Only settings.knowledge.dashscope.api_key is read (main.py:1203-1207 fallback); ConfluenceSettings is read only by src/services/knowledge/confluence/sync_service.py, which is never instantiated (app.state.confluence_sync_service = None).
- app.state.load_balancer and app.state.memory_service are set in main.py:_setup_app_state (lines 901, 909) but never read anywhere; other back-compat state attributes (model_registry, assistant_service, tool_registry...) are set to None explicitly (main.py:1369-1377) — only load_balancer/memory_service are dead non-None values.
- src/core/observability/__init__.py barrel is never imported (all production imports target .observability.metrics directly); it re-exports the dead TracingMiddleware. Empty-barrel __init__ files in src/core/gateway/ and src/models/ are harmless.
- src/persistence/database.py, src/persistence/redis.py, src/models/session.py, and src/persistence/repositories/__init__.py are documented back-compat shims for ai_gateway_core (Phase 5f Batch C / Phase 6) — intentional, keep; new code should import from ai_gateway_core.persistence directly per their docstrings.
- Dead monolith files did not move with their services: src/models/knowledge.py and src/models/job.py are KB-era models left behind when KB moved to apps/knowledge-service (Phase K5b); the live equivalents live in apps/knowledge-service and packages/ai-gateway-core.
- Connectors (src/connectors/grpc.py, websocket.py, message_queue.py) are only reachable dynamically via create_connector() (src/connectors/base.py:83) when a DB service declares connector_type=grpc/websocket/message_queue — no service registers those types, and MessageQueueConnector is exercised only by tests; HTTPConnector is the only one used in production paths.

### 3.2 src-api

死代码 25 | 过时注释 2 | 错误注释 2 | 超大文件 12

#### 死代码

| 位置 | 符号 | 类型 | 证据 | 置信度 |
|---|---|---|---|---|
| `src/adapters/registry.py:146` | AdapterDiscovery / discover_adapters / register_adapter / get_adapter / register_adapter_class / _adapter_registry | 未使用类 | The whole plugin-registry layer (AdapterDiscovery, discover_adapters, entry-points discovery, register_adapter decorator, global _adapter_registry at lines 27-28, 67-313) is never read by production code. Only auto_re... | 高 |
| `src/api/deps.py:71` | get_knowledge_worker | 未使用函数 | Defined but never imported or called anywhere in the repo (apps/knowledge-service has its own get_knowledge_worker in its own deps module). Leftover stub from the KB-service extraction. | 高 |
| `src/api/deps.py:154` | get_guest_session_manager | 未使用函数 | Defined but zero references repo-wide (src/, tests/, apps/). Guest-session flow was removed but the dependency stub remains. | 高 |
| `src/api/deps.py:87` | get_image_storage_service | 未使用函数 | Zero references anywhere; app.state.image_storage_service is still set (src/main.py:609) but no route depends on this helper. | 高 |
| `src/api/v1/proxy.py:181` | _encode_json_body | 未使用函数 | Local underscore-prefixed copy never called; the route code calls the imported encode_json_body (src/proxy/langgraph_run_body, imported at line 70, used at 682/705). The local helper is an orphaned duplicate. | 高 |
| `src/api/v1/proxy.py:168` | _normalize_allowed_services | 未使用函数 | One-line wrapper around imported normalize_service_scope; zero callers repo-wide (checked src/, tests/, apps/). | 高 |
| `src/api/v1/assistant.py:126` | get_model_meta | 未使用函数 | Defined at line 126, never referenced anywhere (routes access request.app.state.model_meta directly, e.g. line 581). rg over src/, tests/, apps/ finds only the definition. | 高 |
| `src/api/v1/stream.py:21` | _get_timing_header | 未使用函数 | Formatting helper defined but never called in stream.py or anywhere else in the repo; timing data is logged inline without this formatter. | 高 |
| `src/proxy/context_injector.py:372` | extract_context_from_scope | 未使用方法 | Defined but zero references repo-wide (src/, tests/, apps/, web/). RequestContext extraction now happens only via build_headers path and _proxy_utils._build_request_context. | 高 |
| `src/proxy/context_injector.py:414` | extract_context_from_request | 未使用方法 | Defined but zero references repo-wide. Same dead static helper family as extract_context_from_scope. | 高 |
| `src/proxy/config_loader.py:350` | ProxyConfigLoader.set_config | 未使用方法 | Defined ('用于测试或静态配置') but no caller exists in src/ or tests/ — configs are only populated via database loading and _cache writes. | 高 |
| `src/proxy/config_loader.py:364` | ProxyConfigLoader.invalidate | 未使用方法 | Defined but never called; rg for invalidate() callers in src/proxy and src/api/v1/proxy.py finds only other classes' invalidate methods (rate_policy, tenant_policies). | 高 |
| `src/adapters/langgraph_proxy.py:144` | LangGraphLoadBalancer.add_instance / remove_instance / mark_unhealthy / mark_healthy | 未使用方法 | The load balancer is constructed in container.py:490 with instances and select_instance() is called 17x internally, but these four state-mutation methods have zero callers anywhere (src/, tests/, apps/). | 高 |
| `src/adapters/langgraph_proxy.py:313` | _make_request | 未使用方法 | Unified request helper defined but never invoked inside langgraph_proxy.py — every public method does its own inline client.request(). Only tests (tests/integration/test_permissions.py, tests/api/*) reference it. | 中 |
| `src/adapters/langgraph_proxy.py:1644` | _invalidate_assistant_cache | 未使用方法 | Defined and internally calls _async_invalidate_redis_assistant, but has no production callers — only tests/unit/test_acl_permissions.py and tests/core/test_background_task_retention.py exercise it. Cache invalidation ... | 中 |
| `src/adapters/langgraph_proxy.py:1665` | _inject_gateway_domain_policy_metadata | 未使用方法 | No production callers; only tests/adapters/test_langgraph.py:271,283 reference it. The run-body pipeline uses merge_gateway_domain_policy_metadata from langgraph_run_body instead. | 中 |
| `src/adapters/langgraph_proxy.py:1685` | _build_run_config | 未使用方法 | No production callers — LangGraphProxy creates runs via _prepare_run_payload/create_run instead. Only tests (tests/integration/test_gateway.py:479,506) call proxy._build_run_config; the tests/services/test_langgraph_m... | 中 |
| `src/proxy/transparent_proxy.py:842` | _inject_assistant_id | 未使用方法 | Never called from proxy()/_proxy_normal/_proxy_streaming; assistant_id injection now happens in the run-body pipeline (langgraph_run_body._apply_assistant_id via api/v1/proxy.py). Only tests/proxy/test_transparent_pro... | 中 |
| `src/proxy/transparent_proxy.py:922` | _ensure_stream_defaults | 未使用方法 | Never called in production; streaming defaults are handled by langgraph_run_body._apply_stream_defaults. Only tests/proxy/test_transparent_proxy.py references it. | 中 |
| `src/proxy/billing_interceptor.py:382` | replay_dead_letter | 未使用方法 | DLQ replay API has no production caller (no admin route wires it); only tests/proxy/test_billing_failure.py:289 exercises it. Dead-letter writes (_push_to_dead_letter) are live, the replay half is not. | 中 |
| `src/proxy/langgraph_run_body.py:859` | billing_request_snapshot | 未使用函数 | Only referenced by tests (test_phase2_proxy_optimizations.py, test_langgraph_run_body.py). No production callers; billing snapshotting is done inline in the interceptor. | 中 |
| `src/proxy/langgraph_run_body.py:901` | inject_domain_policy_metadata_bytes | 未使用函数 | Bytes-level variant superseded by the non-bytes inject path; only tests/api/test_proxy_domain_policy_injection.py uses it. No production callers. | 中 |
| `src/proxy/langgraph_run_body.py:919` | inject_langgraph_gateway_configurable_bytes | 未使用函数 | Only tests/api/test_gateway_langgraph_contract.py references it; production uses the payload-level _apply_gateway_configurable instead. | 中 |
| `src/api/v1/assistant.py:694` | _list_assistant_sessions | 未使用函数 | Backward-compatible listing helper dead in production — list_sessions route (line 748) calls _list_assistant_session_summaries instead. Only tests/api/test_assistant_sessions.py references it. | 中 |
| `src/api/v1/sessions.py:32` | _list_assistant_sessions_for_service_id | 未使用函数 | Only tests/api/test_sessions_assistant_compat.py uses it; no route in sessions.py calls the helper. | 中 |

#### 过时注释

| 位置 | 注释(摘录) | 问题 | 置信度 |
|---|---|---|---|
| `src/api/v1/_route_flags.py:3` | Design reference: ``plans/Roadmap-Post-5a-Extraction-2026-04-23.md`` Phase 5b §杠杆. ... Route names use screaming snake case ... MODELS → ... | The referenced design doc plans/Roadmap-Post-5a-Extraction-2026-04-23.md was deleted from the repo (git shows it was removed in 7ef8cfd 'Prepare standalone open source release'; plans/ no longer ex... | 中 |
| `src/proxy/__init__.py:8` | 提供通用的 HTTP 透明代理功能，支持：... 流式计费抽样 (module docstring feature list) | No sampling logic exists anywhere in src/proxy/: rg for 'sample' (case-insensitive) in src/proxy/ returns nothing — billing_interceptor.py records every streamed event with no sampling rate. The do... | 低 |

#### 错误注释

| 位置 | 注释(摘录) | 错误之处 | 置信度 |
|---|---|---|---|
| `src/api/v1/_route_flags.py:26` | CHAT          → /assistant/chat (non-stream; stream is permanently proxied) | Implies the non-stream /assistant/chat route is switchable via the flag mechanism while only the stream route is permanently proxied. In code both /assistant/chat (src/api/v1/assistant.py:555) and ... | 中 |
| `src/api/v1/config.py:39` | proxy_mode: str = Field("transparent", description="代理模式: transparent ／ adapter") | Presents 'adapter' as an equivalent supported mode, but the proxy stack only honors 'transparent': ProxyConfigLoader._parse_service_row (src/proxy/config_loader.py:226) only special-cases proxy_mod... | 低 |

#### 超大文件

| 文件 | 行数 | 问题 | 拆分建议 |
|---|---|---|---|
| `src/api/v1/agents.py` | 1984 | CRITICAL. Agent Studio god-file: ~40 route handlers + ~25 private helpers covering agent identity CRUD, drafts, versions, release-evaluation gate pipeline (b... | Split into src/api/v1/agents/{identity,versions,publications,members,tokens,release_eval}.py around the existing router vs publication_router split, sharing ... |
| `src/adapters/langgraph_proxy.py` | 1833 | CRITICAL. God class: LangGraphLoadBalancer + HTTP client pool + header building + ACL/ownership verification + quota checks + domain-policy injection + assis... | Extract LangGraphLoadBalancer (already a separate class at line 136) into its own module, plus per-resource client mixins (AssistantsClient, ThreadsClient, R... |
| `src/proxy/transparent_proxy.py` | 1817 | CRITICAL. TransparentProxy mixes availability refresh, capacity leases/admission, concurrency semaphores, upstream selection/LB, run-body mutation, billing s... | Extract ServiceAvailabilityManager (lines 439-544) and the retry/candidate logic (lines 1004-1134) into modules; body mutation already lives in src/proxy/lan... |
| `src/api/v1/agent_runtime.py` | 1736 | CRITICAL. Gateway-owned preview/published runtime: RedisAgentChannelLimiter Lua script, preview/version/published session creation, 3 chat-stream handlers, a... | Split RedisAgentChannelLimiter (lines ~60-160) and the attachment upload handler into dedicated modules; keep session/stream handlers in one file per channel... |
| `src/api/v1/assistant.py` | 1601 | CRITICAL. GPT-like assistant API mono-module: models/datasets/config, local-node pairing, sessions CRUD, artifacts, image generation + blob uploads, tenant/s... | Split into src/api/v1/assistant/{sessions,artifacts,images,local_nodes,metrics}.py reusing shared helpers (_request_id, _check_model_permission, proxied). |
| `src/adapters/langgraph.py` | 1500 | CRITICAL. Old LangGraphAdapter (invoke/stream/_remote_wait/_remote_stream + event extraction, ~30 methods) is a near-parallel implementation of langgraph_pro... | Consolidate: keep one LangGraph client (langgraph_proxy) and fold configure_model_control_plane into it, then delete or shrink langgraph.py. If kept for the ... |
| `src/api/v1/eval.py` | 1368 | LARGE. Eval console mixes trace ingest/query/score, datasets + examples CRUD/import/export, evaluator CRUD, experiment runs/compare/promote, KB RAGAS scoring... | Split into eval/{traces,datasets,evaluators,experiments,ragas}.py around the existing route families. |
| `src/api/v1/proxy.py` | 1209 | LARGE. Transparent-proxy routes plus the full langgraph run-body governance/quota/model-allowlist enforcement pipeline plus service discovery endpoints and s... | Move the governance-heavy handler body (lines ~620-760) into a helper module alongside _proxy_utils; keep route definitions thin. |
| `src/api/v1/confluence.py` | 1184 | LARGE. Confluence integration: credentials testing, space discovery, bindings CRUD, page sync triggers, scheduler start/stop/status — multiple sync-related c... | Split into confluence/{bindings,sync,scheduler}.py mirroring src/services/knowledge/confluence structure. |
| `src/api/v1/dashboard.py` | 1130 | LARGE. LangSmith-style dashboard: WebSocket hub, time-series queries, alerts, summary stats, per-user dashboard, metrics aggregations. | Split the WebSocket hub and the query/aggregation functions; keep only route definitions and response models in the main file. |
| `src/proxy/billing_interceptor.py` | 1067 | LARGE. BillingInterceptor (buffer/flush/DLQ/db+redis writers) plus the whole StreamProcessor SSE parser (lines 553-990, ~440 lines) plus UsageData — three la... | Extract StreamProcessor into src/proxy/stream_processor.py and UsageData into its own module; interceptor keeps orchestration only. |
| `src/api/v1/config.py` | 1006 | LARGE. System-config admin API mixing langgraph service CRUD, auth config, API keys, rate-limit rules, load-balancer config, service config read/update and s... | Split into config/{langgraph_services,auth,api_keys,rate_limits,load_balancer,status}.py. |

#### 模块备注(交叉发现)

- Duplication: src/adapters/langgraph.py (1500 lines, old LangGraphAdapter) vs src/adapters/langgraph_proxy.py (1833 lines, LangGraphProxy) are parallel LangGraph clients both importing from src/proxy/langgraph_run_body. The old adapter survives only via ServiceRegistry registration (src/container.py:299) and main.py's configure_model_control_plane; all /langgraph v1 routes and the transparent proxy use LangGraphProxy. Strong consolidation candidate.
- src/adapters/registry.py is a write-only plugin registry: auto_register_builtin_adapters() (main.py:158) populates the module-global _adapter_registry but no production code ever reads it — GatewayDispatcher uses services/registry/service_registry.py. The AdapterDiscovery/entry-point/plugin machinery (~270 lines) plus the orphaned per-route flag docs indicate aspirational infrastructure from an earlier phase.
- Route double-registration: knowledge router and responses router are included both via api_router under /api/v1 (src/api/router.py:70,81) AND directly under /v1 as compat aliases (src/main.py:356-359) — every /knowledge/* and /responses/* route is exposed twice. Worth confirming the alias surface is intentional before cleanup.
- src/api/v1/presign.py routes are deliberate 501 stubs marked deprecated=True with no in-repo callers (web uploads go through files.py) — not dead per the scan's criteria, but they are dead weight in the OpenAPI surface and could be removed outright.
- src/api/deps.py retains three leftover stubs from the KB-service extraction (get_knowledge_worker, get_guest_session_manager, get_image_storage_service) — the flows they served (KB worker, guest sessions, image storage) have no gateway entry points anymore.
- All 48 v1 route modules are wired via src/api/router.py into src/main.py — no unregistered route files found; every flagged 'unused' item above is internal dead code (dead helpers/methods), not dead endpoints.

### 3.3 src-services

死代码 8 | 过时注释 2 | 错误注释 1 | 超大文件 5

#### 死代码

| 位置 | 符号 | 类型 | 证据 | 置信度 |
|---|---|---|---|---|
| `src/services/knowledge/kb_proxy_client.py:1` | KBProxyClient re-export shim (whole file) | 未使用文件(整文件死) | Zero importers repo-wide: src/main.py:710 imports KBProxyClient from ai_gateway_core.knowledge (the canonical copy); the only re-exporter (src/services/knowledge/__init__.py:20) is itself never imported by anything ou... | 高 |
| `src/services/session/adapters/langgraph_session.py:6` | LangGraphSessionAdapter (whole file) | 未使用文件(整文件死) | Class name and module have zero references anywhere in the repo (src/, tests/, scripts/, apps/, packages/) — not even tests. Session manager submodules are imported directly (container.py:318,330, dispatcher.py:21), n... | 高 |
| `src/services/storage/file_storage.py:1` | whole file (Phase 5f back-compat shim) | 未使用文件(整文件死) | Zero importers anywhere including tests (grep 'file_storage' in src/tests shows only ai_gateway_core paths; runtime uses ai_gateway_core.storage directly at src/main.py:623-626). The 'transition' it was kept for is co... | 高 |
| `src/services/storage/artifact_storage.py:1` | whole file (Phase 5f back-compat shim) | 未使用文件(整文件死) | Zero importers anywhere (grep 'artifact_storage' in src/tests shows only ai_gateway_core paths; runtime uses ai_gateway_core.storage at src/main.py:616-619). Canonical implementation lives in packages/ai-gateway-core/... | 高 |
| `src/services/knowledge/confluence/sync_service.py:1` | ConfluenceSyncService (whole file) | 未使用文件(整文件死) | 3297 lines. Only live importer is the 503-stub router src/api/v1/confluence.py:21-23, which imports just two exception classes (ConfluenceAccessDeniedError, ConfluenceSyncError); every endpoint 503s because src/main.p... | 中 |
| `src/services/knowledge/embedding.py:1` | DashScopeEmbedding / GeminiEmbedding / create_embedding (whole file) | 未使用文件(整文件死) | 1767 lines; no runtime importer — only tests (tests/services/test_dashscope_embedding.py:6, tests/services/test_image_sync.py:511+) and the dead confluence/sync_service.py:1145 import it. Superseded by apps/knowledge-... | 中 |
| `src/services/knowledge/vlm_service.py:1` | DashScopeVLMService (whole file) | 未使用文件(整文件死) | Only importers are the dead knowledge/confluence/image_processor.py (lines 33, 809), which is itself only reachable via tests and the 503-stub router. Byte-identical (390=390 lines) duplicate of apps/knowledge-service... | 中 |
| `src/services/storage/image_storage.py:1` | whole file (Phase 5f back-compat shim) | 未使用文件(整文件死) | Imported only by tests/services/test_image_sync.py (~20 import sites); runtime initializes storage via ai_gateway_core.storage.image_storage (src/main.py:554) with the same ImageStorageService/StorageConfig symbols. C... | 中 |

#### 过时注释

| 位置 | 注释(摘录) | 问题 | 置信度 |
|---|---|---|---|
| `src/services/knowledge/__init__.py:7` | "- ``embedding.py`` / ``vlm_service.py`` — kept as **shared utilities** until K5c reconciles the Confluence integration. ``confluence/`` ... | The deferred work this docstring describes has shipped: src/main.py:635-659 documents Phase K5c as complete ('Confluence scheduler + sync-service moved to knowledge-service. The gateway no longer p... | 高 |
| `src/services/knowledge/__init__.py:12` | "- ``confluence/`` — out of scope for K5b, deferred to K5c." | Confluence is no longer 'deferred' — K5c shipped (src/main.py:635-659: 'Phase K5c: Confluence scheduler + sync-service moved to knowledge-service'). The gateway copy is a dead parallel version of t... | 高 |

#### 错误注释

| 位置 | 注释(摘录) | 错误之处 | 置信度 |
|---|---|---|---|
| `src/services/knowledge/__init__.py:10` | "Both files are byte-identical with their kb-service counterparts." | Factually wrong for embedding.py: `diff` shows src/services/knowledge/embedding.py (1767 lines) vs apps/knowledge-service/src/knowledge_service/services/knowledge/embedding.py (2031 lines) differ s... | 高 |

#### 超大文件

| 文件 | 行数 | 问题 | 拆分建议 |
|---|---|---|---|
| `src/services/knowledge/confluence/sync_service.py` | 3297 | CRITICAL: ConfluenceSyncService god class (page/space sync, incremental sync, webhook paths, embedding, image processing, deletion sweeps) — but it is dead a... | Delete the file (and the rest of src/services/knowledge/confluence/) — the canonical copy lives in apps/knowledge-service/src/knowledge_service/services/know... |
| `src/services/eval/golden.py` | 2044 | CRITICAL: live eval-gate engine mixing three concerns — schema constants + observation validation (lines 1-600), the stateful/tool/runtime expectation evalua... | Split into golden_eval.py (case validation + evaluation engine) and golden_gate.py (threshold gating, rate tolerance, report writing), keeping constants in g... |
| `src/services/knowledge/embedding.py` | 1767 | CRITICAL: 9 embedding classes/adapters (DashScope/Gemini/SiliconFlow/LocalHash, multimodal, caching, unified results) — but dead at runtime: only tests and t... | Delete once tests (test_dashscope_embedding.py, test_image_sync.py) are pointed at the knowledge-service copy or ai_gateway_core equivalent. |
| `src/services/eval/agent_observation_adapter.py` | 1285 | CRITICAL: live (scripts/eval_golden.py + Makefile eval-regression-gate target). One module holds field-level validation helpers (1-600), three large evidence... | Extract _tool/_budget/_hitl projections into eval/observation_projections.py and validation helpers into eval/observation_validation.py; keep adapt_producer_... |
| `src/services/billing/quota_service.py` | 1141 | CRITICAL: live god class — QuotaService holds 20+ methods mixing DB data access (get_user_quota, set_user_quota, _get_or_create_quota with inline SQL), in-me... | Extract quota_repository.py (SQL rows <-> UserQuota mapping) and quota_alerts.py (alert creation/listing); keep QuotaService as policy orchestration (check_q... |

#### 模块备注(交叉发现)

- RUNTIME COPY ANALYSIS: entry point `ai-gateway = "src.main:app"` (pyproject.toml:172) — src/services IS the live gateway layer, but it coexists with extracted copies: src/main.py imports ai_gateway_core.* for logging/exceptions/storage/knowledge/tasks/memory (lines 39,62,554,616,623,710,718,853) while still importing src.services.* for metrics/billing/eval/session/llm/registry. apps/knowledge-service runs as an independent :8092 microservice (docker-compose.yml:461 `uvicorn knowledge_service.main:app`) and apps/knowledge-service + apps/assistant-service NEVER import src.services (verified by rg) — so everything in src/services is exclusively gateway-layer.
- DUPLICATION — src/services/knowledge/ is a fully superseded parallel copy: client.py, models.py, parser.py, scheduler.py are byte-identical with apps/knowledge-service/src/knowledge_service/services/knowledge/confluence/ copies; sync_service.py and image_processor.py are divergent parallel versions (src 3297/823 vs app 3903/854 lines); embedding.py (1767 vs 2031) and vlm_service.py (identical) duplicate the kb-service versions. The only live edge into the tree is the 503-stub router src/api/v1/confluence.py importing 2 exception classes (lines 21-23) — the whole subtree is a deletion candidate after tests are re-pointed.
- DUPLICATION — src/services/storage/ (file_storage.py, artifact_storage.py, image_storage.py, __init__.py) are Phase-5f shims over ai_gateway_core.storage; runtime imports the core copies directly (src/main.py:554,616,623). Only tests/services/test_image_sync.py still imports the legacy image_storage path.
- DUPLICATION (live, not dead) — the task subsystem is wired from three places at runtime: src/services/task (container.py:270-360, dispatcher.py, api/v1/tasks.py), ai_gateway_core.tasks (src/main.py:718,853), and src/core/tasks/queue.py (src/main.py:524-535) — dedup candidate across the extraction boundary.
- The lazy-import barrels src/services/task/__init__.py, src/services/session/__init__.py and src/services/registry/__init__.py (each a __getattr__/import_module map) are never imported anywhere — all consumers import submodules directly (e.g. container.py:270-360, main.py:961, dispatcher.py:21). Same for src/services/knowledge/__init__.py re-exports.
- ruff 0.15.17 (venv) reports the module clean for F-rules (no unused imports/vars); only ARG002 unused-argument findings: registry/load_balancer.py:30,35 (`service_id` ignored in RandomStrategy.select/WeightedStrategy.select) and session/session_manager.py:179-180 (`include_null_service_id`, `status` accepted but ignored in the in-memory list_session_summaries fallback — the API contract at src/api/v1/sessions.py:76,85 passes them; the ai_gateway_core DB implementation honors them).
- No commented-out code blocks (>=5 lines) found anywhere in src/services; no dead feature flags; no unreachable `if False:` branches.
- eval/ layer is partially wrapped over ai_gateway_core.eval: eval_outbox_worker.py subclasses/wraps core EvalOutboxWorker/EvaluatorExecutor (ai_gateway_core.eval import at line 8) — same-named logic in two places; worth consolidating during the next extraction wave.

### 3.4 asst-core-a

死代码 26 | 过时注释 3 | 错误注释 1 | 超大文件 12

#### 死代码

| 位置 | 符号 | 类型 | 证据 | 置信度 |
|---|---|---|---|---|
| `apps/assistant-service/src/assistant_service/core/content/structured_output.py:56` | StructuredOutputConfig | 未使用类 | Only definition exists; zero references repo-wide (single-pass word index over all .py files, incl. tests). Runtime code uses the unstructured repair helpers directly. | 高 |
| `apps/assistant-service/src/assistant_service/core/content/structured_output.py:401` | get_schema_for_model | 未使用函数 | Only definition; zero references repo-wide. Per-provider schema shaping is done in the providers/ modules instead. | 高 |
| `apps/assistant-service/src/assistant_service/core/providers/openai_responses_runtime.py:704` | native_result_blocks | 未使用函数 | Only definition; zero references repo-wide. | 高 |
| `apps/assistant-service/src/assistant_service/core/agent/agui_protocol.py:328` | AGUIEventEmitter.run_error | 未使用方法 | Never called; only 12 of the emitter's 34 public methods have callers, all from content/content_generator.py. Terminal errors flow through run_finished/step events. | 高 |
| `apps/assistant-service/src/assistant_service/core/agent/agui_protocol.py:425` | text_delta / tool_call_start / tool_call_args / tool_call_result / tool_call_end / tool_error | 未使用方法 | Legacy text-delta tool-call emission path; text_delta itself carries the comment '# Legacy text delta support' (line 425). AgentLoop streams tool calls via StreamEventType events, not the emitter. | 高 |
| `apps/assistant-service/src/assistant_service/core/agent/agui_protocol.py:541` | messages_snapshot / artifact_created / file_creating / file_created | 未使用方法 | No callers anywhere. AgentLoop persists artifacts itself via persist_and_collect_events + StreamEventType.ARTIFACT_CREATED.value. | 高 |
| `apps/assistant-service/src/assistant_service/core/agent/agui_protocol.py:639` | document_generation_start / document_generation_result / search_started / search_progress / search_completed / code_execution_start / code_execution_result / image_generation_start / image_generation_result | 未使用方法 | No callers anywhere in the repo (lines 639-794). | 高 |
| `apps/assistant-service/src/assistant_service/core/agent/agui_protocol.py:836` | custom_event / raw_event | 未使用方法 | No callers anywhere in the repo (lines 836-849). | 高 |
| `apps/assistant-service/src/assistant_service/core/quiz/__init__.py:1` | quiz shim module | 未使用文件(整文件死) | Whole re-export shim file has zero importers repo-wide (both dotted and relative forms checked). Docstring claims 'quiz_tool, etc.' legacy callers, but tools/quiz_tool.py imports ai_gateway_core only; core/__init__.py... | 高 |
| `apps/assistant-service/src/assistant_service/core/tasks/task_types.py:1` | task_types shim module | 未使用文件(整文件死) | Zero importers (relative and dotted forms checked); callers use ai_gateway_core.tasks.task_types directly. | 高 |
| `apps/assistant-service/src/assistant_service/core/context_engine.py:1` | context_engine shim module | 未使用文件(整文件死) | Phase 5d re-export shim; only tests import it. Production imports .rag.context_engine directly (assistant_service.py:109, agent_loop.py:64, assembler.py:12). | 中 |
| `apps/assistant-service/src/assistant_service/core/streaming_writer.py:1` | streaming_writer shim module | 未使用文件(整文件死) | Phase 5d re-export shim; only tests import it. Production imports .content.streaming_writer directly (core/__init__.py:54). | 中 |
| `apps/assistant-service/src/assistant_service/core/task_planner.py:1` | task_planner shim module | 未使用文件(整文件死) | Phase 5d re-export shim; only tests import it (production uses ai_gateway_core.task_planner). | 中 |
| `apps/assistant-service/src/assistant_service/core/tools/style_presets.py:1` | style_presets shim module | 未使用文件(整文件死) | Only tests import it; its own docstring acknowledges the 'one ``from ...tools.style_presets`` site in AS' that no longer exists — production uses ai_gateway_core directly. | 中 |
| `apps/assistant-service/src/assistant_service/core/agent/middlewares/permission.py:89` | policy_from_sets | 未使用函数 | Referenced only from tests (e.g. tests/services/assistant/test_harness_middlewares.py); production middleware builds policy objects inline. | 中 |
| `apps/assistant-service/src/assistant_service/core/tasks/task_planner.py:1594` | create_simple_plan | 未使用函数 | Referenced only from tests (test_task_planner.py); production path builds plans via create_plan. | 中 |
| `apps/assistant-service/src/assistant_service/core/tasks/task_planner.py:364` | WorkflowPattern | 未使用类 | Used only by tests; _create_plan_rule_based accepts it but no production caller passes a pattern. | 中 |
| `apps/assistant-service/src/assistant_service/core/agent/agent_loop_models.py:324` | enable_react_loop / react_max_iterations / react_thinking_visible / react_auto_retry | 死配置开关/字段 | Never read by the streaming-first runtime; only referenced by the definition and serialized in test_agent_loop_golden.py assertions. No ReAct loop exists in the codebase. | 中 |
| `apps/assistant-service/src/assistant_service/core/agent/agent_loop_models.py:333` | enable_error_recovery / error_max_retries / error_base_delay / error_max_delay | 死配置开关/字段 | Never read by the streaming-first runtime; only referenced by definition + golden test. Error recovery was never implemented in the loop. | 中 |
| `apps/assistant-service/src/assistant_service/core/agent/agent_loop_models.py:302` | compress_threshold / compressed_context_tokens / max_summary_tokens | 死配置开关/字段 | Never read; staged compaction only reads enable_staged_compaction and min_recent_messages (agent_context_lifecycle.py:993-994). | 中 |
| `apps/assistant-service/src/assistant_service/core/content/structured_output.py:46` | RepairStrategy.NONE / RepairStrategy.LLM_FIX | 死配置开关/字段 | Enum members never referenced anywhere; only EXTRACT_JSON and TRUNCATE are used (lines 49-50 usage at 194-198). | 高 |
| `apps/assistant-service/src/assistant_service/core/agent/agent_context_lifecycle.py:1014` | _summarize_history | 未使用方法 | Zero references repo-wide (plain grep, including callback-style non-paren usage). Legacy LLM summary-compaction path removed with the 8-step pipeline. | 高 |
| `apps/assistant-service/src/assistant_service/core/agent/agent_loop.py:527` | _parse_subagent_configs | 未使用方法 | Zero references repo-wide; streaming subagent dispatch parses configs inline in agent/streaming_tool_execution.py instead. | 高 |
| `apps/assistant-service/src/assistant_service/core/assistant_service.py:1927` | _inject_file_content | 未使用方法 | Zero references repo-wide; file-content injection is handled by the streaming context builder. | 高 |
| `apps/assistant-service/src/assistant_service/core/assistant_service.py:622` | _repair_with_policy | 未使用方法 | Referenced only from tests/services/assistant/test_context_packet_contract.py:907 (noqa SLF001). Dead in production. | 中 |
| `apps/assistant-service/src/assistant_service/core/assistant_service.py:1058` | _retrieve_context | 未使用方法 | Referenced only from tests/services/assistant/test_agent_knowledge_binding.py:522 (noqa SLF001). Dead in production — context retrieval moved to the mixins. | 中 |

#### 过时注释

| 位置 | 注释(摘录) | 问题 | 置信度 |
|---|---|---|---|
| `apps/assistant-service/src/assistant_service/core/agent/agent_loop_models.py:321` | # ReAct Loop parameters (Phase 3) / # ReAct disabled by default - only needed for tool-using tasks / # Enabling this adds ~2-4s latency d... | Describes a ReAct loop that does not exist in the streaming-first runtime (the legacy 8-step pipeline it belonged to was removed); all four fields below the comment are never read by the runtime. | 高 |
| `apps/assistant-service/src/assistant_service/core/agent/agent_loop_models.py:332` | # Error Recovery parameters (Phase 3) | Intelligent error recovery was never implemented in the streaming-first loop; all four fields below (enable_error_recovery, error_max_retries, error_base_delay, error_max_delay) are unread. | 高 |
| `apps/assistant-service/src/assistant_service/core/quiz/__init__.py:3` | This shim keeps existing ``from assistant_service.core.quiz import ...`` sites (quiz_tool, etc.) working — delete once every AS caller mi... | Migration is complete: zero importers remain (quiz_tool.py imports ai_gateway_core directly), yet the shim persists and the comment still claims active legacy callers. | 中 |

#### 错误注释

| 位置 | 注释(摘录) | 错误之处 | 置信度 |
|---|---|---|---|
| `apps/assistant-service/src/assistant_service/core/tasks/task_planner.py:928` | Args: user_request: The user's request / ... / context: Additional context | Docstring documents a parameter named `context`, but the actual signature of _create_plan_rule_based declares `_context` (underscored); the documented name does not exist in the signature. | 低 |

#### 超大文件

| 文件 | 行数 | 问题 | 拆分建议 |
|---|---|---|---|
| `apps/assistant-service/src/assistant_service/core/assistant_service.py` | 2249 | AssistantService god class (streaming entry, context building, execution helpers, _working_memory_legacy_scopes at 1999-2073) plus a mixin and helper classes... | Split by concern: extract _retrieve_context/_build_messages/_inject_file_content into a context-builder module, move _working_memory_legacy_scopes into the m... |
| `apps/assistant-service/src/assistant_service/core/runtime/memory/indexer.py` | 1874 | MemoryIndexer mixes SQLite ingestion, Qdrant vector-store adapter, query orchestration, and 6+ dataclasses (lines 48-133) in one file. | Split into contracts/dataclasses, ingestion pipeline, vector-store adapter, and query orchestration modules. |
| `apps/assistant-service/src/assistant_service/core/tool_invoker.py` | 1741 | RegistryToolInvoker bundles scoped authorization, policy snapshots, ADR-003 result cache (line 187), tool-discovery bridge (line 200/676), argument validatio... | Extract policy-resolution, result-cache, and tool-discovery-bridge helpers into separate modules; keep invoke() orchestration in the invoker. |
| `apps/assistant-service/src/assistant_service/core/agent/agent_context_lifecycle.py` | 1692 | Lifecycle mixin covers compaction (505-605), staged-compaction reads (993), streaming system-prompt assembly (1536), auxiliary context packets (1601), and hi... | Split into compaction module, context-packet assembly module, and the lifecycle mixin itself. |
| `apps/assistant-service/src/assistant_service/core/tasks/task_planner.py` | 1634 | TaskPlanner plus WorkflowPattern, ExecutionPlan, LLM planning (create_plan at 704) and rule-based planning (922) in one file. | Split into plan models, LLM-based planner, and rule-based planner modules. |
| `apps/assistant-service/src/assistant_service/core/trace_writer.py` | 1589 | AssistantTraceContext (458) and AssistantTraceWriter (574) plus span/event serialization helpers in one file. | Split trace data models from the writer/emitter logic. |
| `apps/assistant-service/src/assistant_service/core/agent/subagent_manager.py` | 1563 | SubAgentManager handles dispatch, batch fan-out, streaming result assembly, and _MAX_PARALLEL_SUBAGENTS throttling in one file. | Split dispatch orchestration from streaming/batch result assembly. |
| `apps/assistant-service/src/assistant_service/core/files/file_processor.py` | 1540 | FileProcessor plus PDFPageContent (109), DocumentStructure (142) and _analyze_document_structure (935) in one file. | Extract document-structure analysis and PDF parsing into a separate module from the strategy/processor classes. |
| `apps/assistant-service/src/assistant_service/core/local_node/control_plane.py` | 1496 | LocalNodeControlPlaneService (416) shares the file with state models (245-339), repositories and request handlers. | Split state models, repositories, and the service into separate modules. |
| `apps/assistant-service/src/assistant_service/core/agent/agent_loop.py` | 1420 | AgentLoop (217) plus recovery helpers, subagent helpers and create_agent_loop factory (1397) in one file. | Move subagent config/terminal-validation helpers into subagent_manager.py and recovery helpers into a shared module; keep the loop orchestration. |
| `apps/assistant-service/src/assistant_service/core/runtime/memory/governance_cleanup.py` | 1403 | Governance cleanup service mixes retention-policy computation, chunk/artifact deletion strategies, and scheduling. | Split policy computation from the execution engine. |
| `apps/assistant-service/src/assistant_service/core/code_executor.py` | 1403 | CodeExecutorService (303) plus MATPLOTLIB_SETUP template (238) and get_code_executor factory (1379) in one file. | Extract setup templates into a static module and sandbox execution from the service orchestration. |

#### 模块备注(交叉发现)

- Phase 5d shim migration is effectively complete: quiz/__init__.py and tasks/task_types.py have zero importers, and four more shims (context_engine.py, streaming_writer.py, task_planner.py, tools/style_presets.py) are imported only by tests — all six are safe to delete once test imports are repointed.
- No apps/assistant-service/tests directory exists; tests mirror the module layout at tests/services/assistant/.
- AGUIEventEmitter (agent/agui_protocol.py, 896 lines) is a second, largely-unused event path: only 12 of 34 public methods are called (all from content/content_generator.py); AgentLoop emits artifact events via persist_and_collect_events + StreamEventType. Roughly 22 emitter methods are removable.
- LARGE-tier files (1000-1499 lines) below the CRITICAL threshold also warrant attention: runtime/memory/source_store.py (1386), local_node/provider_adapter.py (1386), gateway/run_lifecycle.py (1357), agent/streaming_tool_execution.py (1356), tools/confluence_tool.py (1326), runtime/compat/runtime_adapter.py (1287), agent/execution_lifecycle.py (1236), runtime/context/assembler.py (1173), agent/streaming_preparation.py (1170), agent/agent_turn_lifecycle.py (1052).
- runtime/compat/runtime_adapter.py (1287 lines) is the central compatibility hub between the legacy gateway runtime and the streaming-first runtime — highest-risk file to split.
- tools/confluence_tool.py (1326 lines) mixes ~20 individual tool registrations with client code; extract per-tool registration blocks.
- Dead-symbol checks were done with a repo-wide Python word index over all .py files plus plain-grep passes for callback-style references (e.g. _invoke_discovered_tool passed as a dict value at tool_invoker.py:691 is alive via tool_discovery.py:313; _on_user_message_persist_done passed to add_done_callback at agent_context_lifecycle.py:1187 is alive).
- Known cross-cutting duplication outside this module's scope: src/services/knowledge and apps/knowledge-service both carry knowledge-service implementations.

### 3.5 asst-core-b

死代码 22 | 过时注释 5 | 错误注释 2 | 超大文件 3

#### 死代码

| 位置 | 符号 | 类型 | 证据 | 置信度 |
|---|---|---|---|---|
| `apps/assistant-service/src/assistant_service/core/docgen/__init__.py:1` | docgen package (entire tree) | 未使用文件(整文件死) | Zero production references to `assistant_service.core.docgen` repo-wide (rg over all .py outside tests/ and the package itself returns nothing; only metric-name strings like 'assistant.core.docgen...' inside the packa... | 高 |
| `apps/assistant-service/src/assistant_service/core/rag/context_manager.py:54` | ContextManager / ContextResult / ContextConfig | 未使用类 | get_context_manager() is called at assistant_service.py:331 and the result stored on self.context_manager, which is never read again anywhere (rg shows no other use of self.context_manager in the file). process_histor... | 高 |
| `apps/assistant-service/src/assistant_service/core/rag/scenario_analyzer.py:526` | DocumentAnalyzer / create_document_analyzer / Urgency / AnalysisContext | 未使用类 | DocumentAnalyzer (line 526) and create_document_analyzer (683) have zero references anywhere in the repo outside this file; Urgency (51) and AnalysisContext (86) are only referenced internally. The whole class is part... | 高 |
| `apps/assistant-service/src/assistant_service/core/rag/scenario_analyzer.py:137` | ScenarioAnalyzer.detect_scenario_fast / detect_scenario_deep / get_scenario_info / get_analysis_dimensions / build_analysis_prompt | 未使用方法 | ScenarioAnalyzer is constructed at agent_loop.py:285 and assistant_service.py:449, but no method of the instance is ever invoked: detect_scenario_fast/deep/get_scenario_info/get_analysis_dimensions have zero external ... | 高 |
| `apps/assistant-service/src/assistant_service/core/rag/scenario_aware_retriever.py:126` | QueryExpander / retrieve_simple / create_scenario_aware_retriever (whole module dead) | 未使用类 | QueryExpander (126), retrieve_simple (529) and create_scenario_aware_retriever (562) have zero external references. ScenarioAwareRetriever is only referenced as a type annotation in agent_loop.py:238 and in a docstrin... | 高 |
| `apps/assistant-service/src/assistant_service/core/rag/rag_metrics.py:647` | RAGMetricsCollector.record_retrieval/record_evaluation/get_recent_metrics/get_aggregate_stats/get_buffer/clear_buffer | 未使用方法 | All six public methods of RAGMetricsCollector have zero external callers (rg across repo: only intra-file refs). The collector is instantiated at agent_loop.py:292 (self.metrics_collector) and never used; get_rag_eval... | 高 |
| `apps/assistant-service/src/assistant_service/core/rag/query_intent_analyzer.py:772` | QueryIntentAnalyzer.cache_stats / clear_cache | 未使用方法 | cache_stats (772) has zero external references. clear_cache (760) is called only from tests/api/test_api_key_self_service_security.py. QueryIntentAnalyzer itself is instantiated at agent_loop.py:287 but never invoked ... | 高 |
| `apps/assistant-service/src/assistant_service/core/agent/agent_loop.py:285` | self.scenario_analyzer / self.query_intent_analyzer / self.metrics_collector / self.scenario_retriever | 未使用变量 | All four analyzer attributes are assigned in __init__ (lines 285-292) and never read again — rg shows no `self.<name>.` call anywhere in the file. Docstring at 266-273 still advertises 'For intent detection', 'For LLM... | 高 |
| `apps/assistant-service/src/assistant_service/core/assistant_service.py:380` | self.rag_evaluator / self.scenario_analyzer | 未使用变量 | self.rag_evaluator = get_rag_evaluator() (line 380) and self.scenario_analyzer = create_scenario_analyzer() (line 449) are assigned during init and never read afterward — rg shows no other occurrence in the file. Both... | 高 |
| `apps/assistant-service/src/assistant_service/core/mcp/oauth.py:172` | MCPOAuthCoordinator + MCPOAuthSession/MCPOAuthGrant/MCPOAuthSessionStore/MCPOAuthSecretWriter/InMemory stores | 未使用类 | The entire OAuth Authorization Code + PKCE flow (455-line module) has no production consumers: MCPOAuthCoordinator.begin/complete are only exercised by tests/security/test_mcp_security.py. Exported via mcp/__init__.py... | 中 |
| `apps/assistant-service/src/assistant_service/core/mcp/manager.py:427` | MCPManager.server_names | 未使用方法 | The property (line 427-429) has zero references repo-wide outside its own definition (rg `server_names`: 1 hit in file = the definition). | 高 |
| `apps/assistant-service/src/assistant_service/core/mcp/manager.py:431` | MCPManager.get_servers_status | 未使用方法 | Only reference outside the definition is a test stub: tests/security/test_management_api_authorization.py:67 `SimpleNamespace(get_servers_status=lambda: [])`. No production caller. | 中 |
| `apps/assistant-service/src/assistant_service/core/mcp/manager.py:385` | MCPManager.refresh_tools | 未使用方法 | Only caller is tests/services/assistant/test_mcp_static_capabilities.py:432. main.py only ever calls initialize_all() and shutdown() on the plugin MCPManager (main.py:698, 360); the tool-invocation path (call_tool at ... | 中 |
| `apps/assistant-service/src/assistant_service/core/mcp/tenant_mcp_config.py:50` | TenantMCPConfigService.set_all_server_names | 未使用方法 | Zero references outside the definition (rg repo-wide). Docstring says 'set after MCPManager init' but main.py:852-863 constructs TenantMCPConfigService without ever calling it; all_server_names is only set via the con... | 高 |
| `apps/assistant-service/src/assistant_service/core/mcp/runtime.py:68` | ConfiguredEnvironmentSecretResolver | 未使用类 | Defined at line 68 with from_env classmethod (86); never instantiated anywhere — rg shows the name only inside runtime.py (definition, __repr__, __all__). main.py:505 wires MappingSecretResolver instead. Still exporte... | 中 |
| `apps/assistant-service/src/assistant_service/core/mcp/connector_mcp.py:1` | ConnectorMCPService / get_connector_mcp_service (re-export shim) | 未使用文件(整文件死) | Phase 5d re-export shim with zero importers: src/api/v1/connectors.py:575-640 imports from ai_gateway_core.connectors directly; nothing imports assistant_service.core.mcp.connector_mcp. Not re-exported by mcp/__init__... | 高 |
| `apps/assistant-service/src/assistant_service/core/mcp/config.py:84` | load_mcp_config | 未使用函数 | Only caller is tests/services/assistant/test_mcp_static_capabilities.py:83. The production startup path (main.py:690-693) uses load_agent_plugin_mcp_config instead. Still re-exported via mcp/__init__.py. | 中 |
| `apps/assistant-service/src/assistant_service/core/prompts/generation_prompts.py:37` | DOCUMENT_GENERATION_SYSTEM_PROMPT/OUTLINE/SECTION/REPAIR/PRESENTATION/REPORT/EMAIL/SUMMARY constants + build_generation_prompt/build_outline_prompt/build_section_prompt/build_repair_prompt/build_presentation_prompt/build_report_prompt/build_email_prompt | 未使用变量 | Repo-wide rg of each exported name finds zero production consumers (re-exported via prompts/__init__.py only; all hits are in __init__.py and tests). The sole live symbol from this module is build_summary_prompt (agen... | 高 |
| `apps/assistant-service/src/assistant_service/core/prompts/guardrails.py:25` | Entire module: GUARDRAILS + 8 scenario constants + SCENARIO_GUARDRAILS_MAP + get_guardrails/get_minimal_guardrails/get_anti_hallucination_guardrails/get_guardrails_for_scenario | 未使用变量 | All 14 exported symbols have zero production consumers (repo-wide rg; only prompts/__init__.py re-exports and tests reference them). The 'guardrails + freedom' prompt stack was superseded per prompts/__init__.py docst... | 高 |
| `apps/assistant-service/src/assistant_service/core/prompts/scenario_analysis_prompts.py:864` | SCENARIO_DETECTION_PROMPT/FAST_SCENARIO_DETECTION_PROMPT/MULTI_DIMENSIONAL_ANALYSIS_PROMPT/DOCUMENT_ANALYSIS_PROMPT/DOCUMENT_QA_PROMPT/KB_ENHANCED_ANALYSIS_PROMPT + get_scenario_types_description/get_scenario_codes/get_scenario_keywords/get_expert_template/get_scenario_metadata/get_retrieval_strategy/get_tool_affinity/get_confidence_threshold/detect_scenario_by_keywords/validate_scenario_detection/get_all_scenario_types/list_scenario_codes | 未使用函数 | None of these ~18 symbols have production consumers. The 5 build_* builders + SCENARIO_TYPES + EXPERT_TEMPLATES + get_analysis_dimensions are imported only by rag/scenario_analyzer.py, whose methods are themselves nev... | 中 |
| `apps/assistant-service/src/assistant_service/core/prompts/system_prompt_v2.py:65` | AGENT_IDENTITY/AGENT_CORE_BEHAVIOR/AGENTIC_WORKFLOW/AGENT_LOOP/ANTI_HALLUCINATION/ERROR_RECOVERY/CONTEXT_MANAGEMENT/PARALLEL_TOOL_CALLING/THINKING_GUIDANCE/STATE_TRACKING/OUTPUT_RULES/SYSTEM_CAPABILITY_TEMPLATE/DEFAULT_TOOL_DESCRIPTIONS + build_scenario_aware_prompt/inject_conversation_history/inject_all_context/get_default_system_prompt/get_minimal_system_prompt/get_tool_focused_system_prompt/get_agentic_system_prompt/get_document_analysis_prompt/estimate_prompt_tokens/get_prompt_size_info/g... | 未使用变量 | The 13 section constants are never referenced internally (build_system_prompt_v2 uses only CORE_ASSISTANT_PROMPT + CACHE_SPLIT_MARKER) and have zero production consumers via the __init__ re-exports. The 12 functions l... | 高 |
| `apps/assistant-service/src/assistant_service/core/skills/executor.py:1` | executor.py / parser.py / builtin/skill_create.py re-export shims | 未使用文件(整文件死) | All three are Phase 5d re-export shims to ai_gateway_core.skills. Their only importers are the package __init__ (which re-exports them) and tests (test_generated_skill_safety.py). No production file imports assistant_... | 中 |

#### 过时注释

| 位置 | 注释(摘录) | 问题 | 置信度 |
|---|---|---|---|
| `apps/assistant-service/src/assistant_service/core/docgen/__init__.py:3` | "See plans/Document-Generation-SOTA-Upgrade-Plan-2026-04-21.md." | The referenced file does not exist anywhere in the repo (no plans/ directory at root; find for *Document-Generation* returns nothing). Additionally the subsystem the docstring describes as the SOTA... | 高 |
| `apps/assistant-service/src/assistant_service/core/models/model_registry.py:4` | "Supports (default catalog as of 2026-04): - OpenAI (gpt-4o, o1) - Anthropic (claude-opus-4-5, claude-sonnet-4-5) ... - Google / Google V... | Outdated catalog summary: model_catalog.py DEFAULT_MODELS now leads with gpt-5.4/gpt-5.4-mini/gpt-5.4-nano (in-code comment at model_catalog.py:200 says 'gpt-5.4 is the recommended production' mode... | 中 |
| `apps/assistant-service/src/assistant_service/core/prompts/__init__.py:7` | "``scenario_analysis_prompts`` and ``generation_prompts`` are the remaining specialized surfaces; each has live callers in ``core/rag`` a... | No longer accurate: of generation_prompts' 16 exports only build_summary_prompt has a live caller (agent_context_lifecycle.py:1047); its 8 constants and other 7 builders are unreferenced. scenario_... | 中 |
| `apps/assistant-service/src/assistant_service/core/prompts/__init__.py:58` | "# New: TTFT-optimized version" / "# New: Fast keyword-based detection" / "# New: Full scenario dictionary" / "# New: Classification thre... | Stale 'New:' change-markers left on import lines; the marked functions have shipped long ago and are now entirely unreferenced in production (see dead-code findings). The markers describe nothing t... | 低 |
| `apps/assistant-service/src/assistant_service/core/agent/agent_loop.py:266` | Constructor docstring: "scenario_analyzer: For intent detection", "query_intent_analyzer: For LLM-driven retrieval decision (Self-RAG sty... | Describes active functionality that is dead wiring: the attributes are assigned (lines 285-292) and never used anywhere in the file or its callers; no method of any of these objects is ever invoked... | 中 |

#### 错误注释

| 位置 | 注释(摘录) | 错误之处 | 置信度 |
|---|---|---|---|
| `apps/assistant-service/src/assistant_service/core/assistant_service.py:1223` | Docstring of _execute_agent_loop: "This is the new enterprise-grade execution path that integrates: - ScenarioAwareRetriever for intellig... | Factually wrong about the code: the path never instantiates or calls ScenarioAwareRetriever (only a type annotation exists in agent_loop.py:238) and never uses RAGMetrics — the collector/evaluator ... | 中 |
| `apps/assistant-service/src/assistant_service/core/mcp/manager.py:5` | Module docstring: "...registers them into the agent's ToolRegistry with prefix `mcp_{server}:{tool}`." | Wrong separator: the code at line 138 builds the name as `f"mcp_{mcp_tool.server_name}__{mcp_tool.name}"` — double underscore, not `:`. The documented format `mcp_{server}:{tool}` does not match an... | 高 |

#### 超大文件

| 文件 | 行数 | 问题 | 拆分建议 |
|---|---|---|---|
| `apps/assistant-service/src/assistant_service/core/models/model_registry.py` | 1987 | CRITICAL: single module mixing wire concerns — 15 module-level helpers (SSE event parsing _parse_sse_event, OpenAI tool-call delta validation, request/error ... | Split into: models/streaming.py (StreamDelta, smoother, SSE parsing), models/provider_errors.py (error/finish-reason taxonomies), models/request_safety.py (s... |
| `apps/assistant-service/src/assistant_service/core/mcp/client.py` | 1277 | LARGE: MCPClient (line 324) handles HTTP transport (initialize/list_tools/call_tool/download_resource_link/close), streaming SSE session management, static-c... | Extract transport concerns: mcp/transport.py (SSE session + request plumbing) and mcp/static_capabilities.py (MCPStaticToolCapability + header merging), keep... |
| `apps/assistant-service/src/assistant_service/core/prompts/scenario_analysis_prompts.py` | 1157 | LARGE: 860 lines of prompt-string constants (SCENARIO_TYPES, 6 detection/analysis prompt templates, EXPERT_TEMPLATES) plus 18 accessor/builder functions at t... | First delete the 18 unreferenced accessors/constants (they duplicate logic in rag/scenario_analyzer.py), then split the remaining builders into prompts/scena... |

#### 模块备注(交叉发现)

- core/docgen (~8,685 lines) is a near-duplicate of packages/mcp-docgen-server/src/docgen (~8,513 lines, same file tree, only formatting/logging diffs) and has zero production importers; the assistant copy is only referenced by tests/assistant/docgen/*. deploy/runbooks/agent-kb-eval-optimization-20260802/README.md:101 already plans its retirement ('MCP docgen 与 shared skill runtime 成为唯一实现且 fallback 测试通过后再删').
- The vendored skill bundles core/skills/{docx,pdf,pptx,xlsx} (proprietary, LICENSE.txt) are consumed only by the dead assistant docgen service (_DEFAULT_SKILL_PATHS in core/docgen/service.py:29); the live mcp-docgen-server keeps its own copy at packages/mcp-docgen-server/src/docgen/_skills_data — the two copies have diverged (files differ). If core/docgen is deleted, these bundles become orphaned data.
- skills/{executor,parser,builtin/skill_create}.py are Phase 5d re-export shims to ai_gateway_core; only tests and the package __init__ import them. skills/tool_bridge.py is the only live module in the package (chat.py:547, agent_context_lifecycle.py:1329).
- The RAG 'Manus-style' analyzer cluster (rag/scenario_analyzer.py, query_intent_analyzer.py, rag_metrics.py, scenario_aware_retriever.py, context_manager.py — ~3,400 lines combined) is constructed at agent_loop.py:285-292 and assistant_service.py:380/449 but never invoked; the entire cluster is dead wiring kept alive only by tests.
- Duplicated parallel implementations: rag/scenario_analyzer.py (ScenarioType enum, detect_scenario_fast, get_scenario_info, get_analysis_dimensions) vs prompts/scenario_analysis_prompts.py (SCENARIO_TYPES dict, detect_scenario_by_keywords, get_scenario_metadata, get_analysis_dimensions) implement the same scenario-classification concept twice with different data structures.
- prompts/__init__.py re-exports ~90 symbols of which ~75 have zero production consumers; its own docstring documents deleting agent_freedom/planning_prompts for exactly this reason ('every symbol they exported was unreferenced outside this package') — the same culling pass should be applied to generation_prompts, guardrails, scenario_analysis_prompts and system_prompt_v2.
- models/model_registry.py header docstring (line 4-8) still documents the 2026-04 catalog while model_catalog.py has since added gpt-5.4 family, claude-opus-4-7/sonnet-4-6/haiku-4-5 and gemini-3.1-* (see outdated-comments entry).
- skills/docx, skills/pdf, skills/pptx, skills/xlsx are vendored third-party skill bundles (proprietary license per LICENSE.txt) — exclude from manual refactor; note their on-disk __pycache__ dirs are gitignored but present.

### 3.6 asst-api

死代码 8 | 过时注释 3 | 错误注释 0 | 超大文件 10

#### 死代码

| 位置 | 符号 | 类型 | 证据 | 置信度 |
|---|---|---|---|---|
| `apps/assistant-service/src/assistant_service/api/routes/images.py:309` | _persist_and_get_url | 未使用函数 | Repo-wide rg for `_persist_and_get_url\b` matches only this file: the def at line 309 and its internal call to _persist_and_get_url_impl at line 327. The live variant is _persist_and_get_url_bounded (called at lines 3... | 高 |
| `apps/assistant-service/src/assistant_service/api/routes/images.py:1320` | orphaned second docstring in _post_generation_bookkeeping | 不可达代码 | Function has TWO consecutive docstrings: lines 1315-1318 (the real one, 'Returns ``latest_advanced``...') then a second bare string literal at 1320-1326 ('Centralized post-success state writes: ... * Claim idempotency... | 高 |
| `apps/assistant-service/src/assistant_service/core/local_node/wiring.py:30` | wire_local_node_control_plane | 未使用函数 | rg repo-wide: referenced only by tests/services/assistant/test_local_node_control_plane.py, test_local_node_sqlite_repository.py, its own module, core/local_node/__init__.py re-export, and a sqlite_repository.py docst... | 中 |
| `apps/assistant-service/src/assistant_service/core/local_node/wiring.py:98` | build_local_node_tool_provider | 未使用函数 | rg repo-wide: referenced only by tests/services/assistant/test_local_node_provider_adapter.py, its own module, and the core/local_node/__init__.py re-export. main.py and production code never call it; production uses ... | 中 |
| `apps/assistant-service/src/assistant_service/config/settings.py:100` | Settings / get_settings (whole config/settings.py module) | 未使用文件(整文件死) | Zero production consumers: nothing in apps/assistant-service/src imports Settings or get_settings (verified with rg for all import forms); the only references are the config/__init__.py re-export (line 1) and tests/se... | 中 |
| `apps/assistant-service/src/assistant_service/api/routes/chat.py:122` | ChatRequest.kb_include_images | 死配置开关/字段 | field_validator raises ValueError('Multimodal knowledge retrieval is not enabled') whenever kb_include_images is True (line 122), so the field can never be True on the wire, yet downstream plumbing still consumes it (... | 中 |
| `apps/assistant-service/src/assistant_service/api/routes/chat.py:146` | ChatRequest.confirm_plan (Literal[False]) | 死配置开关/字段 | Typed Literal[False] and rejected by validator reject_unsupported_plan_confirmation (lines 146-152) which raises when value is True ('Plan confirmation is not supported until durable plan approval and resume are avail... | 中 |
| `apps/assistant-service/src/assistant_service/auth/user_context.py:101` | request.app.state.settings fallback branch | 不可达代码 | The 'Compatibility for isolated route tests' branch reads request.app.state.settings — an attribute nothing in apps/assistant-service/src or tests/services/assistant ever assigns (rg for `state.settings =` and `app.st... | 中 |

#### 过时注释

| 位置 | 注释(摘录) | 问题 | 置信度 |
|---|---|---|---|
| `apps/assistant-service/src/assistant_service/api/routes/image_route_helpers.py:979` | Docstring of _check_idempotency_impl: 'Used by the sync route at request entry to detect already-recorded keys. Async path uses ``record_... | Outdated: the async path (submit_image_generation in images.py) ALSO calls _check_idempotency at images.py:1487, not only the sync route (images.py:887). The look-only probe is now used by both pat... | 高 |
| `apps/assistant-service/src/assistant_service/api/routes/images.py:1325` | Second (orphaned) docstring of _post_generation_bookkeeping lists '* Claim idempotency record (if client_request_id set)' as one of the p... | Contradicted by the NOTE at lines 1392-1395: 'idempotency claim was made BEFORE generation in the route handlers (sync + async paths). Bookkeeping no longer claims — otherwise a worker crash mid-ge... | 高 |
| `apps/assistant-service/src/assistant_service/api/routes/images.py:309` | Comment above the _persist_and_get_url wrapper: '# Keep route-local monkeypatch seams live after helper extraction.' | The premise no longer holds: nothing in the repo (including tests) references _persist_and_get_url — only _persist_and_get_url_bounded is used (lines 394, 1204, 1431). The claimed 'live seam' is de... | 中 |

#### 超大文件

| 文件 | 行数 | 问题 | 拆分建议 |
|---|---|---|---|
| `apps/assistant-service/src/assistant_service/api/routes/images.py` | 1994 | CRITICAL. God module mixing the images route handlers, idempotency claim/replay logic, CAS-advance latest_artifact_id state writes, image_turns inserts, styl... | Finish the extraction already begun in image_route_helpers.py: move the async-route bookkeeping (_post_generation_bookkeeping and friends) into image_generat... |
| `apps/assistant-service/src/assistant_service/core/local_node/control_plane.py` | 1496 | CRITICAL. Single LocalNodeControlPlaneService class implementing the whole device-control state machine: pairing challenges with Ed25519 proof redemption, pe... | Split into a pairing module (create_pairing_challenge/complete_pairing), a permission-snapshot module, and an action-dispatch module (dispatch/cancel/revoke)... |
| `apps/assistant-service/src/assistant_service/api/routes/chat.py` | 1487 | CRITICAL. Hosts two distinct chat routes (internal Agent route + chat route), request/response Pydantic models with validators, the E2E stub LLM path (_E2E_M... | Move the ChatRequest/AgentRuntimeChatRequest models + validators into a chat_contracts.py module, and extract the ASSISTANT_E2E_STUB_LLM stub path (_e2e_memo... |
| `apps/assistant-service/src/assistant_service/config/startup_fingerprint.py` | 1479 | CRITICAL. The entire production composition root: _SettingSpec table for every environment variable (storage, image, gateway-secret, local-node, E2E stub), r... | Split the _SettingSpec registry into per-domain modules (storage_specs, image_specs, gateway_specs, local_node_specs) with the resolver/redaction machinery i... |
| `apps/assistant-service/src/assistant_service/core/local_node/provider_adapter.py` | 1386 | CRITICAL. ControlPlaneLocalNodeToolProvider, LocalNodeRunBinding, SelectedLocalNodeRunBindingResolver, PinnedLocalNodeRunBindingResolver (dev/test-only), and... | Split into bindings.py (LocalNodeRunBinding + resolvers), validate.py (validate_file_result), and keep the provider as the thin orchestrator; the resolver pa... |
| `apps/assistant-service/src/assistant_service/core/gateway/run_lifecycle.py` | 1357 | CRITICAL. RunLifecycleMixin combines run creation, hard checkpoint persistence (SQL + in-memory mirror with CAS), checkpoint serialization, finish/terminal-s... | Move checkpoint serialization/_checkpoint_to_dict and digest helpers into execution_state.py (already the shared sanitization home), leaving run_lifecycle.py... |
| `apps/assistant-service/src/assistant_service/main.py` | 1218 | LARGE. App factory plus lifespan: middleware registration, plugin-agent catalog, startup fingerprint bootstrapping, readiness/drain endpoints, E2E stub wirin... | Extract the middleware stack (auth/CORS/gateway-secret setup) and the plugin-catalog initialization into separate modules under main/ or core/, keeping creat... |
| `apps/assistant-service/src/assistant_service/api/routes/image_route_helpers.py` | 1190 | LARGE. Accumulation bucket for image-route helpers: idempotency probes, artifact persistence impls, watermarking, artifact byte loading, response-shape helpe... | Split into idempotency.py (lookup/record/check helpers), persistence.py (_persist_and_get_url_impl/_bounded_impl + watermark), and loaders.py; the module nam... |
| `apps/assistant-service/src/assistant_service/api/routes/local_nodes.py` | 1182 | LARGE. 18-route fail-closed control-plane facade with Protocol seams, fault-code translation, and event-order guards in a single file. Routes are thin (per-r... | Group the routes by resource into pairing_routes.py, device_routes.py, action_routes.py, event_routes.py sharing one fail-closed error helper module; the __a... |
| `apps/assistant-service/src/assistant_service/api/routes/responses.py` | 1140 | LARGE. Single POST /responses route plus the ResponsesStreamProjector (SSE event rewriting) and _ALLOWED_REQUEST_FIELDS ingress strictness in one file — the ... | Move ResponsesStreamProjector into responses_projector.py (it is independently testable SSE logic) and keep the route file for ingress validation and streami... |

#### 模块备注(交叉发现)

- main.py never calls wire_local_node_control_plane / build_local_node_tool_provider: the entire local-node control plane (pairing, dispatch authority, device channel, gateway receipts) is unwired in production, so every /local-nodes route permanently returns fail-closed 503 (LOCAL_NODE_CONTROL_UNAVAILABLE). wiring.py, sqlite_repository.py, and device_delivery.py are dev/test-only seams, truthfully documented as such.
- config/settings.py (pydantic Settings + get_settings) is fully shadowed by config/startup_fingerprint.py in production: tests/services/assistant/test_runtime_feature_contract.py:369 imports the settings classes only to assert they are 'fingerprinted_or_explicitly_unused'. The user_context.py fallback that would consume them reads request.app.state.settings, which no code ever assigns.
- The core/gateway and core/local_node internals (execution_state, policy_engine, request_router, execution_records, command_lifecycle, run_resume, approval_lifecycle, execution_gateway, tool_bridge, device_channel, device_delivery, sqlite_repository) were read in full: these are security-hardened state machines (DB-authoritative with AUDIT-OK write-through mirrors, durable fences, HMAC receipts) and contain no dead code — every symbol is referenced; policy engine and request router are live via agent_loop.py/assistant_service.py/execution_gateway.py.
- chat.py's E2E stub (_E2E_MEMORY_BY_USER, guarded by ASSISTANT_E2E_STUB_LLM which is registered as a real fingerprinted setting at startup_fingerprint.py:93) is live and intentional — not dead code.
- No incorrectComments (factually-wrong statements) were confirmed in this module after verifying each candidate; the docstring contradictions found are behavior-drift (outdated) rather than wrong-on-arrival.
- API-surface note: 4 helper modules (image_contracts, image_generation_worker, image_route_helpers, image_task_store) are intentionally not mounted in api/router.py — that is deliberate, not dead-route code; the 11 mounted routers all have live handlers.

### 3.7 knowledge

死代码 20 | 过时注释 1 | 错误注释 3 | 超大文件 12

#### 死代码

| 位置 | 符号 | 类型 | 证据 | 置信度 |
|---|---|---|---|---|
| `apps/knowledge-service/src/knowledge_service/services/knowledge/confluence/__init__.py:1` | confluence package (client.py/models.py/parser.py/scheduler.py/sync_service.py/image_processor.py, ~7900 lines) | 未使用文件(整文件死) | Nothing in apps/knowledge-service/src imports the confluence package; main.py has zero 'confluence' references and never sets app.state.confluence_sync_service (routes/knowledge.py:2847 reads it via getattr with None ... | 中 |
| `apps/knowledge-service/src/knowledge_service/api/router.py:12` | api_router (302 lines: /datasets, /{dataset_id}/retrieve, /worker/status) | 未使用文件(整文件死) | Zero importers repo-wide; main.py:605-609 mounts only api/routes/eval.py and api/routes/knowledge.py. The other api_router hits (src/api/router.py, apps/assistant-service) are different packages. The /retrieve surface... | 高 |
| `apps/knowledge-service/src/knowledge_service/services/knowledge/image_processing_queue.py:1` | ImageProcessingQueue module (578 lines) | 未使用文件(整文件死) | String 'image_processing_queue' appears nowhere else in the repo (no imports in src, tests, or non-py files). | 高 |
| `apps/knowledge-service/src/knowledge_service/services/knowledge/multilingual_embedding.py:1` | MultilingualEmbedding/BGE-M3 module (562 lines) | 未使用文件(整文件死) | String 'multilingual_embedding' appears nowhere else in the repo; no dynamic/importlib refs exist in the service. | 高 |
| `apps/knowledge-service/src/knowledge_service/services/knowledge/processor_factory.py:1` | ProcessorFactory module (453 lines) | 未使用文件(整文件死) | String 'processor_factory' has zero references repo-wide (the live path uses processing_mode.py + document_processor.py directly). | 高 |
| `apps/knowledge-service/src/knowledge_service/services/knowledge/section_extractor.py:1` | SectionExtractor module (445 lines) | 未使用文件(整文件死) | String 'section_extractor' appears nowhere else in the repo (not even in tests). | 高 |
| `apps/knowledge-service/src/knowledge_service/services/knowledge/enhanced_ingestion.py:1` | EnhancedIngestionPipeline module (346 lines) | 未使用文件(整文件死) | Zero references repo-wide; a prior review (reports/code-review/AI_PLATFORM_MULTI_AGENT_REVIEW_2026-08-02.md:1041) flagged its _process_simple as unreachable-by-design. | 高 |
| `apps/knowledge-service/src/knowledge_service/services/knowledge/processing_dispatcher.py:1` | ProcessingDispatcher module (197 lines) | 未使用文件(整文件死) | Zero references repo-wide; review report (line 933) notes it calls nonexistent knowledge_service._ingest_document_internal, i.e. it was never wired. | 高 |
| `apps/knowledge-service/src/knowledge_service/services/knowledge/contextual_retrieval.py:1` | ContextualRetrieval module (103 lines) | 未使用文件(整文件死) | Zero references repo-wide; added in 'chore: prepare standalone open-source gateway' and never imported. | 高 |
| `apps/knowledge-service/src/knowledge_service/services/knowledge/retrieval_v2.py:1` | RetrievalPipeline module (808 lines) | 未使用文件(整文件死) | Only importer is tests/services/test_multimodal_rag.py:28. No runtime code imports it; retrieval_service.py is the live retrieval path. | 中 |
| `apps/knowledge-service/src/knowledge_service/services/knowledge/metadata_extractor.py:69` | MetadataExtractor class | 未使用文件(整文件死) | Only referenced by tests/services/knowledge/test_metadata_extractor_endpoints.py:4; no runtime importer. settings.metadata_llm (its config) is likewise never read. | 中 |
| `apps/knowledge-service/src/knowledge_service/core/crypto.py:1` | core/crypto.py (314 lines: encrypt/decrypt/sign) | 未使用文件(整文件死) | Runtime code uses ai_gateway_core.security (tenant_provider.py:9 imports decrypt_value/is_encrypted from there). The only knowledge_service.core.crypto reference is tests/services/knowledge/test_document_asset_storage... | 中 |
| `apps/knowledge-service/src/knowledge_service/core/errors/exceptions.py:1` | core/errors/exceptions.py shim (re-exports core.exceptions) | 未使用文件(整文件死) | Docstring says 'alternative import path' but no file in the repo imports knowledge_service.core.errors; all runtime code imports core.exceptions directly. | 高 |
| `apps/knowledge-service/src/knowledge_service/storage/__init__.py:1` | storage/ package (empty __init__, no modules) | 未使用文件(整文件死) | Empty directory package; nothing imports knowledge_service.storage anywhere. | 中 |
| `apps/knowledge-service/src/knowledge_service/services/knowledge/retrieval.py:760` | normalize_hybrid_scores() | 未使用函数 | Defined at retrieval.py:760 but zero call sites in the file or repo; not exported via services/knowledge/__init__.py. | 高 |
| `apps/knowledge-service/src/knowledge_service/config/__init__.py:48` | RedisSettings class + Settings.redis field | 死配置开关/字段 | Declared (field at line 306) but 'settings.redis' / '.redis.url' / 'redis_settings' are never read anywhere in src (only the class definition matches). | 中 |
| `apps/knowledge-service/src/knowledge_service/config/__init__.py:155` | MetadataLLMSettings class + Settings.metadata_llm field | 死配置开关/字段 | Declared (field at line 313) but never read in src; MetadataExtractor (its consumer) is itself unimported. | 中 |
| `src/services/knowledge/kb_proxy_client.py:1` | KBProxyClient back-compat shim (17 lines) | 未使用文件(整文件死) | Only referenced by legacy __init__.py re-export, and nothing imports src.services.knowledge; src/main.py:710 imports KBProxyClient from ai_gateway_core.knowledge instead. scripts/analyze_assistant_deps.py:105 itself s... | 高 |
| `src/services/knowledge/__init__.py:1` | src/services/knowledge package __init__ | 未使用文件(整文件死) | Nothing in the repo imports 'src.services.knowledge' or 'from src.services.knowledge import ...' (only src/api/v1/confluence.py imports the confluence subpackage, not the package root). | 高 |
| `src/services/knowledge/confluence/sync_service.py:43` | TYPE_CHECKING imports: from ..knowledge_service import KnowledgeService / from ..worker import KnowledgeWorker | 未使用导入 | Both target modules were deleted from the legacy tree (dir contains only __init__, embedding, kb_proxy_client, vlm_service); the imports are stale type-only references that any mypy/pyright run would fail to resolve. | 中 |

#### 过时注释

| 位置 | 注释(摘录) | 问题 | 置信度 |
|---|---|---|---|
| `apps/knowledge-service/src/knowledge_service/services/knowledge/common.py:3` | Docstring: 'Consolidates duplicated functions that previously existed in multiple files: ... - normalize_arabic / Arabic diacritics: retr... | The claimed consolidation never happened: retrieval.py:206 and :235 still define their own detect_language() and normalize_arabic(); retrieval_v2.py:34 defines its own detect_query_language(); and ... | 中 |

#### 错误注释

| 位置 | 注释(摘录) | 错误之处 | 置信度 |
|---|---|---|---|
| `apps/knowledge-service/src/knowledge_service/services/knowledge/__init__.py:15` | Docstring: 'from agent_gateway.services.knowledge import create_kb_tool' | Wrong module path: the package is knowledge_service.services.knowledge. The name 'agent_gateway' appears nowhere else in the repo (verified repo-wide), so this Quick Start snippet would fail on imp... | 高 |
| `apps/knowledge-service/src/knowledge_service/services/knowledge/langgraph_tools.py:13` | Docstring: 'from agent_gateway.services.knowledge import create_kb_tool' (repeated at lines 21, 31 and 1248) | Same wrong module path: package was renamed from agent_gateway to knowledge_service; 'agent_gateway' exists only in these docstrings. | 高 |
| `src/services/knowledge/__init__.py:15` | Docstring: 'embedding.py / vlm_service.py — kept as shared utilities ... Both files are byte-identical with their kb-service counterparts.' | Only half true now: vlm_service.py is byte-identical (diff = 0) but embedding.py has diverged — 464 diff lines vs apps/knowledge-service embedding.py (1767 vs 2031 lines). The 'byte-identical' clai... | 中 |

#### 超大文件

| 文件 | 行数 | 问题 | 拆分建议 |
|---|---|---|---|
| `apps/knowledge-service/src/knowledge_service/persistence/database.py` | 9559 | CRITICAL — repo's largest file. One DatabaseStorage class (line 218) mixing raw-SQL CRUD for ~20 entities (datasets, documents, segments, permissions, api_ke... | Split into persistence/crud/{datasets,documents,segments,permissions,usage}.py plus a dedicated persistence/migrations.py for the ~40 migration-runner method... |
| `apps/knowledge-service/src/knowledge_service/services/knowledge/confluence/sync_service.py` | 3903 | CRITICAL — single ConfluenceSyncService class with ~30 methods (full/partial sync, page upsert, image generation, deletion sweeps, attachment handling). Addi... | Do not refactor: delete the new-service copy as part of the confluence dedup (keep the gateway's live copy, or wire exactly one copy into the service and del... |
| `apps/knowledge-service/src/knowledge_service/services/knowledge/retrieval_service.py` | 3483 | CRITICAL — RetrievalService (line 457) plus ~400 lines of validation helpers (require_shadow_only_dataset, _require_bounded_*), fusion, image-candidate filte... | Extract validation guards to retrieval_validation.py, fusion/rerank composition to retrieval_fusion.py, and keep the pipeline class in retrieval_service.py. |
| `apps/knowledge-service/src/knowledge_service/api/routes/knowledge.py` | 3419 | CRITICAL — one APIRouter with ~60 endpoints across datasets, documents, segments, retrieval, QA and eval, plus ~340 lines of module-level guard helpers. | Split into routes/datasets.py, routes/documents.py, routes/segments.py, routes/retrieval.py, routes/qa.py sharing a routes/_guards.py; mount under the same /... |
| `apps/knowledge-service/src/knowledge_service/services/knowledge/chunking.py` | 3416 | CRITICAL — all 9 chunking strategies (FixedSize, Paragraph, Page, Heading, Regex, Separator, Recursive, Hierarchical, Automatic), ChunkingConfig, and process... | Split per-strategy files under chunking/ (e.g. chunking/strategies/*.py), move config to chunking/config.py, keep process_document orchestration in chunking/... |
| `apps/knowledge-service/src/knowledge_service/services/knowledge/vector_store.py` | 3180 | CRITICAL — VectorStore class (line 97) mixing collection lifecycle (create/rename/delete/backup), Qdrant search, BM25/lexical cutover logic and payload schem... | Split collection admin (vector_store/admin.py), search (vector_store/search.py), and payload mapping (vector_store/payload.py). |
| `apps/knowledge-service/src/knowledge_service/services/knowledge/ingestion_service.py` | 2138 | CRITICAL — ingestion pipeline plus extracted-text budget fences (_require_extracted_text_budget) and image-extraction orchestration in one file. | Extract the budget/fence helpers into ingestion_service/budgets.py and image handling into ingestion_service/images.py (mirroring services/knowledge/ingestio... |
| `apps/knowledge-service/src/knowledge_service/services/knowledge/embedding.py` | 2031 | CRITICAL — all embedding providers (dashscope/gemini/siliconflow), BaseEmbedding, create_embedding factory and multimodal embedding in one module; also diver... | Split per-provider clients (embedding/providers/*.py) with a factory in embedding/factory.py; pick one tree as canonical and re-export from the other. |
| `apps/knowledge-service/src/knowledge_service/services/knowledge/document_service.py` | 1793 | CRITICAL — DocumentService mixing upload handling, document CRUD, versioning, text/URL creation and generation fencing. | Split upload/ingestion flow from document CRUD and versioning into document_service/upload.py and document_service/crud.py. |
| `apps/knowledge-service/src/knowledge_service/services/knowledge/knowledge_service.py` | 1777 | CRITICAL — KnowledgeService facade that delegates to retrieval/chunking/embedding managers but still holds ~1000 lines of its own logic (dataset ops, documen... | Move the remaining inline logic into the manager modules it already delegates to (retrieval_service, chunking_manager, embedding_manager) and keep the facade... |
| `apps/knowledge-service/src/knowledge_service/services/knowledge/worker.py` | 1694 | CRITICAL — KnowledgeWorker combining queue polling/lease logic, per-document processing steps (text OCR, VLM, PDF split), and generation-fence enforcement. | Split queue/lease machinery (worker/queue.py) from document processing steps (worker/steps.py). |
| `apps/knowledge-service/src/knowledge_service/services/knowledge/dataset_service.py` | 1404 | LARGE — DatasetService mixing dataset CRUD, permission management, BM25/lexical config validation and stats. | Extract permissions into dataset_service/permissions.py and lexical-config transition logic into dataset_service/lexical.py. |

#### 模块备注(交叉发现)

- DUPLICATION VERDICT (apps/knowledge-service vs src/services/knowledge): the extraction is only half-complete. The legacy tree is NOT dead — it is still the runtime-authoritative Confluence stack: src/api/v1/confluence.py -> src/services/knowledge/confluence/sync_service.py (3297 lines) which lazily imports ..embedding.create_embedding (line 1145) and via confluence/image_processor.py imports ..vlm_service.DashScopeVLMService (lines 33/809). Meanwhile the new service carries a byte-for-byte copy of client.py/models.py/parser.py/scheduler.py and near-copy of image_processor.py/sync_service.py (diverged by 1592 diff lines) that NOTHING in the new service imports at runtime — only tests. So the ~7900-line confluence stack exists twice with different drift; gateway copy live, service copy dead.  【复核修正见 §7.2】
- The gateway's own comment contradicts reality: src/main.py:647-655 says 'Phase K5c: Confluence scheduler + sync-service moved to knowledge-service ... All of that runs inside apps/knowledge-service (see its lifespan + docker-compose)'. But apps/knowledge-service/src/knowledge_service/main.py contains zero confluence references (grep -c confluence = 0) and never sets app.state.confluence_sync_service (routes/knowledge.py:2847 getattr falls back to None), so the service-side copy is unwired and the gateway still runs legacy confluence in-process. The K5c migration evidently did not ship for the service.
- Migration/schema path regression from the extraction: persistence/database.py resolves migrations via Path(__file__).parent.parent.parent / 'database' / 'migrations' -> apps/knowledge-service/src/database/migrations, which does not exist (verified by resolving the path; the app ships migrations in apps/knowledge-service/migrations/ and the full set in repo-root database/migrations/). Every _auto_apply_* helper (005/006/007/.../033/034) therefore silently no-ops ('Migration not found' or, for 005_account_permission_system.sql, raises RuntimeError) on fresh installs. Copy-paste one-level-off from the gateway's src/persistence pattern where parent.parent.parent == repo root.
- Legacy tree residual status: only confluence/ + embedding.py + vlm_service.py are alive (embedding/vlm only via lazy imports from confluence). src/services/knowledge/kb_proxy_client.py and the package __init__.py are dead; vlm_service.py is byte-identical in both trees (diff = 0) while embedding.py has diverged (464 diff lines).
- 7 unimported service modules (contextual_retrieval, enhanced_ingestion, image_processing_queue, multilingual_embedding, processing_dispatcher, processor_factory, section_extractor; ~2,700 lines) were added in commits 'fix: review hardening' / 'chore: prepare standalone open-source gateway' and never wired; processing_dispatcher even references a nonexistent knowledge_service._ingest_document_internal (per reports/code-review/AI_PLATFORM_MULTI_AGENT_REVIEW_2026-08-02.md).
- apps/knowledge-service has no tests/ directory despite pyproject.toml [tool.pytest.ini_options] testpaths=['tests']; the service's tests live in the monorepo tests/services/knowledge/ instead.
- Minor hygiene: config/settings.py (4 lines) is a re-export shim over config/__init__.py where Settings actually lives — two files for one class; api/router.py's /retrieve surface (embedding cache, qdrant search, worker status) duplicates functionality in routes/knowledge.py and is unmounted.
- The dead api/router.py endpoint /{dataset_id}/retrieve re-implements embedding + qdrant search inline instead of using the live retrieval_service path — if it is ever mounted it will diverge from the authoritative retrieval pipeline (fusion, reranking, fences).

### 3.8 gateway-core

死代码 13 | 过时注释 7 | 错误注释 2 | 超大文件 7

#### 死代码

| 位置 | 符号 | 类型 | 证据 | 置信度 |
|---|---|---|---|---|
| `packages/ai-gateway-core/src/ai_gateway_core/persistence/repositories/user_repository.py:1` | user_repository.py (UserRepository, DatabaseUserRepository) | 未使用文件(整文件死) | Only reachable through the never-read DatabaseStorage.repos dict (persistence/database.py:186) and the barrel persistence/repositories/__init__.py, whose sole consumer is the dead shim src/persistence/repositories/__i... | 高 |
| `packages/ai-gateway-core/src/ai_gateway_core/persistence/repositories/session_repository.py:1` | session_repository.py (SessionRepository, DatabaseSessionRepository) | 未使用文件(整文件死) | Same dead cluster as user_repository: only imported by database.py:178 (repos dict, never read) and repositories/__init__.py barrel -> dead shim src/persistence/repositories/__init__.py. No usage in src/, apps/*, or t... | 高 |
| `packages/ai-gateway-core/src/ai_gateway_core/persistence/repositories/knowledge_repository.py:1` | knowledge_repository.py (KnowledgeRepository, DatabaseKnowledgeRepository) | 未使用文件(整文件死) | Same dead cluster: database.py:176+188 populates repos['knowledge'] but .repos is never read anywhere; barrel -> dead shim. No usage in src/, apps/*, tests. | 高 |
| `packages/ai-gateway-core/src/ai_gateway_core/persistence/repositories/task_repository.py:1` | task_repository.py (TaskRepository, DatabaseTaskRepository) | 未使用文件(整文件死) | Same dead cluster: database.py:179+185 populates repos['tasks'] (never read); barrel -> dead shim src/persistence/repositories/__init__.py which no one imports. No production or test usage. | 高 |
| `packages/ai-gateway-core/src/ai_gateway_core/persistence/repositories/api_key_repository.py:1` | api_key_repository.py (APIKeyRepository, DatabaseAPIKeyRepository) | 未使用文件(整文件死) | Same dead cluster: database.py:175+187 populates repos['api_keys'] (never read); barrel -> dead shim. No usage in src/, apps/*, tests. | 高 |
| `packages/ai-gateway-core/src/ai_gateway_core/persistence/repositories/service_repository.py:1` | service_repository.py (ServiceRepository, DatabaseServiceRepository) | 未使用文件(整文件死) | Same dead cluster: database.py:177+183 populates repos['services'] (never read); barrel -> dead shim. No usage in src/, apps/*, tests. | 高 |
| `packages/ai-gateway-core/src/ai_gateway_core/persistence/repositories/__init__.py:1` | repositories/__init__.py barrel (BaseRepository, 6 dead repo classes, DatabaseAgentRepository, etc.) | 未使用文件(整文件死) | The barrel exists solely to serve src/persistence/repositories/__init__.py, a back-compat shim that nothing imports (rg over src, apps, tests finds no import of src.persistence.repositories). External consumers import... | 中 |
| `packages/ai-gateway-core/src/ai_gateway_core/persistence/database.py:182` | DatabaseStorage.repos dict | 死配置开关/字段 | self.repos = {...} instantiates the six dead repositories but no code anywhere (src, apps, tests, or the package) ever reads .repos — rg for '.repos\b' finds only the assignment at line 182. Comment at line 174 claims... | 高 |
| `packages/ai-gateway-core/src/ai_gateway_core/image/image_state.py:670` | claim_next_image_tasks | 未使用函数 | Defined at line 670 and re-exported in image/__init__.py __all__; rg across the whole repo (excluding defining file) finds zero callers in src/, apps/*, or tests. | 高 |
| `packages/ai-gateway-core/src/ai_gateway_core/events/__init__.py:47` | GROUP_NAMES | 死配置开关/字段 | Defined and exported in events/__init__.py but never referenced anywhere in the repo (rg 'GROUP_NAMES' -> only the defining file, including zero test usage). The consumer docstring advertises it as the default group-n... | 高 |
| `packages/ai-gateway-core/src/ai_gateway_core/config/__init__.py:22` | DASHSCOPE_DEFAULT_LEGACY_BASE_URL | 死配置开关/字段 | Back-compat alias created 'so old import sites don't break during the upgrade window' — no import site anywhere references it (rg repo-wide -> only the definition and __all__). The upgrade window has closed; alias is ... | 高 |
| `packages/ai-gateway-core/src/ai_gateway_core/__init__.py:8` | __version__ = "0.1.0" | 死配置开关/字段 | Package version constant contradicts pyproject.toml version = "0.1.1" and is read by nobody (rg repo-wide finds no consumer). Either bump to 0.1.1 or delete. | 高 |
| `packages/ai-gateway-core/src/ai_gateway_core/security/safe_fetch.py:380` | validate_callback_url | 未使用函数 | Zero production callers; only tests/services/assistant/test_safe_fetch.py exercises it. Re-exported through security/__init__.py __all__ but no src/ or apps/ consumer exists. | 中 |

#### 过时注释

| 位置 | 注释(摘录) | 问题 | 置信度 |
|---|---|---|---|
| `packages/ai-gateway-core/README.md:10` | `auth/`, `persistence/`, `session/`, `metrics/`, `storage/`, `knowledge/` — `typing.Protocol` contracts only; concrete implementations li... | Since Phase 5f Batch C the package owns concrete implementations: DatabaseStorage (~7.3k LOC), RedisStorage, UsageRecorder, RealtimeMetricsService, S3/OSS/file storage backends, skill parser. The '... | 高 |
| `packages/ai-gateway-core/README.md:15` | - No FastAPI, httpx, SQLAlchemy, asyncpg instantiation belongs here (Protocols are fine). / - No runtime dependencies unless absolutely n... | Contradicted by pyproject.toml which now declares fastapi, httpx, asyncpg, redis, aioboto3, oss2, Pillow, numpy and OpenTelemetry SDK/exporters as required runtime deps, and by code that instantiat... | 高 |
| `packages/ai-gateway-core/README.md:18` | See `plans/Assistant-Service-True-Isolation-Plan.md` for the migration context. | Referenced file does not exist anywhere in the repo (find -> no match). Dead documentation link; migration context now lives in per-module docstrings. | 高 |
| `packages/ai-gateway-core/src/ai_gateway_core/storage/artifact_storage.py:28` | # Phase 5f Batch C will move ``DatabaseStorage`` into ai_gateway_core. Until then keep the static-analysis hint pointing at the gateway l... | Phase 5f Batch C has shipped: persistence/__init__.py documents DatabaseStorage moved into ai_gateway_core, and src/persistence/database.py is now a back-compat shim. The TYPE_CHECKING import 'from... | 高 |
| `packages/ai-gateway-core/src/ai_gateway_core/persistence/database.py:174` | # Domain-specific repositories (Phase 2 refactoring) | Describes the self.repos dict (six repository instances) as an active design, but nothing ever reads .repos — the six repository modules are dead (see dead-code findings). Comment describes behavio... | 高 |
| `packages/ai-gateway-core/src/ai_gateway_core/metrics/__init__.py:3` | The concrete recorders (Prometheus/Redis/DB-backed) live per-service; assistant code talks to them via these lightweight Protocols. | No longer true: the package now ships concrete DB-backed UsageRecorder (metrics/usage_recorder.py), RealtimeMetricsService, and ContextMetricsCollector, and get_usage_recorder/get_realtime_metrics ... | 高 |
| `packages/ai-gateway-core/src/ai_gateway_core/persistence/database.py:136` | Acts as a facade over domain-specific repositories. See ``src/persistence/repositories/`` for the extracted implementations. | The extracted implementations live at packages/ai-gateway-core/src/ai_gateway_core/persistence/repositories/; src/persistence/repositories/ now contains only a dead shim that nobody imports. Docstr... | 高 |

#### 错误注释

| 位置 | 注释(摘录) | 错误之处 | 置信度 |
|---|---|---|---|
| `packages/ai-gateway-core/src/ai_gateway_core/session/database_manager.py:19` | # RedisStorage still lives in gateway src/persistence/redis.py and is only used when callers wire a Redis client (gateway does, AS does n... | Factually wrong on both counts: RedisStorage now lives in ai_gateway_core/persistence/redis.py (moved in Phase 6 hot-fix, 2026-04-28), and src/persistence/redis.py is only a back-compat shim. The i... | 高 |
| `packages/ai-gateway-core/src/ai_gateway_core/metrics/realtime_metrics.py:26` | # RedisStorage still lives in gateway src/persistence/redis.py — runtime contract is duck-typed, so Any keeps this importable from AS wit... | Same stale claim: RedisStorage is a concrete class in ai_gateway_core/persistence/redis.py; the gateway src/persistence/redis.py is a shim. The comment names a module path that no longer holds the ... | 高 |

#### 超大文件

| 文件 | 行数 | 问题 | 拆分建议 |
|---|---|---|---|
| `packages/ai-gateway-core/src/ai_gateway_core/persistence/repositories/agent_repository.py` | 7540 | CRITICAL: DatabaseAgentRepository class spans lines 574-7540 (~90 methods: CRUD, drafts, releases, deletion lifecycle, ACL resolution) plus 12 error classes ... | Split by subdomain: agent_spec.py (spec helpers + validation, lines 198-522), agent_repository_errors.py (error classes), agent_repository_crud.py / agent_re... |
| `packages/ai-gateway-core/src/ai_gateway_core/persistence/database.py` | 7322 | CRITICAL: DatabaseStorage god class from line 132 to EOF with 251 methods covering schema bootstrap, permission cache, api-key usage buffering, billing event... | Extract cohesive method groups into mixins or separate modules (schema.py, permission_cache.py, api_key_usage.py, billing.py) each receiving the pool; Databa... |
| `packages/ai-gateway-core/src/ai_gateway_core/persistence/repositories/agent_trace_repository.py` | 3396 | CRITICAL: AgentTraceRepository (line 358+) mixes trace CRUD, versioned gate-metrics aggregation, paired-bootstrap CI statistics, live-case aggregation, and e... | Extract stats/aggregation helpers to a separate analytics module (e.g. trace_metrics.py) and split AgentTraceRepository into TraceRepository + TraceMetricsRe... |
| `packages/ai-gateway-core/src/ai_gateway_core/eval/evaluator_executor.py` | 1991 | CRITICAL: One module holds the full eval pipeline: rule engine (~25 helpers), LLM scoring (JSON parsing, heuristic fallback), target resolution, trajectory s... | Split into scoring_rules.py (pure rule functions lines 217-521), llm_scoring.py (prompt/parse/heuristic), and evaluator_executor.py keeping orchestration only. |
| `packages/ai-gateway-core/src/ai_gateway_core/metrics/usage_recorder.py` | 1940 | CRITICAL: UsageRecorder class (line 134) mixes token accounting, cost calculation via pricing catalog, event-bus dual-write, batch flush, daily aggregation, ... | Extract cost calculation into billing/cost_math.py, aggregation into usage_aggregator.py, and keep UsageRecorder as the orchestration facade with persistence... |
| `packages/ai-gateway-core/src/ai_gateway_core/persistence/repositories/mcp_repository.py` | 1561 | CRITICAL: DatabaseMCPRepository (line 293) plus DatabaseMCPAgentCapabilityResolver (line 1443, used by src/main.py) plus schema-diff/backward-compat validati... | Move the capability resolver (lines 1443-1561) into its own mcp_capability_resolver.py and the schema helpers (lines 67-219) into mcp_schema.py. |
| `packages/ai-gateway-core/src/ai_gateway_core/storage/image_storage.py` | 1527 | CRITICAL: Five classes in one file — StorageBackend enum, ImageUploadParams, StorageConfig, BaseStorageBackend, LocalStorageBackend, S3StorageBackend, OSSSto... | Split per-backend: s3_backend.py, oss_backend.py, local_backend.py, and keep image_storage.py for the service + config. Mirrors the sibling artifact_storage/... |

#### 模块备注(交叉发现)

- Dead-cluster summary: DatabaseStorage.repos dict (database.py:174-188) + six repository modules (user/session/knowledge/task/api_key/service, ~2,074 LOC) + repositories/__init__.py barrel + src/persistence/repositories/__init__.py shim form a fully disconnected chain — nothing anywhere imports src.persistence.repositories, and .repos is never read. The shim was presumably kept for legacy import sites that no longer exist.
- Duplication: MULTIMODAL_EMBEDDING_MODELS / is_multimodal_embedding_model exist in four places — gateway-core knowledge/utils.py, src/services/knowledge/embedding.py, apps/knowledge-service/.../embedding.py, and a private copy in apps/assistant-service/api/routes/models.py — and none of the three service copies imports the gateway-core one. The gateway-core export is dead; the duplication itself is a maintenance hazard.
- Version drift: packages/ai-gateway-core/__init__.py __version__ = '0.1.0' vs pyproject.toml version = '0.1.1'; no consumer reads either, so the in-code constant is pure drift.
- src/services/metrics/{usage_recorder,realtime_metrics,observability}.py are back-compat shims re-exporting from ai_gateway_core (used by src/proxy/billing_interceptor.py, transparent_proxy.py, etc.) — consistent with the extraction pattern but worth auditing for final removal alongside the dead repositories shim.
- The README ('Protocol contracts only', 'must stay lean') is stale relative to the package's current ~50k LOC and heavy deps (asyncpg, aioboto3, oss2, Pillow, numpy, redis, OpenTelemetry); it should be rewritten to document the now-concrete ownership before it misleads the next refactor.
- artifact_storage.py's TYPE_CHECKING import of src.persistence.database (via shim) would break under strict type checking since the canonical module is ai_gateway_core.persistence.database — part of the same shim-cleanup sweep.
- Cross-cutting: agent_repository.py and database.py are each ~7.3-7.5k LOC with documented failed extraction attempts (the repos dict comment); the planned split for the monolith-to-packages migration should treat these two files as the top refactor targets.

### 3.9 docgen (packages/mcp-docgen-server)

死代码 20 | 过时注释 8 | 错误注释 5 | 超大文件 0

#### 死代码

| 位置 | 符号 | 类型 | 证据 | 置信度 |
|---|---|---|---|---|
| `packages/mcp-docgen-server/src/mcp_docgen_server/server.py:56` | SamplingLLMCaller | 未使用类 | Repo-wide rg finds only the class definition (line 56) — never instantiated or imported anywhere in src or tests. A prior code-review report (reports/code-review/AI_PLATFORM_MULTI_AGENT_REVIEW_2026-08-02.md) flagged i... | 高 |
| `packages/mcp-docgen-server/src/docgen/renderers/dispatcher.py:36` | RendererDispatcher.fix | 未使用方法 | rg repo-wide (incl. tests) finds zero callers of `.fix(` on the dispatcher. All four renderers' fix() methods are trivial 'return await self.render(ir, out_dir)' stubs (docx_renderer.py:217, pptx_renderer.py:144, xlsx... | 高 |
| `packages/mcp-docgen-server/src/docgen/renderers/primitives.py:252` | Primitives.background_geometry | 未使用方法 | Zero callers repo-wide (src + tests). Only consumers were set_rotation/set_transparency (pptx_effects.py), which are themselves only called from this dead method. | 高 |
| `packages/mcp-docgen-server/src/docgen/renderers/primitives.py:71` | Primitives.gradient_rect | 未使用方法 | Zero callers repo-wide. Its only consumer set_linear_gradient_fill (pptx_effects.py) is likewise only called from dead code. | 高 |
| `packages/mcp-docgen-server/src/docgen/renderers/primitives.py:165` | Primitives.background_gradient | 未使用方法 | Zero callers repo-wide (src + tests). | 高 |
| `packages/mcp-docgen-server/src/docgen/renderers/primitives.py:204` | Primitives.section_numeral | 未使用方法 | Zero callers repo-wide (src + tests). | 高 |
| `packages/mcp-docgen-server/src/docgen/design_system.py:158` | ShapeTokens | 未使用类 | Dataclass (shadow_blur_emu, shadow_dist_emu, shadow_alpha_per_mille, shadow_dir_deg, radius_sm, radius_md) never read by any renderer repo-wide; primitives.py hardcodes blur=40_000 / dist=23_000 / alpha=18_000. ShapeT... | 高 |
| `packages/mcp-docgen-server/src/docgen/templates.py:97` | TemplateRegistry.unregister | 未使用方法 | Never called anywhere in the repo, including tests (rg verified). | 高 |
| `packages/mcp-docgen-server/src/docgen/design_system.py:80` | contrast_ratio | 未使用方法 | Zero usages repo-wide (src + tests). | 高 |
| `packages/mcp-docgen-server/src/docgen/design_system.py:182` | DesignSystem.slide_frame | 未使用方法 | Zero usages repo-wide (src + tests). | 高 |
| `packages/mcp-docgen-server/src/docgen/renderers/pptx_effects.py:128` | set_stroke | 未使用方法 | Zero callers repo-wide (src + tests). | 高 |
| `packages/mcp-docgen-server/src/docgen/renderers/layouts/blocks.py:181` | draw_table | 未使用方法 | Zero callers repo-wide (src + tests); pptx tables are drawn inline in pptx_renderer instead. | 高 |
| `packages/mcp-docgen-server/src/docgen/sandbox/docker_backend.py:27` | DockerSandbox | 未使用类 | Never imported outside its own module plus the sandbox/__init__.py re-export; the whole sandbox layer (LocalSubprocessSandbox, DockerSandbox) has no production consumers — no src module calls exec_node/exec_python. | 中 |
| `packages/mcp-docgen-server/src/docgen/storage/s3.py:24` | S3ArtifactStore | 未使用类 | Never instantiated outside own module + storage/__init__.py re-export; boto3 is not a pyproject dependency, so the class could not run as written. | 中 |
| `packages/mcp-docgen-server/src/docgen/quality/visual_verifier.py:54` | FreshContextVisionCritic + default_vision_critic | 未使用类 | Only exercised by own-module tests; production never passes a critic to DocgenService (all three transports construct it without one). Also broken at runtime: calls client.messages.create synchronously on AsyncAnthrop... | 中 |
| `packages/mcp-docgen-server/src/docgen/ir/root.py:52` | parse_ir | 未使用函数 | Only used by tests (tests/docgen/ir/); no src caller. IR deserialization goes through pydantic model_validate everywhere else. | 中 |
| `packages/mcp-docgen-server/src/docgen/service.py:110` | DocgenService.stream | 未使用方法 | Only referenced by tests (tests/docgen/storage/test_storage.py:207,221); the MCP server uses generate() exclusively. system_prompt_stub (line 89) is likewise test-only via stream(). | 中 |
| `packages/mcp-docgen-server/src/docgen/ir/base.py:38` | HexColor.with_hash | 未使用方法 | Zero usages repo-wide (src + tests); colors are emitted via str() and f-strings. | 中 |
| `packages/mcp-docgen-server/src/docgen/templates.py:32` | Template.header_text / cover_treatment fields | 未使用变量 | No renderer ever reads these fields (docx header comes from DocxContent.header); they are only set in the enterprise-dark builtin and the parser — pure dead data. Docstring claim they render is false. | 中 |
| `packages/mcp-docgen-server/src/mcp_docgen_server/tool_schema.py:90` | GenerateDocumentOutput / output_json_schema | 未使用类 | Only used by tests; server.py's actual result dict carries artifact_id, download_url, filename, format, size_bytes, sha256, plan_outline, critic_passed, used_llm — fields this schema lacks (format, used_llm), so the s... | 中 |

#### 过时注释

| 位置 | 注释(摘录) | 问题 | 置信度 |
|---|---|---|---|
| `packages/mcp-docgen-server/src/mcp_docgen_server/server.py:3` | "# TODO(agent-A): ensure pyproject.toml [project.dependencies] includes: \"mcp>=2.0,<3\" ... These are listed as hard deps because the en... | Contradicts the actual pyproject.toml: mcp/starlette/uvicorn live in [project.optional-dependencies] under the mcp and dev extras (pyproject.toml:34-45), not in [project.dependencies] (lines 13-27)... | 高 |
| `packages/mcp-docgen-server/src/docgen/pipeline.py:3` | Docstring: "Phase-2 flow — Planner → IR → Renderer. Phase 3 will add the Verifier loop around this." | The verifier loop has shipped: run() calls verify_and_fix with critic/structural verification, and quality/ has an entire verifier package. Also line 7 claims "This module is what the assistant-ser... | 高 |
| `packages/mcp-docgen-server/src/docgen/__init__.py:2` | Module docstring points to "plans/Document-Generation-SOTA-Upgrade-Plan-2026-04-21.md" | The file and the plans/ directory do not exist anywhere in the repo (verified with repo-wide rg — the only hits are this file and the assistant-service fork's copy). Dangling reference. | 高 |
| `packages/mcp-docgen-server/src/mcp_docgen_server/__init__.py:10` | Package docstring lists transports as "stdio (default ...) / sse (deployment ...)" | Omits the http transport (main_http) that was added and is used by the AI gateway via ai-gateway and MCP HTTP. The docstring no longer matches the server's transport surface. | 中 |
| `packages/mcp-docgen-server/src/docgen/design_system.py:7` | Module docstring: "Four built-in systems, each a complete design: stripe, carbon, keynote, editorial, enterprise" | Lists five names while claiming four, and the registry actually has six built-ins (claude was added later). Count and roster both stale. | 高 |
| `packages/mcp-docgen-server/tests/mcp/test_server_stdio.py:16` | Comment: "the ``docgen`` package isn't importable (Agent A still running)" | The docgen package has long since landed (the test imports it and the pyproject packages it under src/); the workaround rationale is stale. | 中 |
| `packages/mcp-docgen-server/src/docgen/renderers/docx_renderer.py:9` | Docstring references "scripts/node_renderers/docx_render.js" and "wired through the sandbox client in Phase 2" | The scripts/node_renderers/ directory does not exist anywhere in the repo, and no node-based renderer was ever wired in; rendering is pure python-docx/reportlab. Phase-2 plan never happened. | 中 |
| `packages/mcp-docgen-server/src/mcp_docgen_server/tool_schema.py:91` | GenerateDocumentOutput docstring describes the server's tool result shape | The schema omits fields the server actually returns (format, used_llm) — the documented contract drifted from the real result dict in server.py (artifact_id, download_url, filename, format, size_by... | 中 |

#### 错误注释

| 位置 | 注释(摘录) | 错误之处 | 置信度 |
|---|---|---|---|
| `packages/mcp-docgen-server/src/docgen/design_system.py:16` | Docstring: "6 type tokens (eyebrow / display / h1 / h2 / lead / body / caption)" | Lists seven names while claiming six, and TypeScale actually has nine fields (eyebrow, display, h1, h2, lead, body, caption, stat, footnote). Both the count and the enumeration are wrong. | 高 |
| `packages/mcp-docgen-server/src/mcp_docgen_server/llm_caller.py:38` | DashScopeLLMCaller docstring: "Falls back to parsing plain text if the model returns raw JSON without the wrapper." | There is no fallback: on JSONDecodeError the code raises RuntimeError (line 116, with a comment explaining the planner treats it as LLM unavailable). Verified by reading the function body. | 高 |
| `packages/mcp-docgen-server/src/docgen/renderers/__init__.py:8` | Package docstring: "``fix`` ... applies targeted edits — typically by patching the IR and re-rendering only the affected pages / slides" | All four fix() implementations are trivial 'return await self.render(ir, out_dir)' stubs — no targeted edits, full re-render. The described behavior does not exist. | 中 |
| `packages/mcp-docgen-server/src/docgen/templates.py:23` | Docstring: "``header_text`` is what shows up in the DOCX header / PPT master footer / PDF running header" | No renderer reads header_text: the DOCX header is populated from DocxContent.header, and pptx/pdf renderers never touch it. The field never shows up anywhere. | 中 |
| `packages/mcp-docgen-server/src/docgen/quality/__init__.py:6` | Docstring: "no artifact leaves this layer without at least one fix-and-verify round" | The pipeline runs verify_and_fix only when the --verify flag is set (pipeline.run(verify=...)); with verify=False — the default in the MCP server — artifacts leave with zero verify passes, contradi... | 中 |

#### 模块备注(交叉发现)

- No file in this module reaches the >=1000-line oversized threshold: largest hand-written file is server.py (646 lines), then design_system.py (568), layout_rules.py (459). The 847-line vendored validators/base.py (below threshold) is worth splitting only as part of de-duplication below.
- 3x byte-identical duplication in _skills_data: docx/pptx/xlsx each vendored a copy of scripts/office/ (12 files per copy, md5-identical, incl. validators/base.py 847 lines x3, soffice.py, pack.py, unpack.py, validate.py, helpers/merge_runs.py, helpers/simplify_redlines.py) — roughly 4,700 duplicated lines. A third divergent variant exists at apps/assistant-service/src/assistant_service/core/skills (56 files, different md5s). Vendored skill content should be a single shared source, not three copies.
- Whole-package fork: apps/assistant-service/src/assistant_service/core/docgen/ (58 py files) is a fork of packages/mcp-docgen-server/src/docgen, and tests/assistant/docgen/ (11 test files + golden/) is an import-rewritten clone of packages/mcp-docgen-server/tests/docgen (diff shows only docgen -> assistant_service.core.docgen import changes). The forks are already diverging (different skills path resolution, Optional vs |None typing). A single shared source of truth would eliminate the drift; docgen package is the canonical copy.
- The entire sandbox layer (docgen/sandbox/: docker_backend.py, local_subprocess.py, node_runner.py, python_runner.py) has zero production consumers — no src module calls exec_node/exec_python. LocalSubprocessSandbox is exercised only by its own tests. Either wire it into the pipeline or delete the layer.
- The vision critic path is unwired: DocgenService is constructed with critic=None in all three transports (main_stdio/main_sse/main_http), so only the structural verifier runs; the anthropic-based FreshContextVisionCritic plus the 'vision' optional extra (pyproject.toml:30) is latent. FreshContextVisionCritic is additionally broken (sync client.messages.create on AsyncAnthropic) and would fail if ever enabled.
- A prior review (reports/code-review/AI_PLATFORM_MULTI_AGENT_REVIEW_2026-08-02.md) already flagged SamplingLLMCaller as dead and the main_sse llm=None default as suspicious; both remain unaddressed as of this scan.
- server.py duplicates the generate-document tool description three times (in code and 3x transport handlers) and the artifact-route handler is near-duplicated between main_sse and main_http — refactor targets for the next change touching that file.
- Build artifacts (egg-info/, __pycache__/, .pytest_cache/) exist on disk but are not git-tracked (git ls-files: 0 hits) — nothing to clean up there.
- The dead-code findings at dispatcher.py:36 and the four renderer fix() stubs are one connected dead subsystem: dispatcher.fix -> renderer.fix chain is unreachable from the pipeline (verify_and_fix patches IR and calls render again); the fix() methods and their dispatch wiring can be removed together.
- MemoryArtifactStore (storage/memory.py) is used only by tests, and LocalSubprocessSandbox only by its own tests — both would become dead once tests are restructured; left out of the dead-code list because removing them would break the test suite.

### 3.10 web

死代码 30 | 过时注释 2 | 错误注释 1 | 超大文件 12

#### 死代码

| 位置 | 符号 | 类型 | 证据 | 置信度 |
|---|---|---|---|---|
| `web/src/pages/Playground.legacy.tsx:1` | Playground.legacy.tsx | 未使用文件(整文件死) | 2244-line legacy chat page; router.tsx lazy-loads '@/pages/playground' instead. rg 'Playground.legacy' across web/src + web/e2e finds zero references outside the file itself. | 高 |
| `web/src/api/langgraph.ts:1` | langgraph.ts | 未使用文件(整文件死) | Entire 'LangGraph API client' module has zero importers in web/src and web/e2e; playground uses the @langchain/langgraph-sdk npm package (useLangGraphStream.ts, utils/langgraph.ts) instead. | 高 |
| `web/src/api/dashboard.ts:1` | dashboard.ts | 未使用文件(整文件死) | 'LangSmith-style Enterprise Monitoring' client has zero importers; live dashboard panels (pages/dashboard/components/panels/*) call api.get() directly against /api/v1/metrics. | 高 |
| `web/src/api/connectors.ts:1` | connectors.ts | 未使用文件(整文件死) | OAuth connectors client has zero references in web/src and web/e2e (rg 'api/connectors' returns nothing). | 高 |
| `web/src/pages/Dashboard.tsx:15` | DashboardPage | 未使用函数 | DashboardPage export is never imported; router.tsx uses EnterpriseDashboard from '@/pages/dashboard' (dir). Only 80 lines but roots the dead chain below. | 高 |
| `web/src/hooks/useAgentStream.ts:344` | useAgentStream | 未使用函数 | 717-line AG-UI streaming hook; the only repo reference is a comment at web/src/lib/sse.ts:431. Nothing imports it. | 高 |
| `web/src/components/agent/TaskResultCard.tsx:1` | TaskResultCard.tsx | 未使用文件(整文件死) | 719 lines; zero references repo-wide (only self and unused barrel components/agent/index.ts). | 高 |
| `web/src/components/agent/AgentStatusStream.tsx:1` | AgentStatusStream.tsx | 未使用文件(整文件死) | Zero references in web/src and web/e2e. | 高 |
| `web/src/components/agent/CitationDrawer.tsx:1` | CitationDrawer.tsx | 未使用文件(整文件死) | Zero references in web/src and web/e2e. | 高 |
| `web/src/components/agent/ErrorDisplay.tsx:1` | ErrorDisplay.tsx | 未使用文件(整文件死) | Zero references in web/src and web/e2e. | 高 |
| `web/src/components/agent/index.ts:1` | components/agent/index.ts barrel | 未使用文件(整文件死) | Dead barrel: no module imports it; live consumers (useAgentTimeline, usePlaygroundStream, chat/TimelineSection) import AgentTimeline/ArtifactCard by file path. | 高 |
| `web/src/components/chat/index.ts:1` | components/chat/index.ts barrel | 未使用文件(整文件死) | Dead barrel: no importers of 'components/chat'; ChatWindow imports ChatMessageItem directly. | 高 |
| `web/src/pages/playground/hooks/useLangGraphStream.ts:234` | useLangGraphStream | 未使用函数 | 719-line hook whose header claims 'SDK-based replacement for usePlaygroundStream' — but playground/index.tsx imports usePlaygroundStream; useLangGraphStream has zero references. | 高 |
| `web/src/pages/confluence/ConfluencePage.tsx:1` | ConfluencePage | 未使用文件(整文件死) | 1171 lines; only reference is the barrel re-export at pages/confluence/index.ts:13 commented '// Legacy page (to be deprecated)'. Not routed (router imports the other 4 pages only) and no direct importers. Caveat: the... | 中 |
| `web/src/components/ServiceCostAnalysis.tsx:1` | ServiceCostAnalysis.tsx | 未使用文件(整文件死) | Only importer is the dead pages/Dashboard.tsx. | 高 |
| `web/src/components/UserServiceUsageAnalytics.tsx:1` | UserServiceUsageAnalytics.tsx | 未使用文件(整文件死) | Only importer is the dead pages/Dashboard.tsx. | 高 |
| `web/src/components/SecurityEventCharts.tsx:1` | SecurityEventCharts.tsx | 未使用文件(整文件死) | Only importer is the dead pages/Dashboard.tsx. | 高 |
| `web/src/api/metrics.ts:1` | metrics.ts | 未使用文件(整文件死) | Only importer is SecurityEventCharts.tsx, itself dead (only pages/Dashboard.tsx). Live dashboard panels hit /api/v1/metrics/* via raw api.get, not this module. Medium: dead via an import chain rather than direct zero ... | 中 |
| `web/src/pages/dashboard/components/SummaryCharts.tsx:538` | SummaryCharts | 未使用函数 | Zero references; dashboard/index.tsx imports only DashboardContext, DashboardLayout, styles, useDashboardEntityLabels, types. | 高 |
| `web/src/pages/dashboard/components/KPICards.tsx:327` | KPICards | 未使用函数 | Zero references in web/src and web/e2e. | 高 |
| `web/src/pages/dashboard/components/DashboardSkeleton.tsx:83` | DashboardSkeleton | 未使用函数 | Zero references in web/src and web/e2e. | 高 |
| `web/src/pages/dashboard/components/DashboardHeader.tsx:28` | DashboardHeader | 未使用函数 | Zero references in web/src and web/e2e. | 高 |
| `web/src/pages/knowledge/components/ConfluenceBindingManager.tsx:489` | ConfluenceBindingManager | 未使用函数 | Zero references in web/src and web/e2e (589 lines). | 高 |
| `web/src/pages/knowledge/detail/HierarchicalSegmentCard.tsx:204` | HierarchicalSegmentCard | 未使用函数 | Zero references in web/src and web/e2e (541 lines). | 高 |
| `web/src/components/HealthBadge.tsx:1` | HealthBadge.tsx | 未使用文件(整文件死) | Zero references in web/src and web/e2e. | 高 |
| `web/src/components/TaskTable.tsx:1` | TaskTable.tsx | 未使用文件(整文件死) | Zero references in web/src and web/e2e. | 高 |
| `web/src/components/ui/slider.tsx:15` | Slider (ui/slider.tsx) | 未使用函数 | shadcn slider never imported (rg 'from "@/components/ui/slider"' = none); agents pages use antd Slider. | 高 |
| `web/src/pages/assistant/components/ProcessSummaryBar.tsx:1` | ProcessSummaryBar.tsx | 未使用文件(整文件死) | 295 lines; only reference is barrel re-export. ChatMessage.tsx header confirms it was 'replaced' by the ActivityPanel flow. Medium: re-exported by live barrel so still bundled. | 中 |
| `web/src/pages/assistant/components/SearchStatus.tsx:1` | SearchStatus.tsx | 未使用文件(整文件死) | 297 lines; zero references (search status now rendered inline in ActivityPanel; useChatSession only uses the SearchStatusItem type, not this component). | 中 |
| `web/src/api/assistant.ts:803` | getArtifact/deleteArtifact/getConversationShare/getSharedArtifactUrl/listMyShares/revokeShare/groupSessionsByDate | 未使用函数 | 7 exported functions never referenced in web/src or web/e2e: getArtifact(803), deleteArtifact(813), getConversationShare(872), getSharedArtifactUrl(891), listMyShares(895), revokeShare(900), groupSessionsByDate(735). ... | 高 |

#### 过时注释

| 位置 | 注释(摘录) | 问题 | 置信度 |
|---|---|---|---|
| `web/src/pages/assistant/components/ChatInputArea.tsx:8` | import { QuickActionsMenu } from "../components/QuickActionsMenu"; // Assume this exists or is moved | Stale refactor leftover: QuickActionsMenu does exist (QuickActionsMenu.tsx) and is rendered at ChatInputArea.tsx:198. The 'Assume this exists or is moved' caveat is obsolete. | 高 |
| `web/src/lib/sse.ts:570` | /** Convert legacy StreamChunk format to AG-UI event format. This helps migrate existing code that uses the old format. */ | Frames streamChunkToAGUIEvent as a temporary migration shim, but it is the permanent production path: usePlaygroundStream.ts:593 calls it for every playground stream. The 'migrate existing code' fr... | 中 |

#### 错误注释

| 位置 | 注释(摘录) | 错误之处 | 置信度 |
|---|---|---|---|
| `web/src/pages/assistant/components/ActivityTimeline.tsx:21` | /** Fallback: build from a ChatMessage. Used by share-page / legacy. */ | SharePage.tsx never imports ActivityTimeline or ActivityPanel (verified: no timeline component references in SharePage.tsx). The message prop is consumed by the assistant page's ActivityPanel (assi... | 高 |

#### 超大文件

| 文件 | 行数 | 问题 | 拆分建议 |
|---|---|---|---|
| `web/src/pages/knowledge/DatasetDetail.tsx` | 4854 | CRITICAL god page: single KnowledgeDatasetDetailPage component with 40+ inline handlers (upload, batch ops, RAGAS, hit-test, QA bench, settings) plus several... | Split by tab: documents/segments management, retrieval eval workbench (RetrievalEvalWorkbench already exists), dataset settings — into separate components/ho... |
| `web/src/pages/assistant/hooks/useChatSession.ts` | 2842 | CRITICAL god hook: single useChatSession handles SSE streaming, sub-agent events, quiz hydration, KB search status, working memory, tool approvals, image gen... | Extract sub-reducers already present (subagentEventReducer, features/chat/stream reducer) further: split quiz logic, RAG status handling and session persiste... |
| `web/src/pages/eval/index.tsx` | 2497 | CRITICAL god page: EvalPage mixes workbench layout, experiment runs comparison, behavior contract editor, RAGAS panel, trace explorer and golden-JSONL import... | Move the inline <style> CSS out to eval/styles.css and extract workbench orchestration into a hook; components/ already exist for each panel. |
| `web/src/pages/agents/agent-studio.css` | 2204 | LARGE CSS monolith: single stylesheet for all agent studio pages (list, create, studio, analytics, preview, release) with ad-hoc selector names; no lint or s... | Split per page/feature (agent-list.css, agent-studio.css, agent-analytics.css) or migrate to tokens.css/theme system. |
| `web/e2e/chat-experience.spec.ts` | 2018 | CRITICAL monolithic e2e spec: 21 tests covering assistant streaming, sub-agents, artifacts, approvals, telemetry, model-tester gating and playground in one f... | Split by concern (stream-protocol, subagents, artifacts/shares, telemetry, permissions) mirroring other spec files. |
| `web/src/pages/playground/hooks/usePlaygroundStream.ts` | 1884 | CRITICAL god hook: streaming, SSE fallback, legacy chunk conversion, tool-call reconciliation, history restore and session lifecycle all in one callback-heav... | Most pure parsing already lives in utils/langgraph.ts; extract fallback-recovery and session-restore phases into separate hooks. |
| `web/src/components/ServiceConfigDialog.tsx` | 1831 | CRITICAL god dialog: three configuration modes (legacy flat config, OAuth/proxy, self-hosted), model failover, load balancing, capacity status — one 1831-lin... | Split per mode into sub-components (LegacyConfigForm, OAuthConfigForm, SelfHostedConfigForm) sharing a validation core. |
| `web/src/pages/eval/styles.css` | 1476 | LARGE CSS monolith for the eval module; duplicates some classes inlined in eval/index.tsx. | Split per component (trace-explorer, run-comparison, ragas, contract-editor) and dedupe with the inline <style> blocks. |
| `web/src/pages/assistant/index.tsx` | 1333 | CRITICAL page mixing layout, panel state machine (rightPanel mutex), mobile drawer rendering, LocalOS panel wiring and portal rendering. | Extract the panel-mutex state machine (rightPanelContext) usage and mobile drawer into components; keep index.tsx as composition. |
| `web/src/pages/knowledge/DatasetCreate.tsx` | 1327 | CRITICAL 3-step wizard (basic info, data selection, index settings) in one file with all step bodies inline. | Split into per-step components (BasicInfoStep, DataSelectStep, IndexSettingsStep) under pages/knowledge/create/. |
| `web/src/pages/confluence/SyncedPages.tsx` | 1219 | CRITICAL page combining tree view, sync table, page-sync config dialog and binding status all inline. | Extract tree and sync-config dialog (PageSyncConfigDialog.tsx already exists) into separate components with a shared hook. |
| `web/e2e/eval-trace.spec.ts` | 1125 | CRITICAL single spec covering trace explorer, score panels, thread view, experiment comparison and golden import in one file. | Split by area (trace-list/detail, scoring, experiments) to match the components/ layout. |

#### 模块备注(交叉发现)

- Dead-code chain worth deleting as one group: pages/Dashboard.tsx -> ServiceCostAnalysis.tsx, UserServiceUsageAnalytics.tsx, SecurityEventCharts.tsx -> api/metrics.ts (all mutually dead; live dashboard uses pages/dashboard/ + raw api.get).
- Three parallel Confluence-sync UIs exist with overlapping feature sets: pages/confluence/* (BindSpace, SyncedPages, ConnectionList, PageSyncConfigDialog, BindingSyncConfigDialog), pages/knowledge/sync/* (AddConfluenceBindingDialog, BindingPagesPanel, SyncOverviewCards, SyncSourcesTab), and pages/tasks/* (BindingTable, ConfluenceSyncTab, ConnectionCard, PageManageDialog, SchedulerStatus). Candidates for consolidation.
- pages/assistant/components/index.ts barrel re-exports ~9 components that no page imports: PromptSuggestions, ModelSelector, KBSelector, CompactKBSelector, TaskPanel, ParallelExecutionView, WebSearchDisplay, WebSearchToggle, ProcessSummaryBar — remnants of the pre-ActivityPanel V1 UI, still shipped via the barrel.
- VITE_ASSISTANT_UI_V2 feature flag (assistant/index.tsx:73, ChatMessage.tsx:45, ChatInputArea.tsx:14, ArtifactsPanel.tsx:103) defaults to the V2 branch, is undocumented (absent from web/.env.example and all scripts/manifests), and its V1-only branches reference components that no longer exist as imports (e.g. TaskPanel at index.tsx:1017). Dead-flag candidate.
- Unused exported API-client surface in live modules: api/knowledge.ts 11 functions (getDocument, uploadImages, updateDocument, setDocumentEnabled, getSegment, setSegmentEnabled, qaBatchTest, getDatasetStatistics, getDatasetSources, batchEnableSegments, getDocumentVersion), api/files.ts 6 (uploadMultipleFiles, listFiles, getFileInfo, deleteFile, compressImage, isDocumentFile), api/confluence.ts 7 (importFromUrl, importSpace, getSyncStatus, listSyncTasks, getSyncTask, cleanupUnsyncedPages, getPageRecord), api/quiz.ts 4 (generateQuiz, generateQuizStream, deleteQuiz, revokeQuizShare), plus api/exams.ts updateExam, api/models.ts getModel/getAccessLevelColor, api/providers.ts getProvider, api/skills.ts getSkill/testSkill, api/users.ts getRole, api/gateway.ts submitService, api/eval.ts getEvalDataset/getEvalEvaluator.
- SSEEventType (pages/assistant/sse-events.ts) has 10 dead members never referenced in web/src: CACHE_METRICS, OUTPUT_WARNINGS, MEMORY_LOADED, SESSION_CREATED, SESSION_UPDATED, TOOL_ERROR, CANCELLED, WEB_SEARCH_RESULTS, FILE_PROCESSED, RAG_EVALUATION.
- e2e audit: all 22 specs reference live routes and current UI; agent-embed.spec.ts mocks /agent-embed-host and /embed/agents/* itself and depends on web/public/agent-embed.js, agent-widget.js, agent-embed.css which all exist. No dead specs found.
- Two oversized files flagged as dead code, so they should be deleted rather than refactored: pages/Playground.legacy.tsx (2244 lines) and pages/confluence/ConfluencePage.tsx (1171 lines, barrel-only re-export).
- Frontend mirrors the known backend duplication: DatasetDetail.tsx (4854 lines) targets both the legacy src/api/v1/knowledge endpoints and apps/knowledge-service endpoints (RetrievalEvalWorkbench vs RAGAS eval), which is part of why it is so large.

### 3.11 sdk

死代码 13 | 过时注释 9 | 错误注释 2 | 超大文件 1

#### 死代码

| 位置 | 符号 | 类型 | 证据 | 置信度 |
|---|---|---|---|---|
| `sdk/cli/src/agent/loop.ts:45` | runAgentLoop | 未使用文件(整文件死) | Whole file dead: repo-wide rg -F -l for 'agent/loop' finds no importers; cli.tsx and app.tsx never reference it. The agent loop (os_tools/tool_results body construction, permission gate) is unreachable from the live C... | 高 |
| `sdk/cli/src/agent/tool-executor.ts:36` | executeTool | 未使用文件(整文件死) | Only imported by the dead loop.ts (verified rg -F -l). No live path reaches it. | 高 |
| `sdk/cli/src/tools/index.ts:16` | OS_TOOL_DEFINITIONS / getOSToolSchemas / executeOSTool | 未使用文件(整文件死) | OS-tools layer (read-file, write-file, edit-file, glob, grep, bash, list-dir, tree, todo) is imported ONLY by the dead tool-executor.ts (verified rg -F -l). README still advertises the OS Agent. | 高 |
| `sdk/cli/src/hooks.ts:90` | runPreToolHooks / runPostToolHooks | 未使用文件(整文件死) | Only imported by dead tool-executor.ts (verified rg -F -l); nothing in the live CLI path uses them. | 高 |
| `sdk/cli/src/app.tsx:129` | handleConfirm + ConfirmState confirm flow | 不可达代码 | handleConfirm is defined but never invoked anywhere in app.tsx (it is in the useCallback dep array at line 647 but never called). The whole permission-confirmation flow — ConfirmState (line 72), confirm state, y/n key... | 高 |
| `sdk/python/ai_assistant/models/response.py:255` | ModelInfo | 未使用类 | Repo-wide rg: ModelInfo appears only in response.py and the models/__init__.py re-export (lines 17, 38). The SDK has no models-list method, and no module, demo, example, or test constructs it. | 中 |
| `sdk/dart/ai_gateway_sdk/lib/src/models.dart:214` | Session | 未使用类 | Dart Session is never instantiated: the package has no test/ directory, and example/example.dart uses only ChatResponse, StreamEvent, Artifact (verified by rg). | 中 |
| `sdk/dart/ai_gateway_sdk/lib/src/models.dart:268` | Message | 未使用类 | Same as Session: no tests exist and example.dart never constructs Message. | 中 |
| `sdk/cli/src/config.ts:46` | saveConfig / savePermissions / loadMCPServers / saveMCPServers + CONFIG_DIR / CONFIG_FILE / SESSIONS_DIR | 未使用函数 | rg -F -c per symbol shows zero uses outside config.ts itself (verified individually). loadConfig (line 43) is used; the save/load-MCP and session-dir surface is dead. | 高 |
| `sdk/cli/src/mcp/manager.ts:44` | callTool / isConnected / isMCPTool | 未使用方法 | grep -rn across sdk/cli: callTool is referenced only by dead loop.ts (which is itself unimported); isConnected and isMCPTool have zero external users. /mcp connect works but MCP tools can never be invoked from the liv... | 高 |
| `sdk/cli/src/permissions.ts:29` | reloadPermissions | 未使用函数 | rg -F -c: 0 uses outside permissions.ts. (validatePath/checkPermission survive only via the dead tools/ layer but the file stays alive through config.ts/skills.ts.) | 高 |
| `sdk/cli/src/memory.ts:51` | getMemoriesForPrompt | 未使用函数 | rg -F -c: 0 uses outside memory.ts; only saveMemory/listMemories are called (by app.tsx). | 高 |
| `sdk/cli/package.json:50` | marked / marked-terminal / cli-highlight / chalk / ink-spinner devDeps | 未使用导入 | None of marked, marked-terminal, cli-highlight, chalk, or ink-spinner is imported anywhere in sdk/cli/src or referenced in esbuild.config.js (verified with grep -rn). 'dependencies' is empty (line 40) with runtime lib... | 高 |

#### 过时注释

| 位置 | 注释(摘录) | 问题 | 置信度 |
|---|---|---|---|
| `sdk/python/ai_assistant/images.py:61` | generate_async(): 'body["async"] = True' posted to /api/v1/assistant/generate-image; docstring lines 72-73: 'Returns immediately with a t... | Gateway has a dedicated POST /api/v1/assistant/generate-image-async (src/api/v1/assistant.py:1367) with its own AsyncImageGenerationRequest schema; the sync /generate-image route has no 'async' bod... | 高 |
| `sdk/python/ai_assistant/images.py:90` | get_task(): GET /api/v1/assistant/tasks/{task_id}, docstring 'Poll the status of an async image generation task.' | No /tasks/{task_id} route exists on the gateway; the correct route is GET /api/v1/assistant/image-task/{task_id} (src/api/v1/assistant.py:1389, apps/assistant-service images.py:1687). This call ret... | 高 |
| `sdk/python/ai_assistant/models/request.py:27` | 'model_id: str = "qwen3.6-plus"' with comment 'LLM model identifier. Gateway default - override per request for other registered models.' | Gateway default is now qwen3.7-plus (src/api/schemas/assistant.py:139, apps/assistant-service chat.py:65, config/startup_fingerprint.py:30), so the SDK default no longer matches. Same stale qwen3.6... | 高 |
| `sdk/python/ai_assistant/sessions.py:50` | Docstring: 'offset: Pagination offset.' | Server list_sessions accepts only limit (src/api/v1/sessions.py:62-64, limit: int = Query(50, ge=1, le=200)); there is no offset parameter, so the SDK's offset is silently ignored. | 中 |
| `sdk/cli/src/app.tsx:448` | /kb search command: client.request("POST", "/api/v1/knowledge/search", ...) | No /knowledge/search route exists: the gateway proxies /api/v1/knowledge/* to knowledge-service, which offers /knowledge/datasets and /knowledge/{dataset_id}/retrieve (apps/knowledge-service/.../kn... | 高 |
| `sdk/cli/src/app.tsx:829` | /resume command: client.request("GET", `/api/v1/assistant/sessions/${sid}/messages`) | No GET /sessions/{id}/messages route: server has POST /sessions/{session_id}/messages (src/api/v1/sessions.py:192) and GET /sessions/{session_id}/history (line 213). The resume path returns 405/404. | 高 |
| `sdk/cli/src/app.tsx:547` | /models command: client.request("GET", "/api/v1/admin/models") | No /admin prefix exists: api_router has no prefix (src/api/router.py:46) and the models router mounts at /models (src/api/v1/models.py, included at router.py:92), full path /api/v1/models (src/main... | 高 |
| `sdk/cli/src/app.tsx:78` | const CLI_VERSION = "1.4.0" | Stale version constant: package.json version is 1.5.0, and the render at line 694 displays this constant to users, so the CLI reports a version that was never published. | 高 |
| `sdk/cli/src/remote/client.ts:15` | const CLI_VERSION = "1.0.3" | Second, older stale version constant in the same package (package.json is 1.5.0); also inconsistent with app.tsx's 1.4.0. | 高 |

#### 错误注释

| 位置 | 注释(摘录) | 错误之处 | 置信度 |
|---|---|---|---|
| `sdk/cli/src/skills.ts:64` | // Upload via multipart-like JSON (gateway accepts content as text) | Factually wrong: the gateway's upload_skill requires a real multipart file (file: UploadFile = File(...), src/api/v1/skills.py:118-121). The CLI sends a JSON body with a content field; that request... | 高 |
| `sdk/java/src/main/java/com/aigateway/ai/EventType.java:6` | "These mirror the gateway's SSEEventType enum exactly and match the raw 'event:' lines emitted by the gateway." | Not exact: the gateway schema enum (src/api/schemas/assistant.py:333) does not contain PHASE_STARTED, SUBAGENT_STEP, TEXT_MESSAGE_START, or state_snapshot, which the Java/Dart/Python SDKs all inclu... | 中 |

#### 超大文件

| 文件 | 行数 | 问题 | 拆分建议 |
|---|---|---|---|
| `sdk/cli/src/app.tsx` | 964 | LARGE (>=500 TS threshold). Single God component mixing the entire CLI: slash-command dispatch (/chat, /kb, /resume, /models, /skills, /mcp, /memory), stream... | Extract the streaming event handler into a useChatStream hook (src/stream.ts already exists for the SSE layer); move slash-command dispatch to src/commands/;... |

#### 模块备注(交叉发现)

- Default-model drift is cross-SDK: all four SDKs default to qwen3.6-plus (Python request.py:27, Java ChatRequest.java:27, Dart chat.dart:52/94, CLI types/config.ts:35 + cli.tsx:24 help text) while the gateway's own default is qwen3.7-plus (src/api/schemas/assistant.py:139, apps/assistant-service chat.py:65, config/startup_fingerprint.py:30) — SDK requests silently override the server default with an older model.
- Python SDK MCP HTTP transport is half-wired: call_tool() on a client connected via _connect_http crashes with AssertionError because _jsonrpc (mcp/client.py:316-318) asserts self._process.stdin, and _process is None for HTTP transport. Only stdio transport works.
- CLI MCP tools are unreachable end-to-end: /mcp connect succeeds (mcp/manager.ts), but callTool is only referenced from the dead agent/loop.ts, so connected MCP servers can never be invoked from the live CLI.
- Test coverage asymmetry: the Dart SDK has no test directory at all; the Python SDK's only tests are live-server integration tests (sdk/python/tests/test_sdk.py, skipped without SDK_TEST_API_KEY) with no unit tests, and its test_models_and_tools test only exercises tools despite its name; Java has unit tests only for the SSE parser and auth headers.
- sdk/cli/package.json packaging oddities: 'dependencies' is empty (line 40) with all runtime deps (ink, react, ink-text-input, meow, fast-glob) in devDependencies, and esbuild.config.js targets node18 while engines declares node >=22.12.0.
- sdk/openapi.json (647KB generated OpenAPI snapshot) is committed and was regenerated 2026-08-12; it already contains generate-image-async and image-task paths that the Python SDK image methods have not caught up with — regenerating SDK surfaces from it (or curating it) would have prevented the images.py drift.
- The CLI README advertises OS Agent and permission-system features whose implementing code (src/tools/*, src/agent/*, src/hooks.ts, handleConfirm flow) is unreachable from the live entry point (cli.tsx -> app.tsx); either wire the agent loop back in or remove the dead layer.
- Image endpoint source of truth: apps/assistant-service/src/assistant_service/api/routes/images.py (sync generate-image at :840, generate-image-async at :1446, image-task GET at :1687); the Python SDK's _GENERATE_PATH/_TASK_PATH constants and get_task should be updated to the /generate-image-async + /image-task pair.

### 3.12 tests-services

死代码 6 | 过时注释 2 | 错误注释 1 | 超大文件 12

#### 死代码

| 位置 | 符号 | 类型 | 证据 | 置信度 |
|---|---|---|---|---|
| `tests/services/assistant/tools/test_confluence_spaces.py:222` | _write_req | 未使用函数 | Helper builds a ToolCallRequest for tool_name='confluence_write', but rg across the repo shows the only occurrence is the def itself (line 222); all 10+ tests in the file use _read_req (line 215) and the file contains... | 高 |
| `tests/services/assistant/test_agentloop_streaming_first_contract.py:201` | ScriptedToolInvoker | 未使用类 | Full scripted-invoker fake class in the 5761-line streaming contract file; rg shows exactly one repo-wide occurrence (the class def). Never instantiated by any test — the file relies on ScriptedToolInvoker's parent Fa... | 高 |
| `tests/services/assistant/test_task_planning_integration.py:44` | mock_tool_registry | 未使用函数 | pytest fixture never requested: AST scan (incl. fixture args) plus rg show only the def line. A same-named fixture in test_tool_orchestrator.py:63 is a separate, used definition. Sibling fixtures mock_model_registry/u... | 高 |
| `tests/services/knowledge/test_pdf_splitter.py:26` | small_splitter | 未使用函数 | Fixture 'Splitter with very small max size for testing' is never requested by any test in the file (rg: only the def at line 26); sibling fixture 'splitter' (line 22) is the one used throughout. | 高 |
| `tests/services/knowledge/test_bm25_v2_shadow.py:90` | _direct_filter_values | 未使用函数 | Module-level helper in the 2055-line BM25 shadow test; rg shows only the def (line 90) and no callers anywhere. Dead leftover from an earlier filter-introspection approach. | 高 |
| `tests/services/knowledge/test_bm25_v2_shadow.py:98` | _bm25_v2_scope_defaults | 未使用函数 | Sibling dead helper: rg shows only the def (line 98), never called in-file or repo-wide. Same leftover as _direct_filter_values. | 高 |

#### 过时注释

| 位置 | 注释(摘录) | 问题 | 置信度 |
|---|---|---|---|
| `tests/services/assistant/test_compressor.py:18` | # Load the compressor module directly without going through __init__.py / # This avoids importing the entire assistant package which has ... | The stated rationale no longer holds: the sibling file tests/services/assistant/test_memory_compressor.py imports the exact same module (assistant_service.core.memory.compressor) through the normal... | 中 |
| `tests/services/assistant/test_google_vertex_provider.py:12` | ...same TLS-reuse bug as AI Studio — see commit b9c5128). | The docstring cites commit b9c5128 as the source of a TLS-reuse bug fix, but no such commit exists anywhere in this repository's git history (git cat-file -t b9c5128 fails; only 66 commits exist an... | 中 |

#### 错误注释

| 位置 | 注释(摘录) | 错误之处 | 置信度 |
|---|---|---|---|
| `tests/services/assistant/test_google_vertex_provider.py:13` | AI Studio — see commit b9c5128). | Factually unverifiable: the referenced commit b9c5128 does not exist in this repo's git history (66 commits total, none matching the prefix). If the bug reference is real it belongs to squashed ups... | 中 |

#### 超大文件

| 文件 | 行数 | 问题 | 拆分建议 |
|---|---|---|---|
| `tests/services/assistant/test_agentloop_streaming_first_contract.py` | 5761 | God-file contract test: 35 fake classes (MockUserContext, FakeModelRegistry, ScriptedToolInvoker, RecordingTraceWriter, various cancel/error model fakes) plu... | Extract all fakes into a shared fixtures module; split by event family (task events / tool events / artifact events / stream lifecycle / cancellation and err... |
| `tests/services/assistant/test_runtime_memory_privacy.py` | 2358 | 22 classes mixing infrastructure fakes (ScopedMemoryDatabase, _FakeAcquire, _FakePool, _FakeTransaction, RecordingVectorStore, CollectionVectorStore, TypedSq... | Extract DB/pool/vector-store fakes to a helper module; split into test_runtime_memory_isolation.py, test_runtime_memory_deletion.py, test_runtime_memory_part... |
| `tests/services/assistant/test_model_registry_provider_boundaries.py` | 2256 | Provider-boundary matrix for ModelRegistry with 6 fakes (_FakeResponse, _StreamContext, _FakeClient, _FailingModelService, _ExplodingPrice, _ModelService); 1... | Split per provider family (google-vertex / openai / llm-hosted) and extract the client/response fakes to a fixtures module. |
| `tests/services/knowledge/test_hierarchical_document_deletion.py` | 2193 | 19 classes: qdrant/database fakes (RecordingVectorStore, DeleteDatabase, FencedDeleteDatabase, ConcurrentSegmentDatabase, BarrierQdrantClient, StatefulQdrant... | Extract shared fakes (vector store, database, embedder) into a knowledge-test helpers module; split by fence (index deletion / segment sweep / lifecycle barr... |
| `tests/services/knowledge/test_bm25_v2_shadow.py` | 2055 | 35 fake client classes (9+ named 'Client') plus shadow write/read/fallback coverage of BM25 v2; contains two confirmed dead helpers (_direct_filter_values li... | Extract fake qdrant/db clients to a helper module; split into shadow-write, shadow-read, and fallback/no-FTS files. Remove the two dead helpers. |
| `tests/services/assistant/test_mcp_runtime.py` | 1984 | 21 classes combining MCP client transport (MCPClient/MCPStdioClient), discovery, circuit-breaker/resilience (BlockingSuccessBreaker, HangingTelemetryReposito... | Split into test_mcp_client.py (transport), test_mcp_discovery.py, test_mcp_resilience.py (circuit breaker/retry/telemetry) and extract client fakes. |
| `tests/services/eval/test_evaluator_executor.py` | 1955 | Only 3 classes and 6 functions but 1955 lines — extremely long parametrized case tables driving the evaluator pipeline (candidate/execution/aggregation stage... | Split the parametrized case tables by pipeline stage or evaluator family, and move the fake repositories into the eval helper module. |
| `tests/services/assistant/test_memory_manager.py` | 1733 | 34 test classes covering every memory layer (working/session/long-term store-retrieve-search-delete), the MemoryManager surface, HybridMemoryRetriever, runti... | Split by concern: per-layer store/retrieve tests, manager API tests, hybrid retriever tests, lifecycle/privacy tests, tool-boundary tests. |
| `tests/services/test_image_sync.py` | 1701 | 11 test classes (storage backends, embedding, image processor, DB save_image_segment, sync integration, parser extraction, models, S3 metadata sanitization) ... | Split per class (each is a separable unit); port to knowledge_service imports as the image_storage extraction lands, then delete the legacy-target copies. |
| `tests/services/knowledge/test_dataset_create_security.py` | 1684 | 13 fake classes (repeated _Embedder/_Acquire/_Pool/_Transaction DB-fake pattern plus 8 _Client qdrant fakes) wrapping dataset-create security fences (identit... | Extract the DB/qdrant fake stack to a shared knowledge-test helper (used by 4+ sibling files); split by security fence concern. |
| `tests/services/assistant/test_agent_trace_capture.py` | 1611 | 13 classes combining capture, resume-cursor, blocking-cursor and failing-DB variants of agent trace capture with FakeExecutionGateway/FakeKnowledgeService fa... | Split into capture / resume / blocking-and-failure groups; extract RecordingDB and cursor fakes to a helper module. |
| `tests/services/assistant/test_responses_api.py` | 1563 | 18 bare test functions (no classes) covering the entire Responses API surface — request building, streaming, tool calls, error mapping — in one flat file. | Group into classes or split by surface (request serialization / streaming / tool handling / error mapping). |

#### 模块备注(交叉发现)

- LEGACY-TARGET TESTS (knowledge duplication): tests/services/test_confluence_client.py, test_confluence_polling_interval.py, test_confluence_remove_pages.py, test_dashscope_embedding.py, test_image_sync.py import src.services.knowledge.* (confluence client/scheduler/sync_service/models/parser/image_processor, embedding) even though structurally identical modules live in apps/knowledge-service/src/knowledge_service/services/knowledge/ — e.g. scheduler.py old vs new has the same classes at the same line numbers (PollingTask:43, PagePollingTask:82, ConfluenceScheduler:116, SchedulerManager:867), i.e. the legacy tree is a direct copy. These tests pin the monolith copies; re-point them at knowledge_service when the monolith's confluence/embedding routes are retired. Exception: src.services.storage.image_storage has NO new equivalent (knowledge_service/storage/ has only __init__.py), so test_image_sync.py's storage tests block migration.
- MIXED-TARGET TESTS: tests/services/knowledge/test_dataset_delete_security.py tests the new DatasetService/fences but imports hash_password/UserContext from legacy src.core.auth.* (new equivalents exist at knowledge_service.core.auth.password / user_resolver); tests/services/knowledge/test_ragas_eval_route_security.py tests the new knowledge_service eval route but imports KbRagasScoreRetrievalRequest from src.api.schemas.eval — that schema exists ONLY in src (no apps/packages copy) and the test will break when monolith schemas are retired.
- src/persistence/database.py is an 18-line re-export shim for ai_gateway_core.persistence.database (Phase 5f Batch C); tests/services/test_image_sync.py TestDatabaseSaveImageSegment exercises DatabaseStorage through this shim. save_image_segment exists only on the ai_gateway_core class (line 2903) and knowledge_service class (line 5132), not on any legacy class.
- DUPLICATED TEST HELPERS: StubEmbedder is copy-pasted in 3 files (tests/services/test_hierarchical_indexer.py:35, test_hierarchical_retriever.py:10, tests/services/knowledge/test_hierarchical_document_deletion.py:579); the _clear_env autouse fixture is duplicated in test_google_vertex_provider.py:40 and test_google_vertex_switch.py:31; RecordingVectorStore appears in 2 files. A shared tests/services/knowledge/fakes module would remove ~4 copies.
- test_compressor.py (831 lines, loads compressor via importlib.spec_from_file_location to bypass the package) and test_memory_compressor.py both test assistant_service/core/memory/compressor.py — duplicated coverage of the same module with two import styles; merge into one file using the package import.
- No whole-file dead tests: all 236 test_*.py files' first-party imports resolve against src/, apps/*/src, packages/ai-gateway-core/src and scripts/ (verified with AST import extraction against on-disk modules). No commented-out test blocks >=5 lines and no unused module-level constants found anywhere in tests/services.
- LARGE-but-not-CRITICAL test files (Python >=1000 lines, below the 1500 critical bar): tools/test_confluence_meta.py 1486, test_tool_orchestrator.py 1475, test_task_planner.py 1350, test_agent_runtime_resolver.py 1346, knowledge/test_retrieve_batch.py 1308, tools/test_tool_runtime_safety.py 1308, test_streaming_writer.py 1129, test_code_executor.py 1077, test_responses_ingress.py 1069, test_context_packet_contract.py 1059, test_agent_runtime_governance_cleanup.py 1055, test_chunking.py 1050, durable_subagent_harness.py 1018 (helper module).
- Tests for modules NOT yet extracted (src.services.llm.*, metrics.*, billing.*, registry.*, session.*, eval.*, auth.api_key_service) still target live monolith code — src/api/v1/providers.py, dashboard.py, dispatcher.py, container.py, main.py etc. all import them, so these tests are not dead; only the knowledge-service group above is stale-target.
- The local_os_acceptance conftest sys.path hack for apps/local-node/src is still necessary (apps/local-node is NOT a uv workspace member — members are ai-gateway-core, assistant-service, knowledge-service, mcp-docgen-server, sdk/python) — do not 'fix' it.
- Scan coverage: 246 Python files in tests/services (236 test_*.py + 3 helper modules durable_subagent_harness.py, in_memory_trace_repository.py, trace_roundtrip_fixtures.py, all three confirmed still imported by live tests), 1 conftest (local_os_acceptance).

### 3.13 tests-rest

死代码 2 | 过时注释 7 | 错误注释 1 | 超大文件 12

#### 死代码

| 位置 | 符号 | 类型 | 证据 | 置信度 |
|---|---|---|---|---|
| `tests/api/test_kb_tools.py:34` | test_kb_tools.py (whole suite) | 未使用文件(整文件死) | Entire 843-line suite disabled: `pytestmark = pytest.mark.skip(reason="KB tools migrated to microservice; tests need rewrite")` at line 34. Docstring names endpoints (POST /kb/search, GET /kb/tool-definitions...) that... | 高 |
| `tests/knowledge/tools/batch_ingest.py:1` | batch_ingest.py (standalone script) | 未使用文件(整文件死) | 354-line standalone script; repo-wide rg (py/sh/yml/toml/md/Makefile) finds no importers or callers — only the file itself and tests/knowledge/README.md (which documents it as an optional manual tool). Pytest never co... | 高 |

#### 过时注释

| 位置 | 注释(摘录) | 问题 | 置信度 |
|---|---|---|---|
| `tests/contract/test_gateway_secret.py:12` | "Reference: GATE G5a-4 in ``plans/SystemDesign-Assistant-Service-True-Extraction-Phase5-2026-04-23.md``." | The plans/ directory and the referenced design doc no longer exist anywhere in the repo (verified with find). Docstring points at a deleted artifact. | 高 |
| `tests/contract/test_auth_e2e.py:26` | "Reference: plans/TechWhitePaper-Service-Extraction-2026-04-23.md §二 item 7, plans/Roadmap-Post-5a-Extraction-2026-04-23.md Phase 5b prom... | Both referenced plan files (and the plans/ directory itself) are gone from the repo — the reference cannot be followed. | 高 |
| `tests/contract/test_gateway_secret_middleware.py:4` | "The runtime docker check in ``plans/verify-phase-5a.sh`` exercises the same middleware over a real compose network" | plans/verify-phase-5a.sh does not exist (plans/ dir absent repo-wide); no verify-phase-5a.sh exists under scripts/ either. The mentioned runtime check cannot be found. | 高 |
| `tests/unit/test_acl_permissions.py:24` | "See plans/kb-fork-merge-report.md." | plans/kb-fork-merge-report.md does not exist in the repo (plans/ directory removed). Reference is dangling. | 高 |
| `tests/integration/test_assistant_isolation_contract.py:7` | "True-Isolation migration (plans/Assistant-Service-True-Isolation-Plan.md §3)." | plans/Assistant-Service-True-Isolation-Plan.md does not exist; the whole plans/ directory was removed from the repo. | 高 |
| `tests/contract/test_find_active_command.py:9` | Docstring item 2: "When the gateway lacks a database, the method falls back to the in-memory dict (transition period — removed in 5c)." | The DB-less in-memory fallback is NOT removed: ApprovalLifecycleMixin._find_active_command (apps/assistant-service/src/assistant_service/core/gateway/approval_lifecycle.py:916-937) explicitly keeps... | 中 |
| `tests/api/test_kb_tools.py:8` | Docstring: "Endpoints tested: - POST /kb/search - POST /kb/multi-search - GET /kb/datasets - GET /kb/tool-definition/{dataset_id} ..." | These endpoints no longer exist in the gateway: src/api/v1/kb_tools.py is a catch-all proxy (/kb-tools/{path:path}) forwarding to the KB service, and the whole file is pytest.mark.skip-ped. The doc... | 高 |

#### 错误注释

| 位置 | 注释(摘录) | 错误之处 | 置信度 |
|---|---|---|---|
| `tests/knowledge/tools/batch_ingest.py:8` | Docstring usage block: "python scripts/batch_ingest.py --dataset agent --files "/path/to/*.pdf"" (and two more invocations with scripts/b... | Wrong path: no scripts/batch_ingest.py exists (verified via ls). The script actually lives at tests/knowledge/tools/batch_ingest.py, which is also the path tests/knowledge/README.md documents. The ... | 高 |

#### 超大文件

| 文件 | 行数 | 问题 | 拆分建议 |
|---|---|---|---|
| `tests/database/test_agent_studio_migrations.py` | 2730 | CRITICAL. 18 async tests spanning migration 071 (agent studio domain), migration 081 (operations governance) plus repository governance suites (role matrix, ... | Move shared fixtures/helpers (_postgres_config, pool fixtures) into tests/database/conftest.py; split per-migration suites into separate files (e.g. test_age... |
| `tests/contract/test_execution_gateway_resume_safety.py` | 2687 | CRITICAL. 40 tests but ~1100 lines are fake classes (_SharedCommandDatabase alone spans ~200 lines, plus _CountingInvoker, _FailingDatabase, _ConfirmingDatab... | Extract all fakes into tests/contract/_execution_gateway_fakes.py; split the suites by behavior (settle-commit, ack-loss recovery, result receipts, DB failure). |
| `tests/api/test_eval_traces.py` | 2050 | CRITICAL. 41 tests and 4 fake repository classes. Mixes unrelated concerns: trace CRUD API behavior, eval scoring/gates/datasets/experiments, an OpenAPI path... | Move the OpenAPI + web/types contract tests into test_eval_web_contract.py; split trace CRUD, scores, gates, datasets and experiments into separate files; sh... |
| `tests/database/test_agent_publication_atomicity.py` | 1858 | CRITICAL. 12+ long async tests over release lifecycle, eval manifest atomicity and tenant serialization. Helper functions _create_agent/_publish/_record_eval... | Move shared helpers and fixtures into tests/database/conftest.py; split release-migration/immutability tests from repository atomicity tests. |
| `tests/scripts/test_judge_real_agent_receipts.py` | 1630 | CRITICAL. 35 tests with ~600 lines of synthetic fixture builders (_contract, _merged_trial, _receipt, _runner_observations...). Mixes receipt HMAC verificati... | Extract receipt/contract builders into tests/scripts/_receipt_fixtures.py; split manifest validation, judge-client and CLI suites into separate files. |
| `tests/api/test_image_redesign.py` | 1613 | CRITICAL. 31 tests plus 4 fake classes (_FakeArtifactStorage, _FakeBlobBackend, _FakeImageStateStore, _Harness ~270 lines). Single file covering the whole im... | Extract fakes to a helper module; split tests by spec-scenario group (lineage/CAS, branching, rendering). |
| `tests/scripts/test_validate_env_quickstart.py` | 1609 | CRITICAL. 62 tests, all driving subprocess runs of scripts/new/validate-env.sh (plus deploy.sh/common.sh/status.sh source checks). Monolithic parameterized m... | Split by mode: test_validate_env_config.py, test_validate_env_runtime.py, test_validate_env_infra_only.py, test_deploy_scripts.py, sharing a small subprocess... |
| `tests/scripts/test_backfill_bm25_v2.py` | 1226 | LARGE. 16 tests but ~230 lines of fakes (FakeQdrant ~135 lines, FakeAuthority ~90). Single suite for the BM25 v2 backfill migration. | Extract FakeQdrant/FakeAuthority into tests/scripts/_qdrant_fakes.py; split backfill-apply tests from migration/legacy-unsupported tests. |
| `tests/unit/test_acl_permissions.py` | 1208 | LARGE. 46 tests across 9 classes: assistant ownership, thread ownership, KB retriever ACL, multi-dataset retriever, dify-compat KB API, retrieval tool, confl... | Split per domain: test_assistant_acl.py, test_knowledge_retriever_acl.py, test_confluence_acl.py (each ~150 lines). |
| `tests/api/test_generate_image_styles.py` | 1153 | LARGE. 30 tests across 9 classes: schema coercion, single/multi-turn style forwarding and lock, signature flow, reference image URL/artifact ID, non-Gemini m... | Split into test_image_style_forwarding.py, test_image_style_multi_turn.py, test_image_reference_idor.py. |
| `tests/scripts/test_real_agent_scenario_runner.py` | 1108 | LARGE. 29 tests: scenario loading/contract/digest checks plus SSE record collection, golden sealing, tool-result canonicalization, delegation and CLI coverage. | Split scenario-loading/contract tests from trial-collection/golden-seal tests (two ~550-line files). |
| `tests/api/test_agent_runtime_api.py` | 1086 | LARGE. 15 tests but 5 fake classes (_Resolver, _Sessions, _Repository ~220 lines, _AtomicChannelLimiter, _FileStorage) plus two client fixtures. | Extract fakes into tests/api/_agent_runtime_fakes.py; split version-resume/signing tests from token/scope/SSE tests. |

#### 模块备注(交叉发现)

- Duplicated helper: `create_test_token` defined in tests/conftest.py:80 (superset: extra_claims param, tenant_id default "test_tenant") and again in tests/integration/test_gateway.py:24 (subset, tenant_id default ""). The local copy diverges in the tenant default; test_gateway.py should import from conftest.
- Cross-test helper imports make tests fragile: tests/database/test_agent_studio_migrations.py provides `_postgres_config` imported by 4 other database tests, and tests/database/test_agent_channel_runtime.py loads helpers via `pytest_plugins = ("tests.database.test_agent_publication_atomicity",)` (line 20). Any refactor of those big files breaks the dependents; helpers belong in tests/database/conftest.py.
- Overlapping coverage: tests/core/test_middleware.py (2 tests: chain execution + order) vs tests/core/test_middleware_chain.py (7 tests incl. execution order) both exercise src.core.middleware.base InvocationMiddleware/MiddlewareChain.
- Overlapping coverage: tests/unit/test_service_access_policy.py (48 lines) duplicates the module coverage of tests/core/auth/test_service_access.py (129 lines) — both test src.core.auth.service_access.
- Overlapping coverage: tests/contract/test_gateway_secret_middleware.py (96 lines) and tests/packages/ai_gateway_core/test_gateway_secret_middleware.py (147 lines) both test ai_gateway_core.auth.gateway_secret_middleware.GatewaySecretAuthMiddleware; the packages version is a superset (adds config validation + v2 body replay).
- tests/api/test_kb_tools.py has a duplicated `import pytest` (lines 19 and 33) — the second one sits after the module-level skip marker, evidence of a hasty conversion to the skip regime.
- tests/knowledge/tools/batch_ingest.py is manual tooling that pytest will never collect; its only documentation is tests/knowledge/README.md. If it must be kept it belongs under scripts/, not inside the test tree.
- Sweep results: all 251 in-scope test files have resolvable intra-repo imports (AST-verified, including scripts/* and knowledge_service); no test file with zero collected test functions; no commented-out test blocks of >=5 lines; all SQL migrations referenced by tests/database exist in database/migrations/.
- tests/deployment/agent_embed_header_fixture.py looks dead from pytest's perspective but is NOT: it is mounted as a volume by scripts/new/test-agent-embed-headers.sh:82 — do not delete.

### 3.14 misc

死代码 24 | 过时注释 2 | 错误注释 2 | 超大文件 5

#### 死代码

| 位置 | 符号 | 类型 | 证据 | 置信度 |
|---|---|---|---|---|
| `scripts/docgen_e2e.py:1` | docgen_e2e.py (family: docgen_harness_ppt.py, docgen_phase1_smoke.py, docgen_pptx_preview.py, docgen_pptx_showcase.py) | 未使用文件(整文件死) | Zero references repo-wide (Makefile, CI, tests, docs, other scripts all checked). Worse: broken — line 32-35 imports src.services.assistant.docgen.planners/pipeline/quality.* which no longer exist; the docgen package ... | 高 |
| `database/cli.py:1` | cli.py (database migration CLI) | 未使用文件(整文件死) | Zero references repo-wide (Makefile 'migrate' targets call scripts/new/migrate.sh; docker/migrate/Dockerfile runs database/migrate_per_service.py; no test imports it). Only mention anywhere is a NOTE inside database/r... | 高 |
| `apps/islamic-content-service/data/recommendations_seed.json:1` | recommendations_seed.json (whole islamic-content-service dir) | 未使用文件(整文件死) | Service dir contains only this seed file; nothing reads/mounts it (rg 'recommendations_seed' and 'islamic' in code/compose/Dockerfile: zero loaders). RELEASE.md lists an islamic-content-service image but tests/scripts... | 高 |
| `scripts/new/prepare-agent-studio-e2e-account.py:1` | prepare-agent-studio-e2e-account.py | 未使用文件(整文件死) | Zero references repo-wide: not in tests/fixtures/agent-studio/regression_manifest.json, not in Makefile, CI, tests or docs. Creates a disposable Assistant account — nothing invokes it. | 高 |
| `scripts/gateway_capacity_inventory.py:1` | gateway_capacity_inventory.py | 未使用文件(整文件死) | Zero references repo-wide (rg across Makefile, docs, tests, .github, other scripts). No Makefile target, no test in tests/scripts/, no doc mention. | 高 |
| `scripts/uat_gateway_capacity_probe.py:1` | uat_gateway_capacity_probe.py | 未使用文件(整文件死) | Zero references repo-wide; no companion test in tests/scripts/ (unlike eval_golden etc. which have test_eval_live.py etc.). | 高 |
| `scripts/uat_gateway_capacity_smoke.py:1` | uat_gateway_capacity_smoke.py | 未使用文件(整文件死) | Zero references repo-wide; sibling of uat_gateway_capacity_probe.py, also orphaned. | 高 |
| `scripts/smoke_native_search.py:1` | smoke_native_search.py | 未使用文件(整文件死) | Zero references repo-wide (only self-mention in its own docstring). | 高 |
| `scripts/test_style_presets_e2e.py:1` | test_style_presets_e2e.py | 未使用文件(整文件死) | Zero references repo-wide; imports src.services.assistant.tools.style_presets which still exists, so it is orphaned-but-functional. | 高 |
| `scripts/diagnose_confluence_tools.py:1` | diagnose_confluence_tools.py | 未使用文件(整文件死) | Zero references repo-wide (no Makefile target, no test, no doc). | 高 |
| `scripts/analyze_assistant_deps.py:1` | analyze_assistant_deps.py | 未使用文件(整文件死) | Zero references repo-wide; docstring says 'Phase 1 of isolation plan' and writes plans/assistant-deps-inventory.json, but no plans/ dir exists and the assistant-service extraction it analyzed has shipped (apps/assista... | 高 |
| `scripts/goal/exercise-shipped-paths.py:1` | scripts/goal/ whole dir (exercise-shipped-paths.py, generate-port-patch.sh, port-only-diff.sh, verify-sync-port.sh, sync-port-only.txt, workstream-a-only.txt, workstream-a-paths.txt) | 未使用文件(整文件死) | Entire dir zero references repo-wide; one-off 'workstream A' port exercise committed 2026-07-16; the .txt files are scratch manifests. scripts/goal/verify-sync-port.sh also references scripts/new/gateway_preflight.py ... | 高 |
| `apps/local-node/scripts/macos_computer_live_acceptance.py:1` | macos_computer_live_acceptance.py | 未使用文件(整文件死) | Zero references repo-wide (its sibling local_live_acceptance.py is at least wired to tests/test_local_live_acceptance.py; this one has no test, no Makefile target, no doc). | 高 |
| `docker-compose.capability.yml:1` | docker-compose.capability.yml | 未使用文件(整文件死) | Zero references in Makefile/README/DEPLOY/CI/scripts. Its purpose (docker-socket capability overlay) is superseded by docker-compose.code-executor.yml, which IS referenced by scripts/new/code-executor.sh and tests. | 高 |
| `docker-compose.kbms.yml:1` | docker-compose.kbms.yml | 未使用文件(整文件死) | Zero references anywhere; duplicates postgres/redis infrastructure already in docker-compose.yml (which has no kbms overlay reference). | 高 |
| `config/langgraph.yaml:1` | config/langgraph.yaml | 未使用文件(整文件死) | Header self-labels as '示例' (example). Nothing loads it: src/config/settings.py:238/651 only reads GATEWAY_LANGGRAPH__ env vars; web/src/components/ServiceForm.tsx generates its own YAML template. mcp_servers.yaml next... | 中 |
| `apps/local-node/src/local_node/credentials.py:36` | UnavailableSecureCredentialStore | 未使用类 | rg -w repo-wide (excluding credentials.py): zero matches. Not referenced internally (no default instantiation), not imported anywhere. | 高 |
| `apps/local-node/src/local_node/transport.py:725` | DeviceOutboundRunner.run_forever | 未使用方法 | Single occurrence in repo (the def). run_once (line 680) is only reachable from it; scripts/local_live_acceptance.py drives the runner differently. Dead method. | 高 |
| `apps/local-node/src/local_node/service.py:83` | LocalNodeRuntime.assert_online | 未使用方法 | Single occurrence in repo (the def); no caller in transport.py (which holds the only LocalNodeRuntime user) or anywhere else. connect() at line 76 is called by transport.py:347, but assert_online never is. | 高 |
| `scripts/native_agent_parity_benchmark.py:58` | _canonical_json | 未使用函数 | Exactly one occurrence in repo (the def, line 58). Sibling helpers _sha256_bytes/_sha256_file are used within the file; this one has no caller. (Other files with same name are separate modules.) | 高 |
| `apps/local-node/src/local_node/doctor.py:35` | CapabilityDoctor | 未使用类 | Imported ONLY by its own test apps/local-node/tests/test_computer_doctor_runtime.py:17. Production doctor paths bypass it: cli.py:51 has its own _doctor(), transport.py:441 has TransportDoctorReport and only consumes ... | 中 |
| `apps/local-node/src/local_node/ledger.py:341` | mark_awaiting_approval / mark_dispatched / mark_running / mark_observed (lines 341-350) | 未使用方法 | All four exercised only by tests/services/assistant/local_os_acceptance/test_action_ledger.py; zero production callers — transport.py/computer.py/files.py drive transitions only through begin()/finish(). Test-only seams. | 中 |
| `scripts/collect_cra_compliance_research.py:1` | collect_cra_compliance_research.py | 未使用文件(整文件死) | Referenced only by tests/services/eval/test_cra_compliance_research_fixture.py:10 (import as cra). No Makefile target, no docs, no other scripts. Live-ish only because the test imports it to reuse fixtures. | 中 |
| `scripts/validate_real_finance_eval.py:1` | validate_real_finance_eval.py | 未使用文件(整文件死) | Referenced only by its own test tests/services/eval/test_real_finance_salesforce_eval.py:11 and by src/services/eval/fixtures/real_finance_salesforce_fy26_q1/README.md usage examples. No Makefile/CI wiring. | 中 |

#### 过时注释

| 位置 | 注释(摘录) | 问题 | 置信度 |
|---|---|---|---|
| `scripts/new/deploy.sh:15` | #   --app        Deploy application services only (gateway, frontend, assistant, #                knowledge, docgen) | docgen is no longer a standalone deployable service. The --app branch actually starts 'gateway frontend knowledge-service assistant-service' (deploy.sh:104) and docker-compose.yml has no docgen ser... | 中 |
| `scripts/snapshot_assistant_openapi.py:4` | "Used by Phase 0 of the Assistant Service True Isolation Migration to produce a baseline spec, and by later phases to verify the extracti... | The isolation extraction shipped: assistant-service now lives in apps/assistant-service and runs as its own docker-compose service. The 'Phase 0 / later phases' framing of an in-progress migration ... | 中 |

#### 错误注释

| 位置 | 注释(摘录) | 错误之处 | 置信度 |
|---|---|---|---|
| `database/run_migration.py:5` | NOTE: This is a legacy script. Prefer using: python database/cli.py migrate | Wrong pointer: database/cli.py has zero references anywhere in the repo (verified repo-wide — Makefile migrate targets, docker/migrate/Dockerfile, scripts/new/*.sh never touch it). The operative mi... | 高 |
| `scripts/docgen_e2e.py:3` | "Runs the FULL pipeline (DocgenService -> planner -> renderer -> verifier -> fix loop -> artifact store)..." with imports from src.servic... | The named module paths no longer exist: src/services/assistant/docgen was moved to apps/assistant-service/src/assistant_service/core/docgen (e.g. quality/visual_verifier.py there). The described pi... | 高 |

#### 超大文件

| 文件 | 行数 | 问题 | 拆分建议 |
|---|---|---|---|
| `scripts/judge_real_agent_receipts.py` | 2608 | CRITICAL. Single-script release judge mixing ~900 lines of contract/schema validation (_strict_json_loads, _identifier, _bounded_text, _integer, _strict_keys... | Split into a package: scripts/judge_real_agent_receipts/ with contract_validation.py (all _validate/_strict helpers), attestation.py (HMAC + binding payloads... |
| `scripts/real_agent_scenario_runner.py` | 2422 | CRITICAL. Scenario contract loading/validation (load_scenarios ~280 lines), source-artifact verification, plugin definition checks, collector attestation, ru... | Split into scenario_contract.py (load_scenarios + _validate_assertion + verification helpers), runner.py (execution orchestration), attestation.py, cli.py; e... |
| `scripts/backfill_bm25_v2.py` | 1532 | CRITICAL. One file holds the Qdrant-native BM25 v2 backfill: Postgres authority snapshot, manifest dataclass + schema fingerprinting, scope/dataset filters, ... | Split into manifest.py (BackfillManifest + schema fingerprint helpers), authority.py (PostgresBackfillAuthority), qdrant_ops.py (filters, profile validation,... |
| `scripts/native_agent_parity_benchmark.py` | 1354 | CRITICAL. Cross-system benchmark harness (ai_platform/hermes/openclaw) mixing manifest loading, per-domain golden validators (finance, tenant access, staged ... | Split into manifest.py, validators.py (the _validate_* family), systems.py (three ingress clients), report.py, cli.py; move Hermes/OpenClaw paths to env vars... |
| `apps/local-node/src/local_node/transport.py` | 1127 | LARGE. Local-node outbound transport: HttpsJsonTransport client, pairing redemption, ClaimReplayGuard (SQLite), DeviceOutboundRunner (dispatch loop, claim/co... | Split into transport_client.py (HttpsJsonTransport + protocols), replay_guard.py, runner.py (DeviceOutboundRunner + doctor report), wire.py (_required_*/_par... |

#### 模块备注(交叉发现)

- skills/ is vendored third-party: skills-lock.json at repo root pins all 21 skills to pbakaus/impeccable with computed hashes. Do not treat skill internals as project code.
- agent-plugins/ is vendored community content (LICENSE + THIRD_PARTY_NOTICES.md per plugin) and is LIVE: docker-compose.yml:527-530 mounts them into assistant-service and pins ai-docgen@1.0.0 as a trusted plugin; scripts/validate_agent_plugin.py (README-documented) validates them.
- Root-level strays (all untracked/ignored local junk, safe to delete): .tmp-general-agent-live-orchestrate.sh (gitignored via `.tmp-*`, contains absolute /tmp/ai-platform-general-agent.* paths), login-page-verification.png, login-page-verified-after-fixes.png, swagger-ui-after-fixes.png (gitignored via `*.png`), tmp/ (gitignored eval outputs), uploads/ (empty untracked dir), claude-code/ (empty untracked dir). None are tracked in git.
- Four parallel DB migration runners exist: database/cli.py (orphaned, see deadCode), database/run_migration.py (legacy, test-only), database/migrate_per_service.py (used by docker/migrate/Dockerfile + compose `migrate` service), scripts/new/migrate.sh (used by `make migrate`). They also use two different tracking tables (public.schema_migrations vs public.schema_migrations_meta), plus database/schema.sql is a hand-maintained 1962-line aggregate that can drift from the 80 migration files. Consolidation candidate.
- docker/monitoring/ stack (docker-compose.monitoring.yml + prometheus/alertmanager/otel/grafana) is referenced only by tests/monitoring/test_alert_rules.py; the overlay compose itself is unreferenced by Makefile/README. deploy/helm/ai-gateway/values-production.yaml likewise has zero references (chart itself is exercised by release-guard tests).
- scripts/goal/ (.txt manifests, port-patch scripts) is one-off 'workstream A' scratch from 2026-07-16 with zero references — see deadCode entry; the dir also references scripts/new/gateway_preflight.py, which is itself only test-referenced.
- scripts/native_agent_parity_benchmark.py hardcodes machine-specific absolute paths (/Users/yang/projects/Hermes_agent, '/Users/yang/projects/open claw/openclaw') — matches the .gitignore comment at line 180 about machine-specific temp paths; should be env-driven.
- docker-compose.override.yml.example is unreferenced but is an intentionally self-documenting example file (compose convention) — leave, or document in README. docker-compose.capability.yml and docker-compose.kbms.yml are not examples and are flagged dead.
- scripts/docgen_* family (5 files) imports src.services.assistant.docgen.* which no longer exists — evidence the assistant-service extraction (apps/assistant-service) did not migrate these scripts; they are both orphaned and broken.
- apps/islamic-content-service/ is an empty shell (single seed JSON, nothing consumes it); RELEASE.md still lists a ghcr image for it and scripts/new/common.sh still probes for a stray `islamic-content-service` container — leftover from a never-shipped service.

## 四、全库超大文件总表(合并去重,按行数降序)

> 全库基线: Python ≥1000 行共 119 个(≥1500 行 59 个,≥3000 行 11 个);web ≥500 行共 66 个(≥1000 行 17 个,≥2000 行 6 个)。下表为各模块代理报告的重点文件。

| 文件 | 行数 | 所属模块 | 级别 | 要点 |
|---|---|---|---|---|
| `apps/knowledge-service/src/knowledge_service/persistence/database.py` | 9559 | knowledge | 🔴 CRITICAL | CRITICAL — repo's largest file. One DatabaseStorage class (line 218) mixing raw-SQL CRUD for ~20 entities (datasets, ... |
| `packages/ai-gateway-core/src/ai_gateway_core/persistence/repositories/agent_repository.py` | 7540 | gateway-core | 🔴 CRITICAL | CRITICAL: DatabaseAgentRepository class spans lines 574-7540 (~90 methods: CRUD, drafts, releases, deletion lifecycle... |
| `packages/ai-gateway-core/src/ai_gateway_core/persistence/database.py` | 7322 | gateway-core | 🔴 CRITICAL | CRITICAL: DatabaseStorage god class from line 132 to EOF with 251 methods covering schema bootstrap, permission cache... |
| `tests/services/assistant/test_agentloop_streaming_first_contract.py` | 5761 | tests-services | 🔴 CRITICAL | God-file contract test: 35 fake classes (MockUserContext, FakeModelRegistry, ScriptedToolInvoker, RecordingTraceWrite... |
| `web/src/pages/knowledge/DatasetDetail.tsx` | 4854 | web | 🔴 CRITICAL | CRITICAL god page: single KnowledgeDatasetDetailPage component with 40+ inline handlers (upload, batch ops, RAGAS, hi... |
| `apps/knowledge-service/src/knowledge_service/services/knowledge/confluence/sync_service.py` | 3903 | knowledge | 🔴 CRITICAL | CRITICAL — single ConfluenceSyncService class with ~30 methods (full/partial sync, page upsert, image generation, del... |
| `apps/knowledge-service/src/knowledge_service/services/knowledge/retrieval_service.py` | 3483 | knowledge | 🔴 CRITICAL | CRITICAL — RetrievalService (line 457) plus ~400 lines of validation helpers (require_shadow_only_dataset, _require_b... |
| `apps/knowledge-service/src/knowledge_service/api/routes/knowledge.py` | 3419 | knowledge | 🔴 CRITICAL | CRITICAL — one APIRouter with ~60 endpoints across datasets, documents, segments, retrieval, QA and eval, plus ~340 l... |
| `apps/knowledge-service/src/knowledge_service/services/knowledge/chunking.py` | 3416 | knowledge | 🔴 CRITICAL | CRITICAL — all 9 chunking strategies (FixedSize, Paragraph, Page, Heading, Regex, Separator, Recursive, Hierarchical,... |
| `packages/ai-gateway-core/src/ai_gateway_core/persistence/repositories/agent_trace_repository.py` | 3396 | gateway-core | 🔴 CRITICAL | CRITICAL: AgentTraceRepository (line 358+) mixes trace CRUD, versioned gate-metrics aggregation, paired-bootstrap CI ... |
| `src/services/knowledge/confluence/sync_service.py` | 3297 | src-services | 🔴 CRITICAL | CRITICAL: ConfluenceSyncService god class (page/space sync, incremental sync, webhook paths, embedding, image process... |
| `apps/knowledge-service/src/knowledge_service/services/knowledge/vector_store.py` | 3180 | knowledge | 🔴 CRITICAL | CRITICAL — VectorStore class (line 97) mixing collection lifecycle (create/rename/delete/backup), Qdrant search, BM25... |
| `web/src/pages/assistant/hooks/useChatSession.ts` | 2842 | web | 🔴 CRITICAL | CRITICAL god hook: single useChatSession handles SSE streaming, sub-agent events, quiz hydration, KB search status, w... |
| `tests/database/test_agent_studio_migrations.py` | 2730 | tests-rest | 🔴 CRITICAL | CRITICAL. 18 async tests spanning migration 071 (agent studio domain), migration 081 (operations governance) plus rep... |
| `tests/contract/test_execution_gateway_resume_safety.py` | 2687 | tests-rest | 🔴 CRITICAL | CRITICAL. 40 tests but ~1100 lines are fake classes (_SharedCommandDatabase alone spans ~200 lines, plus _CountingInv... |
| `scripts/judge_real_agent_receipts.py` | 2608 | misc | 🔴 CRITICAL | CRITICAL. Single-script release judge mixing ~900 lines of contract/schema validation (_strict_json_loads, _identifie... |
| `web/src/pages/eval/index.tsx` | 2497 | web | 🔴 CRITICAL | CRITICAL god page: EvalPage mixes workbench layout, experiment runs comparison, behavior contract editor, RAGAS panel... |
| `scripts/real_agent_scenario_runner.py` | 2422 | misc | 🔴 CRITICAL | CRITICAL. Scenario contract loading/validation (load_scenarios ~280 lines), source-artifact verification, plugin defi... |
| `tests/services/assistant/test_runtime_memory_privacy.py` | 2358 | tests-services | 🔴 CRITICAL | 22 classes mixing infrastructure fakes (ScopedMemoryDatabase, _FakeAcquire, _FakePool, _FakeTransaction, RecordingVec... |
| `tests/services/assistant/test_model_registry_provider_boundaries.py` | 2256 | tests-services | 🔴 CRITICAL | Provider-boundary matrix for ModelRegistry with 6 fakes (_FakeResponse, _StreamContext, _FakeClient, _FailingModelSer... |
| `apps/assistant-service/src/assistant_service/core/assistant_service.py` | 2249 | asst-core-a | 🔴 CRITICAL | AssistantService god class (streaming entry, context building, execution helpers, _working_memory_legacy_scopes at 19... |
| `web/src/pages/agents/agent-studio.css` | 2204 | web | 🔴 CRITICAL | LARGE CSS monolith: single stylesheet for all agent studio pages (list, create, studio, analytics, preview, release) ... |
| `tests/services/knowledge/test_hierarchical_document_deletion.py` | 2193 | tests-services | 🔴 CRITICAL | 19 classes: qdrant/database fakes (RecordingVectorStore, DeleteDatabase, FencedDeleteDatabase, ConcurrentSegmentDatab... |
| `apps/knowledge-service/src/knowledge_service/services/knowledge/ingestion_service.py` | 2138 | knowledge | 🔴 CRITICAL | CRITICAL — ingestion pipeline plus extracted-text budget fences (_require_extracted_text_budget) and image-extraction... |
| `tests/services/knowledge/test_bm25_v2_shadow.py` | 2055 | tests-services | 🔴 CRITICAL | 35 fake client classes (9+ named 'Client') plus shadow write/read/fallback coverage of BM25 v2; contains two confirme... |
| `tests/api/test_eval_traces.py` | 2050 | tests-rest | 🔴 CRITICAL | CRITICAL. 41 tests and 4 fake repository classes. Mixes unrelated concerns: trace CRUD API behavior, eval scoring/gat... |
| `src/services/eval/golden.py` | 2044 | src-services | 🔴 CRITICAL | CRITICAL: live eval-gate engine mixing three concerns — schema constants + observation validation (lines 1-600), the ... |
| `apps/knowledge-service/src/knowledge_service/services/knowledge/embedding.py` | 2031 | knowledge | 🔴 CRITICAL | CRITICAL — all embedding providers (dashscope/gemini/siliconflow), BaseEmbedding, create_embedding factory and multim... |
| `web/e2e/chat-experience.spec.ts` | 2018 | web | 🔴 CRITICAL | CRITICAL monolithic e2e spec: 21 tests covering assistant streaming, sub-agents, artifacts, approvals, telemetry, mod... |
| `apps/assistant-service/src/assistant_service/api/routes/images.py` | 1994 | asst-api | 🔴 CRITICAL | CRITICAL. God module mixing the images route handlers, idempotency claim/replay logic, CAS-advance latest_artifact_id... |
| `packages/ai-gateway-core/src/ai_gateway_core/eval/evaluator_executor.py` | 1991 | gateway-core | 🔴 CRITICAL | CRITICAL: One module holds the full eval pipeline: rule engine (~25 helpers), LLM scoring (JSON parsing, heuristic fa... |
| `apps/assistant-service/src/assistant_service/core/models/model_registry.py` | 1987 | asst-core-b | 🔴 CRITICAL | CRITICAL: single module mixing wire concerns — 15 module-level helpers (SSE event parsing _parse_sse_event, OpenAI to... |
| `src/api/v1/agents.py` | 1984 | src-api | 🔴 CRITICAL | CRITICAL. Agent Studio god-file: ~40 route handlers + ~25 private helpers covering agent identity CRUD, drafts, versi... |
| `tests/services/assistant/test_mcp_runtime.py` | 1984 | tests-services | 🔴 CRITICAL | 21 classes combining MCP client transport (MCPClient/MCPStdioClient), discovery, circuit-breaker/resilience (Blocking... |
| `tests/services/eval/test_evaluator_executor.py` | 1955 | tests-services | 🔴 CRITICAL | Only 3 classes and 6 functions but 1955 lines — extremely long parametrized case tables driving the evaluator pipelin... |
| `packages/ai-gateway-core/src/ai_gateway_core/metrics/usage_recorder.py` | 1940 | gateway-core | 🔴 CRITICAL | CRITICAL: UsageRecorder class (line 134) mixes token accounting, cost calculation via pricing catalog, event-bus dual... |
| `web/src/pages/playground/hooks/usePlaygroundStream.ts` | 1884 | web | 🔴 CRITICAL | CRITICAL god hook: streaming, SSE fallback, legacy chunk conversion, tool-call reconciliation, history restore and se... |
| `apps/assistant-service/src/assistant_service/core/runtime/memory/indexer.py` | 1874 | asst-core-a | 🔴 CRITICAL | MemoryIndexer mixes SQLite ingestion, Qdrant vector-store adapter, query orchestration, and 6+ dataclasses (lines 48-... |
| `tests/database/test_agent_publication_atomicity.py` | 1858 | tests-rest | 🔴 CRITICAL | CRITICAL. 12+ long async tests over release lifecycle, eval manifest atomicity and tenant serialization. Helper funct... |
| `src/adapters/langgraph_proxy.py` | 1833 | src-api | 🔴 CRITICAL | CRITICAL. God class: LangGraphLoadBalancer + HTTP client pool + header building + ACL/ownership verification + quota ... |
| `web/src/components/ServiceConfigDialog.tsx` | 1831 | web | 🔴 CRITICAL | CRITICAL god dialog: three configuration modes (legacy flat config, OAuth/proxy, self-hosted), model failover, load b... |
| `src/proxy/transparent_proxy.py` | 1817 | src-api | 🔴 CRITICAL | CRITICAL. TransparentProxy mixes availability refresh, capacity leases/admission, concurrency semaphores, upstream se... |
| `apps/knowledge-service/src/knowledge_service/services/knowledge/document_service.py` | 1793 | knowledge | 🔴 CRITICAL | CRITICAL — DocumentService mixing upload handling, document CRUD, versioning, text/URL creation and generation fencing. |
| `apps/knowledge-service/src/knowledge_service/services/knowledge/knowledge_service.py` | 1777 | knowledge | 🔴 CRITICAL | CRITICAL — KnowledgeService facade that delegates to retrieval/chunking/embedding managers but still holds ~1000 line... |
| `src/services/knowledge/embedding.py` | 1767 | src-services | 🔴 CRITICAL | CRITICAL: 9 embedding classes/adapters (DashScope/Gemini/SiliconFlow/LocalHash, multimodal, caching, unified results)... |
| `apps/assistant-service/src/assistant_service/core/tool_invoker.py` | 1741 | asst-core-a | 🔴 CRITICAL | RegistryToolInvoker bundles scoped authorization, policy snapshots, ADR-003 result cache (line 187), tool-discovery b... |
| `src/api/v1/agent_runtime.py` | 1736 | src-api | 🔴 CRITICAL | CRITICAL. Gateway-owned preview/published runtime: RedisAgentChannelLimiter Lua script, preview/version/published ses... |
| `tests/services/assistant/test_memory_manager.py` | 1733 | tests-services | 🔴 CRITICAL | 34 test classes covering every memory layer (working/session/long-term store-retrieve-search-delete), the MemoryManag... |
| `tests/services/test_image_sync.py` | 1701 | tests-services | 🔴 CRITICAL | 11 test classes (storage backends, embedding, image processor, DB save_image_segment, sync integration, parser extrac... |
| `apps/knowledge-service/src/knowledge_service/services/knowledge/worker.py` | 1694 | knowledge | 🔴 CRITICAL | CRITICAL — KnowledgeWorker combining queue polling/lease logic, per-document processing steps (text OCR, VLM, PDF spl... |
| `apps/assistant-service/src/assistant_service/core/agent/agent_context_lifecycle.py` | 1692 | asst-core-a | 🔴 CRITICAL | Lifecycle mixin covers compaction (505-605), staged-compaction reads (993), streaming system-prompt assembly (1536), ... |
| `tests/services/knowledge/test_dataset_create_security.py` | 1684 | tests-services | 🔴 CRITICAL | 13 fake classes (repeated _Embedder/_Acquire/_Pool/_Transaction DB-fake pattern plus 8 _Client qdrant fakes) wrapping... |
| `apps/assistant-service/src/assistant_service/core/tasks/task_planner.py` | 1634 | asst-core-a | 🔴 CRITICAL | TaskPlanner plus WorkflowPattern, ExecutionPlan, LLM planning (create_plan at 704) and rule-based planning (922) in o... |
| `tests/scripts/test_judge_real_agent_receipts.py` | 1630 | tests-rest | 🔴 CRITICAL | CRITICAL. 35 tests with ~600 lines of synthetic fixture builders (_contract, _merged_trial, _receipt, _runner_observa... |
| `tests/api/test_image_redesign.py` | 1613 | tests-rest | 🔴 CRITICAL | CRITICAL. 31 tests plus 4 fake classes (_FakeArtifactStorage, _FakeBlobBackend, _FakeImageStateStore, _Harness ~270 l... |
| `tests/services/assistant/test_agent_trace_capture.py` | 1611 | tests-services | 🔴 CRITICAL | 13 classes combining capture, resume-cursor, blocking-cursor and failing-DB variants of agent trace capture with Fake... |
| `tests/scripts/test_validate_env_quickstart.py` | 1609 | tests-rest | 🔴 CRITICAL | CRITICAL. 62 tests, all driving subprocess runs of scripts/new/validate-env.sh (plus deploy.sh/common.sh/status.sh so... |
| `src/api/v1/assistant.py` | 1601 | src-api | 🔴 CRITICAL | CRITICAL. GPT-like assistant API mono-module: models/datasets/config, local-node pairing, sessions CRUD, artifacts, i... |
| `apps/assistant-service/src/assistant_service/core/trace_writer.py` | 1589 | asst-core-a | 🔴 CRITICAL | AssistantTraceContext (458) and AssistantTraceWriter (574) plus span/event serialization helpers in one file. |
| `apps/assistant-service/src/assistant_service/core/agent/subagent_manager.py` | 1563 | asst-core-a | 🔴 CRITICAL | SubAgentManager handles dispatch, batch fan-out, streaming result assembly, and _MAX_PARALLEL_SUBAGENTS throttling in... |

## 五、跨模块主题分析

### 5.1 提取半途而废:knowledge 服务(K5b/K5c)

`apps/knowledge-service` 是从 `src/services/knowledge` 的提取,但只完成了一半:

- **Confluence 栈双份漂移**(≈7900 行 × 2): `client.py`/`models.py`/`parser.py`/`scheduler.py` 两侧逐字节相同;`sync_service.py`(3297 vs 3903 行)与 `image_processor.py`(823 vs 854)已分化;`embedding.py` 分化 464 行;`vlm_service.py` 仍逐字节相同。gateway 侧副本仅被 503 桩路由导入 2 个异常类,从未实例化;service 侧副本仅测试引用。**两侧都未运行** —— Confluence 同步功能实际处于整体断电状态,而 `src/main.py:647-655` 的注释声称迁移已完成(错误注释/过时注释双重问题)。
- **迁移路径断裂**: `apps/knowledge-service/.../persistence/database.py` 按 `Path(__file__).parent.parent.parent / 'database' / 'migrations'` 解析迁移目录,落在不存在的 `apps/knowledge-service/src/database/migrations`(实际迁移在 `apps/knowledge-service/migrations/`),导致 `_auto_apply_*` 全部静默 no-op(005 号甚至抛 RuntimeError)。fresh install 上迁移根本不执行。
- **7 个未接线模块**(~2700 行): `contextual_retrieval`、`enhanced_ingestion`、`image_processing_queue`、`multilingual_embedding`、`processing_dispatcher`、`processor_factory`、`section_extractor`,均为事后添加但从未接入;`processing_dispatcher` 还引用了不存在的 `_ingest_document_internal`。
- **死路由**: `api/router.py` 的 `/{dataset_id}/retrieve` 未挂载,且其内联实现已与权威检索管线(融合/重排/围栏)分化。
- **测试错位**: `tests/services/knowledge/` 中多组测试仍锁定 legacy 副本;`test_image_sync.py` 依赖的 `src.services.storage.image_storage` 在新服务中无对应物,阻塞迁移。

### 5.2 docgen 三份拷贝 + 四处内嵌

- `apps/assistant-service/.../core/docgen/`(58 py,~8685 行)是 `packages/mcp-docgen-server/src/docgen`(~8513 行)的分叉,零生产引用;`tests/assistant/docgen/`(11 文件+golden)是 import 改写克隆。runbook `deploy/runbooks/agent-kb-eval-optimization-20260802/README.md` 已计划退役 assistant 副本。
- 两侧已开始分化(技能路径解析、`Optional` vs `|None`),拖延越久合并成本越高。
- `_skills_data` 中 docx/pptx/xlsx 各内嵌一份 md5 相同的 `scripts/office/`(每份 12 文件,含 847 行 `validators/base.py` ×3),~4700 行重复;第三份变体在 `core/skills/`。
- docgen 服务本身: sandbox 层(4 文件)零生产消费者;vision critic 未接线且实现已坏(AsyncAnthropic 上同步调用);`dispatcher.fix → renderer.fix` 死链。

### 5.3 src/core 死链与 no-op 特性

- **死中间件链**(~1800 行)与**死限流包**(~855 行): 见执行摘要 3、4。
- **TaskQueue 空转**: `main.py:534` 调用 `start_worker()` 但零 handler 注册(全库无 `register_handler`/`enqueue` 调用),worker 从未真正启动,`main.py:536` 日志自证 'worker active, no handlers registered'。process_file 管线已于 Phase 5d 移除,bootstrap 可整体删除。
- **observability 死面**: `tracing.py` 5 个 helper + `TracingMiddleware` 零引用;`metrics.py` 的 `collect_all`/`register_histogram` 零调用;`observability/__init__.py` barrel 从不被导入。
- **死配置**: `src/config/constants.py`(33 常量全零引用)、`settings.py` 的 `KnowledgeSettings`(~175 行)/`ConfluenceSettings`(~34 行)近乎全死(仅 `dashscope.api_key` fallback 被读);`StreamingAuthConfig.anonymous_enabled/anonymous_header`、`StreamingTracingConfig.service_name`、`StreamingLogConfig.log_request_body/log_response_body` 均为只写不读的死字段。
- **遗留孤儿模型**: `src/models/knowledge.py`(311 行)、`src/models/job.py`(131 行)KB 时代遗留,零引用。
- **connectors 动态死面**: `grpc.py`/`websocket.py`/`message_queue.py` 仅能通过 `create_connector()` 动态触达,但无任何 service 注册这些类型。

### 5.4 assistant-service 内部

- **RAG Manus 分析集群**(~3400 行): `rag/{scenario_analyzer,query_intent_analyzer,rag_metrics,scenario_aware_retriever,context_manager}.py` 在 `agent_loop.py:285-292`、`assistant_service.py:380/449` 被构造但从不调用;且与 `prompts/scenario_analysis_prompts.py` 概念重复(两套 scenario 分类实现)。
- **AGUIEventEmitter**(896 行): 34 个公开方法仅 12 个被调用(全部来自 `content/content_generator.py`)。
- **local-node 控制面整体未接线**: `wire_local_node_control_plane`/`build_local_node_tool_provider` 无调用方,全部 `/local-nodes` 路由永久 fail-closed 503;`wiring.py`/`sqlite_repository.py`/`device_delivery.py` 为 dev/test-only 缝隙(文件内自述属实)。
- **设置双重体系**: `config/settings.py` 被 `config/startup_fingerprint.py` 完全遮蔽;`user_context.py` 读取的 `app.state.settings` 无人赋值。
- **Phase 5d shim 收尾**: `quiz/__init__.py`、`tasks/task_types.py` 零引用;`context_engine.py`、`streaming_writer.py`、`task_planner.py`、`tools/style_presets.py` 仅测试引用 —— 6 个文件均可删(测试重定向后)。
- **prompts/__init__.py** re-export ~90 个符号,~75 个零生产消费;其 docstring 自己记录了同类的裁剪先例。

### 5.5 gateway-core 与 src/services 的提取边界

- **死仓库链**: `DatabaseStorage.repos`(database.py:174-188)+ 6 个 repository 模块(~2074 行)+ barrel + shim 完全断开 —— 无处 import `src.persistence.repositories`。
- **billing 双模块是活的**(修正记录见 §7.1): `usage_scheduler.py`/`aggregation_task.py` 经 `main.py:668-680` 启动,非死代码。
- **三处 task 子系统并存**(live 去重候选): `src/services/task`、`ai_gateway_core.tasks`、`src/core/tasks/queue.py`。
- **eval 双层**: `src/services/eval/eval_outbox_worker.py` 包裹 `ai_gateway_core.eval` 同名逻辑。
- **storage 5f shim 收尾**: `src/services/storage/*` 为 Phase 5f 转发 shim,运行时已直连 `ai_gateway_core.storage`;仅 `tests/services/test_image_sync.py` 仍走旧路径。
- **`__version__` 漂移**: `ai_gateway_core/__init__.py` 0.1.0 vs `pyproject.toml` 0.1.1,且无消费者。
- **README 过时**: 自称 'Protocol contracts only / must stay lean',实际 ~50k LOC + asyncpg/aioboto3/oss2/Pillow/numpy/OTel 等重依赖。

### 5.6 web 前端

- **互死链**: `pages/Dashboard.tsx → ServiceCostAnalysis.tsx / UserServiceUsageAnalytics.tsx / SecurityEventCharts.tsx → api/metrics.ts`(live 仪表盘走 `pages/dashboard/`)。
- **三套并行的 Confluence 同步 UI**: `pages/confluence/*`、`pages/knowledge/sync/*`、`pages/tasks/*` 功能重叠。
- **V1 UI 残骸**: `pages/assistant/components/index.ts` barrel 仍导出 9 个无页面引用的组件(PromptSuggestions、ModelSelector、KBSelector、TaskPanel 等);`VITE_ASSISTANT_UI_V2` 未写入 `.env.example` 且 V1 分支引用已不存在的组件(如 `index.tsx:1017` 的 TaskPanel)。
- **API client 死面**: `api/knowledge.ts` 11 个、`api/files.ts` 6 个、`api/confluence.ts` 7 个、`api/quiz.ts` 4 个等约 30 个导出函数无调用方。
- **`SSEEventType` 10 个死成员**(CACHE_METRICS、MEMORY_LOADED、TOOL_ERROR 等)。
- **应删而非重构的超大文件**: `pages/Playground.legacy.tsx`(2244 行,无路由)与 `pages/confluence/ConfluencePage.tsx`(1171 行,仅 barrel 转发)。
- **e2e 全部 22 个 spec 健康**(路由/选择器/静态资源均有效)。
- `DatasetDetail.tsx`(4854 行)同时对接 legacy 与 knowledge-service 两套 endpoint,是巨大的根本原因之一。

### 5.7 SDK 与脚本

- **默认模型漂移**(4 个 SDK 全部): 见执行摘要 8;修复即改默认值 + 文档字符串(Java `ChatRequest.java:75` 注释、CLI help 文本、`sdk/openapi.json` 重新生成对照)。
- **Python SDK MCP HTTP 半接线**: HTTP transport 下 `call_tool()` 因 `_jsonrpc` 断言 `self._process.stdin`(stdio-only)而崩溃。
- **CLI 死层**: `src/tools/*`、`src/agent/*`、`src/hooks.ts`、confirm 流从活入口 `cli.tsx → app.tsx` 不可达;`/mcp connect` 成功但 `callTool` 只被死 `agent/loop.ts` 引用 —— MCP 服务器连上后无法被调用。
- **打包瑕疵**: `sdk/cli/package.json` dependencies 为空(ink/react 等在 devDependencies);esbuild 目标 node18 vs engines node ≥22.12。
- **scripts/docgen_* 家族**(5 文件)导入已不存在的 `src.services.assistant.docgen.*` —— 既孤儿又已损坏。
- **机器特定硬编码**: `scripts/native_agent_parity_benchmark.py` 硬编码 `/Users/yang/projects/Hermes_agent`、`/Users/yang/projects/open claw/openclaw`。
- **`scripts/goal/`**(2026-07-16 一次性 scratch)零引用。

### 5.8 测试卫生

- **全部 487+ 个测试文件 import 均可解析**(AST 验证)—— 无『测试整文件死』;但存在: 全 skip 的 `tests/api/test_kb_tools.py`(843 行)、pytest 永不收集的 `tests/knowledge/tools/batch_ingest.py`(354 行)。
- **重复 helper**: `StubEmbedder` ×3、`_clear_env` ×2、`RecordingVectorStore` ×2、`create_test_token` ×2(conftest 与 test_gateway.py 副本的 tenant 默认值已分化)—— 应提取到各自 conftest。
- **脆弱跨文件 helper 依赖**: `tests/database/test_agent_studio_migrations.py` 的 `_postgres_config` 被 4 个测试 import;`test_agent_channel_runtime.py` 用 `pytest_plugins` 加载另一个测试文件。
- **重叠覆盖**: middleware 链 2 组、service_access 2 组、gateway_secret_middleware 2 组(contract vs packages)需合并。
- **遗留目标测试**: 6 组测试锁定 `src.services.knowledge.*` legacy 副本(见 5.1)。

### 5.9 基础设施与仓库杂物

- **4 套迁移 runner 并存**: `database/cli.py`(孤儿)、`database/run_migration.py`(legacy/test-only)、`database/migrate_per_service.py`(compose migrate 服务)、`scripts/new/migrate.sh`(`make migrate`);两套 tracking 表(`schema_migrations` vs `schema_migrations_meta`);另有手写 1962 行 `schema.sql` 聚合可与 80 个迁移文件漂移。
- **`docker/monitoring/` 整套**(prometheus/alertmanager/otel/grafana)仅被 `tests/monitoring/test_alert_rules.py` 引用;`deploy/helm/ai-gateway/values-production.yaml` 零引用。
- **`docker-compose.capability.yml`、`docker-compose.kbms.yml`** 无引用(与自文档化的 `override.yml.example` 不同,可删)。
- **`apps/islamic-content-service/`** 空壳(仅一个 seed JSON);RELEASE.md 仍列其镜像、`scripts/new/common.sh` 仍探测其容器。
- **根目录杂物(全部 untracked)**: `.tmp-general-agent-live-orchestrate.sh`、3 张验证截图、`tmp/`、`uploads/`、`claude-code/`(空目录)。
- **vendored 内容确认**: `skills/`(21 个技能,skills-lock.json 锁 pbakaus/impeccable)与 `agent-plugins/`(compose 挂载 + 插件校验脚本)为第三方内容,不属项目重构范围。

## 六、复核记录与修正

主代理对 12 项高影响结论做了独立抽查(实际 import 语句级验证,非词法匹配):

| 结论 | 抽查方式 | 结果 |
|---|---|---|
| `src/core/ratelimit/` 零生产引用 | `rg 'import.*ratelimit\|from.*ratelimit' src` | ✅ 确认(先前命中的只是注释/`rate_limit_http` 等词法匹配) |
| `src/config/constants.py` 零引用 | import 语句级 rg | ✅ 确认(`tool_selector.py` 命中的是 assistant 自己的 `core/tools/constants.py`) |
| `src/models/knowledge.py`、`src/models/job.py` 零引用 | 全库 rg | ✅ 确认 |
| `src/core/middleware` 链零生产引用 | import 语句级 rg | ✅ 确认(命中的是第三方 `starlette.middleware.base`) |
| `src/config/logging.py` 零引用 | 全库 rg | ✅ 确认 |
| `Playground.legacy.tsx` 无路由 | `web/src` 全量 rg | ✅ 确认 |
| SDK 默认模型 qwen3.6-plus 漂移 | 双侧 grep | ✅ 确认(服务端 `qwen3.7-plus` ×2 处) |
| knowledge-service 迁移路径断裂 | 目录解析 | ✅ 确认(`src/database` 不存在,迁移实际在 `apps/knowledge-service/migrations/`) |
| docgen 分叉存在 | tests import 抽查 | ✅ 确认(`tests/assistant/docgen` 导入 `assistant_service.core.docgen`) |
| **billing `usage_scheduler`/`aggregation_task` 死代码** | `main.py` 全文核对 | ❌ **误报** — 见 §6.1 |
| **confluence 栈 'gateway 副本活 / service 副本死'** | `confluence.py` + `main.py:640-660` 核对 | ⚠️ **不精确** — 见 §6.2 |

### 6.1 修正:billing 定时任务模块是活代码

tests-services 代理称 `src/services/billing/usage_scheduler.py` 与 `aggregation_task.py` 全库零引用 —— 实际 `src/main.py:668-680` 调用 `init_usage_scheduler()` 并 `start()`,`usage_scheduler.py:15` 导入 `aggregation_task` 并在每日聚合中使用。该条已从报告正文删除;两者是**活跃的账单聚合子系统**,清理时应改为关注其是否应随 billing 提取迁移,而非删除。

### 6.2 修正:Confluence 双副本的运行时真相

knowledge 代理判定 'gateway 副本活,service 副本死' 不精确。实际: `src/api/v1/confluence.py` 从 legacy `sync_service.py` 仅导入 2 个异常类;gateway 从未实例化 ConfluenceSyncService(`app.state.confluence_sync_service = None`),依赖该 state 的端点全部返回 503(见 `main.py:647-655` 注释,其 '迁移已完成' 的说法与事实矛盾 —— service 侧 `main.py` 也毫无 confluence 引用)。**结论: 两侧副本都未运行,Confluence 同步整体断电;≈7900 行逻辑双份漂移,零份工作。** 清理方向不变(保留一份、删除一份),但优先级应比 '删除死副本' 更高 —— 这是一起功能静默下线的回归,需先决策恢复哪一侧。

### 6.3 代理侧未逐条复核的结论

其余 300+ 条明细为代理基于全库 rg 的结构化验证结果(报告中置信度为 '中'/'低' 的条目建议在动手清理前逐条人工确认)。删除任何整文件/整包前,请按代理给出的证据命令再跑一次 `rg`。

## 七、清理优先级建议

### P0 —— 纯删除,零运行时风险(预计 -1.5 万行)

| # | 目标 | 规模 | 依据 |
|---|---|---|---|
| 1 | `src/core/middleware/{base,auth,request_logging,validation,retry,session,circuit_breaker,concurrency,logging,rate_limit}.py` + 链测试 | ~1800 行 | 被 streaming.py 取代,零生产引用 |
| 2 | `src/core/ratelimit/` 包 + 专属测试 | ~855 行 | 零生产引用 |
| 3 | `src/config/constants.py`、`src/models/knowledge.py`、`src/models/job.py`、`src/config/logging.py`(死函数) | ~520 行 | 全库零引用(已复核) |
| 4 | `src/services/knowledge/` 7 个未接线模块 + `kb_proxy_client.py` + 死 barrel | ~2800 行 | 从未接入,其中一处引用不存在的方法 |
| 5 | `tests/api/test_kb_tools.py`(全 skip) | 843 行 | `pytestmark = skip` |
| 6 | web 互死链: `pages/Dashboard.tsx` + 3 个分析组件 + `api/metrics.ts` | ~1000+ 行 | 互死链已闭环 |
| 7 | `web/pages/Playground.legacy.tsx`、`ConfluencePage.tsx`(barrel-only) | ~3400 行 | 无路由引用 |
| 8 | `src/core/tasks/queue.py` 的 worker bootstrap(`main.py:534-536`) | 小 | 零 handler,永远空转 |
| 9 | 根目录杂物(截图/sh/tmp/uploads/空目录) | 磁盘 | 均 untracked |
| 10 | assistant `quiz/__init__.py`、`tasks/task_types.py`(零引用) | 小 | 已复核零引用 |

### P1 —— 删除 + 测试/引用重定向(预计 -2 万行)

| # | 目标 | 规模 | 前置条件 |
|---|---|---|---|
| 1 | assistant `core/docgen/` 分叉 + `tests/assistant/docgen/` 克隆 + 孤儿 `core/skills/{docx,pdf,pptx,xlsx}` bundle | ~8685+ 行 | 按 runbook 计划:mcp-docgen-server 成为唯一实现且 fallback 测试通过后删 |
| 2 | RAG Manus 分析集群(5 文件) | ~3400 行 | 与 `prompts/scenario_analysis_prompts.py` 二选一,构造点(`agent_loop.py:285-292` 等)同步清理 |
| 3 | gateway-core 死仓库链(6 repository 模块 + repos dict + barrel + shim) | ~2074 行 | 确认无外部包依赖(已零引用) |
| 4 | `src/services/storage/` 5f shim 收尾 + `test_image_sync.py` 重定向 | 小 | 该测试同时依赖无新等价物的 `image_storage`(见 5.1) |
| 5 | observability 死面(`tracing.py` helper/TracingMiddleware、`metrics.py` collect_all/register_histogram、死 barrel) | ~300 行 | 与活跃 OTel 路径核对后删 |
| 6 | `StreamingAuthConfig`/`StreamingTracingConfig`/`StreamingLogConfig` 死字段 + `KnowledgeSettings`/`ConfluenceSettings` 死段 | ~250 行 | 保留 `dashscope.api_key` fallback |
| 7 | AGUIEventEmitter 22 个死方法 | ~400 行 | 确认事件路径唯一化 |
| 8 | prompts barrel 75 个死符号 + `observability/__init__.py` 等死 barrel | 小 | — |
| 9 | web: V1 残骸(barrel 9 组件 + VITE_ASSISTANT_UI_V2 V1 分支)、api client 死导出 ~30 函数、SSEEventType 10 死成员 | ~1500 行 | 确认无动态 import(`import()` 字符串) |
| 10 | SDK 默认模型 qwen3.6-plus → 3.7-plus + 文档字符串/help 同步 | 4 处 | 与服务端默认值对齐 |
| 11 | CLI 死层(tools/agent/hooks)与 `scripts/docgen_*`、`scripts/goal/` | ~2000+ 行 | 决策:恢复接线 or 删除 |
| 12 | 测试 helper 去重(StubEmbedder ×3、create_test_token 等)+ 重叠覆盖合并 + 遗留目标测试重定向 | 小 | 逐组进行 |
| 13 | 迁移 runner 四合一(保留 compose + make 两条路径) | — | 统一 tracking 表,决策 `schema.sql` 去留 |
| 14 | local-node 控制面:接线 or 删除(现状全部 503) | ~1500 行 | **产品决策**,非纯工程 |
| 15 | 空壳清理: `apps/islamic-content-service`、`docker-compose.capability/kbms.yml`、`docker/monitoring/`、`values-production.yaml` | 小 | 确认监控栈无外部部署引用 |

### P2 —— 重构级拆分(需设计,逐个立项)

| # | 目标 | 规模 | 拆分思路 |
|---|---|---|---|
| 1 | `apps/knowledge-service/.../persistence/database.py` | 9559 行 | 按 domain(connection/vector/relational)拆 + 修迁移路径断裂(§5.1) |
| 2 | `packages/ai-gateway-core/.../repositories/agent_repository.py` + `persistence/database.py` | 7540 / 7322 行 | 已有失败提取记录;按 repository 拆分 + 拆 ORM 定义 |
| 3 | `web/src/pages/knowledge/DatasetDetail.tsx` | 4854 行 | 按功能块(数据/检索/评测/RAGAS)拆;先定单一后端契约 |
| 4 | Confluence 双副本裁决(§6.2) | ~7900×2 行 | 先决定恢复哪一侧,再删另一侧 |
| 5 | docgen `_skills_data` 三份字节级重复 | ~4700 行 | 单一共享源 + 构建期注入 |
| 6 | `MULTIMODAL_EMBEDDING_MODELS` 四重定义 | 小 | gateway-core 为唯一源,三处消费 |
| 7 | `src/adapters/langgraph.py`(1500)vs `langgraph_proxy.py`(1833) | ~3333 行 | 旧 adapter 仅靠 ServiceRegistry 存活,评估合并 |
| 8 | `src/services/task` / `ai_gateway_core.tasks` / `src/core/tasks/queue.py` 三处并存 | — | 提取边界统一 |
| 9 | 其余 59 个 ≥1500 行 Python 文件(见表 §4) | — | 按依赖热度排序逐个立项 |

### 清理前检查清单(每批删除)

1. 按报告证据命令重跑 `rg`(排除 `__pycache__`/生成物)。
2. 检查 Dockerfile/compose/Makefile/helm/runbook 中的路径引用(代理已查,复核一次)。
3. 跑全量测试 + `ruff check` + `mypy` + web `eslint`;确认 `--cov-fail-under=25` 不因删除而跌破。
4. 涉及 shim 的删除核对 `AGENTS.md` 与 phase 文档(5d/5f/K5b/K5c)的迁移计划,避免删除已计划的兼容层。
5. vendored 内容(`skills/`、`agent-plugins/`、`core/skills` bundle)不得混入项目删除批次。

## 附录 A:代理覆盖范围

| 模块 | 覆盖路径 |
|---|---|
| src-core | src/ 根文件 + src/core、src/models、src/config、src/persistence、src/connectors、src/assets |
| src-api | src/api、src/proxy、src/adapters |
| src-services | src/services(遗留服务层) |
| asst-core-a | apps/assistant-service core/ 根 + agent、runtime、tools、tasks、memory、files、office、content、quality、audit、providers |
| asst-core-b | apps/assistant-service core/docgen、skills、mcp、rag、prompts、models |
| asst-api | apps/assistant-service api/、core/local_node、core/gateway、config、auth + 包根 |
| knowledge | apps/knowledge-service(src + 应用级测试) |
| gateway-core | packages/ai-gateway-core(src + tests) |
| docgen (packages/mcp-docgen-server) | packages/mcp-docgen-server(src + tests) |
| web | web/src、web/e2e |
| sdk | sdk/cli、sdk/python、sdk/java、sdk/dart |
| tests-services | tests/services(551 文件,系统抽样 + rg 全量) |
| tests-rest | tests/ 其余(tests/api、core、database、contract、integration、security、scripts、packages、fixtures 等) |
| misc | scripts/、apps/local-node、apps/islamic-content-service、agent-plugins、skills、database、config、deploy、docker、examples |

## 附录 B:清理跟踪建议

建议把本报告转为逐条可勾选的清理看板(每类一条 issue/卡片),清理时在报告中标注结果(已删/保留及原因/已重构),使本报告成为清理工作的单一日程。后续轮次扫描可用同一套 14 模块工作流增量重跑(脚本保存在会话目录,可复用)。

## 附录 C:2026-08-26 清理轮次记录(codex-harness 迁移后残余)

分支 `audit/codex-harness-parity-cleanup`(PR base: `fix/agent-runtime-image-and-v1-chat-auth`)。按最小粒度原则执行的已删/保留结论:

**已删除(Python / scripts)**

- `scripts/test_style_presets_e2e.py`、`scripts/smoke_native_search.py`(import 已不存在的 `src.services.assistant.*`,本身已损坏)、`scripts/new/prepare-agent-studio-e2e-account.py`、`scripts/goal/exercise-shipped-paths.py`
- `src/proxy/context_injector.py`:`extract_context_from_scope`、`extract_context_from_request`
- `src/adapters/langgraph_proxy.py`:`_make_request` + `LangGraphLoadBalancer.add_instance/remove_instance/mark_unhealthy/mark_healthy`
- `src/core/gateway/capacity.py`:`inventory_rows`;`src/core/middleware/rate_limit_http.py`:`RateLimitMiddleware`、`RateLimitConfig`
- `ai_gateway_core`:`events.GROUP_NAMES`、`config.DASHSCOPE_DEFAULT_LEGACY_BASE_URL`、`image.claim_next_image_tasks`(含 `__all__`/re-export)

**已删除(Web)**

- `web/src/api/*` 中 43 个零调用导出(经 named/namespace/dynamic-import/barrel 四类解析复核;`chat`、`createAgentThread`、`interruptAgentTurn`、`startAgentTurn`、`projectAgentV2Event`、`compressImage`、`fetchSetupState`、`ProcessingModes` 因仅本文件自引用改为取消 export)
- 8 个零引用组件文件:`components/{HealthBadge,TaskTable}.tsx`、`components/ui/slider.tsx`、`pages/dashboard/components/{SummaryCharts,KPICards,DashboardSkeleton,DashboardHeader}.tsx`、`pages/knowledge/detail/HierarchicalSegmentCard.tsx`

**明确保留(报告候选但不属重构残余)**

- `scripts/gateway_capacity_inventory.py`、`scripts/uat_gateway_capacity_probe.py`、`scripts/uat_gateway_capacity_smoke.py`、`scripts/metrics/repair_usage_metrics.py` — 运维探针/修复工具,非老代码残余
- `prepareAssistantRunResume`(`useChatSession.ts:3640` 有活引用);`models/providers` CRUD(`Services.tsx` 经 `import * as modelsApi` 命名空间导入使用,初版静态解析误报为死)

**其他**

- `src/main.py:1340` 启动日志去掉过时的 `model_registry ready` 措辞;`scripts/harness/runtime_dependency_gate.py` docstring 更新(退役树已不在 checkout)
- 5 个 `deploy/runbooks/*/loop-state.json` 追加 `superseded_by` 并终止 `next_action`:AGA-06、AHR-05、ARO-05、ACU-00、CHR-05
- ruff 基线:`src` + `packages/ai-gateway-core/src` 存量 49 错为分支既有债务,本轮零新增(本轮引入的孤儿 import/换行已全部修复,改动文件 ruff 清零)
