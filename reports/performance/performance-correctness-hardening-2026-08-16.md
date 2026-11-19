# AI Assistant 性能与正确性优化执行报告

日期：2026-08-16
输入：`reports/code-review/perf-review-2026-08-16.md`、
`reports/code-review/global-parallel-review-2026-08-16.md`

## 结论

本轮不是关闭思考换取表面速度。Assistant 保持 `qwen3.7-plus`、`thinking=low`，新增首个
生命周期、首个 reasoning 与首个文本的分段指标，并让 Web 在 reasoning/tool activity 到达时
立即给出真实反馈。10 个新会话全部成功：首个 reasoning p50=3.146 秒，文本 TTFT
p50=3.925 秒。用户感知已经从“静默等待文本”变成“先看到真实思考进度”，但文本 TTFT 仍高于
3.41 秒发布门槛。

最终八场景完整实机轮原始通过 5/8、基础设施错误 0。`unknown-effect` 的实际输出正确完成
readback、禁止重试和去重，只被评测器的结构同义词误拒；修复评测器后用该轮原始输出回放通过，
因此语义校正结果为 **6/8**。Security 错拒同租户请求、Research 多项证据和结论错误仍是真失败。
这刚好达到单轮门槛，但没有证明跨轮稳定性，产品发布判断仍为 **NO-GO**。

## 为什么 OpenCode / OpenClaw 感觉更快

同目录实现对照显示，差距不是一个“关闭思考”的开关：

- 它们把 reasoning delta、status、tool activity 作为一等流事件，首个真实活动到达就渲染；
  旧 Web 只把文本当 TTFT，provider 已经在思考时界面仍像卡住。
- 它们按模型/provider 暴露 reasoning effort 和 variant，而不是让所有请求走同一重型路径。
- 当前 Assistant 的本地 SSE/lifecycle 只需约 16 ms；主要等待是 DashScope 首个 reasoning
  token 约 3.15 秒，继续压几十毫秒本地准备无法解决这个 provider 下界。
- 阿里云官方文档说明 Qwen3.5/3 系列支持 thinking 与 non-thinking，但不同模型、区域和 mode
  有独立能力、价格和生命周期约束；因此路由必须能力驱动和可配置，不能把 prompt 文本或任务
  答案硬编码进代码。

本轮采用的产品策略是：保留思考、默认 low、流出真实 reasoning/activity、同时报告 reasoning
TTFT 与 text TTFT；没有将 medium/high 当成复杂任务的万能补丁。Research 的 medium A/B 仍失败，
文本 TTFT 从 low 的约 8 秒升至 21.306 秒，总耗时 89.645 秒。

## 已确认并修复

### Agent 正确性与自修复

- 幂等请求在断开/取消时终止并清理；无 Redis session cache 增加 TTL/LRU 与写后失效。
- Artifact legacy owner 以 tenant/user fail-closed，创建时校验 session 所有权；子代理上下文和
  事件输出统一做角色中和、边界限制与脱敏。
- Approval 恢复预算；内部超时不再误标墙钟耗尽；Native/MCP 取消使用有限 grace，未知外部副作用
  进入 fence；最后一轮工具前导文本不再与 forced synthesis 形成双终答。
- 修复 Code Executor 的关键成熟度缺陷：隔离沙箱里确定性的非零退出不再升级成
  `SIDE_EFFECT_UNKNOWN` 并终止 run，而是把 stderr/exit code 返回模型继续修复。真实 finance 与
  engineering 场景随后 2/2 通过，证明了模型→审批→Docker→失败→模型修复闭环。

### 热路径、流与计量

- Memory 文件读取移出事件循环；文档缓存有界；未变化 source 不重复分块/嵌入；checkpoint
  message digest 每次只算一次。
- Trace root/lifecycle 每 trace 写一次，`drain()` 使用 fixed-point barrier；25 个 delta 的回归
  为 27 次 DB 调用，不再逐事件重复 root/lifecycle upsert。
- `tool_call` discovery bridge 与目标工具使用同一有效超时，修复 docgen 被 30 秒外层取消；真实
  PDF/DOCX 生成、持久化、下载和格式校验通过。
- UsageRecorder 生产 asyncpg 路径将 100 条记录合并为一次多行 `fetch`，同一非空 idempotency
  identity 只入账一次。
- Web terminal first-wins；session epoch 丢弃旧会话迟到事件；SSE 携带认证；transport error 不再
  伪装完成。ChatMessage memo、完成 Markdown block 复用解析结果，移除失效强制 `ui` chunk。
- Web 新增双指标：`interactionToFirstResponseMs` 记录 reasoning/tool/text 中第一个真实响应，
  `interactionToFirstTextMs` 单独记录文本，避免用连接事件冒充用户可见响应。

## 测试证据

| 层级 | 结果 | 覆盖/边界 |
| --- | --- | --- |
| Python 全量 | 6,186 passed / 23 skipped / 0 failed（6,209 collected，exit 0） | 默认关闭 live 项另行执行 |
| Assistant runtime gate | 5/5 groups passed | 121 + 82 + 40 + 121 tests 与 golden |
| Native complex 全轮 | 原始 5/8；原输出语义回放 6/8；0 infra errors | Security/Research 保持真失败 |
| TTFT | 10/10 succeeded | thinking=low；reasoning 与 text 分段 |
| Web type/lint/build | passed | lint 0 errors、10 个既有 warnings；build 795 ms |
| Web targeted Playwright | passed | 新会话非阻塞、首响应/首文本遥测、reasoning 可见 |
| Web 全量 Playwright | 103 passed / 4 skipped / 29 failed / 3 not run | 未配置 `E2E_API_URL`/登录环境，不能记回归通过；定向项另跑通过 |
| Code Executor | smoke pass + Agent live pass | trusted-local Docker/runc；非生产安全声明 |
| Runtime | `make validate`、`make status`、`make harness-check` passed | 当前 Compose 全部 healthy |
| Diff/lint | `git diff --check` passed；本轮核心 Python Ruff passed | 宽目录 Ruff 仍有未触及既有告警 |

全量 Python 的 23 个 skip 包括默认关闭 provider/live、独立迁移 DSN、Windows 专用和外部数据目录；
不能把 skip 当作 pass。Web 全量 E2E 的环境型失败同样不能冒充成功，也没有证据指向本轮 TTFT
改动；直接覆盖改动的定向浏览器用例实际通过。

Python/shared core 已通过 `make hot-update` 进入当前仓库拥有的容器。最新 `web/dist` 也已热更新
到 frontend 容器，重新生成 runtime config，本地与容器 `index.html` SHA-256 一致且 8081
health 通过。前端是实机有效的临时热更新，容器重建后仍会恢复镜像内容。

Code Executor 验收期间使用受信本地 Docker socket；完成 smoke、Agent live 与错误自修复测试后，
已执行 `make code-executor-disable`，再执行 `make hot-update` 恢复最新源码。最终运行态为
`enabled=false / backend=none / docker_socket=no / healthy`，全栈健康；测试证据保留，但高权限
开发开关未遗留。

## 当前 Compose 真实能力

| 能力 | 结果 | 实证/边界 |
| --- | --- | --- |
| OpenAI Responses 非流/SSE | pass | 真实 Qwen、唯一终态、usage |
| Assistant × KB | pass | 生命周期、grounding、跨租户隔离 |
| Docgen MCP | pass | 真实 PDF/DOCX artifact 下载与格式校验；仍慢 |
| Image | pass | 真实 tool call、artifact、下载、图片魔数 |
| Quiz | pass | 真实 tool call、`quiz:ready`、结构化题目 |
| Code Executor | pass | Agent approval/resume、Docker sandbox、非零退出自修复、artifact |
| Todo / web_fetch / progressive discovery / plugin child | pass | 已有真实 result receipt |
| Confluence | unavailable | 当前 0 connection |
| Tenant MCP | unavailable | enabled=0、tools=0 |
| Local Node | unavailable | 当前 endpoint 未接通 |
| Vision / 未配置 providers | not verified | advertised=false 或环境无凭据 |

“可用”只用于真实结果闭环；配置文件、注册状态和 mock 不算。

## 性能与复杂能力

### 简单问题，保留思考

证据：`reports/performance/assistant-ttft-thinking-low-2026-08-16.json`

- 首个 lifecycle event：p50 0.016 s / p95 0.019 s。
- 首个 reasoning：p50 3.146 s / p95 3.358 s。
- reasoning→首文本：p50 0.735 s / p95 1.067 s。
- 文本 TTFT：p50 3.925 s / p95 4.307 s。
- 总耗时：p50 3.976 s / p95 4.465 s。

50-token thinking budget 和 qwen-plus/qwen3.6-flash 的小样本路由探针均未稳定优于当前路径，已撤销，
没有留下基于题型或关键词的硬编码路由。

### 八场景复杂任务

证据：`reports/performance/native-agent-parity-2026-08-16-final-full-rerun/`

- 原始：finance Salesforce、legal、operations、engineering、governance 通过，共 5/8。
- `finance.unknown_effect_recovery` 原答案已证明 `TX-9/PAY-77/12500` committed，禁止重试并取消
  sibling；评测器补充接受 `workflow_id` 与 `duplicate_risk=mitigated` 后，原输出回放通过。
- 校正后：6/8，0 基础设施错误。
- 真失败：security 错拒同租户 R4；research 的 manufacturer 结论和多个 evidence set 错误。
- Medium research A/B 0/1，说明提高 thinking 等级没有解决语义能力，并显著恶化延迟。

本轮还有 finance/engineering 修复后独立 2/2、governance 与 unknown-effect 原输出回放等重复证据，
但没有完成 3 个完整八场景 cohort；因此 6/8 只能作为当前单轮结果，不能声称稳定达到成熟度。

## 仍需完成

1. 文本 TTFT p50 3.925 秒仍高于 3.41 秒门槛；下一步应做 provider/model variant 的可配置
   canary 路由与真实 A/B，不是关闭思考或匹配 prompt。
2. Security 需要更稳定的约束执行；Research 需要基于 evidence graph/schema validation 的通用
   自检与纠错，不能把金标结论写进 system prompt。
3. Trace 仍未实现完整事件批写；当前只消除了重复 root/lifecycle upsert。
4. Admission lease/reaper、配额多 worker、SDK SSE、Knowledge 多模态批嵌入、Local Node watcher
   等报告确认项仍在后续 phase，不能因全量测试通过视为关闭。
5. Docgen 与复杂 code/research 循环仍慢；需要阶段预算、工具结果压缩和多轮 provider tracing。
6. 仓库基线仍是 Node `^22.12.0`；本机 Node 24.14.0 实际通过 Web 门禁但有 engine warning。
   Node 基线升级必须同步 CI/容器/文档，不能混进本轮 Agent 核心修复。

## 发布判断

**NO-GO / changes required**。真实能力已经显著超过玩具级注册验证，工具错误自修复和保留思考的
早反馈也已落地；但文本 TTFT 未达门槛，Security/Research 仍有真失败，6/8 仅单轮且未证明稳定。
不得把 6,186 个通过测试或一次 6/8 校正结果描述为成熟产品。
