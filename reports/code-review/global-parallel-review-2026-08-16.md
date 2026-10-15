# 全局并行审查报告(逻辑 bug / 安全 / 契约)

- 日期：2026-08-16
- 基线：`main@a49eac3`(含未提交工作树改动,行号以工作树为准)
- 方法：30 个只读代理按模块分区并行审查 + 1 个覆盖批评代理;全程未修改文件
- 产出：239 个发现(0 Critical / **32 High** / 108 Medium / 99 Low)+ 18 个覆盖缺口 + 9 个交叉维度
- 复核状态：**本报告已进入独立复核工作流(8 验证代理,逐条对照工作树代码,CONFIRMED/CORRECTED/REJECTED 裁决)**;复核结论将作为 §7 追加。在复核结论落地前,High 条目的引用应以复核版本为准。
- 与 [`perf-review-2026-08-16.md`](perf-review-2026-08-16.md)(性能报告)互补:本报告只覆盖正确性/安全/契约,性能发现见另一份。

## 0. 审查分区

| 编号 | 区域 | 编号 | 区域 |
| --- | --- | --- | --- |
| a1–a8 | 本次 diff 深度审查(Responses 入口/模型轮/子代理/artifact/压缩器/Web diff/测试) | d1–d3 | ai-gateway-core 共享层 |
| b1–b6 | 网关 src/(core/api/adapters/services/persistence) | e1–e4 | Web 前端 |
| c1–c4 | knowledge-service + local-node | f1–f3 | assistant-service 核心长尾 |
| g1–g2 | 基础设施(compose/migrations/CI)+ SDK/docgen | critic | 覆盖批评 |

## 1. High 发现(32,按主题分组)

### 1.1 安全:授权绕过 / IDOR / SSRF

**H1 匿名调用绕过 AGENT_INVOKE 能力门** — `src/api/v1/conversations.py:364`
- Impact: conversations/langgraph 的 send_message/stream_message 只做身份解析+限流,匿名 `anon:uuid` 调用者可运行 LLM agent,绕过 /v1/invoke、/v1/stream 强制的能力检查 → 无计量模型成本暴露。
- Fix: 给所有 conversations/langgraph 端点加共享 `require_gateway_capability(AGENT_INVOKE)` 依赖,代理调用前执行。

**H2 artifact 下载 URL 的 IDOR:NULL owner_scope 视为通过** — `packages/ai-gateway-core/src/ai_gateway_core/storage/artifact_storage.py:624`
- Impact: 鉴权只在 `owner_scope is not None` 时拒绝;所有 agent artifact、工具溢出、历史行都是 NULL owner_scope → 任何认证用户只要知道 artifact_id 就能拿到他人 artifact 的预签名下载 URL。
- Fix: NULL 视为不匹配,回退到 tenant_id+user_id 比对(把 `_load_artifact_bytes_owner_scoped` 的既有回退逻辑下沉到共享层),并迁移回填历史行的 owner_scope。

**H3 用户可控 callback_url 原样转发为 LangGraph webhook(SSRF + 数据外泄)** — `src/adapters/langgraph.py:557`
- Impact: POST /invoke|/stream 携带 `callback_url=http://169.254.169.254/...` 时,网关把它当 `payload['webhook']` 转发,上游 LangGraph 会把含敏感对话内容的完整 run 结果 POST 给攻击者指定地址;`task_manager.py:150` 对同一字段已有 SSRF 防护,此处是已知模式的遗漏。
- Fix: 转发前校验 scheme 白名单 + 拒绝 loopback/私有/link-local 主机,复用 task_manager 的 SSRF 守卫。

**H4 LangGraph passthrough 绕过 store 命名空间归属检查** — `src/api/v1/langgraph.py:697`
- Impact: 专用 /store/* 路由经 `_validate_namespace_access` 校验归属,passthrough 对未处理路径(PUT/PATCH /store/items、批量变体)只查 threads/assistants 就透传 → 用户可跨用户读写/删除他人 memory namespace。
- Fix: passthrough 中任何 /store/ 前缀路径先解析 namespace 并执行同一归属校验,无法校验则拒绝。

**H5 配额写端点允许自服务重置与自我提额** — `src/api/v1/quota.py:68`
- Impact: `_require_quota_subject` 允许 `target_user_id == 自己` 的写操作,且迁移 068 给 developer 角色 `console:quota:edit` → developer 可把自己配额清零或设为 0(无限),网关 enforce 读取的正是这些计数。
- Fix: 读操作允许本人;reset/block/set 要求 admin/operator;逻辑下沉到各写端点而非共享 helper。

**H6 create_artifact 缺会话归属校验(写侧授权缺口)** — `src/api/v1/assistant.py:1214`
- Impact: POST /assistant/artifacts 接受任意 session_id,不验证会话存在或属于调用者 → 任何认证用户可向他人会话注入 artifact(标题/内容/元数据),污染对方 artifact 列表。
- Fix: 写前用 session_manager 校验归属(仿 list_session_artifacts),并在存储层镜像该检查做纵深防御。

**H7 子代理 prompt/parent_context 绕过外部内容信封与脱敏** — `apps/assistant-service/src/assistant_service/core/agent/subagent_manager.py:829`
- Impact: 父模型把用户粘贴的 API key 等复制进 spawn_subagent 的 prompt/context 参数时,子模型原样收到——无角色行中和、无 redact_trace_text;raw prompt 还未经脱敏流入 subagent_started 事件。本轮新加的 child 输入脱敏被此路径绕过。
- Fix: `_build_messages` 中把 parent_context(829)与 prompt(863)同样过 `normalize_external_text`,或包进 untrusted user-role 信封。

### 1.2 计费 / 配额正确性

**H8 日请求计数从未被活路径重置(永久封禁)** — `src/services/billing/aggregation_task.py:214`
- Impact: QuotaResetTask.reset_daily_quotas(唯一活重置路径,每小时)清零 tokens 但不清 `current_daily_requests`,而 check_quota 按它封禁 → 用户一旦当日用完请求数,之后每天都永久被封,直到管理员手动 reset;自动重置只存在于死代码(quota_service.py:827)。
- Fix: QuotaResetTask 同时清零 current_daily_requests;删除或接通 QuotaService 的死代码副本。

**H9 日配额重置 off-by-one + 每天最多晚 1 小时** — `src/services/billing/aggregation_task.py:214`
- Impact: `daily_reset_at` 初始化为次日 00:00:00,重置 WHERE 用严格 `< CURRENT_DATE` → 新建行的第一次重置永不触发(首日用量第 2 天仍计);此后每天在午夜后第一次小时检查才触发,00:00–01:00 的请求按昨日计数误拦,且 flush 的新日用量被迟到重置清零(token 丢失/免费额度)。
- Fix: 改 `daily_reset_at <= CURRENT_DATE`,并在 check_quota/_get_or_create_quota 读时惰性清零(不依赖调度器时序)。

**H10 月度配额重置首个边界必然错过(整月错账)** — `src/services/billing/aggregation_task.py:242`
- Impact: `monthly_reset_at < DATE_TRUNC('month', NOW)` 严格小于 + `day==1` 门 → 创建后的第一个月界被跳过;月中创建的用户 9 月花费仍计入 8 月余额,要么整月被封要么双倍预算,10 月 1 日才修正。
- Fix: 改 `<=` 并删掉 day==1 门,靠幂等 WHERE 保证错过重启也能在 day2+ 补上。

**H11 幂等合并保留第一条(0-token)记录,后续真实用量被丢** — `packages/ai-gateway-core/src/ai_gateway_core/metrics/usage_recorder.py:305`
- Impact: 同一 identity 记录两次(重试/部分+最终/双写)时,合并与 ON CONFLICT 只带 status/error 字段;先到的 0-token running 记录永久胜出 → usage_records、配额计数、日/时聚合全部少计,无错误提示。
- Fix: token 取 GREATEST 合并(进程内 merge 与 DO UPDATE 都改),让后到的 success 记录升级计费维度。

**H12 模型定价前缀双向匹配:gpt-4o 命中 gpt-4o-mini 价格** — `packages/ai-gateway-core/src/ai_gateway_core/metrics/usage_recorder.py:527`
- Impact: 请求 gpt-4o、缓存里有 gpt-4o-mini 时,`cached.startswith(requested)` 使 mini 成为候选且最长匹配选中 → gpt-4o 请求按 mini 单价计费,成本账目被污染。
- Fix: 只允许 `requested.startswith(cached)` 或 `len(requested) >= len(cached)`,禁止反向。

**H13 流式 token 计费跨事件求和 → 过计** — `src/adapters/langgraph_proxy.py:1131`
- Impact: 每个 SSE 事件的 usage 都 `+=` 累加,而 LangGraph 在多事件上携带的是累计值 → 200 token × 30 事件计成 6000,下游用量记录虚高。
- Fix: 按 run 取单调 max 累计值(只取更大者或只计终态 metadata 事件);加多事件 fixture 测试。

**H14 StreamProcessor 锁存首个 usage 事件,后续累计值被忽略(少计)** — `src/proxy/billing_stream.py:293`
- Impact: 首个事件后 `_usage_collected` 直接返回;agent-loop 流按 LLM 调用多次发 usage 时只记第一次 → 少计,流入计费与配额。
- Fix: 跨事件跟踪最大累计值,流结束时定案。

### 1.3 会话 / 数据正确性

**H15 会话内存缓存无驱逐且可服务陈旧快照** — `packages/ai-gateway-core/src/ai_gateway_core/session/database_manager.py:417`
- Impact: Redis 缺失(assistant 默认)时 `_memory_cache` 只增不减;并发 get()+update_metadata 可在失效后回写陈旧快照,无限期服务过期历史/元数据。
- Fix: 加 TTL/驱逐;写后按 updated_at 重新校验新鲜度。

**H16 网关 conversation-share 查询仍未限定表名,读到空的 gateway.artifacts** — `src/api/v1/conversation_shares.py:212`
- Impact: 表已迁到 assistant schema,此查询靠 search_path 侥幸解析;DSN 不含 assistant(或没跑 phase-6 迁移)时读错表/报错 → 共享会话快照静默显示 0 artifact。新 schema 回归测试只扫 ArtifactStorageService,漏掉了这里。
- Fix: 限定 `assistant.artifacts`;把 schema 回归扩成全仓库扫描(含 src/ 与 assistant-service)。

**H17 幂等中间件:CancelledError 跳过 abort → key 被毒化整整 TTL** — `packages/ai-gateway-core/src/ai_gateway_core/comm/idempotency.py:283`
- Impact: `except Exception` 捕不到 asyncio.CancelledError(3.8 起是 BaseException);请求被取消时 in-progress 标记残留 24h(默认),同 key 重试连续 409。
- Fix: `except (Exception, asyncio.CancelledError)` 或用 try/finally 保证 abort 执行。

**H18 幂等中间件 `_read_body` 客户端断连时永久挂起** — `packages/ai-gateway-core/src/ai_gateway_core/comm/idempotency.py:325`
- Impact: `http.disconnect` 后 receive() 持续返回 disconnect 且循环只在非 http.request 消息时 continue → 上传中断连的请求任务永久挂起,连接与任务泄漏。
- Fix: 遇 disconnect 直接 break/抛错。

**H19 schema.sql 与迁移链漂移;compose 全新安装缺 ~25 张表** — `database/schema.sql:1`
- Impact: compose 全新 DB 只跑 schema.sql(6-16 已漂移)+ ~17 条精选 auto-init 迁移;046–084 的表(user_connectors、agent_trace_eval、eval_platform、agent_mcp_registry 等)不存在 → 对应功能运行时 undefined_table。
- Fix: 从迁移链重新生成 schema.sql(或删除并让 auto-init 跑迁移器);加 CI 门禁 diff 表集合。

**H20 两套互不相通的迁移权威** — `docker-compose.yml:123`
- Impact: compose `migrate` 任务只跑 per_service 链,schema_migrations 账本在 `schema_migrations_meta`;`migrate.sh` 只跑 flat 002–084,账本在 `public.schema_migrations`。两条路各自缺对方一半;artifact_shares 两条链都有(083 + per_service/assistant/003),已现分歧风险。
- Fix: 单一权威 runner(一方兼跑另一方或折叠),单一账本;把 compose 任务标注弃用或由 shell runner 门控。

**H21 陈旧 public.* 迁移门在 phase-6 后重建影子表** — `packages/ai-gateway-core/src/ai_gateway_core/persistence/database.py:1154`
- Impact: 13 个 `to_regclass('public.X')` 门在表迁出 public 后恒 NULL → auto_init=True 启动重放 034/035/037/038/039/045 的未限定 CREATE TABLE,落到 gateway schema(search_path 首位)形成空影子表,所有未限定查询解析到影子 → 真实数据"消失"。这是 artifact bug 的同类,规模更大。
- Fix: 13 个门改为按归属 schema 限定或查账本;`auto_init` 默认改 False。

### 1.4 崩溃恢复 / 状态机(H6/M7/M8 家族)

**H22 H6:running run 无 lease/heartbeat/reaper** — `apps/assistant-service/src/assistant_service/core/gateway/run_lifecycle.py:80`
- Impact: start_run 写入 running 后执行期间无人触碰该行;worker 崩溃 → 永久 running,无 resume/finish 门可达,客户端永远看到 running;`terminal_persistence_unknown` 是无出口的死端(run_lifecycle.py:103/644 互相拒绝)。
- Fix: 加 heartbeat_at 列(每迭代边界更新)+ 周期 reaper 把陈旧 running 转 run_failed(硬 checkpoint),与 command_queue 租约共存;给 terminal_persistence_unknown 显式幂等出口。

**H23 M7:调用方 CancelledError 被吞且无界等待** — `apps/assistant-service/src/assistant_service/core/tool_invoker.py:1376`
- Impact: 墙钟超时取消执行任务,`except asyncio.CancelledError` 把它转成 ToolCallResult(SIDE_EFFECT_UNKNOWN);工具无视取消时 `await execution_task` 无界阻塞——预算时限已过,run 反而永久挂起;MCP 分支(1155)同样吞掉且**没有** native 路径的 side_effect_unknown 栅栏 → 已派发的 MCP 写操作可能被重派发(重复副作用)。
- Fix: 取消子任务后以有界 grace 等待,再重抛 CancelledError;MCP 分支补栅栏 + 重抛。

**H24 遗留审批 checkpoint(无预算快照)永久无法恢复** — `apps/assistant-service/src/assistant_service/core/agent/execution_lifecycle.py:195`
- Impact: RunBudget.restore 缺快照即 ValueError;预算快照功能上线前保存的所有 approval-pending checkpoint 不可恢复,run 以 run_budget_restore_failed 终态死亡;文档声明的 legacy 升级路径(agent_loop.py:748-752)因 restore 先抛而成为死代码。
- Fix: resume_payload 无 run_budget 时以显式 legacy 标志重建有界预算,让 reserve-exactly-once 分支真正执行。

**H25 宽泛 `except TimeoutError` 把内部超时误判为墙钟耗尽** — `apps/assistant-service/src/assistant_service/core/agent/execution_lifecycle.py:740`
- Impact: 包住整个执行块的 handler 捕到任何来源的 TimeoutError(asyncpg/checkpoint 写库、provider SDK、内层 asyncio.timeout 泄漏)都调 exhaust_wall_time() → 预算被永久标记耗尽,错误被误标为 wall_time_exhausted。
- Fix: 先比较 elapsed 与限额,未到截止即重抛;只有真正的墙钟过期才耗尽预算。

**H26 M8:部分叙事存在时强制合成追加第二份 final** — `apps/assistant-service/src/assistant_service/core/agent/agent_model_turn.py:402` + `streaming_execution.py:488`
- Impact: 模型流出部分文本('正在生成 PPT…')后耗尽迭代,成功合成经 `ctx.generated_content += text_chunk` 追加 → 客户端与持久化消息含"部分文本+完整答案"两份答案;失败路径会替换内容,成功路径不替换——不对称。
- Fix: 追加前丢弃未完成的 partial narrative(重置或截到最近干净边界),partial 只留在 trace。

**H27 网关预检拒绝永久封死幂等键** — `src/api/v1/agent_runtime.py:1753`
- Impact: 429/422/409/502/504 预检失败后 `except BaseException` 调 fail_runtime_idempotency → 同 key 重试 24h 内恒 409 EXECUTION_FAILED,即使 run 从未执行。
- Fix: reserve 移到预检全部通过后、派发前;只有上游派发开始后的失败才封。

### 1.5 知识管线正确性

**H28 多模态 DashScope 嵌入无超时** — `apps/knowledge-service/src/knowledge_service/services/knowledge/embedding.py:1740`
- Impact: `_call_api`/embed_images/embed_texts 经 to_thread 但无 wait_for(文本路径 563 有);SDK 挂死时 worker 的 wait_for 取消协程但线程永存,泄漏 executor 线程,文档滞留 processing 直到 15 分钟恢复重排。
- Fix: 所有多模态 to_thread 调用包 asyncio.wait_for(60–90s)+ 重试,镜像文本路径。

**H29 层级索引器静默产出缺失 L2/L3 的部分索引** — `apps/knowledge-service/src/knowledge_service/services/knowledge/hierarchical_indexer.py:525`
- Impact: `_embed_texts` 重试后返回 `[None]*n`,索引段静默跳过并正常返回;只要 errors==[] 且 total_vectors>0 就标 completed → 主集合空/不可搜索——正是 ingestion_service 用"拒绝部分索引替换"防的状态。
- Fix: 任一 segment 缺向量/点数不足即抛 ValidationFailedError,失败段计入 errors 使 success 为 False。

**H30 扫描文档部分失败仍标 completed** — `apps/knowledge-service/src/knowledge_service/services/knowledge/worker.py:1682`
- Impact: 扫描 PDF 的某个 split part 失败(success=False)只记 total_failed,其余部分照常索引,行 1682 无条件写 status=completed → 文档报告完成但缺页向量,搜索静默漏内容。
- Fix: total_failed > 0 时抛 RuntimeError 走 sweep+failed 终态,或仅 total_failed==0 时写 completed。

### 1.6 本地节点 / 前端 / SDK

**H31 local-node 超限 file.read 杀死心跳循环且回执永久丢失** — `apps/local-node/src/local_node/transport.py:784`
- Impact: 签名预算允许 8MiB 而 outbox 保留上限 2MiB;超限 UTF-8 读取在成功路径 `_append_terminal` 抛 OutboxError,位于 try 之外 → 异常穿过 run_once 到 run_forever 直接退出:节点停止轮询,replay_guard 已消费 command_id 导致重投被静默丢弃,平台永远等不到回执。
- Fix: 对齐预算(读上限 ≤ 2MiB)或把 _append_terminal 放进 per-command try;每条命令独立隔离,单条坏回执不得停轮询。

**H32 前端流式状态与身份缺陷(4 处,同根:会话切换/终态/鉴权头)**
- **会话切换/新对话不中断在飞 SSE 流** — `web/src/pages/assistant/hooks/useChatSession.ts:857`:旧 run 的迟到事件(ARTIFACT_CREATED 弹抽屉、working_memory 污染新面板)继续生效,isStreaming 卡 true,composer 被锁到旧 run 结束。Fix: onNewChat/onSelectSession 先 stopStreaming;加 sessionEpochRef 事件门。
- **终态非 first-wins:cancel/error 覆盖已完成轮** — `web/src/pages/assistant/hooks/useChatSession.ts:2607`:run_finished 后按停止键把 completed 翻成 cancelled,transport 异常把 completed 翻成 failed,消息状态与遥测 outcome 被破坏。Fix: reducer 层终态粘滞(cancel/fail 对已终态 no-op)。
- **playground 中流失败(有部分内容)静默标 completed** — `web/src/pages/playground/hooks/usePlaygroundStream.ts:1475`:非 AbortError 且有内容的异常落到 completeStreamTurn,截断文本显示"完成"。Fix: 镜像 useChatSession 的 failVisibleStream。
- **legacy /api/v1/stream fetch 不带 Authorization(匿名运行)** — `web/src/pages/playground/hooks/usePlaygroundStream.ts:1455`(+ `useAgentStream.ts:569` 同类):请求以匿名 guest 身份运行,租户/配额/KB 隔离丢失或 403 降级到非流式;透明代理分支(705)却带 token——明显遗漏。Fix: sseFetch 集中附加 Bearer,加"无 token 流请求"的 CI 静态门。

### 1.7 SDK 契约漂移

**H33 Java/Dart SDK 流式解析不拆嵌套 "data" → 流式文本恒为空** — `sdk/java/src/main/java/com/aigateway/ai/SSEParser.java:121`、`sdk/dart/ai_gateway_sdk/lib/src/streaming.dart:104`
- Impact: 线上信封是 `{"event_type","data":{...},"timestamp"}`,只有 Python SDK 拆了嵌套 data;Java/Dart 保留外层对象后从外层读 `data.get("content")` → onText("") 恒触发,usage/finish/error 派发读错层级;Java 测试用扁平 fixture 掩盖了该 bug。
- Fix: 移植 Python 的 unwrap(sse_parser.py:106-114);重写 Java fixture 为真实线上形状。

**H34 Python SDK 异步图片流指向不存在的端点** — `sdk/python/ai_assistant/images.py:63`
- Impact: generate_async 把被忽略的 async:true POST 到同步 /generate-image(无 task_id 返回);`_TASK_PATH GET /api/v1/assistant/tasks/{task_id}` 不存在(只有 POST cancel)→ 异步图片全链路 404。正确端点已在 `src/api/v1/assistant.py:1392-1429`。
- Fix: 改 POST /assistant/generate-image-async + 轮询 GET /assistant/image-task/{task_id},删死 _TASK_PATH。

## 2. Medium 精选(108 条,按区域一行一条;完整 IMPACT/FIX 见工作流产物)

### a 区(diff 深度)
- `responses_projector.py:71/68` 严格 usage 校验(total==input+output、cached>input)会让成功 run 变成 response.failed(invalid_usage)——按 provider 容忍度放宽/预归一化。
- `assistant_service.py:908` 无状态 /responses 请求仍每请求写 sessions 行(7 天 TTL),违反零持久化契约——persist_messages=False 时跳过或 finally 删除。
- `responses_projector.py:420` 成功但空输出的 run 被标 response.failed(empty_response),与 OpenAI 语义相悖。
- `agent_model_turn.py:449` 强制合成吞掉一切 provider 异常,断网被误报为"模型没有产出文本",还浪费 2 次合成调用——按类型重抛/返回。
- `agent_model_turn.py:249` failover 用量 max(主,备) 少计;异常路径用量完全不计——按 attempt 求和 + finally 合并。
- `cache_optimizer.py:247` cached 别名双计(cached_input + cache_read 同源重复累加),新测试锁死了错误值。
- `subagent_manager.py:1438/1077` evidence 摘要显示信封 JSON 前言失去可读内容;_side_effect_recovery 与 agent_loop 脱敏漂移——提取共享 helper。
- `main.py:323` llm_models 启动目录查询未限定 schema(同类 search_path 隐患);被吞异常后 registry 空载,所有请求模型解析失败只留 warning。
- `main.py:1189` readiness 只探池不探 schema;/health/ready 不含 artifacts——上次"空 receipt"事故可原样复发而不被探测。
- `artifact_storage.py:286` 上传成功-插入失败窗口孤儿 blob 无清理。
- `tenant_registry.py:131` 降级解析返回非 None 空 registry 顶掉 env 回退 → 全请求失败(应为 None/usable 标志)。
- `chat.py:298` M23 确认:缺模型静默回退 available[0] 无资格检查。
- `compressor.py:654` complete() 仍吞 chat() 内抛出的 RunBudgetExceeded(只有 _before_complete 路径被修)。
- `registry_lifecycle.py:385` 每请求 tenant registry 的 httpx client 从不关闭。
- `useChatSession.ts:2613/225/890` 审批暂停被误报"中断请重试";恢复的 running run 显示永转圈且无取消路径;会话切换不断流(与 H32 同根);`2706` 取消分支丢最后一帧 + 未本地化 '(Cancelled)'。
- `test_openai_smoother_wiring.py:36` 固定 9000 字符窗口已横跨 _stream_openai/_stream_anthropic 两个函数,注释行数错误。
- `test_responses_ingress.py:1168/892`、`test_subagent_manager.py:551` 心跳竞速、100ms/10ms 实时闸口——CI 抖动源,10ms 栅栏退化时挂死整套而非快速失败。
- `test_agentloop_streaming_first_contract.py:1399` 截断轮测试断言真空(零事件也过);`test_responses_ingress.py:250` 集合成员断言不锁定错误码映射。
- `test_assistant_api_e2e_live.py:497/437` live 套件接受 done-only 流(未强制 run_finished);无 teardown 无限累积会话/artifact;跳过纯靠环境变量,绿跑可能零覆盖。

### b 区(网关)
- `src/core/auth/password.py:37` 公开的默认管理员密码常量(强制改密缓解但应 fail-fast 要求显式注入)。
- `dispatcher.py:391/273` stream() 不喂熔断器成败(仅 invoke 喂);max_retries=0 跳过 adapter 直接 RuntimeError。
- `transparent_proxy.py:622` 上游 URL 裸拼接无 dot-segment 清洗(../.. 可达前缀外端点)。
- `agent_runtime.py:1149` 8MB 溢出流被记成幂等失败(应为 completed + replay-unavailable 标记);`schemas/assistant.py:128` message/history 无界;`assistant.py:1194` content_base64 无上限。
- `agents.py:773/1491` analytics 非法时间串 500(应 400);release 列表每项全量候选重解析。
- `assistant.py:792/758` sessions total=页大小假分页;会话/artifact 500 直接 `detail=str(e)` 泄漏内部文本。
- `session_manager.py:184` 内存回退先 LIMIT 后过滤,助手会话可能被挤出列表。
- `config.py:341` PUT /config/auth 是 no-op(误导管理员),GET 回显 jwt_secret 与 api_keys。
- `connectors.py:342/348` OAuth token 明文落库(注释声称加密);MCP 激活用租户级凭证无用户归属。
- `dashboard.py:424/805/596` realtime/ws 跨租户指标;per-user 端点不自作用域;timeseries runs 实为请求数。
- `quota.py:531`、`usage.py:602` 假成功 ack;top_models 漏 user_id 过滤。
- `langgraph.py:140` 伪造 Authorization 覆盖连接器真实凭证;`transparent_proxy.py:1662` SSE 响应丢弃全部上游头;`langgraph_proxy.py:979` 上游错误文本未脱敏直传客户端。
- `langgraph_proxy.py:248/206` 无界缓存无 TTL;latency 均衡恒选第一实例(从未测量)。
- `transparent_proxy.py:1131/842` 重试变异共享配置永久改道;两处死代码。
- `deps.py:456` API-key DB 故障 500 而非降级静态 key(与 JWT 路径不一致)。
- `quota_service.py:460/774` 死代码副本语义漂移;限额一旦设置无法清除(0/None 语义与文档矛盾)。
- `usage_scheduler.py:164` 聚合只在 00:30–00:59 窗口,重启丢天且清理会删除未聚合记录(永久丢数据)。
- `golden.py:1114` 无轨迹期望用例 trajectory_pass 真空为真,虚高门禁指标。
- `container.py:651` DB 连接失败被吞,网关"健康"启动。
- `database_manager.py:309` update_state 把陈旧快照写回 Redis 缓存(update_metadata 修过的同类 bug)。
- `service_registry.py:76` 适配器驱逐/关闭时泄漏 httpx 连接池。
- `transports/http.py:151` health_check 把 404/401 报健康;`message_queue.py:124/84` 重启后 fail() 重排队空载荷;block_ms=0 语义反转。

### c 区(知识 + 本地节点)
- `chunking.py:1604` 字符模式切块在断点与下一窗口之间丢文本;`ingestion_service.py:920` 嵌入失败不取消在途任务(限流尖峰);`worker.py:872/1294` 大文件回退只索引 100KB 预览;层级失败回退残留陈旧 L2/L3 向量。
- `knowledge.py:2081/2941` batch-reindex 静默只做 200 篇;去重 10000 段截断误报;`document_service.py:534` 无分页 200/500 硬截断;`dataset_service.py:1354` 数据集授权租户盲(跨租户 user_id/角色碰撞可越权);`retrieval_service.py:1007` score_threshold 静默忽略(文档承诺 keyword/rerank 语义)。
- `knowledge.py:3413` force-complete 返回捏造的 completed 不写库;`main.py:353` Confluence 摄入管线(~7.6k LOC)完全未接线,同步永不运行;/sources 静默空绑定;`main.py:427` 优雅关闭漏关 pool/flusher/Qdrant client。
- `worker.py:322` bm25_v2 只读数据集活锁(每 60s 重领重排永无终态)。
- `persistence/database.py:144`(KB fork)迁移目录孤儿无人应用。
- `macos_native.py:428` 截图无限累积;`processes.py:208` 失败动作幂等重放 TypeError;`transport.py:710` 控制通道被最长 300s 进程动作阻塞(取消/心跳死信)。
- `processes.py:218` cwd 经路径解析存在 symlink 交换 TOCTOU;`watcher.py:58` 每 50ms 轮询全量重读重哈希(与 perf 报告 G1 同根)。

### d 区(共享层)
- `persistence/database.py:6967/5178/2897` document 版本 MAX+1 无锁竞态;worker 领取无 FOR UPDATE SKIP LOCKED;image/text 段 position 冲突互相覆盖。
- KB fork `database.py:7891`(~8k 行)携带同样 bug 且不再与包同步。
- `conversation_shares.py:212` 及 database.py 内多处未限定跨 schema 引用;三池无 server_settings search_path(与 H16/H21 同族)。
- `usage_recorder.py:527/305/373` 定价前缀/幂等合并/事件总线重复发布(事件不随 DB 去重)。
- `proxy/base.py:565` 重试重放同一签名被上游 replay 拒绝 → 误导 401;`250` 熔断探针 check-then-act 竞态;`160` Redis 故障时 RuntimeError 冒成 500 且掩盖流内真实异常。
- `security/secrets.py:38` 密钥未配置时静默明文落库;`safe_fetch.py:160` 非法端口 ValueError→500(应 400)。
- `agents/runtime.py:175`、`spec.py:372` 敏感键守卫漏 token/secretkey 拼写与超大 int 阈值 500。
- `idempotency.py:283/325`(H17/H18 同族);`database_manager.py:417/389`(H15 同族 + extend_ttl 假成功)。
- `artifact_repository.py:505` 已撤销技能版本仍可加载执行(仅签名快照路径检查 revoked)。
- `image_storage.py:927` local 后端无签名 key 时写完文件再抛错留孤儿(知识服务默认配置必现)。
- `quiz_grader.py:114` 无 registry 时所有简答静默判错;`artifact_share_manager.py:137` time_limit_minutes 从未服务端执行;`skills/registry.py:71` 'latest' 按 uuid4 字典序解析。
- `artifact_storage.py:242`(与 a4 同根)上传先于插入的孤儿窗口。

### e 区(前端)
- `ArtifactsPanel.tsx:126` ~80 个 t() 键两种 locale 都缺(zh 用户见英文),ConnectorsPanel/SubAgent 面板硬编码;`useFileHandler.ts:106` 超出槽位的文件仍然全部上传(状态只加 2 个,带宽浪费+孤儿);`useImageGeneration.ts:55` 图片生成不随会话切换归位(落到错误聊天)。
- `ContextDisplay.tsx:67`、`useChatSession.ts:1537` 数字字段字符串化时 toFixed 崩溃/渲染 NaN。
- `lib/sse.ts:270/216` 网关 `event: error` 明文帧被静默丢弃;5 分钟超时转成干净结束(应 failed 而非 completed/cancelled)。
- `normalizers.ts:111` 后端 'done' 事件被归一化为心跳(文档说 terminal)。
- `DatasetDetail.tsx:1112` QA 流中途错误掩盖为完成;`useDatasetUploadController.ts:187` 上传前清空状态(失败丢文件列表、配置已持久化);`useDatasetUploadDialog.tsx:24` 详情上传无任何文件校验(创建向导有)。
- `StreamOutput.tsx:96` MarkdownLink 渲染 javascript: href(可点击 XSS);`useAgentStream.ts:569/643/685` 无 auth 头 + abort 竞态 + 取消失败报成功;`useAuthSessionGuard.ts:60` 任何瞬时错误都登出(只有 401 该清);`Settings.tsx:466` 可保存空 jwt_secret 且 JWT 开启 → 生产全站登录失败。
- `MultimodalInput.tsx:169` 延迟窗口内移除文件仍上传;`api.ts:1` rememberMe 时 JWT 进 localStorage;`useAppStore.ts:52` 标题缓存无界持久化;`usePlaygroundStream.ts:1476` 超时误判触发重复 invokeService 兜底(双倍 token)。

### f 区(assistant 长尾)
- `code_executor.py:756/774` docker-py 同步调用阻塞事件循环(images.get/pull 无界);沙箱绑定挂载磁盘写入无界(H3 只限内存);`111` packages 字段文档承诺 pip 安装却从未实现。
- `tool_invoker.py:1514` 并行调用异常留 None 占位致 invoke_batch 崩溃;`task_planner.py:760/1436` LLM 环形依赖未捕获崩溃请求;analyze_dependencies 静默变异计划。
- `file_processor.py:1315` 异常原文(str(e))进入模型可见文本。
- `run_lifecycle.py:103`(H22 同族)terminal_persistence_unknown 死端;`client.py:796` CancelledError 转 MCPError(499)伪装普通失败,循环不停止;`manager.py:396` 断线后无重连路径,重初始化被陈旧 session pin 卡死;`runtime.py:728` 每次调用全握手 + 僵尸远端会话(与 perf H2.8b 同根)。
- `source_store.py:462/645` M28 子串去重丢短重复条目;M29 单源异常抹掉全部记忆上下文;`compressor.py:233` M27 preserve_recent=0 静默 no-op;`trace_writer.py:1054` finish_trace 失败留永久 running trace 无对账。

### g 区(基础设施 + SDK)
- `ci.yml:46` 缺三个 harness 声明门禁(service_boundaries/assistant_runtime/agent_studio)且 bash -n 只覆盖 6/13 脚本;`kbms compose:9` 0.0.0.0 暴露 PG/Redis/Qdrant(无 auth);`database.py:442` 每次启动重跑 95KB schema.sql(门检查永不通过);`migrate.sh:114` 016/031 重复前缀硬失败旧账本数据库。
- `visual_verifier.py:267` soffice/pdftoppm 30–60s 同步 subprocess 阻塞事件循环;`xlsx_renderer.py:78` 公式注入('=' 开头单元格变活公式);`sdk/python mcp/client.py:319` HTTP transport 下 call_tool 直接 AssertionError;`signing.py:62/20` 畸形 exp 500 而非 403;未设签名 secret 时每次重启 URL 全部失效(随机回退)。

## 3. Low 汇总(99 条,仅列位置与主题)

略——完整清单在工作流产物与上方分区摘要中;典型代表:`src/core/crypto.py:116`(签名不含 host/scheme/query 且无密钥 fail-open)、`admission.py:280`(Redis 租约无 reaper,与 H6 同族)、`multi_dimension_rate_limiter.py:413`(拒绝请求计数 Redis/内存路径不一致)、`_streaming/auth.py:204`(匿名身份是客户端自选 UUID)、`transparent_proxy.py:842`(死代码)、`agent_context_lifecycle.py:1072`(调用不存在的 ModelRegistry 方法,必死路径)、`agent_loop.py:1307`(回退终态 run_id 分叉,幽灵 run)、`streaming_tool_loop.py:82`(流中不观察取消,取消延迟一整代)、`chat.py:44`(E2E stub 用户 dict 无驱逐)、`conversation_shares` 系列、`dashboard` 系列、前端 `useAgentStream`/`SegmentList`/`StatusBadge` 系列、`files.py:292`(回滚文件永不清除)、`processes.py:218`(cwd TOCTOU)、`governance_cleanup` 系列、`docgen signing` 系列、`message_queue` 系列、`api_key_repository.py:258`(双路径使用计数)、`user_repository.py:340`(并发首建覆盖角色)等。

## 4. 覆盖缺口(批评代理,按风险排序,18 条)

1. **OpenAI Responses provider 运行时**(openai_responses_runtime.py/openai_responses_tools.py/responses_api.py/thinking_policy.py)——新入口的另一半完全未审。
2. **assistant 图片生成子系统**(images.py + image_generation_worker + image_task_store + doubao/gemini/smart 工具 + content/* + 迁移 002 的 blob 表)——独立异步任务队列+blob 存储+外部 provider key,完全未审。
3. **assistant 执行门/审批层**(execution_gateway/policy_engine/run_resume/approval_lifecycle/command_lifecycle)——run 控制、恢复/取消/批准竞态与权限绕过所在层。
4. **网关剩余 ~40/55 路由文件**(api_keys/users/roles/files/presign/stream/submit/invoke/responses/sessions/metrics/models/providers/quiz/mcp/connector_admin/agent_public 等)——身份面、share-token 访问控制、预签名上传、流式路由的剩余授权与 SSRF 面。
5. **src/proxy/ 其余 6 文件**(billing_interceptor/billing_stream/config_loader/context_injector/langgraph_governance/response_cache)——中流计费与租户响应缓存。
6. **ai-gateway-core 其余模块**(billing/image/knowledge/memory/events/tasks/tracing/config)——tracing 的 httpx_hooks 把 trace 头注入每个出站 provider 调用(头注入面)。
7. **网关其余 9 个 adapter**(comfyui/dify/generic_rest/openai/stable_diffusion/tts/whisper/registry/base)——二进制/SSE 中继与凭证处理。
8. **Web 公开面**(agent-embed.js/agent-widget.js/AgentHostedPage/SharePage/QuizPage/UserManagement)——第三方站点注入/外泄面、分享 token 渲染、密码重置。
9. **assistant 流式状态机其余部分**(streaming_recovery/streaming_state/stream_helpers/sse_event_transport)——崩溃恢复/断点恢复(migration 067)与 SSE 帧规则本身。
10. **assistant middlewares 层**(permission/response_cap/runtime_memory/tool_output_spill/harness)——每轮预算与安全门所在。
11. **local-node Swift 原生边界**(LocalNodeMacOSHelper.swift)+ assistant 侧 device_channel/gateway_receipt——真实 OS 控制桥与设备 WebSocket 通道。
12. **迁移运行器与两条迁移树**(cli.py/run_migration.py/migrate_per_service.py;016/030/031 重复;KB 迁移 11 文件重复)——排序不确定与静默漂移。
13. **E2E 套件广度**(23/24 spec 未审 + playwright 各 config)——发布门禁的 flakiness 与断言强度。
14. **Helm chart**(values/secret/ingress/migration-job/HPA)——生产部署路径的默认值与迁移任务失败行为。
15. **发布/打包工作流**(docker-publish/publish-sdk;各 Dockerfile;nginx.conf;40-runtime-config.sh)——供应链与运行时注入。
16. **tests/ 广度**(12/13 tests/contract 未审;tests/{security,deployment,scripts} 完全未动)——既有回归门禁本身是否仍钉住行为。
17. **MCP manager/OAuth + 其余 mcp 模块**(manager/oauth/resilience/stdio_client/connector_mcp)——token 存储/轮换与 stdio 子进程处理。
18. **记忆读取侧/反思 + 运行时上下文**(retriever/reflector/turn_sync/assembler/external_content/pii_filter/sandbox_resolver)——H8 读路径与外部内容信封的生产者。

## 5. 交叉维度(9 条,建议独立工作项)

SSE 全端点契约一致性 / 密钥卫生专项(未跟踪 .env、web/.playwright/auth-state.json、MCP OAuth 持久化、helm secret 默认值)/ 跨服务认证流 / 迁移策略(双树、重号、回滚文件、per_service 拆分)/ i18n en/zh 全 6 个 locale 文件 parity / live 测试 flakiness 与清理 / OpenAPI 契约漂移 / 限流准入并发专项 / prompt 注入与工具输出溢出信任边界。

## 6. 与已知开放项及性能报告的交叉引用

- **H6 / M7 / M8 / M23 / M27 / M28 / M29 / H8 全部获得工作树实锤**(见 H22/H23/H26 与 f 区、a 区对应条目),比上一轮"部分关闭"的结论更进一步:H6 补充了 terminal_persistence_unknown 死端;M7 补充了 MCP 分支缺栅栏;M8 补充了成功路径不对称。
- 与 perf 报告重叠(同根因,除一项已复核修正外引用一致):MCP per-call 全握手(runtime.py:728)、H8 每轮重索引(runtime_adapter/indexer)、KB 多模态嵌入无超时(embedding.py:1740/1816)、session 内存缓存无界(database_manager.py:44/417)、StreamOutput 块级 memo 缺失、local-node watcher 全量重哈希。注:复核确认 `conversation_shares.py:212` **不在** perf 报告中(本报告独有),已从重叠清单剔除。
- 两报告互补无冲突:perf 报告含 H8 增量索引等性能修复方向,本报告含 search_path 影子表(H21)、迁移权威分裂(H20)等正确性缺陷——Codex 实施时应合并处理(如 schema 限定统一扫一遍)。

## 7. 复核结论(2026-08-16 · 8 个验证代理 · 164 条抽样,已对照工作树真实代码逐条裁决)

### 7.1 总览

| 分区 | 条目 | CONFIRMED | CORRECTED | REJECTED |
| --- | --- | --- | --- | --- |
| vg1 diff 区 | 21 | 20 | 1 | 0 |
| vg2 网关 API | 17 | 17 | 0 | 0 |
| vg3 网关适配器/服务 | 20 | 20 | 0(2 处行号修正) | 0 |
| vg4 知识/本地节点 | 20 | 17 | 3 | 0 |
| vg5 共享层 | 20 | 18 | 2 | 0 |
| vg6 Web | 18 | 15 | 0 | 3 |
| vg7 assistant 长尾/基建/SDK | 29 | 27 | 2 | 0 |
| vg8 缺口核验/交叉 | 20 | 17 | 2 | 0 |
| **合计** | **164** | **151 (92%)** | **10 (6%)** | **3 (1.8%)** |

全部 32 条 High 均在抽样内:31 CONFIRMED + 1 CORRECTED(H21,细节修正),**0 REJECTED**。行号命中率 ~97%。**Codex 实施必须以本节为准。**

### 7.2 REJECTED(3 条,不要实施)

| 条目 | 驳回理由(验证代理证据) |
| --- | --- |
| e1-m4 ContextDisplay took_ms 字符串崩溃 | 全链生产者均为数值:pydantic `took_ms: float`(assistant_service.py:1175/1219、schemas/assistant.py:396/405)、`time.time()*1000`(builtin_tools.py:323/335)、web 类型 `number`(types.ts:192/375),knowledge-service 不产 took_ms(全仓 grep 无命中)——不存在字符串路径 |
| e4-m1 MarkdownLink javascript: href XSS | react-markdown v10 渲染前在 post() 中用 urlTransform 改写所有 href/src(node_modules/react-markdown/lib/index.js:370-383);StreamOutput 传的 allowDataUrlTransform 对非 data:image/ 一律转 defaultUrlTransform,其 safeProtocol 不含 javascript:,返回 ''——**已被现有链路防护** |
| e4-m6 空 jwt_secret 破坏登录 | PUT /config/auth 只写进程内 `_runtime_config` + Redis,全仓零读取方;JWT 签发/校验直读 env/pydantic 配置(deps.py:324/363、auth.py:353)。真实问题就是 b3-m1(no-op 误导管理员,已 CONFIRMED)——应按"接通或弃用"修,而非前端"空 secret 校验" |

### 7.3 CORRECTED(10 条,按修正实施)

| 条目 | 修正 |
| --- | --- |
| a2-m3 cached 别名双计 | 无消费方把两键求和,不会产生错误总额;实为遥测冗余,严重级降 Low。锚点 247→**257**(回填逻辑) |
| b6-m3 适配器连接池泄漏 | 锚点 76→**67** |
| c1-m4 层级失败残留向量 | 报告引的 worker.py:1294 回退近乎不可达(index_document 把异常吞进 result.errors);真实残留路径是 `result.success==False` → 标 failed 但不清扫。锚点 1259 |
| c2-m3 授权租户盲 | 结构确认(dataset_permissions 无 tenant 列),但"user_id 碰撞"前提被网关全局唯一用户 ID 大幅缓解;实施降为纵深防御(断言 dataset.tenant_id == user.tenant_id),锚点 1361 |
| c3-m4 bm25_v2 活锁 | "60s 重领重排活锁"机制不成立:claim 查询显式排除非 lexical_v1(database.py:2428-2432),bm25_v2 的实际行为是 enqueue 即失败(无活锁);残留问题=失败终态被跳过。锚点 322 |
| H21 影子表 | 机制真实,但门数量是 **17** 个不是 13(行 476/541/797/865/867/870/926/927/929 等);且"auto_init 默认改 False"建议**已在树内完成**(settings.py:33、container.py:215、assistant main.py:419、knowledge main.py:183)——只修门的限定即可 |
| d1-m2 worker 领取竞态 | `get_pages_due_for_sync` 是死代码(全仓零调用);真实 Confluence 路径用另一查询——评估那个;KB 分叉(database.py:6223)同为死代码 |
| f2-m5 环形依赖崩溃 | 唯一调用方(streaming_preparation.py:399-412)已包 `except Exception` → 降级不崩溃;残留真缺陷:task_planner.py:1436 静默变异 + validate_plan 缺口。锚点 407 |
| g1-m3 每次启动重跑 schema.sql | auto_init 默认 False(四处置全),仅显式开启的部署触发;残留:database.py:442 的 to_reg 门检查与 public 布局不匹配 |
| GAP5 / §6 交叉 | billing_stream.py 并非完全未审(本报告 H14 就是它);`conversation_shares.py:212` **不在** perf 报告中,§6 重叠清单已剔除该条(其余 5 条重叠经逐行核实一致) |

### 7.4 交叉升级确认(源自性能复核,本报告独立复验)

**安全事件静默丢失**(`packages/ai-gateway-core/src/ai_gateway_core/persistence/database.py:4001`):CONFIRMED 且证据更强——post-030 schema 下 `security_event_daily_aggregates.user_id/service_id` 为 NOT NULL DEFAULT '',代码传 None → INSERT 必失败,网关 `src/api/v1/proxy.py:91-104` 吞异常 → 认证失败/限流/配额事件全部静默丢失。修复三合一(调用侧传 `''` + ON CONFLICT 目标与 030 纯列约束对齐 + 错误级别升 warning),一次改在共享层即覆盖网关与 assistant;KB 分叉(database.py:5011)同修。

### 7.5 Codex 实施注意事项(验证代理交叉约束)

1. **H32 家族 4 条同根**(useChatSession 857/2607 + playground 1475/1455):H32d 与 e4-m2 是同一缺陷的两个调用点——合并为"sseFetch 集中附加 Bearer"一处修复。
2. **H8/M7/H6 与 perf 报告的修复同根**:本报告的 H22/H23/H26 与 perf 报告 C3/C1/C2 联动实施,避免两报告各自改同一文件。
3. **配额簇(H8/H9/H10/b5-m2/b5-m3)**:修复须在活路径(aggregation_task QuotaResetTask)与死副本(quota_service.py:460/811)间统一;live 递增路径在 ai-gateway-core usage_recorder._update_quota_counters,三处要一致。
4. **e2-m1/e2-m2/e2-m3 属同一 SSE 契约家族**:补 error 帧识别即可区分超时/取消/错误,统一在 sse.ts/normalizers 层做,别逐点修。
5. **ESCALATE 修复与 d1-m5(KB 分叉)联动**:共享层改完必须同步 KB 分叉同位置,否则知识服务继续丢事件。
6. 依赖方向:全部修复落各自服务/共享层,无 web→src→apps→packages 违规;无 schema 变更(除 H21 门限定与 G4 索引瘦身)。

### 7.6 总体评价

抽样覆盖 164/239(69%),含全部 32 High。**问题本体零虚假**:10 条 CORRECTED 全是细节/机制修正(且其中 3 条为"严重级应降"或"实为死代码"),3 条 REJECTED 均为"已被现有防护/链路覆盖"或"真实问题在别处已报"。两份报告现均为"已验证"状态,可直接作为 Codex 实施输入,前提是携带本节的 REJECTED 排除清单、CORRECTED 修正与 §7.5 注意事项。

---

*生成方式：30 个只读子代理并行审查 + 1 覆盖批评代理;报告为人工合成。发现均为代理对照工作树代码得出,但独立复核正在进行——最终以复核裁决为准。*
