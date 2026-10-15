# Claude Code 交接：完成剩余 SOTA 性能优化

> **状态:** archived — 此一次性交接已经消费，不得再次复制执行。现行性能状态只看
> `deploy/runbooks/sota-performance-dual-gate/loop-state.json`。

把下面「提示词」整段复制给 Claude Code。不要改范围。做完后不要 commit / push。Grok 会按本文 §验收清单 review 和跑测试。

---

## 提示词（复制从这里开始）

你是本仓库的实现代理。先读 `AGENTS.md`、`docs/harness/README.md`、`docs/harness/runtime-and-secrets.md`、`docs/plans/sota-performance-optimization-2026-08.md`。状态以该计划为准，不要另起一套优化故事。

### 任务

把 `docs/plans/sota-performance-optimization-2026-08.md` 里**尚未落地**的阶段做完（或做到有证据的明确阻塞）。第一道门禁增量已经由 Grok 完成，**禁止重做、回退或“顺手重构”那些文件的既有行为**。

### 已经完成（只许加固测试，不许改语义）

| ID | 已落地 |
| --- | --- |
| G0 / PCH-07 | 停止走 owner-checked cancel；整批 tool 补合成 result；provider 发网前拒 unpaired/duplicate/orphan；unknown side-effect 不当成 cancelled |
| W1 | 新会话先开 SSE；`persistNewChatSession` 把同一条 client `session_id` 传给 `POST /api/v1/sessions` |
| W2 | `thinking_*` / `subagent_*` 经 `createActivityFlushQueue` RAF 合并 |
| A1 | 哈希 / chunk_count / chunk_config 短路在 `chunk_markdown` 和 embed 之前；`memory_index_metrics` |
| GW1 | `SecurityHeadersMiddleware` 纯 ASGI；`/api/v1/assistant/chat/stream` 与 `/v1/responses` 在 `STREAMING_PATHS` |

关键文件（视为热路径，改之前先读现有测试）：

- `web/src/features/chat/newChatStream.ts` + `newChatStream.test.ts` + `coalesceUpdates.ts`
- `web/src/pages/assistant/hooks/useChatSession.ts`
- `src/api/v1/sessions.py`（`SessionCreate.session_id`）
- `src/core/middleware/_streaming/security_headers.py`、`paths.py`、`src/main.py`
- `apps/assistant-service/.../runtime/memory/indexer.py`、`index_metrics.py`
- `apps/assistant-service/.../models/request_safety.py`
- `tests/services/assistant/test_agentloop_streaming_first_contract.py`
- `tests/services/assistant/test_model_registry_provider_boundaries.py`
- `tests/services/assistant/test_runtime_memory_index_noop.py`
- `tests/middleware/test_security_headers_streaming.py`
- `tests/api/test_sessions_assistant_compat.py`

工作树里还有用户和其他会话的未提交改动（auth、docker、e2e setup、PCH runbook 等）。**只改本任务需要的文件。** 逐文件看 diff 再编辑。不要 revert 别人的改动。

### 按这个顺序做，一次只做一个可证伪切片

不要一次改 schema + AgentLoop + 前端。每做完一项：写/改驱动真实入口的测试，跑定向门禁，把命令和输出追加到 `reports/performance/sota-spo-remaining-2026-08.md`。

1. **SPO-00 仪表（无 Compose 则跳过实机 TTFT，但必须写清 skipped）**
   热路径计数器钩子：Redis RTT、每轮 trace SQL、每检索 `get_collection`。已有 `memory_index_metrics` 和 `streamStartMetrics`，复用不要另起一套。
   若当前 checkout 拥有 Compose 且栈已 up：10-trial thinking=low TTFT + 至少 1 个八场景，写入 `reports/performance/` dated 文件。未拥有栈则 **skipped**，禁止编造数字。

2. **W3**
   发送框用上次选中的模型解锁；`listModels` / `getConfig` 不挡第一条消息。
   门禁：composer 在 catalog 返回前可发送；有上次模型缓存测试。

3. **SPO-02 剩余 = GW2 + GW3**
   - 限流只留一个权威点（中间件或路由，不要两套滑窗都计数）。
   - 准入一个 Lua：expire + card + add + 租户份额。
   - JWT 中间件验一次，deps 复用已验证 claims，不要解两次。
   **不要**在这一刀改用量 GREATEST / 配额语义。
   门禁：假 Redis 暖路径 chat/proxy ≤ 4 RTT；Lua TOCTOU 单测；只增加一次限流计数。

4. **SPO-03 剩余 = A2 + A3 + A4 + A5**
   - A2：日记按字节水位增量索引，保留 `O_NOFOLLOW`。20 轮 embed 次数 O(新条目)。
   - A3：`agent_trace_events` 按 run 批写（≤25 或 50ms / finish）。25 个 delta ≤ 4 条 SQL。
   - A4：去掉 `tool_call_completed.metadata.contexts` 全文；metadata 可 spill。单帧 ≤ 64KB。
   - A5：packet 复用 per-message token；后缀+tools digest 未变则跳过 `bind_model_boundary`。
   **禁止削弱 PCH-07 配对校验。**
   门禁：未变化源仍 0 chunk/embed；评测夹具若能跑则 ≥ 6/8 且 0 infra error，否则写 skipped。

5. **SPO-04 = K1–K4**
   collection 元数据缓存 + 写失效；`process_document` / `fitz` 进 `to_thread`；缺省交互检索 12+12 hybrid、关 rerank/MMR/扩写；多模态至少超时 + 有界并发，批形状先录回执再批。
   门禁：无 rerank 时每次 retrieve `get_collection` ≤ 1；摄入杀 embedding ⇒ `failed` 不是 `completed`。

6. **SPO-05 = G1 + D1 + D3 + D2 轻量**
   流式用量单调 max；`ON CONFLICT` 对 tokens/cost `GREATEST`；配额只在首次 insert 累加。P95 走小时聚合。`request_traces` 7–14 天保留。每进程单池 + `command_timeout`。Redis 会话缓存摘要+tail，**不要**在本阶段做会话消息表大迁移。
   门禁：部分+最终 SSE、跨日/月、`gpt-4` 不得定价 `gpt-4o`；日期查询半开区间不是 `created_at::date`。

7. **SPO-06 = A6 + K5**
   只读工具并行；写工具 / 未知副作用仍串行。MCP client 按 `(tenant, connection)` 复用，DNS `to_thread`。compose 增加独立 knowledge-worker 或等价隔离。
   门禁：两个只读工具墙钟 ≈ max 不是 sum（±20%）；同一 MCP 连接 TTL 内 `initialize` = 1。

8. **SPO-07 = W4 + W5 + S1**
   虚拟列表；泄漏过滤只跑脏后缀；登录/公开不进 antd；i18n 按语言切；Python/Java/Dart/CLI 同一套 SSE 内层信封。
   门禁：`pnpm -C web type-check`；SDK 夹具 `data: {"event_type":"text_delta","data":"Hi"}` 四端都得到 `"Hi"`。包体预算若加 CI，用现有 build 量，不要发明未测 gzip 数字当“已达标”。

9. **SPO-08 = P1 only**
   能力表驱动的模型/variant canary，禁止题面正则分流。P2 语义缓存默认**不要做**。
   没有真实 A/B 就标 skipped，不要关思考换 TTFT。

### 硬约束

- 读 `AGENTS.md`：`apps/*` 不得 import `src/`；一 app 不得 import 另一 app。
- 不要关思考，不要按 prompt 关键词分流，不要换 AgentLoop 框架。
- 不要用吞吐换租户隔离、计费幂等、`jti` 吊销、fail-closed 配额。
- 不要 commit、push、deploy、`compose down -v`、rebuild/prune。Docker 前先读 `docs/harness/runtime-and-secrets.md` 并确认 compose 所有权。
- 验证用定向命令，不要全量 pytest 的 `--cov-fail-under`。
- Python：`uv run --all-packages --extra test pytest -q --no-cov <paths>`
- Lint：`uv run --all-packages --extra dev ruff check <paths>`
- Web：`pnpm -C web type-check`；相关则 lint。Playwright 只放 `web/e2e/`。
- 测试必须打真实入口。禁止在测试里重实现被测逻辑、写死期望、从半截开始测。
- 静态估算不是实测。没跑过的门禁写 **not verified** 或 **skipped**。
- 每阶段更新 `docs/plans/sota-performance-optimization-2026-08.md` 顶部状态。不要静默改 `docs/harness/architecture.md` §4 合同。

### 交付物（Grok 用这些做 review）

1. `reports/performance/sota-spo-remaining-2026-08.md`
   按阶段列出：改了哪些文件、跑了什么命令、通过/失败/skipped、剩余风险。
2. 每个阶段至少一组驱动真实函数的新测试或加固测试。
3. 不要留下半截 TODO / 假实现。做不完的项在报告里标 `blocked` 和原因，代码保持可编译、原门禁不回退。

### 完成定义（对你）

- 上表 2–8 项要么有测试证据的 pass，要么书面 blocked/skipped。
- G0/W1/W2/A1/GW1 的现有测试仍然全绿。
- `pnpm -C web type-check` 通过。
- 你改过的 Python 路径 ruff 通过。
- 未做 SPO-08 实机 canary 可以 skipped；把其余能在无 provider 密钥下完成的做完。

## 提示词（复制到这里结束）

---

## Grok 验收清单（你做完后我按这个 review + 测）

对照 `docs/plans/sota-performance-optimization-2026-08.md` §5–§6：

1. 抽查 diff：没有回退 PCH-07 配对、没有两套 session id、没有再次引入 `await call_next` 安全头。
2. 重跑本轮已有门禁：
   `tests/services/assistant/test_model_registry_provider_boundaries.py` 里 unpaired/duplicate/orphan/no-HTTP
   `test_agentloop_streaming_first_contract.py` 取消整批 / 幂等 / unknown-side-effect
   `test_runtime_memory_index_noop.py`
   `tests/middleware/test_security_headers_streaming.py`
   `tests/api/test_sessions_assistant_compat.py`
   `node --experimental-strip-types --test web/src/features/chat/newChatStream.test.ts`
   `pnpm -C web type-check`
3. 跑 Claude 新增的定向测试，确认打的是真实入口。
4. 读 `reports/performance/sota-spo-remaining-2026-08.md`：每个声称 pass 的项必须有命令输出；skipped 必须可复现原因。
5. 抽查 GW2 Lua、A2 增量日记、A3 批写、K1 缓存、G1 GREATEST 是否真改了热路径，而不是只加了未调用的函数。
