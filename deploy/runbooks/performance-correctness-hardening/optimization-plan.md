# 详细优化计划

> 执行状态（2026-08-16）：计划已推进到 PCH-06 集成检查点。组件、Python 全量回归、
> Web 静态/定向 E2E、容器和关键真实能力门禁已通过；思考保持 low，首个 reasoning
> p50=3.146 秒，但文本 TTFT p50=3.925 秒仍未达标。复杂任务完整轮原始 5/8，修复一个
> 评测误报后对同一实机输出语义回放为 6/8、0 基础设施错误；Security/Research 仍是真失败，
> 且尚无三轮完整 cohort 的稳定性证据，因此当前发布判断仍为 NO-GO。详细证据见
> `reports/performance/performance-correctness-hardening-2026-08-16.md`。

## 1. 输入裁决

两份报告合计 388 个静态发现，但必须先按复核章节裁决：

- 性能报告：70 条已抽样复核，58 CONFIRMED、10 CORRECTED、1 REJECTED。
- 全局报告：164 条已抽样复核，151 CONFIRMED、10 CORRECTED、3 REJECTED；
  32 条 High 中 31 CONFIRMED、1 CORRECTED、0 REJECTED。
- 同根问题只建立一个实施项，例如 H8 记忆读取/短路/增量索引、H6 lease/reaper、
  M7 cancellation、Web SSE terminal、usage 计量。
- 性能报告中的估算不能作为验收数字，必须在当前代码上记录前后基线。

## 2. 风险排序

优先级按实际损害排序，而不是报告数量：

1. 越权、IDOR、SSRF、配额自提、跨租户污染。
2. 错账、永久幂等键、永久 running、重复副作用、部分索引伪完成。
3. 每请求或每模型轮的 DB/Redis/文件/序列化放大。
4. 浏览器每 token 渲染、首屏包体、SDK 合同漂移。
5. 仅在大规模或特定部署触发的结构优化。

## 3. 分阶段实施

### PCH-00：基线和台账

- 保存当前 Git 基线、脏工作树边界和容器所有权。
- 记录简单问题 TTFT/input/context、8 场景复杂基准、Web 构建 chunk、组件测试。
- 将每条 High/Critical 标记为 open、already-fixed、confirmed、corrected、rejected 或 blocked。
- 对即将修复的项先添加失败测试或可观察计数器。

退出条件：基线可复跑，REJECTED 不进入代码，上一轮已修项不重复修改。

### PCH-01：安全和状态正确性

- Artifact legacy NULL owner_scope 必须回退 tenant_id+user_id，不能 fail-open。
- create_artifact 必须校验 session 的 tenant/user 归属。
- 子代理 prompt、parent_context 和 started event 统一角色中和与脱敏。
- 无 Redis session cache 增加 TTL/LRU；写后不得回填陈旧快照。
- IdempotencyMiddleware 在 CancelledError 时 abort，http.disconnect 不再死循环。
- 关闭 H6/H24/H25/M7/M8：lease/reaper、legacy approval resume、真实 wall-time
  分类、有限取消 grace、partial narrative 与 forced synthesis 单终答。
- Web 会话切换中止旧流并用 epoch 丢弃迟到事件；terminal first-wins。

退出条件：安全/状态定向测试通过，取消、重试、跨用户访问和恢复均有负向用例。

### PCH-02：Assistant 热路径

- H8 联动设计：mtime/size 新鲜度门、同步读取转线程、chunk-config 指纹短路、
  append-only 日记水位增量索引；保留 symlink/O_NOFOLLOW 防护。
- Trace 按 run 批写并保留 resume_sequence drain barrier；finish 强制 flush。
- ContextPacket 复用 token/digest；checkpoint message digest 每次只计算一次。
- KB 工具 SSE 元数据压缩；大结果只传 receipt，不重复完整文本。
- Code Executor 限制 Docker 日志并移除模型不可见的 base64；先测冷启动再决定热池。
- MCP DNS 解析不阻塞事件循环，连接复用必须有 TTL、失效和关闭路径。

验收指标：

- 未变化 memory source 不调用 chunk_markdown/embedder，事件循环无同步文件读。
- 同一 run 的 trace DB round trip 从事件数线性增长降为有界批次。
- checkpoint digest 调用计数减半且恢复 hash 不变。
- 简单对话 input/context 不回退，TTFT p50 不劣化超过 10%。

### PCH-03：Gateway 计费和入口放大

- 统一日/月配额边界、请求数重置和惰性 reset，活路径与死副本语义一致。
- Idempotent usage 以 GREATEST 合并；模型价格只允许 requested.startswith(cached)。
- LangGraph 累计 usage 取单调 max；多模型调用在最终计费层按调用求和。
- usage flush 多行 upsert，日期查询改半开区间，保留既有清理任务。
- Admission Redis 原子 Lua；重复 rate-limit 只保留单一权威校验。
- quota cache/RPM 多 worker 方案与 Redis 后端门禁同批落地。

退出条件：跨日/月、部分+最终、累计 SSE、多 worker 和 SQL query-shape 测试通过；
用假 Redis/PG 计数证明 RTT 上界。

### PCH-04：Web 和 SDK

- ChatMessage memo；thinking/subagent delta 统一 RAF 批处理。
- StreamOutput 对已完成 Markdown block memo，只重解析尾块；长回复保留 fenced code/math 语义。
- 移除失效 manualChunks，locale 按当前语言异步加载；建立 chunk budget。
- Playground/legacy stream 统一 Authorization、error/timeout/cancel terminal。
- Java/Dart SSE 解包真实 data 信封；Python async image 使用真实端点。

验收指标：

- 20k 字符、200 历史消息的流式浏览器场景无每 token 全列表渲染。
- run_finished 后 cancel/error 不得覆盖完成态；切换会话后旧事件零可见副作用。
- login/public 初载预加载 gzip 明显低于当前基线；CI 对超预算失败。
- 三种 SDK 使用同一线上 SSE fixture。

### PCH-05：Knowledge、Local Node 和基础设施

- 多模态嵌入先实测 DashScope 批返回形状，再做有界并发、timeout 和 retry。
- 层级/扫描部分失败必须 failed，禁止缺向量却 completed。
- 检索 N+1 批量查询、BM25 候选收窄、Qdrant collection cache 带写失效。
- Local Node 单命令异常不退出心跳；watcher 使用 mtime/size 快速路径。
- 迁移入口统一另开设计审查；先修限定 schema 门和迁移互斥，不直接重写生产账本。

退出条件：失败注入、部分索引、10k 文件 watcher、批检索和迁移 fresh-install 门禁通过。

### PCH-06：整体验收

- 运行 make harness-check、make validate、make status。
- 运行 Assistant/shared core、Gateway、Knowledge、Web type/lint/build/i18n/E2E。
- 当前 Compose 热更新后复跑真实 provider、KB、Code Executor、Docgen、Quiz、Image、memory。
- 简单 TTFT 至少 10 trials；复杂 8 场景至少 3 trials 或明确成本阻塞。
- 执行并发/慢客户端/取消/重启故障测试。

发布判断阈值：

- 组件门禁零失败；所有 skip 单列。
- 复杂场景至少 6/8 且零基础设施错误，低于此值不得称成熟。
- 简单 TTFT p50 不高于当前约 3.1 秒基线的 110%；同时报告 provider 与本地分段。
- 所有已宣称接入必须有真实 result receipt。

当前检查点实况：简单 TTFT 已完成 10 trials；复杂任务完成一个最新完整 cohort，加上
finance/engineering、governance、unknown-effect 和 research medium 的隔离复测，但未完成三次
完整 cohort。Web 全量 Playwright 因缺少 `E2E_API_URL`/登录环境出现 29 个环境型失败，直接覆盖
本轮改动的定向用例通过。两项都必须按未完整验证记录，不能算 release pass。

### PCH-07：用户停止与工具交换闭合（2026-08-17）

问题不是“停止按钮是否变灰”，而是停止发生在模型已经产生 `tool_use`、运行时尚未追加
`tool_result` 的窗口时，模型 transcript、SSE 生命周期和持久化检查点必须同时闭合。否则下一次
Anthropic/OpenAI 兼容请求会携带孤立工具调用并被 provider 以 HTTP 400 拒绝。

核心不变量：

1. 每个已接受的 assistant tool-call ID，在任何后续模型边界或非暂停 terminal checkpoint 前，
   必须恰好存在一个同 ID 的 tool-result；并行批次必须全量闭合，不能只闭合当前项。
2. 用户停止先走 owner-checked task cancel API，让服务器有机会产生规范的 cancelled result；SSE
   保留一个有界 grace 窗口接收 terminal 事件，超时后才 abort transport。
3. 已知未执行/安全取消使用 synthetic cancelled result；dispatch 后副作用未知继续走
   `side_effect_unknown` 阻断恢复，禁止伪造成 cancelled 或 success。
4. synthetic result 必须同时进入模型消息、turn tool metadata、公开 SSE 和取消 checkpoint；修复逻辑
   必须幂等，重复清理不能产生第二个 result。
5. provider request builder 在网络请求前验证工具调用/结果配对；残缺 transcript 本地失败，不再把
   可预防的 400 交给外部 API。

实施顺序：

- Web：记录 `run_started.task_id`；停止时立即标记用户意图、调用 `/assistant/tasks/{id}/cancel`，
  继续消费闭合事件，并在 2.5 秒后 abort 兜底。停止早于 task ID 时，在收到 `run_started` 后补发取消。
- Runtime：对当前及同批尚未执行的工具追加 bounded synthetic result，更新 `ctx.messages` 和 turn
  records，保存 `tool_call_cancelled` checkpoint，再发 cancelled terminal。
- Provider boundary：Anthropic 与 OpenAI-compatible body 在发送前拒绝 orphan、duplicate、missing
  tool result，错误中只含安全的 call ID 摘要。
- UI projection：cancelled tool 不得显示为 completed；run terminal 保持 first-wins，停止后仍可立即
  发送下一轮消息。

分层验收：

- 单元：单工具、并行两工具、停止前/执行中/结果后竞态、重复清理、unknown-side-effect 分支。
- Provider contract：完整 pair 可构造请求；missing/orphan/duplicate 在本地抛错且不发网络请求。
- Web：`run_started` 后停止必须先命中 cancel API；grace 内保留 tool-result/run-error；无 task ID 时
  只走有界 transport fallback。
- Docker/内置浏览器：启动真实长工具任务，在活动面板出现 tool start 后停止；确认 cancelled result
  与 terminal 可见、服务无 provider 400，并在同一会话发送后续工具请求成功。

性能阈值：停止点击后 UI 状态立即切换；本地可取消工具的 canonical terminal 目标不超过 3 秒；
正常成功路径不得增加 provider 调用次数，非停止请求仅增加 O(n) 的小批次配对校验。

## 4. 回滚与边界

- 每个 phase 单独可回滚；不混合 schema、前端和 AgentLoop 大改。
- 当前工作树包含用户和上一轮未提交修改，逐文件检查 diff 后再编辑。
- 当前本地 migration 或容器开关必须在报告中标出，并在测试后恢复安全默认。
- 不 commit、不 push、不 deploy，除非用户另行授权。
