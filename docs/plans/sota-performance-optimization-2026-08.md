# 核心微服务 SOTA 性能优化计划

> **状态:** superseded as execution instruction — 后续状态只看
> `deploy/runbooks/sota-performance-dual-gate/loop-state.json`；本文仅保留分析与历史指标。
> **日期:** 2026-08-17
> **性质:** 计划，不是实测验收。文中延迟数字除标明「2026-08-16 实测」外，均为当前代码结构推断。
> **证据:** [`reports/performance/sota-microservice-review-2026-08-17.md`](../../reports/performance/sota-microservice-review-2026-08-17.md)
> **前置程序:** 历史 PCH 已被 Rust cutover supersede；不得按旧 Python 执行路径恢复。
> **不要做:** 关闭思考、按 prompt 关键词硬编码分流、用估算数字当验收、用吞吐换租户隔离或计费正确性。

---

## 0. 一句话结论

简单对话的墙钟时间已经被 **provider 首 token**（Qwen thinking=low：首个 reasoning p50 **3.146 s**，文本 TTFT p50 **3.925 s**）钉死；本地准备约 **16 ms**。下一轮性能工作的杠杆不在「再抠几十毫秒 AgentLoop」，而在四件事：

1. **把等待做成人能感知的响应**（新会话不再先等 `createSession`；思考 delta 不再每事件整表重渲）。
2. **砍掉请求路径上串行且可合并的 Redis/PG 往返**（双限流、非原子准入、JWT 验两次、`BaseHTTPMiddleware` 包 SSE）。
3. **切断多轮复利成本**（日记每轮全量重切块重嵌入、trace 逐事件 INSERT、MCP 每次握手、只读工具串行）。
4. **把交互检索和摄入从同一进程、同一过宽召回配置里拆开**。

文本 TTFT 要从 3.925 s 降到约 3.41 s 的发布门槛，只能靠 **可配置的模型/variant canary + 真实 A/B**，不能靠关思考或改本地序列化。

---

## 1. 本计划与既有工作的关系

| 已有资产 | 角色 | 本计划怎么用 |
| --- | --- | --- |
| `performance-correctness-hardening`（PCH-00…07） | 正确性 + 部分热路径已落地 | **PCH-07 是硬门**。本计划不重做已修项，不把未关的计费/停止缺陷算作性能胜利。 |
| `assistant-runtime-optimization`（ARO，已 verified） | harness / 审批 / durable / eval 外环 | 不再开第二套 AgentLoop。本计划只动可测的热路径与观测。 |
| `docs/plans/kb-rag-optimization-plan.md`（2026-07） | 检索 **质量 / UX / 评测台** | 不混进本计划。Contextual Retrieval 已不在热路径；不要为了对齐旧质量计划而在默认摄入上再加每段 LLM。 |
| `reports/code-review/perf-review-2026-08-16.md` | 静态估算，149 热点 | 已抽样复核。本计划按 **当前工作树** 重判：C1 大部分已修，C2 部分修，C3/C4 仍开。 |
| 2026-08-16 实机报告 | 唯一可用的延迟/评测基线 | 实施前必须复跑，不能把 3.146 / 3.925 / 6/8 当本周数字。 |

当前 `loop-state.json`：`active_phase = PCH-07`。停止按钮若在 `tool_use` 已出、`tool_result` 未到的窗口里直接掐 SSE，下一轮会被 provider HTTP 400 拒绝。在这条不变量闭合前，**禁止宣称任何性能优化已完成**。

---

## 2. 业界 SOTA 对照（2025–2026）

对照对象是生产级 LLM 网关和 agent harness，不是框架宣传页。我们已对齐的标为 ✔，差一口的标为 △，明确落后的标为 ✗。

### 2.1 网关（Cloudflare / Kong / Portkey / LiteLLM / Helicone / Bifrost）

| SOTA 能力 | 我们 | 差距 |
| --- | --- | --- |
| 单权威限流（Lua / 令牌桶，一请求一脚本） | 中间件 3 维 + 路由 4–6 维两套滑窗 | ✗ 重复计数、RTT 放大 |
| 原子准入（expire+card+add 一次 EVAL） | `ZREMRANGE` → `ZCARD` → `ZADD` 三次往返 | ✗ 且多 worker TOCTOU |
| Prompt cache 作为成本 SLI | 前缀已拆稳定/易变；DashScope 走隐式前缀 | △ 无 cache hit 发布门禁 |
| 语义缓存 | 无（正确：默认聊天不能做） | 仅 FAQ/确定性路径可选项 |
| 流式路径禁止 BaseHTTPMiddleware | 多数中间件已纯 ASGI；`security_headers` 仍是 `@app.middleware("http")` | ✗ 经典 TTFT 陷阱 |
| 用量 last-or-max / GREATEST | 批量 upsert 已有；冲突只改 status | ✗ 部分+最终 SSE 会少记账 |
| 边沿验签一次 | JWT 中间件 + deps 再解一次 | △ |

### 2.2 Agent harness（Anthropic context engineering、OpenAI Agents SDK / Codex、Claude Code、MCP tool search）

| SOTA 能力 | 我们 | 差距 |
| --- | --- | --- |
| 上下文是有限注意力预算，追求最小高信号 token | 发现模式默认只挂 `tool_search` / `tool_describe` / `tool_call` | ✔ 不要退回 power 全量 schema |
| 稳定前缀 + 易变尾巴（prompt cache） | system 静态、记忆/时间在 user `<context>` | ✔ 形状对；`cache_optimizer` 对 DashScope 仍是指标层 |
| 延后工具 / MCP tool search | 默认 discover | ✔ |
| JIT 取上下文，而不是预填全部记忆 | 记忆仍可能在请求路径上触发索引 | △ |
| 工具结果先压缩再回模型 | `compact_tool_result_for_model` 存在；KB `tool_call_completed.metadata.contexts` 仍是全文 | ✗ |
| 只读工具并行 | `streaming_tool_loop` 严格 `for` 串行 | ✗ |
| MCP 会话复用 | 每次 `MCPClient()` + `initialize()` + 同步 DNS | ✗ |
| 自适应 compaction | 有预算与 packet；每次 `bind_model_boundary` 多次 deepcopy + 全量估 token | △ |
| 增量记忆（Claude Code MEMORY.md / 追加日记） | 日记每轮 DELETE+重切块+重嵌入 | ✗ 最大复利成本 |

Anthropic 2025-09 的原则可以直接当验收语言：**每次模型调用只放当时最可能产生正确行为的最小 token 集**。OpenAI / LangChain 把这叫 harness engineering：旋钮是 system / tools / middleware，不是换一个 loop 框架。

### 2.3 RAG（hybrid + RRF + 两阶段重排，2026 默认栈）

| SOTA 能力 | 我们 | 差距 |
| --- | --- | --- |
| 默认 hybrid + RRF(k=60) | 已有，且有 native Qdrant 批查询 | ✔ |
| 交互路径短召回，评测路径宽召回+rerank | 缺省 `top_k*4` / `*10` 过宽；rerank 默认关但 preset 会开 | △ 缺交互 profile |
| 查询嵌入缓存 | 进程内 1800s / 1000 key | △ 多副本不共享 |
| Collection 元数据缓存 + 写失效 | 每次 retrieve `get_collection`，常再 COUNT | ✗ |
| 批嵌入 + 超时重试 | 文本 DashScope 已批；多模态文本仍 1 chunk 1 RPC 且无超时 | ✗ |
| 内容哈希增量，不按位置错位全量重嵌 | 位置键跳过；插入一行后面全重做 | △ |
| 摄入与检索隔离 | 同一 uvicorn、同一事件循环、compose 摄入并发 1 | ✗ |
| ColBERT / 量化 / Matryoshka | 无 | 召回平台期之前不要上 |

TREC / T²-RAGBench 一类结果只说明：**两阶段 hybrid→rerank 是质量默认，不是交互默认**。交互 agent 工具调用应走短 hybrid；评测/accurate preset 再开 rerank。

### 2.4 控制台与数据面

| SOTA 能力 | 我们 | 差距 |
| --- | --- | --- |
| 交互到首个真实活动（思考/工具/文本）分开报 | `interactionToFirstTokenMs` vs `interactionToFirstTextTokenMs` | ✔ |
| 流开始不依赖会话创建 RTT | 新会话仍 `await createSession` 再开 SSE | ✗ |
| 思考/子代理 RAF 合并 | 文本有 RAF；`thinking_delta` 每事件 `setMessages` | ✗ |
| 增量 Markdown + 虚拟列表 | 流式块 memo 有；结束时整文重解析；消息列表无虚拟化 | △ |
| 首屏按路由拆 antd / 语言包 | `manualChunks` 巨包已拆；antd 与双语言仍进入口 | △ |
| 会话规范化（消息表 + tail 缓存） | `history` 仍是无界 JSONB | ✗ |
| RED + TTFT 直方图 | 有 OTel，默认采样 1.0；P95 扫 24h 明细 | ✗ |
| 本地 compose 当压测床 | PG 384m / Qdrant 384m / 每服务 1 worker | 不能当生产结论 |

---

## 3. 当前真实基线

### 3.1 已测（2026-08-16，须复跑）

来源：`reports/performance/assistant-ttft-thinking-low-2026-08-16.json`、`performance-correctness-hardening-2026-08-16.md`。

| 指标 | 值 | 含义 |
| --- | --- | --- |
| 首个 lifecycle | p50 16 ms / p95 19 ms | 本地 SSE 已足够快 |
| 首个 reasoning | p50 3.146 s / p95 3.358 s | provider 下界 |
| 文本 TTFT | p50 3.925 s / p95 4.307 s | 仍高于约 3.41 s 门槛 |
| 简单问答总时长 | p50 3.976 s | 几乎等于 TTFT |
| 输入 token（优化后 2+2） | 约 1301（曾从 2392 降 45%） | schema 精简已兑现，TTFT 几乎不动 |
| 八场景 | 原始 5/8，语义回放 6/8，0 基础设施错误 | Security / Research 真失败；无三轮 cohort |
| 发布判断 | **NO-GO** | 本计划不改这个判断，直到复跑 |

### 3.2 结构推断（今日代码，不是测量）

热路径跳数（墙钟仍由 LLM 主导；跳数决定 p50 附加值和故障面）：

```
简单聊天:
  Browser → frontend → gateway
    JWT×2, 限流 7–17 Redis RTT, 可选 session JSONB
    → assistant → provider
    异步: usage / trace

RAG 聊天: 上式 + assistant → knowledge → embed + Qdrant + PG hydrate
           + 第二次 provider 调用

摄入: gateway → knowledge(同一进程) → 解析/切块(事件循环)
      → embed HTTP → Qdrant 小批 upsert → PG
```

本地 compose 默认：Postgres 384m、Redis 192m `allkeys-lru`、Qdrant 384m、网关 512m、知识 512m、助手 640m、各服务 `--workers 1`。**在这个剖面里宣称「优化成功」没有意义**；先分清 laptop profile 与 perf lab。

---

## 4. 分服务裁决（当前工作树）

详细条目与行号见审查报告。这里只保留会变成实施项的根因。

### 4.1 Gateway — PCH-03 部分落地，请求税仍在

**已修:** 用量多行 `INSERT … ON CONFLICT`；API key 使用次数批写；权限 / service-access / proxy-config 短 TTL；`ServiceProxy` 连接复用。

**仍开:**

1. 两套滑窗限流同时跑。
2. 准入三次 Redis 命令，非 Lua，多副本可超卖。
3. 配额每次 `SELECT *`；RPM 是进程内 deque + 全局锁。
4. 流式用量 first-wins；冲突更新不含 `GREATEST`。
5. `security_headers` 是 Starlette HTTP 中间件；`/v1/responses` 不在显式流式集合里（assistant `/stream` 靠关键词命中）。
6. JWT 解两次。

**不要优化:** 租户隔离、用量幂等身份、fail-closed 配额、`jti` 吊销、准入租约释放。

### 4.2 Assistant — 简单聊天已不是本地瓶颈；复利仍在

**已修:** 记忆文件读出事件循环；mtime/size 新鲜度 LRU；checkpoint digest 每 save 一次；trace root/lifecycle 每 trace 一次；默认 discover 工具。

**仍开:**

1. **C3：** 日记每轮变 → 哈希短路永不命中 → 全文件 `chunk_markdown` + 重嵌入 + Qdrant 替换。`expected_chunks = len(chunk_markdown(...))` 甚至发生在哈希返回之前。
2. **C2 残留：** 每个 SSE 事件一次 `INSERT`，120 字符切块；25 delta ≈ 27 次语句，不是有界批次。
3. 每次模型边界 `bind_model_boundary`：deepcopy + 多次 JSON + CJK 估 token（高估约 15%）。
4. MCP：每次新 client、同步 `getaddrinfo`。
5. 工具批次严格串行；每次工具完成还 await checkpoint。
6. KB `tool_call_completed.metadata.contexts` 仍是全文。
7. Code executor 每次冷容器（先测再决定热池）。

**脏树 PCH-07**（`request_safety.py` / `streaming_tool_execution.py` / `model_registry.py`）是正确性，线性扫描可忽略，**禁止当性能回退砍掉**。

### 4.3 Knowledge — 质量栈成熟，交互延迟和摄入隔离落后

**已修:** 标准摄入并发嵌入 + 超时；部分索引不得 `completed`；检索段/图片批量 hydrate；BM25 候选上限 80。

**仍开:**

1. 每次 retrieve 现场 `get_collection`，常加精确 COUNT。
2. 默认 over-retrieve（`max(top_k*4,20)` / `*10`）。
3. `process_document` / `fitz` 在 FastAPI 事件循环上跑。
4. 多模态文本嵌入仍串行、无超时。
5. Qdrant upsert 批 32→4，每次还 `get_collection`。
6. 层级索引器默认没挂上；若挂上，失败仍可能标 `completed`。
7. 检索与摄入同进程。

层级 LLM 摘要、Contextual Retrieval 不在默认热路径。不要为了「对齐 7 月质量计划」把它们默认打开。

### 4.4 Web / SDK — 文本路径已诚实，等待仍像卡住

**已修:** 文本 RAF、ChatMessage memo、流式块 memo、antd 巨包拆掉、SSE Authorization、terminal first-wins、会话 epoch、listSessions 离开首字节。

**仍开:**

1. 新会话 `await createSession` 后才 `chatStream`；后端本就可自铸 `session_id`。
2. `thinking_delta` / 子代理事件绕过 RAF，每 token `setMessages`。
3. 泄漏过滤对整串跑；`isStreaming` 结束后整文重解析。
4. 消息列表无虚拟化；切会话一次挂最多 200 棵 Markdown 树。
5. 发送按钮等 `listModels` + `getConfig`。
6. 双语言与 antd ConfigProvider 仍在入口；knowledge/agents barrel 把详情页打进列表路由。
7. Java/Dart SSE 不拆内层 `data` 信封。

### 4.5 数据面 — 正确性优先，本地被内存上限封顶

1. 会话 `history` 无界 JSONB；Redis 缓存整行；`allkeys-lru` 下大会话会挤掉 `auth:token:*`。
2. 用量 flush 持锁；P95 对租户 24h 明细做 `PERCENTILE_CONT`。
3. `request_traces` 无保留任务。
4. Knowledge 开两个独立 PG 池；Assistant 默认 `pool_max=20` 对 1 worker 过大，且无 `command_timeout`。
5. 幂等后端 compose 默认 `memory`。

---

## 5. 实施项（同根只立一条）

按损害排序，不是按发现数量。

| ID | 项 | 主服务 | 类 | 依赖 |
| --- | --- | --- | --- | --- |
| G0 | 闭合 PCH-07：停止 → cancel API → 合成 cancelled result → provider 拒孤立 tool_use | assistant + web | 正确性门 | 无 |
| G1 | 用量冲突 `GREATEST` 代币/费用；流式用量单调 max | gateway + core | 记账 | G0 |
| W1 | 新会话先开流，会话创建并行 | web | 感知延迟 | G0 |
| W2 | 思考 / 子代理 RAF 合并（≤30 Hz） | web | INP | 无 |
| W3 | 用上次模型解锁发送框；目录后台拉 | web | TTI | 无 |
| GW1 | `security_headers` 改纯 ASGI；补齐流式路径集合 | gateway | TTFT | 无 |
| GW2 | 单一 Lua 限流 + 单一 Lua 准入 | gateway | RTT / 正确性 | 无 |
| GW3 | 中间件写入已验证 claims，deps 不再解 JWT | gateway | CPU | GW1 |
| A1 | `chunk_markdown` 移到哈希短路之后；请求路径禁止重建 | assistant | 本地 + $ | 无 |
| A2 | 日记按字节水位增量索引（保留 `O_NOFOLLOW`） | assistant | 复利 $ | A1 |
| A3 | `agent_trace_events` 按 run 批写（≤25 或 50 ms / finish） | assistant | 连接池 | 无 |
| A4 | 去掉 `tool_call_completed.metadata.contexts`；metadata 可 spill | assistant | SSE / token | 无 |
| A5 | packet 复用 per-message token；后缀+tools digest 未变则跳过 bind | assistant | 本地 CPU | 无 |
| K1 | collection 元数据缓存 + 写失效；首次 ready 后不再 COUNT | knowledge | 检索 p50 | 无 |
| K2 | `process_document` / `fitz` 进 `to_thread` | knowledge | 事件循环 | 无 |
| K3 | 交互检索 profile：12+12 hybrid，关 rerank/MMR/扩写 | knowledge | 工具延迟 | 无 |
| K4 | 多模态嵌入：超时 + 有界并发；批形状先录回执再批 | knowledge | 摄入 | 无 |
| D1 | 每进程一个 PG 池；显式 `pool_max`；`command_timeout` | 全服务 | 稳定性 | 无 |
| D2 | 会话改为消息表或至少 Redis 只缓存摘要+tail | core | 序列化 | 设计审查 |
| D3 | P95 走小时聚合；`request_traces` 保留 | core | DB | 无 |
| A6 | 只读工具并行；MCP client 按 (tenant, connection) 复用 + DNS `to_thread` | assistant | 工具轮 | G0 |
| K5 | 摄入 worker 独立进程 / 独立池 | knowledge + compose | 隔离 | D1 |
| W4 | 虚拟列表；泄漏过滤只跑脏后缀；结束后不塌成单棵树 | web | 长会话 | W2 |
| W5 | 登录图去掉 antd；语言包按需；拆 knowledge/agents barrel | web | 首屏 | 无 |
| S1 | Python/Java/Dart/CLI 同一套 SSE 内层信封 | sdk | 合同 | 无 |
| P1 | 能力驱动的模型/variant canary（真实 A/B，不读 prompt） | gateway + assistant | 文本 TTFT | G0 + 评测 |
| P2 | 租户作用域语义缓存，仅确定性/FAQ 路由 | gateway | 可选 | 隐私审查 |

---

## 6. 分阶段计划

一阶段一件可证伪的事。每阶段单独可回滚，不把 schema、前端和 AgentLoop 捆在一次改动里。

### SPO-00 — 仪表与基线（不改行为）

**目的:** 以后每一阶段都能说清「本地 vs provider」「Redis/PG 次数」「嵌入次数」。

工作：

- 复跑 10 次简单 TTFT（thinking=low）和至少 1 个八场景 cohort，写入新的 dated 文件。
- 给热路径加**计数器**（不是日志）：每请求 Redis RTT、每轮 `chunk_markdown`/embed、每轮 trace SQL、每检索 `get_collection`、`interactionToStreamStartMs`。
- 明确 laptop compose 与 perf-lab compose（PG/Qdrant ≥ 1–2 GiB）两套剖面；lab 未启动时不报告检索/摄入吞吐。

**退出:** 基线文件可复跑；计数器有测试钩子。PCH-07 仍是阻塞项则本阶段只建仪表。

### SPO-01 — 感知延迟（Web，1–3 天）

对应 W1–W3。

- 后端已允许缺省 `session_id`。前端先开 SSE，会话创建并行，回来再补绑定。
- `thinking_delta` / `subagent_*` 走与文本相同的 RAF（或 32 ms 节流）。
- 发送框用 zustand 里上次模型；`listModels` 不挡首条消息。

**门禁:**

- 暖 JS：`interactionToStreamStartMs` p75 ≤ 80 ms。
- Playwright：首条消息的网络序里，SSE 不得排在 `createSession` 之后。
- 思考夹具：INP p75 ≤ 200 ms。
- 简单聊天 first-reasoning p50 相对基线劣化 ≤ 10%。

### SPO-02 — 网关请求税（不含记账语义变更）

对应 GW1–GW3。

- 纯 ASGI 写安全头；`/v1/responses`、`/api/v1/assistant/chat/stream` 进入流式集合。
- 限流只留一个权威点；准入一个 Lua（清理 + 计数 + 写入 + 租户份额）。
- JWT 验一次。

**门禁:**

- 假 Redis：暖路径 chat/proxy **≤ 4 次 RTT**（jti + 限流 + 准入 + 释放）。
- 流式测试：handler 结束前必须已经 `send` 过第一块 `http.response.body`。
- Lua TOCTOU 与「只增加一次计数」单测。

### SPO-03 — Assistant 复利（原 PCH-02 未完成部分）

对应 A1–A5。

- 哈希短路提到 `chunk_markdown` 之前。
- 请求路径：新鲜或后台 sync 待处理时 **禁止** `index_source`。
- 日记增量索引（水位 + 只嵌新增；非追加修改才全量）。
- trace 批写；`model_turn_started` 可 fire-and-forget，`run_*` / `approval_*` 仍等待。
- 去掉 KB 完成事件里的全文 contexts。

**门禁:**

- 未变化源：`chunk_markdown` / embed **0 次**。
- 20 轮同会话：embed 次数 O(新条目)，不是 O(文件大小)。
- 25 个 delta：**≤ 4** 条 SQL（root + lifecycle + 1 batch + finish）。
- KB 工具轮：单帧 SSE ≤ 64 KB（或不 spill 失败）。
- 评测 ≥ 6/8，0 基础设施错误。

### SPO-04 — Knowledge 交互 p50 与摄入不堵环

对应 K1–K4。

- collection 缓存 + 写失效。
- 切块 / 光栅化离开事件循环。
- 数据集缺省走交互 profile；`balanced`/`accurate` 才宽召回 + rerank。
- 多模态：先录 DashScope 批返回形状，再批；在此之前至少有超时和有界并发。

**门禁:**

- 无 rerank 的交互检索：每次 `get_collection` ≤ 1。
- 摄入杀 embedding：状态 `failed`，不得 `completed`。
- 未改 500 chunk 文档重入：embed 0 chunk。
- 同集合、暖嵌入缓存：agent retrieve p50 相对本阶段开始 ≤ 1.2×（需先有 SPO-00 基线）。

### SPO-05 — 记账与数据面

对应 G1、D1–D3、D2 的轻量步。

先做不改 schema 的：

- 流式用量单调 max；`ON CONFLICT` 对 tokens/cost `GREATEST`；配额仍只在首次 insert 累加。
- 用量 P95 改读小时聚合或 Redis 直方图。
- `request_traces` 7–14 天保留。
- 每进程单池 + `command_timeout`；Assistant 显式小池。
- Redis 会话缓存改为摘要 + 最近 N 条（schema 未改前的止血）。

会话消息表（D2 全量）单独立项，先设计审查，不与上面捆在一起。

**门禁:**

- 部分+最终 SSE、跨日/月、多 worker RPM 夹具。
- `EXPLAIN`：用量日期查询走 `created_at` 范围，不是 `::date`。
- 定价：只允许 `requested.startswith(cached)`，`gpt-4` 不得命中 `gpt-4o`。

### SPO-06 — 工具轮与进程隔离

对应 A6、K5。

- 只读工具（discover / KB / describe）并行；写工具、未知副作用仍串行。
- MCP client 缓存 + 失败/pin 变化关闭；DNS 出环。
- compose 增加独立 `knowledge-worker`（或等价进程），检索进程不再跑重 CPU 摄入。

**门禁:**

- 两个独立只读工具：墙钟 ≈ max，不是 sum（±20%）。
- 同一 MCP 连接、TTL 内：`initialize` 次数 = 1。
- 摄入满载时检索健康检查不得饿死（lab profile）。

### SPO-07 — 控制台规模与 SDK

对应 W4、W5、S1。

- 虚拟列表；恢复会话不做入场动画。
- 泄漏过滤 / RTL 只处理脏后缀；完成态保留块 memo。
- KaTeX 按需。
- 登录/公开路由不进 antd；i18n 按语言和命名空间切。
- 四端 SSE 同一夹具。

**门禁:**

- login/public gzip JS ≤ 180 KB；单 chunk min ≤ 250 KB（CI 失败）。
- 20k 字符流：解析次数 = 块数 + 尾块帧数。
- 100 条历史恢复：主线程长任务 ≤ 50 ms（虚拟化后）。
- SDK：`data: {"event_type":"text_delta","data":"Hi"}` 四端都得到 `"Hi"`。

### SPO-08 — Provider canary 与发布

对应 P1；P2 默认不做。

- 按 **模型能力表** 配 variant / 区域 / thinking 档，A/B 必须是真实流量或同等实机，禁止题面正则。
- 发布包：本地 prep p50、first reasoning p50、text TTFT p50、input tokens p50、每轮 SQL、每轮 embed、八场景 ≥ 6/8 且至少 3 个完整 cohort 或书面成本阻塞。
- 语义缓存只有在「只读、无工具、租户隔离、可失效、可审计」四条都满足时才开设计；默认聊天路径不开。

**发布门槛（沿用 PCH-06，不放宽）:**

- 组件门禁零失败；skip 单列。
- 复杂场景 ≥ 6/8，0 基础设施错误。
- 简单 first-reasoning p50 ≤ 基线 110%（相对最新 SPO-00，不是 3.146 这个历史点）。
- 文本 TTFT 若仍高于产品门槛，只能写「provider 下界未破」，不能写性能优化失败或成功。
- 已宣称接入必须有真实 result receipt。

---

## 7. 观测与门禁命令

实施时跑与改动匹配的门，而不是全量套件（`docs/harness/commands.md` §7）：

| 改动 | 命令 |
| --- | --- |
| 助手运行时 | `uv run --all-packages --extra test pytest -q --no-cov tests/services/assistant` 及相关定向；`make verify-assistant-runtime-dev` |
| 网关 / 用量 | `tests/api/`、`tests/services/` 定向；假 Redis/PG 计数测试 |
| 知识 | knowledge-service 服务测试 + 失败注入 |
| Web | `pnpm -C web type-check` · `lint` · `build`；定向 `web/e2e/chat-experience.spec.ts` |
| 契约 | `make test-isolation`；OpenAPI 若动到 envelope |
| 实机 | `make hot-update` 后 `make status`；TTFT JSON 与 native-parity 目录 |

禁止：用全量 pytest 的 `--cov-fail-under` 掩盖定向失败；把 skip / 缺 `E2E_API_URL` 写成 pass；在未确认 compose 所有权时动 Docker。

---

## 8. 明确不做

1. 关闭思考，或把「简单问题」写成关键词路由。
2. 替换 AgentLoop / 迁到 LangGraph 内核（违反 platform law L5：网关不承载 agent 语义）。
3. 默认打开层级 LLM 摘要、每 chunk Contextual Retrieval、ColBERT、图 RAG。
4. 在 384 MiB Postgres / 单 worker 剖面上报告吞吐胜利。
5. 用语义缓存加速带工具、带租户记忆或带副作用的请求。
6. 为了少一次 Redis 而丢掉 `jti` 吊销、租户份额或用量幂等。
7. 把 PCH-07 的配对校验当成开销删掉。
8. 未授权的 commit / push / deploy / `compose down -v`。

---

## 9. 建议执行顺序（给下一会话）

```
PCH-07 闭合（正确性）
    → SPO-00 复跑基线 + 计数器
    → SPO-01 感知（用户立刻有感，风险低）
    → SPO-02 网关 RTT（与记账解耦）
    → SPO-03 日记/trace/KB 元数据（最大 $ 与下一轮延迟）
    → SPO-04 检索 p50 / 摄入不堵环
    → SPO-05 记账 GREATEST + 池 + 保留
    → SPO-06 并行只读工具 + MCP 复用 + worker 隔离
    → SPO-07 包体 / 虚拟列表 / SDK
    → SPO-08 canary + 发布证据
```

授权实施时，把本文件提升为 `deploy/runbooks/sota-performance-optimization/`，一阶段一个 `phase-NN-*.md`，状态只写在 `loop-state.json`。在此之前本文件是系统中的计划源。
