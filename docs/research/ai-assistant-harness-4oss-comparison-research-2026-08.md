# AI Gateway assistant-service 设计改进研究报告

**定位**：面向代码助手（codex）的一站式改进蓝图——把 assistant-service（FastAPI，端口 8093）从"安全优先、能力默认关"的迁移中间态，演进为事件溯源、门控统一、自我进化的通用 agent harness。

**元数据**：日期 2026-08-08｜范围：`/Users/yang/projects/AI--Platfform/apps/assistant-service/`（文中 `core/...` 相对路径均指 `apps/assistant-service/src/assistant_service/core/...`）｜四竞品基准 grok-build（Rust，commit `afbc0fb` = **v1.0.0**，初稿基于 0.2.101 `8adf901`，2026-08-08 已按 1.0 全部重读并重写本文 grok 章节）、openclaw（TypeScript，commit `fbf5d5636`）、Hermes_agent（Python，commit `7230fcb`）、opencode（TypeScript，commit `284214c`）｜方法：多代理并行深度源码研读（9 代理 + 对抗式复核）→ 横向归因 → 按杠杆（上下文成本/可靠性/可扩展性/安全/质量）排序｜2026-08-08 已对文中全部 AI--Platfform 路径与主要借鉴源路径做过存在性复核，见 [§9 复核记录](#9-复核记录2026-08-08)。

---

## 目录

- [3. 执行摘要：Top 10 改进](#3-执行摘要top-10-改进)
- [4. 四项目横向对比](#4-四项目横向对比)
- [5. 分主题深入章节](#5-分主题深入章节)
  - [5.1 智能体主循环与编排](#51-智能体主循环与编排)
  - [5.2 工具系统](#52-工具系统)
  - [5.3 上下文与记忆](#53-上下文与记忆)
  - [5.4 子代理与委派](#54-子代理与委派)
  - [5.5 MCP/ACP 集成](#55-mcpacp-集成)
  - [5.6 技能与任务规划](#56-技能与任务规划)
  - [5.7 权限/安全/沙箱](#57-权限安全沙箱)
  - [5.8 Prompt 与系统设计](#58-prompt-与系统设计)
  - [5.9 流式与协议](#59-流式与协议)
  - [5.10 模型路由与多供应商](#510-模型路由与多供应商)
  - [5.11 可观测性](#511-可观测性)
  - [5.12 自我进化与质量闭环](#512-自我进化与质量闭环)
  - [5.13 已知短板：本次未深覆盖](#513-已知短板本次未深覆盖)
- [6. 分阶段实施路线图](#6-分阶段实施路线图)
- [7. 设计原则提炼](#7-设计原则提炼)
- [8. 附录：关键文件改进索引](#8-附录关键文件改进索引)
- [9. 复核记录（2026-08-08）](#9-复核记录2026-08-08)

---

## 3. 执行摘要：Top 10 改进

按 影响/成本 排序（*注：此表按 影响/成本 排序，与 §6 的 P0/P1 路线图不完全等价——例如"审计 fail-closed"影响较窄但属 P0 安全止血，排不进 Top-10；checkpoint 根治项影响最高但成本高，按序列有意放 P1，见 §5.1 优先级说明）。

| # | 改进 | 影响 | 成本 | 借鉴源 |
|---|------|------|------|--------|
| 1 | **会话执行升级为 durable event-sourced 模型**（修复 checkpoint 非可恢复 transcript 的根因） | 高 | 高 | opencode V2 |
| 2 | **统一并默认开启主链门控**，消除 middleware fail-open 事故面 | 高 | 低 | grok-build / openclaw |
| 3 | **多供应商 failover + 统一错误分类**（修复单 provider 单点） | 高 | 中 | openclaw / hermes / opencode |
| 4 | **工具输出三层预算 + managed file 可寻址恢复** | 中高 | 低 | hermes / opencode |
| 5 | **上下文压缩语义保真 + 降级阶梯**（full-replace 摘要 + verbatim→fitted→lossy 三级降档） | 高 | 中 | hermes（preflight+anti-thrashing）/ openclaw（staged 分块+标识符保留）/ grok（full-replace+三级降档） |
| 6 | **技能系统落地**：AST 自注册 + 本地沙箱执行器 + 恶意扫描 + 注入预算 | 高 | 高 | hermes / openclaw |
| 7 | **静态 MCP 按 operation 分类 read_only + 与运行时 MCP 统一** | 中 | 中 | opencode / hermes（grok 仅贡献"运行时 MCP 与内置工具统一注册"，无按 operation 归只读的机制） |
| 8 | **子代理默认开启但严格受控 + push-based 完成事件** | 中高 | 中 | opencode / hermes / grok（child SessionActor） |
| 9 | **事件总线 + runId 单调 seq**（重放/审计基石） | 中 | 中 | openclaw / opencode |
| 10 | **自我改进闭环**（后台反思 + curator 治理 + memory dream） | 中高 | 高 | hermes（反思+curator）/ grok（仅 memory dream） |

---

## 4. 四项目横向对比

✅=该主题有强设计（值得借）。

| 主题 | grok-build (Rust) | openclaw (TS) | Hermes_agent (Python) | opencode (TS) |
|------|------|------|------|------|
| 主循环/编排 | ✅ actor+select! 单循环 | ✅ lane 队列+降级阶梯 | ✅ 双预算+one-shot 恢复守卫 | ✅ durable runner+coordinator |
| 工具系统 | ✅ ToolDispatch 流式工具 | ✅ 分层 policy 管线 | ✅ AST 自注册+统一门+Tool Search | ✅ scoped registry+advertised identity |
| 上下文/记忆 | ✅ 传输无关压缩+dream | ✅ hybrid 检索+staged 压缩 | ✅ ContextEngine 插件+preflight | ✅ Context Epoch+事件溯源 |
| 子代理 | ✅ 独立 child session | ✅ ACP spawn+key 编码深度 | ✅ fork 受限代理+toolset 交集 | ✅ durable subagent session |
| MCP/ACP | ✅ 依赖隔离+ACP | ✅ ACP 桥+mcporter | ✅ MCP task 刷新+Sampling | ✅ MCP resources 一等公民 |
| 技能/规划 | ✅ skills 多源优先级+goal | ✅ 技能预算化+扫描 | ✅ curator+skill 生命周期 | ✅ skill as context source |
| 权限/安全 | ✅ 签名托管配置+沙箱 | ✅ 外部内容 marker+scanner | ✅ threat_patterns+三层预算 | ✅ ruleset findLast+policy |
| Prompt | ✅ 构造器+缓存+提醒块 | ✅ 宪法式安全段+静默 token | ✅ stable/context/volatile 缓存优先 | ✅ SystemContext 代数+epoch |
| 流式/协议 | ✅ ReplayBuffer 协商合并 | ✅ runId+seq 事件总线 | ✅ 类型化事件词汇 | ✅ durable 事件+live delta 分离 |
| 模型路由 | ✅ 重试分类纯函数 | ✅ FailoverError status map | ✅ 任务键路由+参数剥离 | ✅ Route 组件组合+request executor |
| 可观测 | ✅ 每 turn trace 落 GCS | ✅ 事件总线+hook | ✅ trajectory+insights | ✅ 事件溯源即 trajectory |
| 自我进化 | ✅ dream+laziness 检测 | 弱（自愈为主） | ✅ background_review+curator | 弱（skill/AGENTS.md） |

**逐仓一句话**：
- **grok-build**：工程质量标杆。`classify_error` 纯函数重试、actor 会话、批内并发 401 共享恢复、签名托管配置——把"工程健壮性"做到了教科书级，但对通用 assistant 大量 coding 细节（bash/文件锁）需裁剪。
- **openclaw**：分层壳层哲学——内核可替换黑盒、session-key 规范化、外部内容永不直插 system prompt、危险工具按执行面差异化 deny。安全默认值最完整，最像"生产网关"。
- **Hermes_agent**：唯一完整自我改进闭环（background_review + curator + skill 生命周期），且是 Python 栈——工具三层预算、threat_patterns 注入模式库、上下文 preflight 压缩直接可抄。
- **opencode**：架构最先进——durable event-sourced 会话 + Context Epoch + 进程级 coordinator。是 assistant-service"checkpoint 不可恢复"问题的终极解，但工程量大、迁移需渐进。

---

## 5. 分主题深入章节

### 5.1 智能体主循环与编排

**现状**：`core/agent/agent_loop.py`（8955 行）是唯一 streaming-first 执行路径，`AgentLoopPhase` 8 阶段 + `_execute_streaming_first`；`core/assistant_service.py` 的 `chat_stream`/`_execute_agent_loop` 是唯一入口。防护主链默认只挂 RuntimeMemory + ResponseCap 两个中间件（`core/agent/middlewares/`），`harness.py` 的 CallLimit/LoopDetection/TimeBudget/PreCompletionChecklist/TraceSensor 明确"intentionally not registered by default"；`core/agent/middleware.py` 的 MiddlewareChain 是 **fail-open**（回调异常不阻断）。真正硬防护在 `core/gateway/execution_gateway.py`（DB-authoritative durable command + approval），与中间件是两套未统一体系。run checkpoint 只存 `message_state_hash` 摘要（`restorable_from_checkpoint: False`）。

**优秀设计**：
- **opencode**：durable event-sourced 会话——每轮 run 提交为带 `sessionID + 单调 seq + versioned type` 的事件流，同一 SQLite 事务内提交+投影+内存 pub/sub（`/Users/yang/projects/opencode/packages/core/src/session/runner/llm.ts`、`/Users/yang/projects/opencode/packages/core/src/event.ts`）。输入先进 durable inbox（`admit→promote`，`packages/core/src/session/input.ts`），进程级 SessionRunCoordinator 按 session 串行、跨 session 并发（`run-coordinator.ts`）。崩溃后遗留 running tool 被 durable fail 为 "Tool execution interrupted"，绝不静默重放副作用。
- **grok-build**：actor + 单 `tokio::select!` 循环同时收命令/模型切换/定时器（`crates/codegen/xai-grok-shell/src/session/acp_session_impl/run_loop.rs`，select 分支含 cmd_rx/模型切换/FlushReplay 定时器）；doom-loop 检测（`crates/codegen/xai-grok-sampling-types/src/doom_loop.rs`，**服务端驱动**：SSE `response.doom_loop_check` 上报 `tail_repetition:{t}@{channel}`/`low_logprob@{channel}`，客户端仅采信 thinking 通道 tail_repetition≤max_threshold=8，DoomLoopSignalCollector 去重后中止流式请求并抖动退避重采样）；纯函数重试分类 `classify_error`（`crates/codegen/xai-grok-sampler/src/retry.rs`）。
- **openclaw**：session 生命周期一等公民——session-key 规范化 `agent:{agentId}:{channel}:{kind}:{peer}`（`src/routing/session-key.ts`）+ lane 队列（`src/process/command-queue.ts`）+ run state machine 60s heartbeat（`src/channels/run-state-machine.ts`）；溢出恢复阶梯 in-attempt compaction→显式 compact（≤3）→结果截断，迭代上限 clamp 32–160（`src/agents/pi-embedded-runner/run.ts`）。
- **Hermes**：双预算（`api_call_count` + `IterationBudget` 线程安全 consume/refund）+ one-turn grace call（`agent/iteration_budget.py`、`agent/conversation_loop.py`）；`TurnRetryState` 把十几个恢复分支收敛成可单测对象（`agent/turn_retry_state.py`）。

**落地建议**：
1. **引入 durable inbox + 事件序列**：在 `core/gateway/execution_gateway.py` 现有 durable command 队列之上，把 `run` 的可见状态改为事件投影（借鉴 opencode `input.ts`/`projector.ts`）。最小可行：`api/routes/runs_approvals.py` 的 GET /runs/{id} 改为按 `(run_id, seq)` 分页重放，`core/turn_event_collector.py` 的 reducer 升级为持久 projector；checkpoint 补存 transcript 事件，修 `restorable_from_checkpoint: False`。
2. **统一门控**：把 `core/agent/middlewares/harness.py` 的五个中间件默认注册进主链（`core/agent/middleware.py` 的 `MiddlewareChain`），并把 `core/run_budget.py` 的 RunBudget 作为第一个中间件（预算语义借鉴 grok select 循环内的 `ChatStateEvent::ImageBudget` 处理 `run_loop.rs:381-405` 与重试预算 `DEFAULT_MAX_RETRIES=15` `retry.rs:55`；上限 clamp 借鉴 openclaw）。注：grok 1.0 **无 RunBudget 中间件概念**，此处的"预算作中间件"是本平台设计，grok 只提供预算语义参考。
3. **loom 卫生**：在 `_execute_streaming_first` 内联加 doom-loop 检测（语义借鉴 grok：检测到重复输出即中止当前流式请求并重采样。grok 由服务端 SSE `response.doom_loop_check` 上报 `tail_repetition`/`low_logprob`，仅 thinking 通道 tail_repetition≤max_threshold=8 采信，`doom_loop.rs:56-58,105-117` + `retry.rs:87-97`。我方无服务端信令，可落地为：thinking 通道连续 N 个相同 tool call/输出前缀即触发中止+退避重采样）与 idle/stall 检测（借鉴 grok `laziness_classifier.rs`，idle 10s 由会话模型当分类器判定 stalled，min_confidence 0.7，默认关）。
4. **双预算**：`core/run_budget.py` 增加可 `consume()/refund()` 的线程安全计数器（借鉴 hermes `iteration_budget.py`），并加 grace-call 语义。

**优先级**：P0/P1 分拆说明——统一门控（把五件套默认挂上主链）是 **P0**（低成本、直接消除 fail-open 事故面）；checkpoint 可恢复的根治方案（durable event-sourced transcript）工程量大，**按成本/风险序列有意排到 P1 #5**，尽管其影响在 Top-10 中排第 1。两者需分开实施，不应被同一优先级标签合并。

---

### 5.2 工具系统

**现状**：`core/tools/tool_registry.py`（743 行）定义 `ToolDefinition/ToolCallRequest/ToolCallResult/ToolExecutor`；`core/tools/tool_selector.py` 打分 0-1.0、tier 排序；`core/tool_invoker.py`（2001 行，顶层 `core/`）定义 `CapabilityAllowlist` 并承载调用编排；真正门控在 `core/gateway/execution_gateway.py:invoke_tool`。缺失：统一参数强转、工具输出预算、渐进披露、guardrail 循环检测。

**优秀设计**：
- **Hermes**：AST 自注册工具发现（`/Users/yang/projects/Hermes_agent/tools/registry.py` 的 `discover_builtin_tools`，只扫 module body）+ generation 计数器作缓存失效心跳 + check_fn 30s TTL 探测；统一门 `handle_function_call`（`/Users/yang/projects/Hermes_agent/model_tools.py`）：`coerce_tool_args`（string→int/bool、裸值→list）→ 中间件 → pre 钩子 → 审批 → dispatch → post 钩子（duration_ms）→ transform 钩子；Tool Search 渐进披露（`tools/tool_search.py`，bridge 工具 `tool_search/describe/call`，`_HERMES_CORE_TOOLS` 永不延迟）；三层输出预算 + `<persisted-output>` spill（`tools/tool_result_storage.py`，per-tool cap / per-result 持久化 / per-turn 聚合 200K）。
- **opencode**：不透明 `Tool.make`（唯一 executor + 自包含 schema codec），Scope 化叠加注册 latest-wins，`advertised` identity 陈旧调用检测（`packages/core/src/tool/registry.ts`）；ToolOutputStore 2000 行/50KB 边界 + managed file 7 天保留（`packages/core/src/tool-output-store.ts`）。
- **grok-build**：`ToolDispatch` trait 返回 Progress+恰好一个 Terminal 流（`crates/common/xai-tool-runtime/src/dispatch.rs`）；批内并行 + 每路径文件锁 + 共享 401 认证恢复（`xai-grok-shell/src/session/acp_session_impl/tool_calls.rs` 的 `shared_recovery: Arc<OnceCell<bool>>` 同批去重，`call_with_auth_retry` 401 检测/刷新在 `acp_session_impl/sampler_turn.rs:5,65,97`）。
- **openclaw**：7 层 tool-policy 管线（`src/agents/tool-policy-pipeline.ts`）+ deny-first glob（`src/agents/sandbox/tool-policy.ts`）。

**落地建议**：
1. 在 `core/tool_invoker.py` 前插 `coerce_tool_args` 参数强转（借鉴 hermes `model_tools.py`），解决开源模型常见输出漂移。
2. 新增 `core/tools/tool_result_storage.py` 实现三层预算：per-tool cap（复用 `core/run_budget.py` 的 `max_tool_result_bytes`）、per-result 写沙箱文件并返回可寻址预览、per-turn 聚合上限。
3. 新增 `core/tools/tool_guardrails.py`：按 `(tool_name, sha256(args))` 签名检测 identical-failure / 幂等无进展，默认 warn 不阻断（借鉴 hermes `agent/tool_guardrails.py`；hard-stop 显式 opt-in）。
4. 渐进披露：`core/tools/tool_selector.py` 当 MCP/技能 schema 超阈值时替换为 bridge 工具（借鉴 hermes `tools/tool_search.py`），`core/tools/tool_selector.py` 现 tier 排序可保留为披露顺序。

**优先级**：P0（参数强转 + 输出预算，直接影响执行质量）。

---

### 5.3 上下文与记忆

**现状**：`core/rag/context_engine.py` 已按四层稳定性前缀做 KV-cache 优化（Anthropic cache_control），`ContextBudgetManager` 裁剪；`core/memory/compressor.py`（514 行）用 4 chars/token + 保留最近 6 条 + LLM 摘要，**无语义保真与重建校验**；`core/runtime/memory/` 的 HybridMemoryRetriever 权重固定 0.65/0.35；`sync_turn_to_memory` 只在 terminal succeeded 写长期记忆；工作记忆作用域偏窄（`core/runtime/memory/working_state.py` 的 v2 key 绑定 tenant+user+session，跨会话不共享）。

**优秀设计**：
- **Hermes**：ContextEngine 可插拔 ABC + preflight 压缩（≤3 pass、defer-to-real-usage 启发式，`agent/context_engine.py`/`agent/turn_context.py`）；anti-thrashing（savings<10% 判定无效并累计 `ineffective_compression_count`，`agent/context_compressor.py:2167`）；**凭据/密钥在压缩与落盘时一律脱敏**（`agent/redact.py` 的 `redact_sensitive_text` + GitHub token 替换为 `[REDACTED]`，`context_compressor.py` 多处调用）；压缩后 session 轮换带 lineage + 自动编号（`agent/conversation_compression.py`）。
- **opencode**：Context Epoch——不可变 baseline（provider 缓存）+ 模型隐藏结构化 snapshot 原子推进（`packages/core/src/session/context-epoch.ts`）；SystemContext 代数 typed Source（key/codec/baseline/update/removed 渲染器，combine 拒重 key，Unavailable=stale-while-revalidate，`packages/core/src/system-context/index.ts`）；**上下文变更在 Safe Provider-Turn Boundary 惰性采样 admit，绝不异步 push**（CONTEXT.md）；滚动摘要 `<conversation-checkpoint>`（`packages/core/src/session/compaction.ts`）。
- **grok-build**：传输无关压缩引擎 trait seam（`crates/common/xai-grok-compaction/`，`intra_compaction/traits.rs:22-42` CompactionTarget 三档粒度 Steps/History/FullReplace 共用 orchestrator）；压缩输入**三级降级阶梯 verbatim→fitted→lossy**（`compaction.rs:1051-1066`，context_overflow 视为确定性错误不重试、自动降档，`compaction.rs:1160-1215`）；压缩后注入 `<system-reminder>` 状态提醒（最后一项追加，`compaction.rs:1529-1538` + `assemble.rs:97-99`），但 **system prompt 原样 verbatim 保留**（`compaction.rs:1606`），被精简的仅是摘要器输入；压缩历史有双保险 sanitize/validate（`compaction.rs:1616-1650`）；memory dream 会话末 consolidation（`xai-grok-shell/src/session/acp_session_impl/memory_dream.rs`）。
- **openclaw**：hybrid 检索 FTS bm25 + 向量 + MMR（λ=0.7）+ 时间半衰期衰减 + relaxedMinScore 兜底（`src/memory/manager.ts`、`src/memory/temporal-decay.ts`）；只读 DB 自动降级；**staged 分块压缩 + MERGE_SUMMARIES_INSTRUCTIONS + strict identifier 保留**（UUID/哈希/ID/token/API key/hostname/IP/URL/文件名等不缩写，`src/agents/compaction.ts:17,32-33` 的 `summarizeInStages`）。

**落地建议**：
1. `core/memory/compressor.py` 加 identifier 保留（`PRESERVE_PATTERNS` 已含 url/code，补 UUID/token 正则）与 staged 分块 + 摘要合并指令 + anti-thrashing 计数。**注意脱敏边界**：identifier 保留只针对不透明的非机密 ID（UUID/哈希/文件名/IP/hostname），API key/token/password 须走 Hermes 式脱敏（`[REDACTED]`），严禁原文写回压缩上下文（openclaw 的指令按原样保留 API key 对压缩后持久化场景是凭据暴露风险，采纳时务必叠加 Hermes 的脱敏层）。
2. 将 `core/rag/context_engine.py` 的 ContextStructure 升级为 Context Epoch 概念：`cache_stable_prompt_hash` 演进为不可变 baseline key，运行时变更（记忆写入/技能清单）以"时序 System 消息"插入而非改写（借鉴 opencode `context-epoch.ts`）；**在 provider turn 边界统一采样上下文变更**，禁止异步 push。
3. `core/runtime/memory/retriever.py` 加 MMR 重排 + 时间衰减，权重改为可配置（借鉴 openclaw `mmr.ts`/`temporal-decay.ts`）；`sync_turn_to_memory` 增加"失败轮次教训"写路径（低优先级）。
4. 压缩后注入 `<system-reminder>` 状态提醒块（最后一项追加，借鉴 grok `compaction.rs:1529-1538` + `assemble.rs:97-99`）；**system prompt 原样保留不换精简版**（grok 就是原样保留 `compaction.rs:1606`，精简只作用于摘要器输入），采用 verbatim→fitted→lossy 三级降档而非一次性截断。
5. **工作记忆跨会话共享**：`core/runtime/memory/working_state.py` 增加 tenant+user 级（无 session）的 v2 key 变体与生命周期，使长期偏好/画像跨会话累积（对应基线 gap #9）；与 `sync_turn_to_memory` 的 succeeded-only 写入叠加放开"失败轮次教训"写路径。
6. **原生搜索路由补链**：`should_use_native_search` 的关键词白名单启发式之外，增加"时效性/无关键词命中"时的 web-search 专用链路（`core/agent/tool_dedup.py` 已只剩 KB，需补回 web 搜索去重与检索链，对应基线 gap #10）。此项可并入 §5.10 的辅助 LLM 路由设计。

**优先级**：P1（当前启发式压缩对长会话可靠性是隐性风险；context epoch 是 token 成本杠杆；#5/#6 为 P2 用户画像与搜索能力增强）。

---

### 5.4 子代理与委派

**现状**：`core/agent/subagent_manager.py`（886 行）已有 spawn/spawn_parallel（max_concurrency=5）、`_bounded_config` 预算钳制、`_get_tools` canonical authorization boundary、EXPLORE/TASK/PLAN 类型（`subagent_types.py`）；但 **ASSISTANT_SUBAGENTS_ENABLED 默认关**；子代理是简化 loop 非完整会话。

**优秀设计**：
- **opencode**：subagent 是**独立 durable session**（同进程 Effect Fiber 隔离执行），sessions.active() 只计 foreground drains；任务经 admission inbox + wake 路由；结果经 fiber settlement 落成 durable events（`packages/core/src/session/runner/llm.ts`、`packages/opencode/src/tool/task.ts`）。后台 job registry 带 extend/promote（`packages/core/src/background-job.ts`）。
- **Hermes**：`delegate_tool.py` 工具集与父**求交集**（"子代理绝不获得父没有的工具"）+ role leaf/orchestrator + max_spawn_depth + kill-switch；子代理独立 provider:model（便宜模型跑子任务），provider 变更强制重推导 api_mode；父中断传播子代理（`tools/delegate_tool.py`）。
- **grok-build**：子代理是独立 child SessionActor（各自 chat history + tool context，隔离天然，`xai-grok-shell/src/agent/subagent/handle_request.rs`，嵌套子代理带 `child_depth` 受 `subagents_max_depth` 限制，超限从 child 剥离 task 工具 `handle_request.rs:384-390`）；goal 的 planner/strategist/verifier 各 spawn general-purpose 子代理带 `RoleSpawnOverride{model, agent_type}`（`goal_planner.rs:44` + `spawn_with_fail_open_retry` 首次失败以当前模型重试一次；角色 prompt 用角色自身工具名渲染 `goal_role_tools.rs:18-85`）。
- **openclaw**：session-key 编码 subagent 深度（`src/routing/session-key.ts` 的 `getSubagentDepth`），ACP spawn 带 idempotencyKey 防重（`src/agents/acp-spawn.ts`）；push-based 完成（SUBAGENT_SPAWN_ACCEPTED_NOTE 禁止轮询）。

**落地建议**：
1. 默认开启 `ASSISTANT_SUBAGENTS_ENABLED`，但按 opencode/Hermes 模式收紧：`core/agent/subagent_manager.py` 增加"工具集与父交集"校验（替代/补充现有 `_get_tools`）、子代理 depth 编码进 session key（`core/agent/subagent_types.py`）。
2. 子代理完成改 push-based：`core/agent/subagent_manager.py` 增加完成事件回传（subagent_ended 事件写入 `core/agent/agui_protocol.py` 事件流），父 turn 不得 sleep-poll。
3. 子代理执行升级为完整会话路径（复用 `_execute_streaming_first` 而非 `_run_loop` 简化版），使子代理获得与父级一致的预算/记忆/审计（借鉴 grok：`spawn_session_on_thread` → `spawn_session_actor` 在独立 OS 线程跑完整 SessionActor，继承父级 `task_output_token_budget`/`memory_config`/`attribution_callback`，`handle_request.rs:722,757,993+` + `acp_session_impl/spawn.rs:2320-2360`）。
4. 增加后台子代理模式（借鉴 opencode `background-job.ts` 的 start/extend/promote）。

**优先级**：P1（能力解锁 + 资源护栏并存）。

---

### 5.5 MCP/ACP 集成

**现状**：两套并行体系——静态 MCP（`core/mcp/config.py` 强制 `${ENV_VAR}`，`manager.py` 对所有静态工具硬编码 operation_kind=unknown/idempotency_supported=False/read_only=False，**任何不确定失败都 SIDE_EFFECT_UNKNOWN→pause**）；运行时 MCP（`core/mcp/runtime.py`，tenant authorization + OAuth PKCE + read_back，已相当成熟）。二者互不相通。`core/mcp/client.py` fail-closed 已很好（DNS pin IP、拒私有段、circuit breaker）。无 ACP。

**优秀设计**：
- **opencode**：MCP resources 提升为一等工具 `list_mcp_resources/read_mcp_resource`，mime/blob 大小门控（`packages/opencode/src/session/tools.ts`）；状态机 connected/disabled/failed/needs_auth/needs_client_registration（`packages/opencode/src/mcp/index.ts`）；MCP 工具注册进同一 registry 走同一 output-bounding/权限。
- **Hermes**：`MCPServerTask` 每 server 独立 asyncio Task + per-server RPC lock 防 wedge + 动态工具刷新（`notifications/tools/list_changed` → nuke-and-repave，`tools/mcp_tool.py` `_refresh_tools`）；OSV 恶意包检查（`tools/osv_check.py`，npx/uvx 启动前查 MAL-* advisory，fail-open）；SamplingHandler 反向调用 LLM（tools/mcp_tool.py）。
- **grok-build**：依赖隔离（rmcp 2.1 + reqwest 0.13 私有化，`xai-grok-mcp/src/lib.rs:5-14` 明确 "Quarantines rmcp 2.1 and reqwest 0.13"）；MCP 工具与内置工具注册进同一 ToolBridge（`bridge.rs:162-183 register_mcp_tools`）+ 按前缀反注册 `unregister_tools_by_prefix`；`tools/list_changed` 通知经自定义 rmcp ClientHandler 路由到会话派发器（`servers.rs:2267-2275`）；MCP 工具双可见性 `_meta.ui.visibility`（model 可见 vs app-only，`servers.rs:1248-1255`）；凭据库 + 浏览器 OAuth 编排（`credentials.rs`、`oauth.rs`）。
- **openclaw/hermes**：ACP 全套（server + translator + policy），含 MAX_PROMPT_BYTES 2MB、session-create 速率限制、token 走文件而非 argv（`src/acp/translator.ts`、`src/acp/server.ts`）；Hermes 的 `acp_adapter/server.py` 完整 agent 侧实现（history replay、session rotation lineage via `_meta.hermes.sessionProvenance`）。

**落地建议**：
1. `core/mcp/manager.py:_capability_metadata` 改为按 operation 分类——从 MCP server 声明/已知 read 语义推导 read_only，仅真正未知才 operation_kind=unknown（修复 gap 5 的"只读工具被当写工具"）；并入 `core/mcp/runtime.py` 的能力模型。
2. 静态 YAML MCP 增加 `read_only` 显式声明字段 + 与运行时 MCP 用同一 `MCPInvocationPolicy`（`core/mcp/runtime.py`）统一 invoke 路径。
3. 动态工具刷新：`core/mcp/manager.py` 增加 tool list change 监听 + registry generation 失效（借鉴 hermes `_refresh_tools` + grok 前缀反注册）。
4. MCP resources 作为工具暴露（借鉴 opencode `session/tools.ts`），mime 门控。
5. 新增 `core/acp/` 服务端（复用 `core/agent/agui_protocol.py` 事件映射），先做只读最小面（借鉴 openclaw `src/acp/` 的安全四件套）。

**优先级**：P1（静态 MCP 分类修复是低成本高杠杆；ACP 属 P2）。

---

### 5.6 技能与任务规划

**现状**：技能系统是空壳——`core/skills/executor.py`/`parser.py`、`core/runtime/skills/registry.py`/`models.py`/`builder.py`、`builtin/skill_create.py` 全部是 ai_gateway_core 的 re-export shim，assistant-service 侧无本地 Python 技能解析/校验/沙箱执行器，只有 docx/pdf/pptx/xlsx 的 node 脚本；skills 默认关（`core/runtime/compat/runtime_adapter.py` 的 `AssistantRuntimeFeatures.skills=False`）。`core/tasks/task_planner.py`（1629 行）已存在但 enable_task_planning=False。

**优秀设计**：
- **Hermes**：技能注入为 **user message 而非 system prompt**（保 prefix cache，`agent/skill_commands.py`）；AST 自注册 + skill_manage 创建/编辑/删除 + `_security_scan_skill` + pinned 防篡改 + curator 生命周期（stale/archive/reactivate 三态 + umbrella 合并，`agent/curator.py`）；Skills Hub quarantine→scan→install（`tools/skills_hub.py`）；task/todo 单工具全量回显。
- **openclaw**：技能注入**预算化**——最多 150 技能/30K 字符二分裁剪（`src/agents/skills/workspace.ts` 的 `applySkillsPromptLimits`）；多源优先级 extra<bundled<managed<personal<project<workspace；技能执行前恶意扫描（LINE_RULES+SOURCE_RULES+requiresContext 双模式降误报，`src/security/skill-scanner.ts`）。
- **grok-build**：skills 按优先级发现（Local→Repo→User→Server→Bundled）+ config.ignore/disabled 过滤 + canonical path 与 name 双重去重保持 first-seen 优先（`xai-grok-agent/src/prompt/skills.rs:49-72,112-130`）；内置技能**运行时**从 vendor/bundled 目录发现（`xai-grok-tools/src/implementations/skills/discovery.rs:816-818`，**非编译期内嵌**）；goal 编排由模型经 update_goal 工具驱动（`xai-grok-tools/src/implementations/grok_build/update_goal/mod.rs:171-210`，oneshot 阻塞等 SessionActor 真实裁决；处理端 `session/acp_session_impl/goal.rs:770,1522,2007`）+ turn-end TodoGate nudge（`session/acp_session_impl/turn.rs:2502-2543` `evaluate_todo_gate` + `reminders.rs:88`；仅 `/goal` Active 且内容型 turn 以 pending/in-progress todos 结束才触发，配合 todo_write 工具 `goal.rs:769`）。
- **opencode**：skill-as-context-source（权限过滤的可用列表注入 baseline，`packages/core/src/skill/guidance.ts`），远程 index.json 发现带路径穿越防护（`packages/core/src/skill/discovery.ts`）。

**落地建议**：
1. `core/skills/` 落地本地执行器：将 `core/skills/parser.py` 的 re-export shim **替换为本地实现**（SKILL.md frontmatter 解析 + 路径 containment 校验；保留对 `ai_gateway_core.skills.parser` 的兼容 re-export 或同步更新 importers，勿直接覆盖 import 源）；新建 `core/skills/validator.py`（调用参数校验）、`core/skills/sandbox.py`（本地 Python 沙箱执行，借鉴 hermes tool_guardrails + openclaw skill-scanner）；`core/skills/tool_bridge.py` 的 register_skill_as_tool 接入。
2. 注入预算化：`core/rag/context_engine.py` 的 ContextBudgetManager 增加技能预算上限（借鉴 openclaw `applySkillsPromptLimits`），并改为 user message 注入（当前 Manus-style v2 是 system 拼接，需评估 KV-cache 折中）。
3. 恶意扫描：新建 `core/skills/skill_scanner.py`（规则 + mtime 缓存），挂到 `core/skills/tool_bridge.py` 注册路径。
4. 开启 task_planning 最小集：`core/tasks/task_planner.py` 已有 WorkflowPattern，先启用 confirm_plan=True 的 plan 模式 + turn-end todo nudge。借鉴 grok TodoGate（`turn.rs:2502-2543`）：仅当 plan/goal 处于 Active 且内容型 turn 以 pending/in-progress todos 结束时才 nudge（配合 todo_write/update_goal 工具），而非无条件每 turn 提醒。

**优先级**：P1（技能空壳 + 默认关是最明显的"文档能力与现实落差"）。

---

### 5.7 权限/安全/沙箱

**现状**：`core/gateway/policy_engine.py`（HIGH/MEDIUM risk 表）+ `core/gateway/execution_gateway.py:invoke_tool` 全链路（policy→lattice→requires_confirmation→SandboxResolver→audit→durable 认领→approval→LaneScheduler）已是生产级；`core/runtime/security/pii_filter.py` 有 PII 正则；但 `core/audit/tool_audit.py` 在 DB 异常时 **fail-open** 放行限流；`core/quality/domain_policies.py` 是 no-op；无外部内容 marker 包裹、无技能/插件扫描、无签名托管配置。

**优秀设计**：
- **openclaw**：外部内容永不直插 system prompt——唯一随机 8 字节 marker 包裹 + 同形字折叠 + SUSPICIOUS_PATTERNS 监测（`src/security/external-content.ts`）；凭据走文件不 argv；明文 ws 禁连非 loopback（`src/gateway/client.ts`）。
- **Hermes**：threat_patterns 单一注入模式库，scope∈{all,context,strict}，工具结果宽检测不阻断、memory 写/技能安装才 block（`tools/threat_patterns.py`）；SSRF 云 metadata floor 不可关 + CGNAT + IPv4-mapped IPv6（`tools/url_safety.py`）；透明 git shadow-store checkpoint（`tools/checkpoint_manager.py`）；guardrail 循环检测 warn 默认开（`agent/tool_guardrails.py`）。
- **grok-build**：签名托管配置——Ed25519 签名信封绑定 principal + expiry + 磁盘篡改检测（`xai-grok-config/src/signed_policy.rs`，含编译期自检 `const_str_eq` + `EMBEDDED_V1_PUBKEY_SHA256_HEX` 钉扎防静默改键 `:33-61`）；权限判定带结构化 reason——`PermissionOutcome`（Allow/Reject/Cancel/Followup/Error）+ `PermissionDecisionReason` 闭枚举 23 变体（`xai-grok-telemetry/src/events/permission_analytics.rs:45-96`，canonical 词表单一拥有者 `permission/manager/reasons.rs:5-56`，跨 crate 漂移测试强制双向一致）；secrets sanitizer 对 URL query 参数**不用正则**——17 个 `SENSITIVE_QUERY_PARAMS` 命名 key 名单 + `url::form_urlencoded` 解析重组防二次 percent-encode（`xai-grok-secrets/src/sanitizer.rs:55-69,285-318`）。
- **opencode**：PermissionV2 ruleset findLast 通配符 + 默认 ask + Deferred 阻塞 + reject 级联 + always 持久化（`packages/core/src/permission.ts`）；provider policy last-match-wins + 用户全局覆盖 repo（`specs/v2/provider-policy.md`）。

**落地建议**：
1. `core/audit/tool_audit.py` 改为 **fail-closed**（审计不可用即拒，对齐 `_audit_agent_tool_policy_decision` 已存在的 AGENT_TOOL_AUDIT_UNAVAILABLE 语义），限流异常不得放行。
2. 新增 `core/runtime/security/external_content.py`：MCP/检索内容用 marker 包裹 + 同形字折叠（替代/增强现有 `_sanitize_external_text`——它位于 `core/mcp/manager.py:311`，`core/mcp/client.py` 的对应物是 `_sanitize_description`（946 行））。
3. `core/runtime/security/pii_filter.py` 扩展为 threat_patterns 式注入模式库（scope 三档），挂到 `core/memory/memory_manager.py` 与 `core/runtime/memory/lifecycle.py` 的写入路径。
4. `core/gateway/policy_engine.py` 增加 permission reason 三元组（allow/deny, reason, reason_detail）写入 trace（`core/trace_writer.py`）。注意：grok **不存在** `reason_code`/`decision_reason` 字符串字段——它用 `PermissionDecisionReason` 闭枚举（23 变体）保证跨 crate 双向一致（canonical 词表单一拥有者 `permission/manager/reasons.rs:5-56`，漂移测试强制）；我们落地时用**闭枚举 + 单次序列化词表**（不用自由字符串），而非复用某个 open 字符串字段。
5. 补 `core/quality/domain_policies.py` 内置最小金融/医疗预检（当前 no-op）。
6. **分层工具策略落地到现有 lattice**：把 openclaw 的 7 层 tool-policy 管线（scoped 继承、deny-first glob、owner-only 工具、`stripPluginOnlyAllowlist`）映射到已存在的 `core/runtime/tools/policy_lattice.py`（`ToolPolicyLattice`，现为 allow/deny/approval 分层）+ `core/tools/tenant_tool_policy.py`：增加 tenant→user→session→tool 的四级继承与 deny-first glob 匹配，使工具可见性策略可继承、可覆盖、与执行分离（对应 §4 对比表中 openclaw 的"分层 policy 管线"）。

**优先级**：P0（fail-open 审计 + 注入防御是直接安全缺口）。

---

### 5.8 Prompt 与系统设计

**现状**：`core/prompts/system_prompt_v2.py`（1356 行）Manus 风格模块化 + `CACHE_SPLIT_MARKER` KV-cache 分割 + 时间块注入 user-turn 尾部，`core/agent/runtime_context.py` 有 `<runtime_trust_boundary>`——这套已是四竞品中较先进。可补：系统提示每会话只构建一次缓存、stable/context/volatile 显式分层、外部内容作为数据对待。

**优秀设计**：
- **Hermes**：system prompt **每会话只构建一次缓存**（`agent/turn_context.py` 的 `_cached_system_prompt`），volatile 层时间戳**只含日期**保证字节稳定，发送前消息归一化；cache_control 断点 `system_and_3` 布局（`agent/prompt_caching.py`）；记忆注入带 `<memory-context>` 围栏 + "NOT new user input" 注记（`agent/memory_manager.py`）。
- **opencode**：SystemContext 代数把环境/AGENTS.md/skill guidance 合成 system context，combine 拒重 key，baseline 不可变供缓存，运行时变更以时序 System 消息插入（`packages/core/src/system-context/index.ts`）；自动 cache breakpoint（最后 tool def/最后 system part/最新 user msg，`packages/llm/src/cache-policy.ts`）。
- **openclaw**：宪法式安全段（"You have no independent goals"，`src/agents/system-prompt.ts`）+ SILENT_REPLY_TOKEN + HEARTBEAT_OK + think/final 标签。
- **grok-build**：ReminderPolicy + `<system-reminder>` 块统一注入纠正（compaction 后/todo nudge/goal 续跑，`xai-grok-agent/src/prompt/`）。

**落地建议**：
1. `core/agent/agent_loop.py` 增加 system prompt 会话级缓存 + volatile 时间戳只含日期（借鉴 hermes `system_prompt.py`/`turn_context.py`）；`core/prompts/system_prompt_v2.py` 显式拆 stable/context/volatile 三段并文档化字节稳定约束。
2. `core/rag/context_engine.py` 现有 KV-cache 前缀与 prompt 缓存打通：system baseline 用 `cache_stable_prompt_hash` 派生 key，记忆/技能变更时开新 epoch（借鉴 opencode `context-epoch.ts`）。
3. 记忆注入加 `<memory-context>` 围栏 + 系统注记防注入（`core/agent/runtime_context.py` 现有 `<runtime_trust_boundary>` 扩展）。
4. 引入 `core/agent/system_reminder.py` 统一 `<system-reminder>` 提醒块（todo nudge/goal 续跑/压缩后）。

**优先级**：P1（缓存一致化是 token 成本杠杆；注入注记是廉价安全）。

---

### 5.9 流式与协议

**现状**：`core/agent/agui_protocol.py` AGUIEventEmitter 已产出类型化 SSE（约 50 个事件，`core/assistant_service.py` StreamEventType）；`core/turn_contract.py` 保证每轮恰好一个 terminal 事件——协议面已是竞品水准。缺：事件单调 seq、durable 与 live 分离、ReplayBuffer 合并、请求-响应 vs 事件溯源统一。

**优秀设计**：
- **openclaw**：`src/infra/agent-events.ts`——runId + 每 run 单调 seq + stream 判别 + 可选 sessionKey 注入；一个事件源同时喂 UI/ACP/subagent（可作 assistant-service 的 EventBus 范本）。
- **opencode**：durable `session.next.*` 事件 + live-only Delta 分离，光标 = 一个持久化 seq，重连安全（`packages/schema/src/session-event.ts`、`packages/core/src/event.ts`）；SSE 先 server.connected + 15s 心跳 + subscriber capacity 256（`packages/server/src/handlers/event.ts`）。
- **grok-build**：ReplayBuffer 客户端协商合并（max_items/max_bytes/max_duration_ms，`xai-grok-shell/src/agent/update_chunk_merge.rs`），meta 带 token 计数与时间戳。
- **Hermes**：类型化事件词汇，"Events describe transport, never context"，穷尽匹配（`gateway/stream_events.py`）。

**落地建议**：
1. `core/turn_event_collector.py` 与 `core/agent/agui_protocol.py` 增加 `run_id + seq` 单调计数与 `stream` 判别字段（借鉴 openclaw `agent-events.ts`），作为重放/审计索引。
2. 把 durable（agent history）与 live-only delta 分离：`core/assistant_service.py` 的 SSE 生产者输出 live delta，持久化只走 turn contract 终态（对齐 hermes "events describe transport, never context"）。
3. `core/agent/agui_protocol.py` 增加 meta（total_tokens/event_id/agent_timestamp_ms）与客户端协商 buffering（借鉴 grok `update_chunk_merge.rs`）。

**优先级**：P1（seq + durable/live 分离是第 1 项事件溯源的地基）。

---

### 5.10 模型路由与多供应商

**现状**：`core/models/model_registry.py`（2793 行）已统一多 provider，但 wire_protocol 只有**两种常量**（`CHAT_COMPLETIONS_WIRE_PROTOCOL`/`RESPONSES_V1_WIRE_PROTOCOL`，`model_registry.py:32-33`；Anthropic 走 provider 专用 body builder，不是第三种 wire protocol）；**无 provider 级 failover/降级**（responses_v1 配置后不回退；非流式遇 tool_calls 抛错）；无统一错误分类、无任务键路由、无 provider 健康探测——gap 7 单点风险。

**优秀设计**：
- **openclaw**：`FailoverError` 折叠到 HTTP 风格 status map（402/429/503/401/403/408/400/404/410，`src/agents/failover-error.ts`）+ `runWithModelFallback` allowlist 强制的候选回退 + auth-profile 轮换带 cooldown（`src/agents/model-fallback.ts`、`src/agents/auth-profiles.ts`）；usage 带 last-call cache 修正。
- **Hermes**：任务键路由 `call_llm(task=...)`（compression/title/vision/mcp 各用不同模型不同超时，`agent/auxiliary_client.py`）；错误驱动的**参数剥离重试**（temperature 不支持→去 temperature，max_tokens→去 max_tokens）+ `_AUX_UNHEALTHY_TTL_SECONDS=600` 失败冷却 + stale-model self-heal（`_is_model_not_found_error`→重拉目录重试一次）；models.dev 四级缓存（`agent/models_dev.py`）。
- **grok-build**：纯函数 `classify_error → RetryDecision`（500/502/503/504/520 重试、429 短阈值、413 RetryWithImageStrip、401 EmitToSession，`xai-grok-sampler/src/retry.rs`）；1.0 增强：`RetryWithClientRebuild`（HTTP/1.1 client 重建，`retry.rs:149-151`）、Retry-After 响应头 clamp 到 ≤30s + ±20% 抖动（`57-64`）、`x-should-retry:false` 上游 veto（服务端显式拒绝重试，`201-216`）、`DEFAULT_MAX_RETRIES=15` 上限（`55`）。
- **opencode**：Route 组件组合（protocol/endpoint/auth/framing）+ RequestExecutor 统一 secret redaction / retryable status / retry-after / statusReason 分类（`packages/llm/src/route/executor.ts`）。

**落地建议**：
1. `core/models/model_registry.py` 新增 `ServiceError` 分类层（HTTP 风格 status，借鉴 openclaw `failover-error.ts`）+ `classify_error` 纯函数（借鉴 grok `retry.rs`），统一 401（交会话刷 token）与 403（刷无用）。
2. 新增 `core/models/model_fallback.py`：按候选 allowlist + provider 健康探测的降级路由（responses_v1 失败回退 chat_completions），修复 gap 7。
3. 新增 `core/models/auxiliary_client.py`：为压缩/标题/检索/场景分析等辅助 LLM 调用打任务键，统一模型选择/超时/冷却（借鉴 hermes `auxiliary_client.py`）。
4. `_stream_openai` 的 `<think>` 状态机与 `_smooth_text_delta` 保留；为 responses_v1 增加容错反序列化（借鉴 grok `deserialize_response_event` 的 strip-unknown-tools 重试）。
5. **provider 无关的结构化输出**：用 synthetic 工具拦截实现统一 structured output（借鉴 grok `StructuredOutput` 工具 3 次纠错重试 / opencode `generateObject` 强制 synthetic 工具），复用已存在的 `core/content/structured_output.py`（`OutputFormat`/`RepairStrategy`/`StructuredOutputConfig`），修复非流式 `chat()` 遇 tool_calls 抛 `nonstream_tool_calls_unsupported`（`model_registry.py:1948`）与 responses_v1 容错的根因。

**优先级**：P0（单 provider 单点是可用性硬伤；#5 为 P1）。

---

### 5.11 可观测性

**现状**：`core/trace_writer.py`（1369 行，trace id/redact/terminal 状态）+ `core/turn_event_collector.py`（契约校验 reducer）+ `core/audit/tool_audit.py` + OTelInbound——已是生产级。缺：每 turn 完整 trace 归档、事件总线、子系统日志隔离、feedback 收集。

**优秀设计**：
- **grok-build**：每 turn trace 落盘——`upload_turn_result`（`xai-grok-shell/src/upload/turn.rs:693`）+ `upload_streaming_partial`（739，每 turn 一个 `{session_id}/turn_N/streaming_partial.json` 可离线重放）；**注意 `upload_full_prompt_txt` 是 disabled stub**（`trace.rs:439-443` 由 `prompt_content_upload_disabled` 短路，1.0 未启用完整 prompt 归档）；feedback_manager 用户 turn 级评价（`session/feedback_manager.rs`）；`#[tracing::instrument]` span 带 session_id/prompt_id/model_id。
- **openclaw**：事件总线 runId+seq 即轨迹回放；hook 即 instrumentation（before_prompt_build/llm_input/llm_output/agent_end）；RuntimeEnv 抽象让日志可注入可测试（`src/runtime.ts`）。
- **Hermes**：trajectory JSONL（成功/失败样本分离，`agent/trajectory.py`）+ insights SQL 聚合（`agent/insights.py`）+ 会话重放（hermes_state.py SessionDB FTS5）。
- **opencode**：durable 事件溯源即完整 trajectory；结构化日志带 runID（`packages/core/src/observability/logging.ts`）。

**落地建议**：
1. `core/trace_writer.py` 增加 turn 级 streaming partial 归档（对象存储，借鉴 grok `upload/turn.rs:693,739` 的 turn 级 streaming_partial.json 可重放设计）。**完整 prompt 归档是自研差异点**：grok 1.0 的 `upload_full_prompt_txt` 是 disabled stub（`trace.rs:439-443`），我们若要此能力属于超越竞品的净新增，需自行处理敏感内容脱敏再归档。
2. 把 `core/turn_event_collector.py` 升级为进程级 EventBus（runId+seq），SSE/审计/日志/子代理完成统一订阅（借鉴 openclaw `agent-events.ts`；与 5.9 同一工作项）。
3. `core/trace_writer.py` 的 `_redact_trace_text` 已好，补 URL query 参数级清洗：用**命名 key 名单 + URL 解析重组**（17 个 `SENSITIVE_QUERY_PARAMS` + `url::form_urlencoded` parse/re-serialize，`xai-grok-secrets/src/sanitizer.rs:55-69,285-318`），不要用正则（对 `&` 分隔/percent-encode 组合易漏）。
4. 增加用户 feedback 通道（approval/评分落库，对齐 `api/routes/runs_approvals.py`）。

**优先级**：P1。

---

### 5.12 自我进化与质量闭环

**现状**：`core/quality/` 的 cache_optimizer + guardrails 已存在；`core/runtime/memory/reflector.py` 关键词偏好抽取（弱）；`core/quality/domain_policies.py` no-op；无后台反思、无 curator、无 memory dream、无技能自动沉淀。

**优秀设计**：
- **Hermes**：完整闭环——nudge 计数器（`_iters_since_skill`/`_turns_since_memory`）→ `background_review.py` fork 受限 review agent（继承缓存 system prompt + memory/skills 白名单 + auto-deny 审批 + `_memory_write_origin` 溯源 + 明确质量纪律提示词"不写环境依赖失败/负面工具断言/一次性叙事"）→ curator 生命周期治理（`agent/curator.py`，独立 review model、三态迁移、umbrella 合并、Markdown 报告）。
- **grok-build**：memory dream consolidation（gate + DreamLock + **30 分钟超时** `Duration::from_secs(30*60)`——注：doc comment 写 60s，与代码不一致，以代码为准 `memory_dream.rs`）+ laziness detector（idle 10s 分类器 nudge，`laziness_classifier.rs`）+ recap。
- **opencode**：skill-as-context-source 是低成本自改进（`packages/core/src/skill/guidance.ts`），无自动反思（明确 gap）。

**落地建议**：
1. 新建 `core/quality/background_review.py`：turn 后异步 fork 受限代理写长期记忆/技能（继承 `core/rag/context_engine.py` 的缓存 prompt + 白名单 + 自动拒绝审批 + source 标记）；`core/runtime/compat/runtime_adapter.py` 的 `sync_turn_to_memory` 增加"失败轮次教训"与"经验沉淀"路径。
2. 新建 `core/quality/curator.py`：定时后台 LLM 审查技能库（stale/archive/reactivate + umbrella 合并 + pinned 保护），独立 review model（借鉴 hermes `curator.py`）。
3. `core/runtime/memory/lifecycle.py` 增加 dream 机制（会话末 consolidation，gate + 锁 + 超时，借鉴 grok `memory_dream.rs`）。超时参数取代码真实值 30 分钟（`Duration::from_secs(30*60)`），不要照抄其 doc comment 的 60s——以代码为准。
4. `core/quality/domain_policies.py` 落地最小内置策略 + 答案校验（当前 no-op）。

**优先级**：P2（前置依赖 5.6 技能落地与 5.9 事件总线）。

---

### 5.13 已知短板：本次未深覆盖

以下短板在基线分析中被识别，但未在本报告给出完整改造方案，特此显式列出以避免"一站式蓝图"的遗漏感：

- **原生搜索仅关键词启发式**（基线 gap #10）：`should_use_native_search` 靠白名单关键词，对时效性强但非关键词命中的请求会漏；`web_fetch` 只是兜底，无独立 web-search 检索链（`core/agent/tool_dedup.py` 已只剩 KB 去重）。建议在 §5.10 辅助 LLM 路由落地时一并补 web-search 专用链路。已提示于 §5.3 落地建议 #6。
- **工作记忆跨会话作用域**（基线 gap #9）：`core/runtime/memory/working_state.py` 的 v2 key 绑定 tenant+user+session，用户画像跨会话累积慢。已给出方向性建议于 §5.3 落地建议 #5（P2）。
- **前端 / AGUI 客户端侧**：本报告聚焦后端 harness；前端消费 SSE 的 buffering/重连语义（对应 §5.9 的 ReplayBuffer 协商）未深读 `web/` 侧代码。

---

## 6. 分阶段实施路线图

**P0（现在，2-4 周）—— 止血与可靠性**
1. 统一门控：默认注册 `core/agent/middlewares/harness.py` 五件套 + RunBudget 作中间件；`core/agent/middleware.py` 改 fail-closed 语义。文件：`core/agent/middleware.py`、`core/agent/middlewares/harness.py`、`core/run_budget.py`。收益：消除 fail-open 事故面。
2. 模型 failover：`core/models/model_registry.py` 加 `classify_error` + `core/models/model_fallback.py` 候选降级。收益：单 provider 单点消除。
3. 工具参数强转 + 三层输出预算：`core/tool_invoker.py`（顶层 `core/`）、新增 `core/tools/tool_result_storage.py`。收益：执行质量 + 上下文保护。
4. 审计 fail-closed：`core/audit/tool_audit.py`。收益：审计不再可绕过。

**P1（短期，4-8 周）—— 能力解锁与上下文**
5. durable event-sourced transcript：`core/gateway/execution_gateway.py` checkpoint 改事件投影 + `core/turn_event_collector.py` 持久化 reducer。收益：checkpoint 可恢复、可审计回放（对标 opencode V2 的最小可行集）。注：该项影响 Top-10 排第 1，但工程量大，按成本/风险有意排在此处；P0 只做门控统一（roadmap #1）先行止血。
6. 事件总线 runId+seq：`core/agent/agui_protocol.py`。收益：重放/审计/UI 同步的基石。
7. 技能落地：`core/skills/` 本地 parser/validator/sandbox + `core/skills/skill_scanner.py` 恶意扫描 + 注入预算。收益：能力闭环 + 安全边界。
8. 静态 MCP operation 分类 + 动态刷新：`core/mcp/manager.py`。收益：减少误判 SIDE_EFFECT_UNKNOWN。
9. 上下文压缩语义保真 + Context Epoch：`core/memory/compressor.py`、`core/rag/context_engine.py`。收益：长会话可靠 + token 成本。
10. 子代理默认开启 + push-based 完成：`core/agent/subagent_manager.py`。收益：编排能力兑现。

**P2（长期，8-16 周）—— 自我进化与生态**
11. 自我改进闭环：`core/quality/background_review.py` + `core/quality/curator.py` + dream（`core/runtime/memory/lifecycle.py`）。收益：能力随使用增长。
12. ACP 服务端：`core/acp/`（编辑器集成）。收益：生态接入面。
13. MCP resources 工具化 + SamplingHandler：`core/mcp/runtime.py`。收益：双向能力。

---

## 7. 设计原则提炼

1. **上下文是每次 turn 可重建的派生视图，而非可变状态**（opencode Context Epoch / Hermes stable/context/volatile）。
2. **事件溯源 + 单一事务（提交/投影/通知）优于手写 CRUD**（opencode / openclaw seq）。
3. **失败是分类学而非 if-else**：`classify_error`/`FailoverError` 让**重试决策与用户提示词共享同一状态分类**——grok 的 `is_retryable_api_status`（`xai-grok-sampling-types/src/error.rs:574`）同被 retry decision 与用户可见措辞 `"server unavailable - please retry"`（`retry.rs:342`）消费，`RetryPolicy` 预设把 429/5xx 边界集中一处（`edge_client`/`server`/`client_storage`，`retry_policy.rs:58-90`）；**计费走独立的 `cost_usd_ticks` 元数据键**（`client.rs:187`），不混入错误分类（grok / openclaw）。
4. **策略即管道**：权限/工具可见性做成可继承、可覆盖的分层对象，与执行分离（openclaw / opencode）。
5. **外部内容永不直插 system prompt**：marker 包裹 + 围栏注记 + 注入模式扫描（openclaw / Hermes）。
6. **降级阶梯而非一次性重试**：压缩重启→参数剥离→结果截断，重试上限 clamp（Hermes / openclaw / grok）。
7. **自省与主循环隔离**：后台反思必须资源受限、写操作带来源标记、有治理器防腐烂（Hermes 完整闭环；**grok 仅 memory dream consolidation**，无 background reflection/curator——凡涉及后台反思资源限制、来源标记、治理器，借鉴源都应是 hermes，不要误用 grok）。
8. **safe boundary 采样**：上下文变更只在安全 provider-turn 边界惰性 apply，绝不异步 push（opencode）。

---

## 8. 附录：关键文件改进索引

| AI--Platfform 文件 | 改进动作 | 借鉴源 |
|---|---|---|
| `core/agent/agent_loop.py` | doom-loop 检测（我方无服务端信令，落地为客户端前缀/工具调用重复检测，见 §5.1 落地建议 #3）、system prompt 会话级缓存、grace-call 双预算 | grok `doom_loop.rs`（仅机制参考，服务端驱动）/ hermes `turn_context.py` |
| `core/agent/middleware.py` | fail-open→fail-closed，默认注册 harness 五件套 | grok permission reasons |
| `core/agent/middlewares/harness.py` | 默认启用五件套（CallLimit/LoopDetection/TimeBudget/PreCompletionChecklist/TraceSensor） | — |
| `core/agent/agui_protocol.py` | 加 runId+seq+stream 判别+meta | openclaw `agent-events.ts` / grok `update_chunk_merge.rs` |
| `core/agent/subagent_manager.py` | 工具集交集校验、push-based 完成、depth 编码、完整 loop 复用 | hermes `delegate_tool.py` / opencode `task.ts` / grok `handle_request.rs` |
| `core/gateway/execution_gateway.py` | checkpoint→事件投影；遗留 running 工具 durable fail | opencode `runner/llm.ts` |
| `core/turn_event_collector.py` | 升级为持久 projector + EventBus | opencode `projector.ts` / openclaw `agent-events.ts` |
| `core/turn_contract.py` | 终端事件携带 seq 索引 | openclaw / opencode |
| `core/run_budget.py` | consume/refund + grace-call | hermes `iteration_budget.py` |
| `core/tool_invoker.py`（顶层 core/） | coerce_tool_args 参数强转、定义 CapabilityAllowlist | hermes `model_tools.py` |
| `core/tools/tool_selector.py` | 渐进披露 bridge 工具 | hermes `tool_search.py` |
| `core/tools/tool_registry.py` | generation 计数器 + 前缀反注册 | hermes `registry.py` / grok `bridge.rs` |
| `core/rag/context_engine.py` | Context Epoch baseline、safe-boundary 采样、技能预算 | opencode `context-epoch.ts` / openclaw `applySkillsPromptLimits` |
| `core/memory/compressor.py` | staged 分块 + identifier 保留（openclaw `compaction.ts`）；anti-thrashing（hermes `context_compressor.py`）；凭据脱敏参照 hermes `redact.py` | openclaw `compaction.ts` / hermes `context_compressor.py`、`redact.py` |
| `core/runtime/memory/retriever.py` | MMR + 时间衰减、可配置权重 | openclaw `mmr.ts` / `temporal-decay.ts` |
| `core/runtime/memory/lifecycle.py` | dream consolidation、失败轮次教训 | grok `memory_dream.rs` / hermes background_review |
| `core/models/model_registry.py` | ServiceError 分类 + classify_error + responses 容错反序列化 | openclaw `failover-error.ts` / grok `retry.rs` |
| `core/mcp/manager.py` | operation 分类、动态工具刷新、前缀反注册 | hermes `mcp_tool.py` / grok `bridge.rs` |
| `core/mcp/runtime.py` | MCP resources 工具化、SamplingHandler | opencode `session/tools.ts` / hermes `mcp_tool.py` |
| `core/skills/tool_bridge.py` | 接本地 parser/validator/sandbox + 恶意扫描 | hermes `registry.py`/`skills_guard.py` / openclaw `skill-scanner.ts` |
| `core/prompts/system_prompt_v2.py` | 显式 stable/context/volatile 三层 | hermes `system_prompt.py` |
| `core/agent/runtime_context.py` | `<memory-context>` 围栏 + "NOT user input" 注记 | hermes `memory_manager.py` |
| `core/audit/tool_audit.py` | fail-open→fail-closed | — |
| `core/quality/domain_policies.py` | 内置最小领域预检 | opencode provider-policy 思路 |
| `core/quality/cache_optimizer.py` | 保留；与 epoch 打通 | opencode `cache-policy.ts` |
| `core/trace_writer.py` | streaming partial 归档、URL query 级 redact | grok `upload/turn.rs` / `xai-grok-secrets` |
| `core/tasks/task_planner.py` | 开启 plan 模式 + turn-end todo nudge（仅当计划处于 Active 状态才注入 `<task_completion_discipline>`） | grok `turn.rs` TodoGate（`xai-grok-shell/src/session/acp_session_impl/turn.rs:1973,2502-2543`；不是 goal_planner.rs） |
| `core/runtime/tools/policy_lattice.py` + `core/tools/tenant_tool_policy.py` | 扩展四级分层继承 + deny-first glob（租户→用户→会话→工具） | openclaw `tool-policy-pipeline.ts` / `sandbox/tool-policy.ts` |
| `core/content/structured_output.py` | synthetic 工具统一 structured output | grok `StructuredOutput` / opencode `generateObject` |

## 9. 复核记录（2026-08-08）

本报告交付前已由主控流程对全部引用路径做过**存在性复核**，而非依赖研究代理转述。

**grok-build 专项复核（1.0.0）**：初稿基于 0.2.101（`8adf901`），2026-08-08 用户升级 grok 至 **v1.0.0**（`afbc0fb`）后，已按指令"直接对 1.0 重新阅读，不要修修补补"对本文全部 grok 内容重读重写，关键符号全部逐条 grep/sed 对照源码验证（见下）。复核中**证伪/修正**的旧稿错误引导：① `RunBudget` 中间件不存在于 grok 1.0（预算为 `ChatStateEvent::ImageBudget` 处理 + `DEFAULT_MAX_RETRIES=15`，见 §5.1 落地建议 #2 注）；② 内置技能非 `include_str!` 编译期内嵌，而是运行时从 bundled_skill_dirs 发现（§5.6）；③ TodoGate 不在 `goal_planner.rs`，在 `turn.rs:2502-2543` + pager `--todo-gate` flag，且仅 `/goal` Active 时激活（§5.6、§8 附录）；④ `RoleSpawnOverride` 在 `goal_planner.rs:44`（非 handle_request.rs）；⑤ `reason_code`/`decision_reason` 字符串字段不存在，用 `PermissionDecisionReason` 闭枚举（§5.7 落地建议 #4）；⑥ `upload_full_prompt_txt` 是 disabled stub（`trace.rs:439-443`），§5.11 已移除相关能力 claim；⑦ memory_dream 超时是 30 分钟 `Duration::from_secs(30*60)`，doc comment 写 60s 与代码不符（§5.12）；⑧ MCP 工具硬编码 `ToolKind::Other`，无按 operation 的只读分类（§3 表 row 7 已如实标注）；⑨ URL query 脱敏是非正则（17 个 `SENSITIVE_QUERY_PARAMS` 命名名单 + form_urlencoded 重组，§5.7）。

**AI--Platfform 侧（相对路径基于 `apps/assistant-service/src/assistant_service/`）**
- 关键断言均已 `grep`/`wc -l` 验证：`agent_loop.py`=8955 行、`core/tool_invoker.py`=2001 行（`CapabilityAllowlist` 定义于此，**非** `core/tools/` 下）、`core/tools/tool_registry.py`=743 行、`core/memory/compressor.py`=514 行（`preserve_recent=6`）、`core/models/model_registry.py`=2793 行（`nonstream_tool_calls_unsupported` @ L1948）、`core/tasks/task_planner.py`=1629 行、`core/prompts/system_prompt_v2.py`=1356 行、`core/trace_writer.py`=1369 行。
- 安全/可靠性断言已验证：`core/audit/tool_audit.py:138` 限流 DB 异常 `return True  # Fail open`；`core/agent/middlewares/harness.py:3` 五件套"intentionally not registered by default"；`core/agent/middleware.py` 多处 `except Exception` fail-open；`execution_gateway.py:470` `restorable_from_checkpoint: False`；`core/mcp/manager.py:364-366` 硬编码 `read_only=False/operation_kind=unknown/idempotency_supported=False`；`.env.example:114` `ASSISTANT_SUBAGENTS_ENABLED=false`；`runtime_adapter.py` 的 skills/scheduler/failover_v2 等默认 `False`。
- 全部 11 个"新建文件"建议（`core/tools/tool_result_storage.py`、`core/quality/background_review.py`、`core/models/model_fallback.py` 等）经 `ls` 确认当前**均不存在**，属真实新增而非覆盖既有文件；唯一例外 `core/skills/parser.py` 已存在（re-export shim），故 §5.6 改为"替换 shim 而非新建"。

**四个开源项目侧（绝对路径）**
- 抽查 40+ 处借鉴源路径，全部存在，包括：opencode `packages/core/src/session/runner/llm.ts`、`event.ts`、`session/input.ts`、`tool/registry.ts`、`tool-output-store.ts`、`system-context/index.ts`、`permission.ts`、`session/context-epoch.ts`、`session/run-coordinator.ts`、`background-job.ts`、`packages/llm/src/route/executor.ts`、`packages/opencode/src/tool/task.ts`、`packages/core/src/skill/guidance.ts`；hermes `tools/registry.py`、`model_tools.py`、`tools/tool_result_storage.py`、`tools/tool_search.py`、`agent/context_engine.py`、`agent/turn_context.py`、`agent/iteration_budget.py`、`agent/curator.py`、`agent/background_review.py`、`tools/delegate_tool.py`、`tools/threat_patterns.py`、`agent/tool_guardrails.py`、`tools/mcp_tool.py`；grok-build v1.0.0 复核路径：`crates/codegen/xai-grok-sampler/src/{retry,doom_loop}.rs`（retry.rs 含 RetryWithClientRebuild/Retry-After clamp/x-should-retry veto @ 149-151,57-64,201-216）、`crates/codegen/xai-grok-shell/src/session/acp_session_impl/{run_loop,tool_calls,sampler_turn,memory_dream,turn}.rs`（turn.rs TodoGate @ 1973,2502-2543；memory_dream.rs 30min 超时）、`session/{goal_planner,goal_role_tools,reminders}.rs`、`agent/subagent/handle_request.rs`、`xai-grok-config/src/signed_policy.rs`、`xai-grok-compaction/src/compaction.rs`（verbatim→fitted→lossy 三级降档 + system prompt verbatim 保留）、`xai-grok-shell/src/upload/{trace,turn}.rs`（upload_full_prompt_txt 为 disabled stub）、`xai-grok-mcp/src/{lib,servers}.rs`（ToolKind::Other 硬编码）、`xai-grok-tools/src/{bridge.rs,implementations/skills/discovery.rs}`、`xai-grok-secrets/src/sanitizer.rs`（SENSITIVE_QUERY_PARAMS 名单）、`xai-grok-telemetry/src/events/permission_analytics.rs` + `workspace/permission/manager/reasons.rs`（PermissionDecisionReason 闭枚举）、`xai-circuit-breaker/src/retry_policy.rs`、`xai-grok-memory/src/dream.rs`；openclaw `src/routing/session-key.ts`、`src/agents/tool-policy-pipeline.ts`、`src/agents/sandbox/tool-policy.ts`、`src/security/external-content.ts`、`src/security/skill-scanner.ts`、`src/infra/agent-events.ts`、`src/agents/skills/workspace.ts`、`src/memory/manager.ts`、`src/process/command-queue.ts`、`src/agents/pi-embedded-runner/run.ts`。
- 已修正的引用偏差：openclaw `src/process/command-queue.js` → **`command-queue.ts`**；hermes `tools/tool_guardrails.py` → **`agent/tool_guardrails.py`**（报告正文引用本就正确）；grok 侧 9 处 0.2.x→1.0 证伪/修正见本段上方"grok-build 专项复核"清单。

**使用提示**：openclaw 仓库目录名含空格（`/Users/yang/projects/open claw/openclaw`），shell 引用时需加引号；实施前仍建议以 `ls`/`grep` 复核目标文件最新状态。