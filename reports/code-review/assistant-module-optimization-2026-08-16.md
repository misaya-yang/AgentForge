# AI Assistant 优化与实机能力验收

- 日期：2026-08-16
- Git 基线：main@a49eac32d26ef5d19ed75aa1a7a7941f0ab281fc
- 范围：Assistant runtime、Gateway Responses API、工具/MCP/插件、持久化、Web chat 与真实容器能力
- 运行态：当前 checkout 所属 Docker Compose
- 结论：本轮关闭了多项可达的正确性、性能和真实接入缺陷；最终完整轮经评测误报校正为 6/8，但文本 TTFT 与跨轮稳定性仍不达发布标准
- Git 边界：未 commit、未 push；用户已有 .claude/launch.json 未触碰

> 最新集成结果以
> [performance-correctness-hardening-2026-08-16.md](../performance/performance-correctness-hardening-2026-08-16.md)
> 为准。本文后续保留较早迭代的诊断证据；其 4/8、旧 TTFT 和 Code Executor disabled 状态
> 不代表最终运行态。

最终更新：思考保持 `low`；10 轮首 reasoning p50=3.146 秒，text TTFT p50=3.925 秒。
最新八场景完整轮原始 5/8、0 基础设施错误；unchanged live unknown-effect 输出在通用评测语义
修复后回放通过，校正为 6/8。Security 和 Research 仍是真失败。Code Executor 已在 trusted-local
容器完成 approval→Docker sandbox→失败回传→模型自修复→artifact 的真实闭环。

## 1. 规范、历史文档与最新策略

用户指出的 Claude Code 完整审查文档已经定位：
[assistant-module-deep-review-2026-08-12.md](../../docs/archive/2026-08/assistant-module-deep-review-2026-08-12.md)。
它首次进入 Git 的 commit 为 bf74ff67e038e25cb352cc29038f02708ebf0be7（2026-08-14），
文档自身日期为 2026-08-12。本轮把它作为历史缺陷基线，而不是把旧报告当成当前通过证据。

当前实现和验收同时遵循：

- [项目架构约束](../../docs/harness/architecture.md)
- [项目标准命令](../../docs/harness/commands.md)
- [容器和密钥边界](../../docs/harness/runtime-and-secrets.md)
- [OpenAI Harness engineering](https://openai.com/index/harness-engineering/)
- [OpenAI Agents SDK evolution](https://openai.com/index/the-next-evolution-of-the-agents-sdk/)
- [OpenAI latest model guide](https://developers.openai.com/api/docs/guides/latest-model)
- [OpenAI prompt caching](https://developers.openai.com/api/docs/guides/prompt-caching)
- [OpenAI compaction](https://developers.openai.com/api/docs/guides/compaction)
- [OpenAI agent evals](https://developers.openai.com/api/docs/guides/agent-evals)
- [Anthropic agent evals](https://www.anthropic.com/engineering/demystifying-evals-for-ai-agents)
- [Anthropic context engineering](https://www.anthropic.com/engineering/effective-context-engineering-for-ai-agents)
- [Anthropic tool design](https://www.anthropic.com/engineering/writing-tools-for-agents)
- [Anthropic long-running agent harnesses](https://www.anthropic.com/engineering/effective-harnesses-for-long-running-agents)
- [MCP tools specification 2025-11-25](https://modelcontextprotocol.io/specification/2025-11-25/server/tools)

本轮采用的核心原则是：保留单一 AgentLoop 和现有 ToolRegistry/MCP/SubAgentManager，
缩小首轮上下文，按需发现工具；用结果级、多层级、真实 provider 验收，而不是以注册成功、
mock 或单元测试代替能力可用。Prompt cache 和原生 compaction 没有在缺少 provider 能力、
隐私门禁及 A/B 证据时直接上线。

## 2. 性能实测

### 2.1 简单问题

同一真实 Qwen 路径、全新会话的诊断样本：

| 场景 | trials | 首轮工具 schema | 估算上下文 token | provider input token | TTFT p50 | 总耗时 p50 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 优化前：2+2 | 3 | 7 | 3581 | 2392 | 3031.9 ms | 3274.3 ms |
| 优化后：2+2 | 5 | 3 | 1565 | 1301 | 3078.1 ms | 3333.5 ms |
| 优化后：三句话解释机器学习 | 5 | 3 | 同一精简路径 | — | 3166.2 ms | 4273.4 ms |

优化后 2+2 的 TTFT p95 为 3373.1 ms、总耗时 p95 为 3654.4 ms；短解释的 TTFT p95
为 3565.0 ms、总耗时 p95 为 4676.2 ms。所有 10 个优化后 trial 都收到唯一 run_finished。

结论：

- provider input token 从 2392 降到 1301，下降 45.6%。
- 估算上下文从 3581 降到 1565，下降 56.3%。
- TTFT 没有出现有意义的改善，仍约 3 秒；目前主要瓶颈在 provider 首 token，而不是 schema 数量。
- 这是小样本运行态诊断，不是并发压测或统计学性能声明。

此外关闭了两个确定的本地延迟：

1. OpenAI-compatible adapter 原先把每个 provider frame 再切成 4 字符并逐块 sleep 20 ms；
   长回答观测到 2096/2109 个 text_delta，理论上额外引入约 42 秒。本轮已删除该人工平滑。
2. 新会话 Web stream 原先等待 listSessions 返回后才开始；现在拿到 session id 即启动 stream，
   列表刷新后台执行，并新增 5 秒延迟列表请求的回归测试和 interactionToFirstTokenMs 遥测。

### 2.2 复杂任务

较早真实 provider 基准结果：
[native-parity-low-balanced-approval-resume-20260816T150653Z](../benchmark/results/native-parity-low-balanced-approval-resume-20260816T150653Z)。
最终结果见
[`native-agent-parity-2026-08-16-final-full-rerun`](../performance/native-agent-parity-2026-08-16-final-full-rerun/summary.json)。

| 指标 | 结果 |
| --- | ---: |
| 场景通过率 | 4/8（50%） |
| 基础设施错误 | 1 |
| 有效 turns | 9 |
| TTFT p50 / p95 | 8.396 s / 49.191 s |
| 总耗时 p50 / p95 | 14.336 s / 102.451 s |

通过：库存预留修复、Salesforce 现金质量、分阶段发布、安全租户访问审查。

失败：

- unknown-effect recovery 缺 unknown_effect_reconciliation。
- ambiguous export 第二轮缺 export_bounded_plan。
- Title VII transfer 触发 gateway_approval_limit_exceeded，属于基础设施错误。
- CRA source resolution 完成工具 approval/resume，但语义断言失败。

该表是修复前基线，不是最终验收。最终完整轮原始通过 finance Salesforce、legal、operations、
engineering、governance，共 5/8；unknown-effect 原输出经通用结构同义词修复后回放通过，语义
结果为 6/8、0 基础设施错误。Security 错拒同租户 R4，Research 的 evidence/conclusion 失败仍
保留为真失败。

三个 code-tool approval/resume 流程均真实执行；research 场景是在执行成功后语义失败。
高 thinking 配置更差，仅 2/8 且有 3 个基础设施错误。因此本轮没有用更长思考掩盖能力不足。
相对既有基线通过率没有提升，复杂任务质量是当前最直接的发布阻塞。

### 2.3 图片生成

真实 wan2.6-t2i：

- 首个 SSE 活动：307 ms。
- 图片工具/provider：20,489 ms。
- 首个文本 token：29,941 ms；总耗时 30,708 ms。
- 产物：PNG，816,777 bytes，1024×1024；真实下载 URL、artifact receipt 和 PNG magic 均通过。
- 唯一成功终态：image_generation_result=1、artifact_created=1、run_finished=1、run_error=0。
- 最大 SSE event 26,313 bytes，没有 SSEEventTransportError 或 DuplicateTerminalError。

这里的 307 ms 只是流已建立并有活动，不能冒充文本 TTFT。文本发生在工具执行和第二次模型调用之后。

## 3. 已实现的产品修复

### 3.1 流式终态和错误语义

- Responses API 以 canonical run_finished/run_error 投影唯一 terminal，到达后主动关闭上游。
- 统一 heartbeat；partial output 失败时以 incomplete 闭合，不丢已生成内容。
- Web 不再把 transport done 当成功；无 terminal EOF fail-closed，取消有独立 cancelled 状态。
- usage 在单次 streaming call 内去重、跨多次 model call 求和。
- outer/inner RunBudgetExceeded 不再被 compressor 的 provider fallback 吞掉。

### 3.2 持久化和跨服务一致性

- artifact SQL 显式使用 assistant.artifacts，修复 Assistant 写入但 Gateway 在另一 search_path 读取为空。
- session、history、governance 和 conversation share 路径统一使用 assistant.sessions，
  修复真实多轮会话读不到前文。
- DB pool 缺失时 artifact startup/persist fail-closed。
- 子代理输出进入 parent/progress/evidence 前统一 external-content envelope 和 secret redaction。

### 3.3 首轮上下文和工具选择

- 普通简单问题首轮只暴露 3 个 discovery bridges；明确相关能力仍可直接暴露。
- 无 discovery 能力时保留有界 fallback schemas。
- 不再 blanket pin 全部 skill_* 工具。
- ASCII alias 使用词/短语边界，run 不再误命中 returned；泛化 analyze/data 不再误暴露 Python。

### 3.4 Quiz、图片和公开分享

- Quiz 在 startup 注册。
- Quiz 参数规范化改为深拷贝，executor 不再修改调用者 args，checkpoint command hash 保持一致。
- Quiz Playwright 真实绑定 KB、生成、持久化、答题、分享、匿名提交和删除。
- 图片默认模型更新为 wan2.6-t2i；适配新 endpoint、messages/content.text、参数和轮询结构，
  1536 请求规范化为 provider 支持的 1280。
- 所有持久化 output_files（包括图片）从 SSE 中剥离 content_base64，仅保留 artifact_id/download_url，
  关闭大图片导致 SSE event 溢出。
- ArtifactShareManager 显式使用 assistant.artifact_shares、
  assistant.artifact_share_submitters 和 assistant.quiz_attempts。
- 当前本地 Compose 已执行 per-service assistant migration，应用 003_artifact_shares.sql；
  这只是当前本地运行态，不代表其他环境已迁移。

## 4. 真实接入能力矩阵

| 能力 | 当前证据 | 结论/边界 |
| --- | --- | --- |
| 普通对话、多轮历史、当前事实覆盖、跨用户隔离 | 真实 Qwen + Gateway/Assistant + canonical history API | 可用 |
| OpenAI Responses 非流/SSE | 真实 provider，唯一 terminal、usage、sequence | 可用 |
| Code Executor | trusted-local 实际 approval/resume、stdout、artifact，且非零退出可由模型自修复；make code-executor-test 通过 | 可用；仅限受信本地 Docker/runc，不代表生产安全默认 |
| Docgen MCP | 真实 MCP 生成、metadata、Gateway 查询、PDF 下载/格式 | 可用 |
| web_fetch | 真实 Example Domain marker、工具结果、run_finished | 可用 |
| 渐进工具发现 | 真实 tool_search → tool_describe → tool_call → web_fetch | 可用 |
| Plugin subagent | 已安装 doublecheck，child spawned/completed，parent 返回精确 marker | 最小真实垂直切片可用；强 fixture 多 trial 未跑 |
| Todo | 两轮真实 write/read 精确 marker | 可用 |
| KB lifecycle | 真实创建/摄取/检索/清理 | 可用 |
| Durable memory | Playwright 真实 provider，跨两个新 session 命中 | 基本可用；restart/delete/TTL 未证明，测试留下 1 条生成记忆 |
| Quiz | 真实 Qwen + KB + UI 完整分享/匿名提交/删除 | 可用 |
| Image generation | 真实 wan2.6，PNG/artifact/download/SSE | 可用，但文本 TTFT 约 30 秒 |
| Qwen native web search | 返回官方 Alibaba URL 和 wan2.6 marker | 可用；尚无独立 citation event |
| Confluence | 0 connections | 不可用 |
| Tenant MCP | 配置 3 个，enabled=0，tools=0 | 不可用 |
| Local Node | endpoint 503，未接通 | 不可用 |
| Google/OpenAI/DeepSeek/Tavily provider | 当前环境未配置 | 未验证 |
| Vision | runtime advertised=false，包括 qwen-vl | 不可宣称可用 |
| context_compact/read_tool_artifact | 未独立做 live result 验收 | 未验证 |

能力表只把真实结果闭环记为“可用”；注册、配置文件存在或 mock 通过不算。

## 5. 测试与运行态门禁

### 5.1 最终离线/组件门禁

| Gate | 实际结果 |
| --- | --- |
| Assistant + shared core + artifact API suites | 2611 passed, 1 skipped in 10.99s |
| 上述唯一 memory PG skip 的当前 Compose 补测 | 1 passed in 0.41s |
| Artifact-share focused | 15 passed, 1 skipped |
| Artifact-share real PG migrations | 2 passed in 0.14s |
| Quiz focused | 35 passed |
| Image focused | 12 passed |
| Artifact/SSE focused | 141 passed |
| make verify-assistant-runtime-dev | 119 + 82 + 37 + 119 tests 及 golden，全通过 |
| make agent-eval-core-gate | 83 + 22 tests，全通过 |
| make verify-eval-dev | 47 + 134 + 204 + 36 + 17 tests，golden/RAG 通过 |
| Ruff（全部本轮 Python 文件，忽略既有未触及 SIM105） | 通过 |
| git diff --check | 通过 |
| make harness-check | 通过 |

verify-eval-dev 有 12 条既有 duplicate Operation ID warnings；它们没有被记作新增失败。
未触及的 src/api/v1/conversation_shares.py:321 仍有既有 SIM105 warning。

### 5.2 真实容器/Provider/UI

| 场景 | 实际结果 |
| --- | --- |
| 综合 live dialogue：code approval、artifact、docgen、多轮、隔离、history | 1 passed in 122.71s |
| KB lifecycle | 1 passed in 21.06s |
| Responses 非流 + SSE | 2 passed in 7.58s |
| Durable memory Playwright | 1 passed in 12.1s |
| Quiz API | 3 题，ready 11.20s、terminal 14.63s，唯一 run_finished |
| Quiz 完整 UI | 1 passed in 26.3s（test body 19.9s） |
| Web chat mock terminal/latency suite | 27 passed, 1 skipped |
| Image | 真实 PNG/artifact/download，total 30.708s |
| make validate | 通过；仅 1 条本地 bootstrap 默认密码告警 |
| make status | 8 个逻辑依赖全部 healthy |
| make code-executor-status | trusted-local 验收时 enabled=true/backend=docker/smoke 通过；最终已恢复 enabled=false/backend=none/docker_socket=no/healthy |

Web 最终门禁：

- type-check 通过。
- lint 为 0 errors、10 个既有 warnings。
- build 通过，有既有大于 500 kB chunk warning。
- i18n check 通过。

仓库当前明确要求 Node 22；主机是 Node 24.14.0，因此 pnpm 有 engine warning，但上述门禁实际通过。
本轮按用户收窄后的核心范围没有迁移 Node 基线，也没有把“主机可运行”误写成项目已支持 Node 24。

## 6. 仍然开放的发布阻塞

1. 复杂任务只有 4/8，且出现 approval limit 基础设施错误；这是整体质量失败，不能被 2611 个组件测试遮住。
2. 简单问题 TTFT 仍约 3 秒，复杂任务 TTFT p50 8.396 秒；provider 首 token 和复杂 agent loop
   仍需分段 tracing、模型/路由 A/B 与并发压测。
3. Claude H6 仍开放：普通 running run 没有 lease/heartbeat/reaper。失败图片测试曾留下 stale run，
   重启后 TaskManager 已丢失任务，只能做精确测试数据清理。
4. M7 的工具中途取消/未知副作用，M8 的 partial text + forced synthesis 重复终答仍未关闭。
5. Confluence、Tenant MCP、Local Node 和多个 provider 当前不可用或未配置，不能宣称“所有接入可用”。
6. Durable memory 未覆盖重启、删除、TTL；强 plugin fixture 仍缺 HMAC/attestation 凭据；
   context_compact/read_tool_artifact 未独立 live 验收。
7. 尚未进行长时间 soak、高并发、故障注入、跨副本恢复和发布环境迁移验证。

## 7. 交付判断

本轮不是单纯检查：源码已经热更新到当前 Assistant/Gateway 容器，并通过真实 provider、
真实工具、MCP、数据库、浏览器、artifact 下载和复杂基准反复暴露问题后继续修复。
最有价值的结果是关闭了人工 42 秒级流延迟、首轮上下文浪费、错误终态、跨 schema
session/artifact、Quiz hash、图片协议/SSE 溢出和公开分享 schema 等实际缺陷。

但“能力可用”不等于“产品成熟”：当前简单 TTFT 仍未下降，复杂任务只通过一半，
若以发布为目标，下一阶段应优先关闭 H6/M7/M8、修复四个复杂场景，并建立并发与故障恢复门禁。
本轮没有 commit、push 或 deploy。
