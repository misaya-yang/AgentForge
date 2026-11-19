# AI Assistant 模块深度审查报告

- **日期**: 2026-08-12
- **范围**: `apps/assistant-service`(11.6 万行 Python,当前工作树状态,对应 docker 镜像 `review-current`)
- **方法**: 5 个并行子系统审查(API/模型层、工具/MCP、流式执行、Gateway/生命周期、记忆/RAG)+ 对 agent 核心循环的逐行精读;所有 HIGH/CRITICAL 级发现均复核过源码;运行时证据来自容器日志
- **测试**: `tests/services/assistant/` 共 **2311 passed / 1 skipped**(17.6s,唯一跳过为需 PG 集成测试 DSN);仓库整体覆盖率 5.18%(低于 25% 门槛,含大量其他服务代码)

---

## 1. 总体结论

**架构层面这是全仓库工程化程度最高的模块之一**:TurnKernel 状态机、run-budget 双重闸门、checkpoint + 操作 fence、外部内容注入边界(`instruction_authority="none"` 信封)、压缩 lineage 哈希校验、审批 CAS 原子认领——这些在多数开源 agent 框架里都不存在。

**但离"高度专业的通用智能体"的差距是真实的,且在生产日志里有直接证据**(见 §4)。差距集中在三条线:

1. **可靠性**:故障恢复链在真实流量中失败了——工具失败 → 迭代耗尽 → 强制 synthesis 两轮全抛 `RuntimeError` → 用户收到降级文案。同时存在 9 个 HIGH 级缺陷(含 2 个安全类)。
2. **专业性(智能体本身的质量)**:系统提示词平庸(英文清单体、无模型特定调优、无 few-shot);默认工具集只有 6 个;检索无相关性评分体系;无质量指标闭环——没有任何机制告诉你智能体"有没有变好"。
3. **工程护栏**:模型 `access_level` 授权绕过、无速率限制/配额、代码沙箱容器泄漏、崩溃后 run 永久悬挂无回收器。

---

## 2. Findings(按严重度,标注验证状态)

### Critical

无。唯一 CRITICAL 提名(历史裁剪,memory 报告)经源码复核后降级为 HIGH——其"provider 400"结论仅在输入已含连续同角色消息时成立,主危害是非连续历史造成的语义错乱。

### High(9 项)

| # | 位置 | 问题 | 影响 | 建议 |
|---|------|------|------|------|
| H1 | `api/routes/chat.py:217` + `core/models/model_failover.py:206` ✅已亲自验证 | 模型 `access_level`(admin/premium/public)**从未对请求的模型强制**;failover 的资格检查只对 fallback 候选执行(`if model_id != requested_model`) | 任何已认证用户可点名调用 admin/premium 级模型 → **模型分级授权绕过**,两个 API 面均受影响 | 对请求模型同样执行 `_model_is_eligible`;fallback 选择也过资格检查 |
| H2 | `core/code_executor.py:741-766` ✅已亲自验证 | 沙箱超时未生效且容器泄漏:`container.wait(timeout=...)` 在 executor 线程里抛 `requests.ReadTimeout`(docker-py 内部 30s 传输超时),不是 `asyncio.TimeoutError`,因此 kill 分支永不触发;tuple 解包未完成又使 `execute()` 的 `finally` 因 `container is None` 跳过清理 | 不受信代码可**无限运行**、容器泄漏(每个 ~512MB);`cleanup_all()` 只回收已停止容器 | 在 `_run_container` 内捕获传输超时类并 kill+remove 后重抛;或去掉内层 wait 超时,统一由 `asyncio.TimeoutError` 分支处理 |
| H3 | `core/code_executor.py:1012-1022`(子代理报告) | `_collect_output_files` 无尺寸上限地全量读入 `output/` 全部文件并 base64 进结果;docker 路径的 `container.logs()` 同样无界 | 沙箱内 `open('output/x','wb').write(...)` 一条语句即可 OOM 网关或撑爆上下文 | 单文件 + 聚合字节上限,超限截断并带 visible 截断标记 |
| H4 | `core/agent/streaming_tool_execution.py:1182` vs `streaming_tool_call.py:449`(子代理报告) | `tool_call_completed/failed` checkpoint 持久化的消息快照在 tool 结果消息追加**之前**生成 → 快照里 assistant 带 tool_calls 却没有对应 tool 消息 | 从这些 checkpoint 恢复时模型上下文非法 → 必然 provider 400;post-tool 恢复路径明确拒绝修复此形态 → 恢复卡死 | 先 append tool 消息再存 checkpoint |
| H5 | `core/agent/streaming_tool_call.py:382-387`(子代理报告) | KB 去重的 `mark_completed` 被 `state.contexts_for_persistence` 非空门控;成功但 0 命中的检索永不标记 | 模型可无限重复完全相同的查询,烧工具预算——正是 dedup 要防的循环 | 仅以 `step_success is True` 作为标记条件 |
| H6 | `core/gateway/run_lifecycle.py:645-649`(子代理报告,已 grep 复核) | `finish_run` 终态 fence 要求 status='running',resume 只恢复 'blocked';**没有 run 租约/心跳/回收器** | 进程崩溃后 run 行永久 'running':既不能恢复也不能终态化,命令结果永不 reconcile | run 行租约 + 心跳 + 过期回收器(以 `terminal_persistence_unknown` 语义终态化),resume 允许修复超租约的 running 行 |
| H7 | `core/trace_writer.py:565-574` + `execution_lifecycle.py:207-222`(子代理报告) | resume 时 `drain(strict)` 把遥测侧任何 dropped/timed_out 事件(含 256 缓冲溢出)升级为**终态失败** | 一个遥测事件丢失 = 审批暂停的 run 不可恢复 = 用户点了批准却永不执行 | 恢复时容忍非终态事件丢失(log + 从 MAX(sequence_no) 续),仅终态记录缺失才失败 |
| H8 | `core/runtime/compat/runtime_adapter.py:298-332` + `core/runtime/memory/indexer.py:197-287` ✅已亲自验证 | 每轮模型调用都重读全部近期 markdown 源并**全量重索引**:`index_source` 无条件删点 → 重切块 → 重 embed → upsert,无 content-hash 短路 | 每次用户消息 O(全部记忆) 重嵌入,秒级延迟;embedder/Qdrant 变慢直接拖慢每一轮 | 写入路径(turn_sync)只对 content_hash 变化的源建索引;文件 I/O 走 `asyncio.to_thread` |
| H9 | `core/rag/context_engine.py:244-259` ✅已亲自验证(降级) | `_trim_history_preserving_tool_pairs` 在 unit 放不下时**继续考察更旧 unit**,产生带空洞的非连续历史 | 模型看到更旧内容却看不到较新内容;若输入本身含连续 user 消息,空洞两侧可形成同角色相邻(Anthropic 400) | 一旦某 unit 放不下即停止考察更旧 unit(保留最新连续后缀),并在 flatten 时断言角色交替 |

### Medium(24 项)

**工具/MCP 层**
- M1 `tool_invoker.py:1513-1524` — `_invoke_parallel` 对异常项 continue 留 `None`,`invoke_batch` 在 1476 行 `AttributeError` → 批量调用整体 500 而非部分失败。修复:异常项合成失败结果。
- M2 `tool_invoker.py:1111-1167` — MCP 分支忽略 `effective_timeout`(无 `wait_for`),agent 级 15s 预算被静默突破;且无兜底异常捕获。修复:包裹 `asyncio.wait_for` 并转换异常为失败结果。
- M3 `tool_invoker.py:1033-1054` — inflight 指纹 check-then-add 非原子,并行批次可双发同一次有副作用写入。修复:claim-then-verify。
- M4 `tool_orchestrator.py:389-425` — 消费者提前停止迭代时无 finally 取消未完成的并行工具任务 → 断连后写操作继续执行。修复:finally 取消 pending 任务。
- M5 `tool_registry.py:368-372` — 租户可配置的 MCP `input_schema` 正则(≤256 字符)在事件循环内做 `iter_errors` 验证,`(a+)+$` 级别模式可 ReDoS 卡死循环。修复:拒绝嵌套量词/backref,或对输入长度封顶。
- M6 `tool_registry.py:779` + `tool_invoker.py:1100-1107` — 注册表与调用器两套超时层互相截断,重试预算按定义值燃烧。修复:单一执行点。
- M7 `tool_invoker.py:1152-1162` — MCP 分支吞掉 `CancelledError` 并返回结果,违反结构化并发语义。修复:清理后重抛。

**流式/循环层**
- M8 `core/agent/streaming_execution.py:388-455` ✅亲自发现 — 强制 synthesis 把第二份完整回答**追加**在已流出的部分内容之后(泄漏叙事场景),用户看到拼接文本,且两份都持久化进会话历史。修复:强制 synthesis 前清空/标记部分内容,或按"替代"而非"追加"持久化。
- M9 `core/tool_result_formatter.py:125,146` — `float(score)` 遇非数字字符串抛 ValueError,把成功的 KB 检索误标为工具失败。修复:转换失败回退 0.0。
- M10 `streaming_tool_call.py:328-359` — approval/checkpoint 持久化失败两条路径 return 时未设 `step_status_override`,前端看到未执行工具显示 "completed"。修复:设 "blocked"。
- M11 `streaming_preparation.py:841` — skill trigger 正则无编译保护,一条坏配置拖垮所有请求。修复:compile 失败跳过。
- M12 `streaming_tool_execution.py:1138-1153` — `tool_call_completed` 事件携带完整 KB contexts(无上限),单条 SSE 可达数 MB。修复:只发 compact 元数据 + artifact receipt。
- M13 `streaming_tool_validation.py:73,149-175` — 去重命中/策略拒绝的调用 `turn_call_record` 停留 "running",会话历史显示错误的执行状态。修复:stop_processing 前设终态。
- M14 `stream_helpers.py:67-69` — `merge_stream_tool_calls` 对 dict 分片 `str()` 拼接产生非法 JSON,合法调用被拒为 invalid args。修复:dict 分片按 json 语义合并。
- M15 `middleware.py:220-247` ✅亲自复核 — `run_on_tool_call` 对异常/非 ToolVerdict 返回按 **allow** 处理(fail-open)。已核实最终强制在 gateway 声明路径 fail-closed,但这是设计风险点,应在文档中明示依赖关系,新中间件作者容易踩坑。

**Gateway/生命周期层**
- M16 `run_lifecycle.py:100-121` — start_run 的隐式复活闸只查 status='blocked',不查既有 approval_pending checkpoint;用户发新消息后旧审批静默失效,点了批准无反应。修复:轮到有 pending 审批的 run 时路由进审批恢复流或先退役旧审批。
- M17 `execution_lifecycle.py:692-715` — 终态事件先于未配对工具的修复事件发出;流消费端在终态处关闭会漏掉修复事件。修复:缓冲修复事件,先发后终态。
- M18 `run_lifecycle.py:1199-1360` — ack 窗口崩溃留下 `result_recorded_*` 命令,死 run 永不 reconcile,`finish_run` 的 fence 又阻止终态化 → 死锁态。修复:resume/repair 时执行 reconcile,fence 忽略无 pending 完成的 result_recorded 命令。
- M19 `subagent_manager.py:1341-1355` — 父取消时子代理工具被报 "cancelled",但服务端副作用可能已落地(网关侧有 side_effect_unknown 机制,子代理没用)。修复:取消时等待工具结果或报 side_effect_unknown。
- M20 `approval_lifecycle.py:150-155` — `approve()` 的 DB 故障与合法不可用同样返回 None,基础设施故障对用户不可见。修复:类型化异常/结果。
- M21 `run_lifecycle.py:1142-1147` — 非 hard checkpoint 写入无 run-fence,僵尸 run 可收晚到 checkpoint,ack 路径可能信任已终态 run 的"confirmed"。修复:统一 eligible_run fence。

**API/模型层**
- M22 `api/routes/responses.py:1007-1046` — `/responses` 只查全局默认租户 registry,租户级模型在 chat 可用但 OpenAI 兼容 API 400。修复:与 chat 路径一致走租户 resolver。
- M23 `api/routes/chat.py:230-240` — 请求模型缺失时静默回落 `available[0]`,不做租户/权限考量(与 H1 叠加)。修复:422 或对回落选择执行资格检查。
- M24 `registry_lifecycle.py:142` + `main.py:144-187` — 无整体请求 deadline;单 provider 调用固定 120s,非流式 /chat 可钉死 worker,SSE 永不超时。修复:每请求总 deadline 传入循环,降低 provider 超时。

**记忆/RAG 层**
- M25 `core/memory/compressor.py:641-646` — `ModelRegistryLLMService.complete` 用裸 `except Exception → ""` 吞掉 `RunBudgetExceeded`(调用方明确处理了它),预算耗尽时压缩静默 no-op 并误报原因。修复:重抛控制类异常。
- M26 `core/runtime/memory/indexer.py:185-195` — advisory lock 解锁是 fire-and-forget,解锁失败时池化连接持锁,同源后续索引永久阻塞。修复:同连接 try/finally + `SET LOCAL lock_timeout`。
- M27 `core/memory/compressor.py:230-231` — `preserve_recent=0` 时 `messages[:-0]` 为空 → 什么都不压缩、摘要基于空内容(语义反转),公开 API 陷阱。修复:显式处理 ≤0。
- M28 `core/runtime/memory/source_store.py:462-468` — 子串包含去重,真实用户记忆(短事实)被静默丢弃。修复:整行/整段全等。
- M29 `core/runtime/memory/source_store.py:644-658` — symlink 文件异常从文档循环传播,一条坏条目令整次记忆检索失败。修复:预检逐条跳过。
- M30 `core/runtime/context/assembler.py:1091-1104` — `_cache_contract` 的 `dimensions.setdefault("model", "")`,缺 model 时不同模型共享同一 cache key。修复:model 为必选维度。
- M31 `core/memory/memory_manager.py:549-562, 632-634` — 坏偏好值(非 dict)令读取崩溃;layer 公开属性可绕过脱敏门控。(注:该 manager 是死代码,见 §5)

### Low(17 项,按子系统分组)

**工具**: `confluence_tool.py:643-656` 客户端缓存无界(持有 base64 凭据);`mcp/runtime.py:290-306` semaphore 字典永不驱逐;`tool_invoker.py:955-965` 缓存命中绕过限流检查;`tool_registry.py:776-779` 同步工具返回非 awaitable 使 wait_for TypeError;`mcp/config.py:104-107` headers 允许明文密钥(仅 api_key 强制 env 引用);`code_executor_tool.py` AST 黑名单可平凡绕过(建议声明为 best-effort);`tool_orchestrator.py:287,344` 模型可控字符串 f-string 日志注入/泄密。

**流式**: `agui_protocol.py` 是未接线的死代码且 `run_error` 原地改 caller metadata;`streaming_tool_call.py:411` 结果计费抛异常发生在公共事件已发出之后;`tool_output_spill.py:41,149` 字符/字节单位混用。

**Gateway**: `run_lifecycle.py:638-644` `json.dumps(usage)` 在持久化 try 外;`subagent_manager.py:545-549` 相同兄弟 prompt 生成相同 task_id;`task_planner.py:1411-1456` LLM 计划重复 task ID 产生误导性的空环 CircularDependencyError;`task_planner.py:701,1286` 无超时且默认模型已过时。

**API**: `chat.py:41` E2E stub 记忆字典无界增长;`main.py:190-198` 依赖 `database._pool` 私有属性;`chat.py:808-812` 非流式 /chat 所有失败坍缩为通用 500;`model_registry.py:1127` 空 choices 时 IndexError 500;`local_node/control_plane.py:1076-1082` 幂等重放把时间戳算进 digest,安全重试被 409(偏 fail-closed,破坏性低)。

**记忆**: `memory_manager.py:310` 工作记忆搜索对不可序列化值无保护;`memory_manager.py:76-84` 注入防护正则误伤正常文本且在写入时不可逆破坏原文;`compressor.py:47-52,404` PRESERVE_PATTERNS 死代码 + 非贪婪 JSON 提取残缺;`compressor.py:278-302` 脱敏漏掉 tool_calls.arguments;`compressor.py:516-562` token 估算漏计 identifiers/artifacts。

---

## 3. 为什么"远远没达到专业通用智能体"——差距分析

这是本次审查的核心问题。逐项对照行业标准(Claude Code / Manus / Cursor 级通用智能体)的实际差距:

### 3.1 可靠性差距(最致命)
- **故障恢复链在生产中真实失败**(日志实证,§4):工具失败 → 迭代耗尽 → 强制 synthesis 两轮全抛 → 降级文案。用户实际遇到的就是"远远没达到"的直接体验。
- **崩溃无恢复**:H6/H7/M18 意味着进程一挂,run 永久悬挂、审批永久失效、ack 窗口永久死锁。专业智能体必须有 run 回收器和可恢复性。
- **断线即孤儿**:streaming 报告确认 resume 只对审批暂停开放(`resume_run_id AND resume_approval_id`),客户端中途断开没有续跑路径——而这是 Claude Code/Manus 的核心体验。

### 3.2 智能体本身的质量差距(我的核心精读结论)
- **系统提示词平庸**:`system_prompt_v2.py` 是通用英文清单体(身份/原则/工作流/反幻觉/错误恢复 ~8 大节),没有针对 Qwen/Gemini 的行为调优、没有工具使用的 few-shot、没有输出格式的强约束、没有"什么时候该停止工具调用"的明确判据。通用智能体的提示词是产品本身,当前只是及格线。
- **工具集太薄**:默认租户只见 6 个工具(KB 检索、spawn_subagent、tool_call/describe/search、update_user_memory)。无文件操作、无代码执行(配置外)、无浏览器、无多模态输入工具。通用智能体的竞争力一半在工具生态。
- **检索质量无评分**:BM25+余弦固定 0.65/0.35 融合,无 reranker、无时间衰减、无最低分阈值——无关命中照样进上下文。专业级 RAG 的最低配是 rerank + 阈值。
- **记忆无生命周期**:每日记录/反思/事实全混在一个 chunk store,无 episodic→semantic 整合管道、无遗忘曲线、无 recency-frequency 排序、无冲突消解("偏好 Python"与"偏好 Go"共存无裁决)。
- **无质量闭环**:没有 eval 集、没有 grounding 指标、没有相关性反馈——**没有任何机制能告诉你改提示词/改检索之后智能体变好还是变坏**。这是"专业"与"业余"最本质的分界线。

### 3.3 工程护栏差距
- **安全**:H1(模型分级绕过)+ api-models 报告确认:无速率限制/租户配额、无请求体大小统一上限、重放防护默认内存态(多副本失效)、匿名模式信任伪造 X-User-Id。auth 是共享密钥 header-trust 模型。
- **多租户**:无每租户配额/计费闸门,一个认证密钥可以无限烧 LLM 支出。
- **可观测性**:trace 只有事件序列,无按工具的延迟/重试/重复调用指标;去重与预算拒绝静默发生,指标层不可见;subagent 工作不是独立 span,追踪里不可见。

### 3.4 已做对的地方(值得保留)
- **提示注入防护体系**:外部内容信封 + `instruction_authority="none"` + 工具结果 envelope + 注入边界语句——设计到位,且 KB/记忆注入都走这套。
- **状态机与契约**:TurnKernel 终态唯一性强制、双终端事件抛错、checkpoint receipt 确认后才前进——罕见严谨。
- **压缩 lineage**:父/子上下文哈希 + 摘要哈希 + 受保护约束逐字校验 + 验证失败全量回退 no-op——教科书级防御。
- **审批完整性**:原子 CAS 认领(FOR UPDATE + 条件 UPDATE),子代理无权限逃逸路径(5 层防线:目录 host 过滤、allowlist 重绑定、深度封顶、网关逐次派发重评估、无父路径 fail-closed)。
- **失败口径**:工具失败有结构化事件、有 `_side_effect_recovery`(side_effect_unknown 暂停 + 补偿元数据)——比大多数框架成熟。

---

## 4. 运行时证据(2026-08-12 容器日志)

```
16:42:45 WARNING Loop ended without clean answer (iter=5, max_iter_exhausted=True,
                last_tool_failed=True, content_empty=False). Running forced synthesis pass 1.
16:42:48 ERROR   Forced synthesis (full) raised; continuing to next fallback (exception_type=RuntimeError)
16:42:48 WARNING Forced synthesis #1 did not complete. Retrying with compacted history (system + user + tool digest).
```

- 工具失败 → 5 次迭代耗尽 → 强制 synthesis **两轮都失败** → 用户收到降级文案(run_error 信号)。这是"没达到要求"的直接实证。
- 另一回合:6 次迭代、**TTFT 29.6s**、总时长 31.4s、输出 215 字符。工具回合 5s/迭代的节奏尚可,但端到端体感慢。
- 每轮模型调用前 prep 累计时间线性增长(102ms → 5.3s → 9.5s → 15.5s → 21s → 25.4s,累计值)——与 checkpoint 全量消息序列化 + 上下文重绑定的成本一致,有优化空间。

---

## 5. 死代码(建议后续清理)

- `core/memory/memory_manager.py`(MemoryManager 从未实例化)—— 但它**是文档化的公开 API**,且有多项 MEDIUM 级缺陷(M25/M27/M31 等),建议要么下线要么修复。
- `QueryIntentAnalyzer` / `ScenarioAnalyzer` / `DocumentAnalyzer`(agent_loop.py:288-290 构造后从未调用)—— 含提示注入绕过隐患(M28 形态),死代码应直接删除以缩小攻击面。
- `core/agent/agui_protocol.py` — AG-UI emitter 未接线,已与真实 SSE schema 分叉。
- `react_executor.py` 已在工作树删除(git status 显示),方向正确。

## 6. 建议修复路线图(按优先级)

**P0 — 安全与数据(先修,工作量小)**
1. H1 模型 access_level 强制(chat + /responses + fallback 选择三处)
2. H2/H3 沙箱超时与输出上限(容器泄漏是资源事故)
3. 速率限制 / 租户配额(至少 LLM 调用层)
4. 统一请求体大小上限

**P1 — 可靠性与恢复(用户体感最直接)**
5. H4/H5 checkpoint 时序与 KB 去重(修完即消除一类恢复死锁与重复检索循环)
6. H6/H7/H18 run 租约 + 回收器 + 恢复时容忍遥测丢失
7. 断线恢复路径:至少先让"模型轮次之间"的 checkpoint 支持续跑(或先明确降级为"重试同一消息")
8. 强制 synthesis 的 append 语义修正(M8),以及提高 synthesis 成功率(当前两轮全失败说明需要把工具摘要做得更稳)

**P2 — 智能体质量(回答"专业性"问题)**
9. 系统提示词重写:按主力模型(Qwen 3.7)行为调优 + few-shot 工具模式 + 停止条件判据
10. 检索升级:rerank + 最低分阈值 + 时间衰减(先上阈值和 rerank,投入产出比最高)
11. 记忆索引增量化(H8)——每轮全量重嵌入是纯浪费
12. 质量闭环:小规模 eval 集(grounding/工具选择/端到端)+ 每轮指标

**P3 — 打磨**
13. 历史裁剪修复(H9)、compressor 异常语义(M25)、超时层统一(M2/M6)
14. SSE heartbeat(/responses 缺失)、事件载荷上限(M12)、tool_call_completed 元数据瘦身
15. 死代码清理(§5)

---

*报告由 Claude Code 生成:5 个子系统审查代理 + 核心循环人工精读;所有 HIGH 级发现均经第二人(主会话)复核源码。子代理行号引用与工作树一致;如合并代码请以当前工作树为准。*
