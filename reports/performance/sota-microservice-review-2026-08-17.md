# 核心微服务性能审查（2026-08-17）

> **性质:** 只读静态审查证据。延迟/吞吐除引用 2026-08-16 实机文件外，均为结构推断。
> **计划:** [`docs/plans/sota-performance-optimization-2026-08.md`](../../docs/plans/sota-performance-optimization-2026-08.md)
> **范围:** Gateway (`src/`)、Assistant、Knowledge、Web/SDK、`packages/ai-gateway-core` + compose 数据面。
> **方法:** 5 个并行只读审查代理 + 主代理对关键路径抽查。未改文件、未跑 Docker、未复跑 TTFT。
> **工作树:** `main` 跟踪 `origin/main`，含 PCH 未提交改动；PCH-07 相关脏文件按正确性而非性能裁决。

---

## 1. 审查分区

| 代理 | 范围 | 结论摘要 |
| --- | --- | --- |
| Gateway | 准入、限流、鉴权、代理、配额、用量、SSE 中间件 | PCH-03 部分落地；暖路径仍 7–17 次 Redis RTT |
| Assistant | AgentLoop、记忆、trace、MCP、工具、上下文 | 简单聊天本地 ~16 ms；C3 日记全量重建仍开 |
| Knowledge | 摄入、嵌入、Qdrant、检索 v2 | 质量栈成熟；交互 p50 被 collection GET / 过宽召回拖累 |
| Web / SDK | 流状态、Markdown、分包、SSE 解包 | 文本 RAF 已有；思考路径与新会话 RTT 仍开 |
| 数据面 | 池、会话 JSONB、保留、compose 上限 | laptop 剖面封顶；会话 history 无界 |

主代理独立复核并确认：

- `admission.py` 仍是 `zremrangebyscore` → `zcard` → `zadd`。
- `usage_recorder.py` 已批量 `VALUES … ON CONFLICT`，冲突不更新 tokens。
- `indexer.py:274` 在哈希短路前调用 `chunk_markdown`。
- `embedding.py:1814-1817` 多模态文本仍逐条 `_call_api`。
- `vite.config.ts` 已去掉 antd `ui` 巨包。
- `useChatSession.ts:1216-1236` 新会话仍 `await createSession`。
- `useChatSession.ts:1548-1561` `thinking_delta` 直接 `updateAssistantMessage`。
- `src/main.py:298-310` `security_headers` 仍是 `@app.middleware("http")`。
- `streaming_tool_loop.py:280-295` 工具批次串行。
- `model_registry.py:377-379` OpenAI-compat 路径剥掉 Anthropic cache marker。

---

## 2. 相对 2026-08-16 静态报告的重判

| 旧 ID | 旧结论 | 2026-08-17 |
| --- | --- | --- |
| C1 同步读记忆 + 全量哈希 | Critical | **大部分已修**：`to_thread` + mtime LRU。请求路径在 miss 时仍双哈希 + 提前 `chunk_markdown`。 |
| C2 每事件 3 次 DB | Critical | **部分修**：root/lifecycle 每 trace 一次；事件仍 1 INSERT。25 delta ≈ 27 语句。 |
| C3 日记全量重嵌 | Critical | **仍开**。PCH-02 承诺的水位增量索引不存在。 |
| C4 多模态串行嵌入 | Critical | **仍开**（文本多模态）。图片侧已 semaphore。文本 DashScope 已批+超时。 |
| C5 每帧全量 Markdown | Critical | **部分修**：流式块 memo 存在；整串泄漏过滤 + 结束时塌成单树仍开。 |
| C6 antd 1.39MB ui chunk | Critical | **已修** `manualChunks`。antd 仍从 `App.tsx` 进入口。 |
| 用量逐行 INSERT | High | **已修** 批量 upsert。 |
| 检索 N+1 | High | **大部分已修** 批量 hydrate。 |
| 部分索引标 completed | High | **标准路径已修**；层级/扫描若挂上仍开。 |
| Web terminal / epoch | High | **已修**。 |
| 准入 Lua / 单一限流 | High | **仍开**。 |
| 配额缓存 / 多 worker RPM | High | **仍开**。 |

---

## 3. Gateway 发现

### 3.1 双限流

`StreamingRateLimitMiddleware` 每请求跑 global + user/guest + IP。代理/助手路径再跑 `MultiDimensionRateLimiter` 4–6 维。每维一次滑窗 pipeline。PCH-03「单一权威」未关。

### 3.2 非原子准入

```516:523:src/core/gateway/admission.py
            await self.redis.zremrangebyscore(key, 0, now_ms)
            count = int(await self.redis.zcard(key))
            if count >= budget.limit:
                raise CapacityRejected(...)
            await self.redis.zadd(key, {member: expires_at})
```

两副本可同时看到 `count < limit`。流式租约常乘两套预算（upstream + tenant）。

### 3.3 配额与用量

- `quota_service._get_or_create_quota`：每次 `SELECT *`，无短 TTL。
- RPM：进程内 deque + 全局 `asyncio.Lock`，多 worker 低执行。
- `billing_stream._extract_usage`：first-wins。
- 批量 upsert 冲突分支只合并 status/error/metadata，tokens 停在首行。

### 3.4 SSE 中间件

`security_headers` 使用 Starlette 函数中间件（BaseHTTPMiddleware 语义）：`await call_next` 后再改头。`STREAMING_PATHS` 仅显式列出 `/api/v1/stream`；assistant chat 靠路径里的 `/stream` 关键词命中。`/v1/responses` 不在集合中。

### 3.5 鉴权

中间件解 JWT 取身份；`get_user_context` 再解一遍（含 aud/iss），然后 `EXISTS auth:token:{jti}`。权限 60s 缓存已存在。

### 3.6 暖路径跳数（估计）

| 路径 | Redis | PG | HTTP |
| --- | --- | --- | --- |
| 代理 runs/stream | 12–17 RTT | 1（配额） | 1 上游 SSE |
| assistant chat/stream | 7–10 | 1–2（会话未命中时 `SELECT *`） | 1 → assistant |

冷路径另加健康探测最多 3 个上游 HTTP。

---

## 4. Assistant 发现

### 4.1 C3 日记

`CompletedTurnMemorySync` 追加当日文件后后台 `index_source` 整个文件。内容已变，哈希短路不可能命中。`_replace_source_derivatives` 仍 DELETE 全部 chunk 再 `chunk_markdown` + 嵌入。4 MB 日记可到数百 chunk、数秒嵌入费用/轮。

### 4.2 短路前的 `chunk_markdown`

```272:274:apps/assistant-service/src/assistant_service/core/runtime/memory/indexer.py
        content_sha256 = hashlib.sha256(content.encode()).hexdigest()
        content_md5 = hashlib.md5(content.encode(), usedforsecurity=False).hexdigest()
        expected_chunks = len(chunk_markdown(content, self.chunk_config))
```

未变化的大文件在进程冷启动或 LRU 逐出后仍付一次全量切块扫描。

### 4.3 Trace

`start_trace` 对已初始化 trace 直接返回。`record_event` 仍 `_submit(_record_event → INSERT)`。`split_text_for_stream` 仍 120 字符。`max_pending = 256`。`drain` 有 generation barrier（PCH-02 已加），但不是批写。

### 4.4 模型边界序列化

`_stream_model_turn` 在 provider 调用前 `await bind_model_boundary`。内部：`deepcopy(messages)`、后缀 canonical JSON、每条消息 deepcopy+`json.dumps`+CJK 计数、tools 两次规范化、`receipt()` 再物化。短对话约 1–15 ms，长工具誊本可到数十 ms，并约 15% 高估 token。

### 4.5 MCP 与工具

- `mcp/runtime.py`：每次工厂新建 client 并 `initialize()`。
- `mcp/client.py`：`socket.getaddrinfo` 在事件循环上，initialize 与每次 RPC 都跑 pin 检查。
- 工具循环严格串行；`tool_call_completed` 后 await checkpoint。
- `builtin_tools.py` 把完整 `contexts` 放进 metadata；`_SPILLABLE_FIELDS` 不含 `metadata`。
- Code executor：`containers.run` 每次冷启动；`output_files` 仍带 `content_base64`。

### 4.6 已对齐 SOTA 的部分

默认 `select_tools(..., mode="discover")` 只广告发现工具。system 前缀稳定，记忆/时间在 user `<context>`。`cache_optimizer.build_optimized_messages` 不被 DashScope 路径调用；OpenAI-compat 会剥掉 `CACHE_SPLIT_MARKER`。隐式前缀缓存依赖「不要弄脏 system 字符串」，不是第二套缓存层。

### 4.7 延迟瀑布（简单聊天）

| 阶段 | 量级 | 是否主导 |
| --- | --- | --- |
| 入口 / 会话 / run | 1–8 ms | 否 |
| 技能 + 工具发现 + 策略 | 5–25 ms | 冷 MCP 目录时才明显 |
| 记忆 lstat + 可能的索引 | 10–80 ms；日记重建可到秒 | 仅当 C3 撞上请求路径 |
| packet / checkpoint | 2–15 ms | 否 |
| **Provider 首 reasoning** | **p50 3.146 s（2026-08-16）** | **是** |

工具轮：第一次采样 ~3 s + 各工具之和 + 第二次采样。感知速度由工具和第二次采样决定。

---

## 5. Knowledge 发现

### 5.1 摄入

| 项 | 状态 |
| --- | --- |
| 标准路径并发嵌入 + 60/90s 超时 + 3 次重试 | 已修 |
| 标准路径拒绝部分 text/image generation | 已修 |
| `process_document` 在事件循环 | 仍开（预览路径已 `to_thread`） |
| `fitz` 光栅化在 worker 循环 | 仍开 |
| 位置哈希跳过；插入导致后续全重嵌 | 仍开 |
| `UnifiedMultimodalEmbedding.embed_texts` 逐条、无超时 | 仍开 |
| 默认 worker 未注入 `HierarchicalIndexer` | 代码开、生产默认关 |
| 扫描+层级失败仍标 `completed` | 若挂上则开 |
| Qdrant upsert 32/16/8/4 + 每次 `get_collection` | 仍开 |

`contextual_retrieval.py` 只剩 `.pyc`；`contextual_prefix` 列无写入。不要把它当运行时能力。

### 5.2 检索

- 每次 `require_collection_readable` → `get_collection`，缓存命中也做。
- Native hybrid：`query_batch_points` dense+sparse RRF。回退：PG FTS + 最多 80 篇 Python tokenize。
- 缺省 `vector_k`/`keyword_k` = `max(top_k*4, 20)`，`candidate_k` = `max(top_k*10, 50)`。
- `retrieve()` 不走结果缓存；`retrieve_with_images_v2` 走进程内 TTL 缓存。
- 结果 N+1 基本关闭（`filter_active_segment_ids`、`get_segment_associations_batch`）。
- `list_datasets` 仍逐行等权限；`list_segments` 是 `SELECT *`。

### 5.3 建议默认 profile

**交互 agent:** hybrid + RRF、native、top_k=5、12+12、candidate 20、rerank/MMR/扩写关。
**评测:** top_k=10、40+40、candidate 80、`qwen3-rerank` top_n=20。
`bm25_v2` 读路径保持 fail-closed（shadow-only）。

---

## 6. Web / SDK 发现

### 6.1 新会话 RTT

`sendMessage` 在无 `activeSessionId` 时 `await createSession`，然后才打开 `/api/v1/assistant/chat/stream`。后端 `session_id = body.session_id or uuid`。`listSessions` 已 defer 到首个流事件（PCH-04 仍成立）。

### 6.2 思考路径绕过 RAF

文本/工具走 `1347-1394` 的 RAF。`thinking_delta` 与 `subagent_*` 每次 `setMessages(prev.map)`。`buildTimeline` 可在 ChatMessage、页芯片、ActivityPanel 同一 tick 算三次。

### 6.3 Markdown

`splitStreamingMarkdownBlocks` + `MarkdownBlock` memo 存在。`filterToolJsonOutput` + `isRtlText` 对整串每次 RAF。`isStreaming` 变 false 时 blocks 塌成 `[filteredText]`，整文 ReactMarkdown+KaTeX。`katex.min.css` 静态导入。

### 6.4 列表与首屏

`messages.map` 无虚拟化；切会话 `getSession` + `getSessionHistory(200)` + artifacts + `restoreLatestRun`。挂载 `Promise.all(listModels, listDatasets, getConfig, connectors)`；发送键要求 `models.length > 0`。

i18n 入口同步导入 `zh-CN` 与 `en-US` 以及 eval/agents 文案。`App.tsx` 引入 antd `ConfigProvider`。`router.tsx` 对 knowledge/agents 用 barrel。

### 6.5 SDK

| 客户端 | 拆内层 `data` | 终态 |
| --- | --- | --- |
| Python | 是 | `done` 停 |
| CLI JS | 是 | `DONE` 停 |
| Java | 否 | 读到 EOF |
| Dart | 否 | `isDone`；`lineBuffer +=` 可能平方 |

---

## 7. 数据面发现

### 7.1 Compose 默认上限

Postgres 384m、Redis 容器 256m / `maxmemory` 192m `allkeys-lru`、Qdrant 384m、gateway 512m、knowledge 512m、assistant 640m。Knowledge `WORKER_CONCURRENCY` 默认 1，uvicorn workers 1。低内存文档写明约 4 GiB Docker。

### 7.2 连接池

| 进程 | workers | 池 |
| --- | --- | --- |
| Gateway | 1 | `pool_max_size=10` |
| Knowledge | 1 | **两个**独立池，各 2–10 |
| Assistant | 1 | 默认 `DB_POOL_MAX_SIZE=20` |

未见 `command_timeout` / `max_inactive_connection_lifetime`。

### 7.3 会话与用量

- `assistant.sessions.history` 数组追加 `history || $2::jsonb`；`get` / `list()` 仍 `SELECT *`。
- Redis 缓存整行 1h。元数据有 1 MB 帽，history 无。
- `UsageRecorder.record` 在 `_buffer_lock` 内 flush。
- 采样 trace flush 对租户 24h `usage_records` 做 `PERCENTILE_CONT(0.95)`。
- `usage_records` 有 30 天删除；`request_traces` 无保留。`agent_traces` 有 `retention_expires_at`。

### 7.4 跳税

内部 httpx 池 50/10 keepalive 已复用。每次可变内部调用可新铸 `Idempotency-Key`。前端不直连 assistant/knowledge。`local-node` 不在聊天/RAG/摄入热路径。

---

## 8. 未验证

- 未复跑 10-trial TTFT 或八场景。
- 未跑 `EXPLAIN` / `pg_stat_statements` / `docker stats`。
- 未在浏览器里复现思考路径 jank。
- 未测 `security_headers` 是否在本 Starlette 版本实际缓冲 SSE（机制风险成立，幅度未测）。
- 未测 DashScope 多模态批返回形状。

这些必须在 SPO-00 / 对应阶段用门禁补上。不得把本节写成已经通过。
