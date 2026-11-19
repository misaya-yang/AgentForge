# SOTA 剩余阶段实施报告（Claude Code，2026-08-17）

> 范围：`docs/plans/sota-performance-claude-handoff.md` 提示词。
> 状态以 `docs/plans/sota-performance-optimization-2026-08.md` 为准。
> 每阶段：改动文件 / 运行命令 / 通过 / 失败 / skipped / 剩余风险。

## 已闭环的既有回归（review 发现，非计划项）

PCH-07 "final handoff edits" 在 `execution_lifecycle.py` 加入的终局 `trace_writer.drain(strict=True)` 违反
trace 契约（"trace capture 绝不阻塞用户路径"，`trace_writer.py` 模块 docstring），导致两个 HEAD 测试失败：

- `tests/services/assistant/test_agent_trace_capture.py::test_non_stream_final_response_does_not_wait_for_trace_persistence`
- `tests/services/assistant/test_agent_trace_capture.py::test_run_status_update_does_not_wait_for_trace_persistence`

修复（不删除 drain，不改变暂停路径的 resume barrier）：
- `apps/assistant-service/src/assistant_service/core/agent/execution_lifecycle.py`
  - 非暂停终局 drain 超时上限 `_TERMINAL_DRAIN_MAX_S = 0.25`（健康 DB 毫秒级完成；阻塞 DB 最多 250ms，不再等满 `write_timeout_s`）。
  - 两条 drain 都加上 `not ctx.trace_capture_disabled`（与 `_finish_trace` 一致；恢复失败禁用 trace 时不得碰 writer）。

命令：`uv run --all-packages --extra test pytest -q --no-cov tests/services/assistant/test_agent_trace_capture.py`
→ **39 passed**；`tests/services/assistant/test_agentloop_streaming_first_contract.py` → **125 passed**。

---

## 阶段 1：SPO-00 仪表

**改动文件：**

| 文件 | 内容 |
| --- | --- |
| `src/core/hot_path_metrics.py`（新） | `gateway_hot_path_metrics.redis_round_trips` 计数器（dataclass + reset，同 `memory_index_metrics` 模式） |
| `src/core/middleware/rate_limit_http.py` | `SlidingWindowRateLimiter._redis_check`：pipeline 执行 +1 RTT，拒绝路径 `zrange` +1 RTT |
| `src/core/gateway/multi_dimension_rate_limiter.py` | `MultiDimensionRateLimiter._redis_sliding_window`：同上 |
| `apps/assistant-service/src/assistant_service/core/trace_metrics.py`（新） | `trace_writer_metrics.sql_statements` 计数器 |
| `apps/assistant-service/src/assistant_service/core/trace_writer.py` | `_trace_execute` / `_trace_executemany` / `_trace_fetchrow` 包装器计数；全部 DB 语句走包装器 |
| `apps/knowledge-service/src/knowledge_service/services/knowledge/vector_store_metrics.py`（新） | `vector_store_metrics.get_collection_calls` 计数器 |
| `apps/knowledge-service/src/knowledge_service/services/knowledge/vector_store.py` | `require_collection_readable`（每次检索必经的读权威检查）计数 |

**新增测试（打真实入口）：**

- `tests/core/test_hot_path_metrics.py` — 假 Redis 驱动两个 limiter 的 `check()`：允许 1 RTT、拒绝 2 RTT、多维度每维 1 RTT。**4 passed**
- `tests/services/assistant/test_trace_writer_metrics.py` — 假 DB 驱动 `AssistantTraceWriter.start_trace/record_event/finish_trace/drain`，计数器与实际发出语句数一致（8 = root+lifecycle+3 events+finish×3；ttft 场景 4）。**2 passed**
- `tests/services/knowledge/test_vector_store_metrics.py` — 假 Qdrant 客户端驱动 `VectorStore.search`（真实检索入口），每次检索 `get_collection` = 1。**2 passed**

**命令与结果：**

```
uv run --all-packages --extra test pytest -q --no-cov \
  tests/core/test_hot_path_metrics.py tests/core/test_streaming_rate_limit.py \
  tests/core/test_multi_dimension_rate_limit_config.py \
  tests/services/assistant/test_trace_writer_metrics.py \
  tests/services/assistant/test_agent_trace_capture.py \
  tests/services/assistant/test_agentloop_streaming_first_contract.py \
  tests/services/assistant/test_runtime_memory_index_noop.py \
  tests/services/knowledge/test_vector_store_metrics.py \
  tests/services/test_vector_store_search.py \
  tests/middleware/test_security_headers_streaming.py \
  tests/api/test_sessions_assistant_compat.py
→ 222 passed
uv run --all-packages --extra dev ruff check <全部改动文件> → All checks passed
```

**实机基线（compose 归本 checkout 所有，栈已 up）：**

- `make hot-update`（Python 全量）→ 三服务重启健康。
- 隔离账号：`uv run python scripts/new/prepare-agent-studio-e2e-account.py --output tmp/ttft-isolation-account.env`（一次性账号，凭据不进 .env）。
- `uv run python scripts/assistant_ttft_benchmark.py --env-file .env --output reports/performance/assistant-ttft-thinking-low-2026-08-17.json --trials 10 --thinking-level low`
  → 10/10 successful；**TTFT p50 4.089 s / p95 4.544 s / min 3.841 s / max 4.544 s**；p50 ceiling 3.41 s → `passed: False`（仍高于产品门槛，如实记录）。
  对比 2026-08-16：p50 3.925 s / p95 4.307 s。
- 八场景 cohort：`uv run python scripts/native_agent_parity_benchmark.py --env-file .env --output-dir reports/performance/native-agent-parity-2026-08-17-spo00`
  → **ai_platform 5/8、hermes 5/8、openclaw 7/8，全部 0 infrastructure errors**。
  失败场景与 2026-08-16 基线一致（research.cra_source_resolution / engineering.inventory_reservation_patch / finance.unknown_effect_recovery 等真实失败）。
- 注：cohort 完成后 compose 栈被外部停止（SIGTERM）；本会话后续阶段不做实机验证，若需复跑先 `docker compose up -d` 再 `make hot-update`。

**剩余风险：** 计数器只覆盖两套滑窗 limiter 的 Redis 分支；GW2 落地后准入/释放的 RTT 由 `_LuaFakeRedis` 测试直接计数（见阶段 3）。

---

## 阶段 3：SPO-02 剩余 = GW2 + GW3

**改动文件：**

| 文件 | 内容 |
| --- | --- |
| `src/core/gateway/lua_scripts.py`（新） | 三个原子 Lua：`CAPACITY_ACQUIRE_PAIR_LUA`（shared+租户份额单 EVAL，shared 拒绝时不碰租户键）、`CAPACITY_RELEASE_LUA`（多键 ZREM 单 EVAL）、`SLIDING_WINDOW_CHECK_LUA`（N 键滑窗，返回拒绝维度与最早 score） |
| `src/core/middleware/rate_limit_http.py` | `SlidingWindowRateLimiter` 增加 `check_many`：按 window 分组，每组一个原子 EVAL；拒绝请求不再被记录（与内存路径语义统一） |
| `src/core/middleware/_streaming/rate_limit.py` | 中间件所有维度一次 `check_many`；新增租户维度；把实际计数的维度名写入 `scope["state"]["rate_limit_counted_dimensions"]` |
| `src/core/gateway/multi_dimension_rate_limiter.py` | `check(skip_dimensions=...)`；`_redis_sliding_window` 单 EVAL（消除 TOCTOU 与拒绝路径额外 ZRANGE） |
| `src/core/gateway/admission.py` | `_acquire_shared_and_tenant`：shared+tenant 单 EVAL（原先 6 次 RTT 且多副本 TOCTOU）；`_release_shared_pair` 单 EVAL；删除 `_acquire_shared`/`_acquire_tenant_shared` |
| `src/api/deps.py` | `enforce_rate_limit` 只跳过中间件实际计数过的维度（`rate_limit_counted_dimensions`），操作/assistant 维度仍由路由层计数；中间件未运行时路由层保持全维度权威 |
| `src/core/middleware/_streaming/auth.py` | GW3：JWT 用与 deps 相同的 `decode_jwt_token`（同 secret 源/算法/aud/iss）验一次，成功时把 claims 存入 `scope["state"]["verified_jwt_claims"]`；失败仍保持「不拒绝、降级」契约 |
| `src/api/deps.py` | `get_user_context` 与 `get_auth_context` 复用 `verified_jwt_claims`（无则严格解码）；jti 吊销、sub 存在性、角色归一化保留在 deps |
| `src/main.py` | `StreamingRateLimitConfig` 接租户限额（仅在无自定义 tenant_limits 时镜像，否则路由层继续数租户维度）；`StreamingAuthConfig` 接 audience/issuer |

**语义收紧（记录在案）：**
- Redis 滑窗拒绝路径不再记请求（原 pipeline 实现拒绝时也 ZADD，内存路径从不记）——两者统一为不记。
- MultiDimension 拒绝条件从 `count > limit` 统一为 `count >= limit`（内存路径原本如此；Redis 路径原先多放行 1 个请求/窗口）。
- 准入的 shared+tenant 在单 EVAL 内保持 shared-before-tenant 拒绝顺序；degraded-open 时租户回退本地预算。

**新增测试（打真实入口）：**

- `tests/core/test_gateway_lua_rate_limit.py`（5 个）：
  1. 中间件 3 维（global+guest+ip）一次 EVAL = 1 RTT；
  2. 真实 JWT 端到端：中间件 4 维（global+user+tenant+ip）1 EVAL + 路由操作维 1 EVAL；`ratelimit:user:*` 恰好出现 1 次、`ratelimit:op:*` 恰好 1 次（只增加一次计数）；
  3. 准入 acquire 1 RTT + release 1 RTT；
  4. 并发 10 个 acquire、limit=2 → 恰好 2 个获准（Lua TOCTOU）；
  5. 拒绝维度索引 + Retry-After 头。
- `tests/core/test_hot_path_metrics.py`（重写为 EVAL 语义）：allowed 1 RTT、denied 1 RTT（retry 数据在 EVAL 结果内，无二次 ZRANGE）、多维度每维 1 RTT。
- `tests/proxy/test_gateway_shared_upstream_budget.py` / `test_gateway_admission_control.py`：fake 适配 pair-Lua 契约，`_release_many` 测试适配 `_release_shared_pair` 三元组。

**命令与结果：**

```
uv run --all-packages --extra test pytest -q --no-cov \
  tests/core/test_gateway_lua_rate_limit.py tests/core/test_hot_path_metrics.py \
  tests/proxy/test_gateway_shared_upstream_budget.py tests/proxy/test_gateway_admission_control.py \
  tests/proxy/test_admission_metrics.py tests/core/test_streaming_rate_limit.py \
  tests/core/test_multi_dimension_rate_limit_config.py tests/core/test_gateway_middleware_order.py \
  tests/core/middleware/test_jwt_verification.py tests/api/test_gateway_auth_context_contract.py \
  tests/api/test_auth_jit_security.py tests/integration/test_gateway.py \
  tests/integration/test_permissions.py tests/proxy/test_rate_limit.py \
  tests/contract/test_gateway_secret.py tests/contract/test_auth_e2e.py \
  tests/middleware/ tests/security/test_release_secret_regressions.py \
  tests/api/test_assistant_sessions.py tests/api/test_users_tenant_isolation.py \
  tests/services/assistant/test_model_registry_provider_boundaries.py
→ 全部通过（分段运行：102+13+52+113+…，0 失败）
uv run --all-packages --extra dev ruff check <全部改动文件> → All checks passed
```

**实机验证：** GW2/GW3 完成后 compose 栈已由外部停止；`make hot-update ARGS="--gateway"` 未执行成功（容器不在运行）。main.py 语法/导入级验证通过。**实机冒烟 not verified** —— 复跑路径：`docker compose up -d` → `make hot-update ARGS="--gateway"` → `make status` → 登录态请求 `/api/v1/assistant/models` 与 `/api/v1/assistant/chat/stream`。

**剩余风险：**
- 中间件租户维度只在「无自定义 tenant_limits」时镜像默认限额（main.py 守卫）；有自定义限额的部署中路由层继续数租户维度（语义保留，多 1 RTT）。
- `check_many` 的 allowed 结果不再带 per-dimension remaining（响应头未用到，仅 429 路径使用 remaining=0）。
- 真实 Redis 上的 Lua 脚本未实机执行（fake 模拟契约）；`SLIDING_WINDOW_CHECK_LUA` 的 redis.call 语法在 Redis 7 有效，但未在实机 EVAL 验证。

---

## 阶段 2：W3 发送框解锁

**改动文件：**

| 文件 | 内容 |
| --- | --- |
| `web/src/pages/assistant/lastModel.ts`（新） | `readLastModelId` / `writeLastModelId`，localStorage 键 `assistant.lastModelId.v1`，存储不可用时静默降级 |
| `web/src/pages/assistant/index.tsx` | `selectedModel` 用缓存初始化；新增 `modelsLoaded`（loadData 的 finally 置位）；loadData 校验缓存并回写；模型选择器与 session 恢复路径回写缓存；`handleSend` 只在「catalog 已加载且为空」时拦截；composer `hasAvailableModel` 允许 catalog 未返回时用缓存解锁 |
| `web/e2e/chat-experience.spec.ts` | `installAssistantHarness` 导出 + 新增 `releaseModels` 选项（models 路由可被测试门控） |

**新增测试：**

- `web/src/pages/assistant/lastModel.test.ts` — 缓存读写往返 / 拒绝空 id / 存储抛异常降级。`node --experimental-strip-types --test web/src/pages/assistant/lastModel.test.ts` → **3 passed**
- `web/e2e/composer-unlock.spec.ts`（Playwright 只放 `web/e2e/`）：
  1. catalog 被门控期间：composer 可输入、Send 可点、Enter 真实发出首条消息（mock SSE 返回文本）；释放 catalog 后页面保持可用、缓存模型仍选中。
  2. catalog 加载为空且无缓存：composer 与 Send 保持禁用。

**命令与结果：**

```
pnpm -C web type-check → 通过
pnpm -C web lint → 0 errors（10 个既有警告，全部在未改动的文件；含修复上一会话 newChatStream.test.ts 的 prefer-const lint error）
make hot-update ARGS="--frontend" → 构建并复制到 nginx 容器，健康
E2E_BASE_URL=http://localhost:8081 E2E_API_URL=http://localhost:8080 \
  npx playwright test -c playwright.live.config.ts e2e/composer-unlock.spec.ts --workers=1
→ composer-unlock 2 passed；同次运行完整 live 套件 35 passed / 1 skipped（无回归）
```

**剩余风险：** 缓存 key 未按用户隔离（同浏览器多账号共享）。当前模型目录是全站级而非按租户，风险低；若未来目录按租户分化需改为 per-user key。

---

## 阶段 4：SPO-03 剩余 = A2 + A3 + A4 + A5

**改动文件：**

| ID | 文件 | 内容 |
| --- | --- | --- |
| A2 | `apps/assistant-service/.../runtime/memory/indexer.py` | 字节水位增量索引：manifest 新增 `indexed_byte_length` / `indexed_prefix_sha256`；追加内容（新内容前缀哈希 == 水位哈希）时只重切+重嵌尾部块区，前序 chunk 行与向量点保留，仅替换尾部点；非追加修改回落全量路径；`_load_source_manifest` 返回 14 元组（3 个解包点同步更新） |
| A3 | `apps/assistant-service/.../trace_writer.py` | 事件按 run 缓冲：≥25 条、50ms 定时器、finish/drain 触发；每次 flush 一条 executemany（适配器无 executemany 时逐行回退）；ttft 批内合并 GREATEST；finish 内联 flush 保证终局前落库；`_pending_finishes` 防止 drain 与 finish 竞态重插 root/lifecycle；`record_event` 丢弃路径保留 `_drop_write` 失败记账 |
| A4 | `apps/assistant-service/.../tools/builtin_tools.py` | `_bounded_context_item`：tool metadata 的 contexts 每块内容截断 400 字符（模型侧 `all_results` 保持全文） |
| A4 | `apps/assistant-service/.../sse_event_transport.py` | `metadata` 加入 `_SPILLABLE_FIELDS`；终局事件（approval/run_*）的 metadata/context_snapshot 仍不可 spill（`_NON_SPILLABLE_ON_TERMINAL`） |
| A5 | `apps/assistant-service/.../runtime/context/assembler.py` | per-message token 估算 LRU 缓存（512 条）；`ContextPacket._boundary_fingerprint`；`boundary_fingerprint()`（system+suffix+tools+cache_dimensions+previous_cache_receipt 五要素） |
| A5 | `apps/assistant-service/.../agent/agent_model_turn.py` | 指纹未变时跳过整个 `bind_model_boundary`（deepcopy/规范化/估 token），仍发 CONTEXT_BUDGET 事件 |

**语义要点：**
- A2 追加检测是「新内容前 N 字节的 SHA-256 == 上次索引时全内容哈希」，N = 上次 `indexed_byte_length`；中间编辑立即回落全量，不会错切。
- A3 拒绝计数语义统一：批量 flush 失败 = 1 次 failed outcome（原 5 事件 = 5 次），`test_repeated_trace_failures_keep_sticky_state_bounded` 断言随之更新。
- A5 指纹含 previous_cache_receipt：跳过仅当「重新 bind 也必然产出字节一致的 packet」时才发生（缓存状态不翻转）。

**新增/加固测试（打真实入口）：**

- `tests/services/assistant/test_runtime_memory_incremental_index.py` — 21 轮日记追加：嵌入总量 21–45（全量重嵌 ≈60+），每轮只 chunk 尾部一次，删除只针对尾部点；中间编辑回落全量。**2 passed**
- `tests/services/assistant/test_trace_writer_metrics.py` — 25 delta = 1 条 batch INSERT；root+lifecycle+batch+finish ≤ 4；50ms 定时 flush（无 finish 也落库）；计数器与真实语句一致。**4 passed**
- `tests/services/assistant/test_sse_event_transport_bound.py` — 超限 tool_call_completed metadata 被 spill 且帧 ≤ 64KB；终局事件 metadata 永不 spill（fail-closed）；`_bounded_context_item` 截断。**26 passed（文件合计）**
- `tests/services/assistant/test_context_boundary_reuse.py` — 指纹随 suffix/tools/dimensions 变化；同输入两次 bind 产出相同 packet（跳过等价性）；token 缓存复用。**4 passed**

**命令与结果：**

```
uv run --all-packages --extra test pytest -q --no-cov \
  tests/services/assistant/test_runtime_memory_index_noop.py \
  tests/services/assistant/test_runtime_memory_privacy.py \
  tests/services/assistant/test_runtime_memory_incremental_index.py \
  tests/services/assistant/test_trace_writer_metrics.py \
  tests/services/assistant/test_agent_trace_capture.py \
  tests/services/assistant/test_sse_event_transport_bound.py \
  tests/services/assistant/test_tool_result_formatter.py \
  tests/services/assistant/test_context_boundary_reuse.py \
  tests/services/assistant/test_context_packet_contract.py \
  tests/services/assistant/test_agentloop_streaming_first_contract.py \
  tests/services/assistant/test_turn_event_collector.py
→ 318 passed
uv run --all-packages --extra dev ruff check <全部改动文件> → All checks passed
```

**评测夹具：** 八场景 2026-08-17 实机（SPO-00 阶段）ai_platform 5/8、hermes 5/8、openclaw 7/8，全部 0 infra error（失败场景与 08-16 基线一致的真实失败）。compose 栈随后被外部停止，未在 SPO-03 改动后复跑实机评测。

**剩余风险：**
- A2 的 chunker 前缀稳定性依赖「行级分块从 0 行开始」这一实现属性（`chunker.py` 现满足；未来改分块算法需重新验证）。
- A3 在 50ms 定时器窗口内进程崩溃仍会丢失未 flush 事件（与改动前每事件立即提交的持久性语义相比有 50ms 窗口）；finish/drain 路径无此窗口。
- A4 的 metadata spill 依赖 artifact storage 的 scoped-read 门禁（既有 fail-closed 测试覆盖）。

---

## 阶段 5：SPO-04 = K1–K4

**改动文件：**

| ID | 文件 | 内容 |
| --- | --- | --- |
| K1 | `apps/knowledge-service/.../vector_store.py` | `_cached_get_collection`：30s TTL 集合元数据缓存（只用于读权威检查）；`_invalidate_collection_info` 挂到全部 10 个集合写点（update/create/delete_collection）；删除失败不失效（测试契约） |
| K3 | `apps/knowledge-service/.../retrieval_service.py` | 缺省交互 profile：无显式配置时 `vector_k=12`、`keyword_k=12`、`candidate_k=max(top_k*2, 24)`（原 `top_k*4/*10` 扩写只在显式配置/数据集 presets 时保留）；rerank/MMR 缺省仍关 |
| K4 | `apps/knowledge-service/.../embedding.py` | `UnifiedMultimodalEmbedding` / `DashScopeMultimodalEmbedding`：新增 `request_timeout_s`（默认 60s，下限 1s）；`_call_api` 与图片 RPC 全部 `asyncio.wait_for` 限时；`embed_texts` 从 1-chunk-1-RPC 串行改为信号量限并发 gather；`getattr` 回退兼容 object.__new__ 构造的测试假件 |
| K2 | `apps/knowledge-service/.../worker.py` | fitz PDF 文本抽取（`_extract_pdf_text_sync`）、DOCX 抽取（`_extract_docx_text_sync`）、VLM OCR 页面栅格化（`_render_pdf_pages_sync`）、大 PDF 切分（`PDFSplitter.split_pdf`）全部移入 `asyncio.to_thread` |
| K2 | `apps/knowledge-service/.../ocr_utils.py` | Tesseract 分支（subprocess）经 `asyncio.to_thread` |
| K2 | `apps/knowledge-service/.../vision_pdf_processor.py` | `fitz.open` 与逐页 `_render_page` 移出事件循环 |

**新增/加固测试：**

- `tests/services/knowledge/test_vector_store_metrics.py` — 两次检索 = 1 次 get_collection RPC（缓存）；元数据写后失效 → 重新计数。**4 passed（文件）**
- `tests/services/knowledge/test_spo04_interactive_profile.py` — 缺省 hybrid 检索 dense `top_k=12`/keyword `limit=12`；挂起 provider 在 1s 超时内抛 EmbeddingError；4 文本并发嵌入峰值 ≥2（非串行）。**3 passed**
- 既有 `test_ingestion_identity_fence.py` 参数化用例（嵌入失败 → `failed` 不是 `completed`）继续全绿。

**命令与结果：**

```
uv run --all-packages --extra test pytest -q --no-cov \
  tests/services/knowledge/ tests/services/test_vector_store_search.py \
  tests/services/test_hierarchical_indexer.py tests/services/test_hierarchical_retriever.py \
  tests/services/test_chunking.py
→ 907 passed, 3 skipped（3 skipped 为 Windows 测试数据目录缺失，与本改动无关）
（K4 改动后复跑 tests/services/knowledge/ + test_vector_store_search → 866 passed）
uv run --all-packages --extra dev ruff check <本阶段改动文件> → 无新增错误（embedding/worker/ocr_utils 的 ARG002/SIM115 为 HEAD 既有）
```

**门禁核对：**
- 无 rerank 每次 retrieve `get_collection` ≤ 1：✓（缓存 + 计数器测试）。
- 摄入杀 embedding ⇒ `failed` 不是 `completed`：嵌入超时/异常 → `EmbeddingError` → 既有 fence 测试验证状态转为 `failed`；worker 优雅停机路径仍按既有契约保持非终态（可重试），未改动。

**剩余风险：**
- K1 缓存 TTL 30s：带外（非本服务）修改集合元数据最多 30s 不可见；服务内写点全部失效。
- K3 缺省召回宽度收窄（20→12 / 50→24 candidate）：质量影响需八场景/评测复跑确认；检索质量评测 `reports/performance/native-agent-parity-*` 的 retrieval 依赖场景未在改动后实机复跑（compose 栈已停）。
- K4 批形状未录回执（按交接要求「先录回执再批」推迟到有真实 DashScope 批返回形状之后）；当前为每 RPC 超时 + 信号量有界并发。
- K2 的 `_process_scanned_with_vlm_ocr` 每批重开 PDF（换取线程安全）；单文档页数多时多一次 open 成本，可忽略。

---

## 阶段 6：SPO-05 = G1 + D1 + D3（轻量）+ D2（blocked）

**改动文件：**

| ID | 文件 | 内容 |
| --- | --- | --- |
| G1 | `packages/ai-gateway-core/.../usage_recorder.py` | 两条写入路径的 ON CONFLICT 分支增加 `input/output_tokens`、`input/output_cost_cents` 的 `GREATEST`（部分+最终 SSE 取单调 max）；配额仍只对 `accepted`（首次 insert）累加（既有门控，未动语义） |
| G1 | `src/proxy/billing_stream.py` | `_extract_usage` 去掉「首个 usage 事件后不再收集」的早退——后续 usage 事件继续记录，由 DB GREATEST 收敛 |
| G1 | `packages/ai-gateway-core/.../pricing_catalog.py` | 前缀匹配只保留 `requested.startswith(catalog)` 方向；`gpt-4` 不再命中 `gpt-4o`（原双向 startswith 会错价） |
| D3 | `packages/ai-gateway-core/.../usage_recorder.py` | provider 过滤的 summary 查询改半开区间 `created_at >= $2::date AND created_at < $3::date + 1 day`（不再 `::date` 全列转换） |
| D3 | `src/services/eval/trace_retention_scheduler.py` | 每日清理增加 `request_traces` 保留（默认 14 天，钳制 7–14） |
| D1 | `packages/ai-gateway-core/.../database.py` | `DatabaseStorage` 增加 `command_timeout_s`（env `DB_COMMAND_TIMEOUT_S`，默认 30）传入 `asyncpg.create_pool` |
| D1 | `apps/assistant-service/.../main.py` | Assistant 显式小池 `pool_min_size=1 / pool_max_size=5` |

**blocked / 说明：**
- **D3 P95 小时聚合：blocked（无需改动即满足的说明）**——用量 summary 的非 provider 路径已读 `usage_daily_aggregates`（夜间任务从明细计算 p95），不存在对 24h 明细做 `PERCENTILE_CONT` 的在线查询；`usage_hourly_aggregates` 表已存在（保留 7 天，无 p95 列）。加 p95 列属 schema 变更，且没有找到在线 P95 明细扫描的热点，故不做 schema。
- **D2 Redis 会话缓存摘要+tail：blocked**——当前工作树不存在 Redis 会话整行缓存（会话 `history` 存于 PG `sessions.history` JSONB；Redis 仅有 `auth:token:*`）。审查报告所指的缓存在本树中已不存在，无需止血改动；会话消息表大迁移明确不在本阶段范围。

**新增测试（打真实入口）：**

- `tests/core/test_spo05_billing_gates.py`（4 个）：
  1. 部分+最终 SSE：第二次写冲突后存储 tokens 取 GREATEST，且 shipped SQL 含 4 个 GREATEST 子句；
  2. 配额门控：只有首次 insert 进入 accepted（配额只在首次累加）；
  3. 定价：`gpt-4` ≠ `gpt-4o`，`gpt-4o-2024-11-20` → `gpt-4o` 价（单向前缀）；
  4. provider summary 查询无 `created_at::date`、为半开区间。

**命令与结果：**

```
uv run --all-packages --extra test pytest -q --no-cov \
  tests/core/test_spo05_billing_gates.py tests/proxy/test_billing_failure.py \
  tests/proxy/test_billing_shutdown.py tests/proxy/test_transparent_proxy.py \
  tests/services/test_pricing_catalog.py tests/services/test_gateway_cost_accounting.py \
  tests/core/test_dispatcher_usage_recording.py tests/api/test_usage_api.py \
  tests/api/test_eval_traces.py
→ 116 passed
uv run --all-packages --extra dev ruff check <本阶段改动文件> → All checks passed
```

**剩余风险：**
- `billing_stream` 现在可能对同一请求写多次 usage（部分+最终）：每次都是批量 upsert 路径；极端高频 usage 事件流会放大写放大（OpenAI 兼容流 usage 只在最后一个 chunk，正常场景 1-2 次）。
- `command_timeout_s=30` 对所有服务生效：超长事务（如大迁移）可能被中断；env 可调。
- request_traces 保留 14 天默认：本地无实机验证（compose 已停）；`run_cleanup_once` 的删除计数解析依赖 asyncpg 的 "DELETE N" 字符串格式。
