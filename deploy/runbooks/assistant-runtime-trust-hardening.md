# 通用智能体对标与缺陷清零：目标与执行计划

> 文档类型：目标、对标方法、验收标准、执行边界
>
> 实现基线：`32457e6`；交付分支：`codex/hermes-openclaw-parity-hardening`
>
> 与 `assistant-local-os-product-contract.md` 的关系：那份定义 Local OS 的*产品能力*；
> 这份定义*对标方法*与*运行时欠账*。

## 2026-08-13 完成状态

本轮按结果而不是工具数量收口，且未修改 Hermes/OpenClaw 的工具集、源码或运行策略：

| 验收项 | 状态 | 当前证据 |
| --- | --- | --- |
| 8 个真实企业任务三方对标 | 完成 | AI--Platfform 6/8；Hermes 5/8；OpenClaw 5/8；24/24 有终态，基础设施错误 0。公开摘要：`reports/benchmark/native-agent-parity-summary-2026-08-13.json`。 |
| P0-1 默认失控循环拦截 | 完成 | canonical `AgentLoop` 默认接入 time/call/repeat/checklist/trace；真实 fake-provider 重复调用测试证明第 4 次在副作用前拒绝。 |
| P0-2 内部异常可诊断 | 完成 | `ai_gateway_core.logging._exceptions` 统一输出稳定事件码、类型、指纹、有界代码坐标和关联上下文；禁止 message/args/locals/raw traceback；AST 门禁覆盖 Assistant 及共享 memory/skill 吞错边界。 |
| P1-1 启动配置真值 | 完成 | 单一 immutable typed snapshot 同时进入启动日志、readiness、trace metadata 和 canonical context；指纹在启动时实时计算（环境相关、未 pin，随 secrets 存在性 / build 版本 / source 变化），例如 `sha256:295335558e1b368049bcfb11b6861fd9db9b6e462ad533b09c1c67901885c518`。 |
| P2-1 migration fixture 闭包 | 完成 | channel runtime PostgreSQL fixture 执行到 migration 081，保持附件物理占用语义。 |
| P2-2 SSE 单事件上界 | 完成 | shared transport seam 强制 64 KiB JSON data 上界；整体超限时才把闭集大字段转 tenant/user/session scoped redacted artifact；存储或限界失败均 fail closed。 |
| canonical AgentLoop 唯一执行面 | 保持 | benchmark AI 入口为 Gateway → Assistant → `_execute_agent_loop`; Responses 是同一事件流的 adapter，无第二个 loop。 |
| 企业边界 | 保持 | 未放宽多租户、KB dataset 权限、审批、CapabilityAllowlist、sandbox 或脱敏；production ToolInvoker 注入 tenant-scoped durable audit。 |

原始 benchmark 候选文本和协议证据位于 Git 忽略的 `reports/benchmark/results/native-parity-20260813-final7/`，全部文件 mode 0600；仓库只提交无敏感内容的有界摘要。

---

## 第一部分：目标（GOAL）

### 第一性原理

这是一个**企业级专业通用智能体**，对标 Hermes Agent 与 OpenClaw。目标只有三件事：

> **找到所有缺陷 → 升级优化 → 测试回归。**

其中"缺陷"不限于 bug。**能力缺口也是缺陷**——对标对象能完成而我们完不成的任务，就是缺陷。

### 对标对象（本机可直接读源码，不要依赖二手描述）

| 对象 | 路径 | 语言/规模 |
|---|---|---|
| Hermes Agent | `/Users/yang/projects/Hermes_agent` | Python，约 6.5k 源文件 |
| OpenClaw | `/Users/yang/projects/open claw/openclaw` | TS，约 8k 源文件 |

两者都在本机完整可读。**不要用文档宣称或版本说明代替读源码。**

### 需求结果

产出一份**能力差距矩阵**，并把矩阵里的每一项推进到有明确结论的状态。矩阵完成即目标达成。

---

## 第二部分：对标方法

数文件个数、比工具数量**不算对标**。真实对标必须同时做三层，缺一层结论就不成立。

### 第 1 层：任务级对标（最高权重）

选 8–12 个**真实企业任务**，三个系统各跑一遍，比最终交付物，不比事件形状。

任务须覆盖：多步推理、外部信息检索、文件产出、需要澄清的歧义输入、长时任务、失败恢复。

每个任务记录：

- 是否完成（结果级判定，**"非空输出"不算完成**）
- 步数 / 耗时 / token
- 失败时卡在哪一步、什么原因
- 三方结果的质量差异（人工判定即可，但要写明判据）

**这一层是唯一能证明"我们比对方差在哪"的证据。** 架构对比和能力清单都只是解释它的辅助。

### 第 2 层：能力面对标

对每一项对标对象具备的能力，给出**三选一**的结论，不允许留空：

- **已覆盖**——写出我们的对应实现位置
- **采纳**——写出为什么值得做、落到哪个里程碑
- **不采纳**——写出理由。合法理由包括：与企业多租户模型冲突、与审批/隔离边界冲突、目标用户不需要。
  **"没时间"不是理由**，那属于"采纳但排后"。

重点比较维度（Codex 自行读源码展开，不要照抄本文件的猜测）：

- 执行模型：agent loop 形态、中断/恢复、检查点
- 工具体系：工具粒度、动态发现、扩展机制（plugin / skill / MCP）
- 环境控制：浏览器交互、终端/进程、Computer Use、文件系统
- 人机协同：澄清提问、审批、接管、打断
- 会话与记忆：压缩、检索、跨会话
- 多智能体：委派、并行、结果聚合
- 安全：权限模型、沙箱、凭据处理、供应链校验
- 可观测：trace、回放、成本归因

### 第 3 层：架构取舍对标

对标对象是**单用户本地优先**架构，我们是**企业多租户**架构。因此：

- 它们的某些设计我们**不能照抄**（如全主机信任、全局 token、sandbox 默认关闭）
- 我们的某些成本它们没有（租户隔离、审批网关、KB 数据集权限）

这一层的产出是：**明确写出哪些差距是"我们该补的"，哪些是"架构选择的必然代价"。** 把后者
误当缺陷去补，会破坏企业级定位。

### 交付物

一份 `reports/benchmark/agent-capability-gap-YYYY-MM-DD.md`，包含：

1. 任务级对标结果表（含每次运行的原始 trace 或 SSE artifact 引用）
2. 能力差距矩阵（每项有三选一结论）
3. 架构取舍说明
4. 由矩阵推导出的优先级排序，附理由

---

## 第三部分：已确认的运行时欠账

以下是 2026-08-12 审查中**已实测确认**的缺陷，不需要再对标即可动手。它们与对标并行推进。

### P0-1　接线可靠性中间件

`core/agent/middlewares/harness.py` 提供 5 个中间件——`CallLimitMiddleware`、`LoopDetectionMiddleware`、
`TimeBudgetMiddleware`、`PreCompletionChecklistMiddleware`、`TraceSensorMiddleware`——现已**无条件注册进
默认生产链**（`agent_loop.py:_build_default_middleware_chain`，`chain.add` 共 8 处：上述 5 个 +
`RuntimeMemoryMiddleware` / `ToolOutputSpillMiddleware` / `ResponseCapMiddleware`），不再有
"intentionally not registered by default" 的陈旧声明。阈值（默认 call 256 / repeat 3 / time 1800s /
trace 256 events 64 errors）在调用时从 `ctx.config` 与 canonical `RunBudget` 解析，可按租户/执行档位覆盖。

失控循环在服务端沙箱里是烧 token，在 Local OS 场景下是在用户真机上反复改文件、反复调外部服务。

**验收**　5 个注册进默认链，阈值可配、可按租户/执行档位覆盖；**结果级证明**——构造真实会重复
调用同一工具的对话，抓 SSE 流证明拦截发生；默认阈值宽到接线前后全量失败数不变。
（`test_agentloop_streaming_first_contract.py::test_default_loop_detection_denies_fourth_identical_tool_dispatch`
以真实 AgentLoop + fake-provider 证明第 4 次重复调度在副作用前被拒绝，`invocation_count == 3`。）

**边界**　`harness.py:266-267` 的 `if False: yield None` **不要"修"**。它让该函数成为 async
generator，`MiddlewareChain.run_on_error` 用 `async for` 迭代，删掉会直接坏。可换写法，但须保持
async generator 语义。

### P0-2　异常可诊断

系统性模式。实测：`apps/assistant-service/src/` 中 `"exception_type=%s"` 记录点 **116 处**，
其中带 `exc_info` 的 **0 处**。典型形态（`core/agent/streaming_execution.py:547`，主流式循环兜底
handler）只记录类名，无 message、无 traceback、无位置。

代价实测过：本次审查中一个 `AttributeError` 完全无法定位，靠临时加 `exc_info=True` 才查出——
那还是在本地、有源码、能改代码重跑的条件下。

**验收**　每个服务进程内宽泛异常边界都记录稳定事件码、异常类型、安全指纹和有界代码坐标；有
cause/context 时记录有界链并防循环，`OSError` 只允许范围校验后的整数 `errno`，异常参数只记录
封闭类别的 shape，`ExceptionGroup` 只记录有界成员数量、成员类型和安全指纹。日志须携带现有
request/trace 关联字段，使同一指纹可回到具体请求与代码位置。SSE、trace 用户可见字段和 artifact
**仍然**只有稳定脱敏摘要；测试断言原始异常文本不出现在日志或任何对外 payload。

这里的“可诊断”指能够定位失败边界和根调用点、关联请求、聚合同类故障，并通过显式稳定事件码或
受控数值码区分已知故障；不承诺从任意自由文本中恢复根因。原建议“服务端读取脱敏 message 与
traceback”被结构化诊断取代：当前文本脱敏是已知模式 denylist，无法证明无标签 token、PII、查询
内容或业务数据不会穿透。因而统一 helper **不得**调用异常或参数的 `str`/`repr`，不得记录 message、
source、locals、原始 traceback 或异常对象；formatter 必须再次按闭合 schema 校验字段。

**边界**　“对内可定位、对外仍脱敏”，而不是“对内可读取任意异常文本”。不得把原始异常文本泄漏
到服务日志、SSE、trace 用户可见字段或 artifact。统一 helper 及 AST/source 门禁是唯一异常记录
路径，不得在调用点以 `logger.exception`、`exc_info=True` 或格式化 caught exception 绕过。

### P1-1　配置真值唯一

`ASSISTANT_SUBAGENTS_ENABLED` 实测：代码 `_env_truthy`（`main.py:67`，无默认参数→缺失即
false）、`.env.example`、`docker-compose.yml:538` 三处一致写 `false`；
`docker exec ai-gateway-assistant-service printenv` 是 **`true`**。运行态无法从任何一份可读声明
推导，只能问容器。

**验收**　启动时输出"已解析开关指纹"（每开关最终生效值 + 来源），纳入 trace；密钥类只输出
是否已设置；测试断言 `.env.example` 与代码默认值一致，有意差异需显式豁免。

**边界**　本项**不改变任何开关的当前生效值**，只让它可见且一致。

### P2-1　附件测试迁移闭包漂移

`agent_repository.py:5314` 与 `:7370` 查询 `attachment.deleted_at`；该列并非缺失：
`database/migrations/081_agent_studio_operations_governance.sql:429` 已添加 `deleted_at`、`deletion_id`
与 `cleanup_error`，并创建 `WHERE deleted_at IS NULL` 的 active-expiry 索引。唯一离线失败来自
`tests/database/test_agent_channel_runtime.py` 只应用 079/080，却调用了依赖 081 的当前 Repository。

**验收**　隔离 PostgreSQL fixture 按正式顺序执行至 081，并补齐 081 所依赖的同 schema 基表，确保
未限定表名不会落到 `public`；081 重跑幂等；原失败用例及文件级测试通过。

**语义**　保留 `deleted_at IS NULL`。过期但对象存储尚未清理、或清理失败的附件仍占物理空间，必须
继续计入租户配额；成功清理后当前实现会硬删除记录。仅改成 `expires_at > NOW()` 会让未清理对象逃逸
配额，不能采用。

### P2-2　单事件传输上界

本轮已修掉 turn_state 的二次增长（每事件恒定 ~450 B，单轮 128 KB → 32 KB）。`sse_event_transport.py`
在传输缝对每个 `data:` payload 强制 64 KiB 上界（`SSE_DATA_PAYLOAD_MAX_BYTES`）：低于上界原样放行；
**整体超限**时才把闭集大字段（`context_packet`/`context_detail`/`provenance`/`debug`/`tool_result`/
`result`/`context_snapshot`）脱敏后转 tenant/user/session scoped artifact，事件只留 ID；存储或限界
失败均 fail closed，终态事件永不改写。注意这是"封顶 + 超限 spill"，不是逐字段策略——单条
`context_budget`（约 8.7–10 KB）低于上界，仍原样内联传输。

**验收**　测试断言任意单条 SSE 事件 payload 小于硬上界；超限走 artifact 引用，事件只留 ID，
且内容仍可通过 ID 完整取回。

---

## 第四部分：边界

**结构性**

- 不得新建第二条执行路径。canonical AgentLoop / ExecutionGateway / CapabilityAllowlist 是唯一
  执行面，Local Node 与 OpenAI Responses 都是其 adapter。
- 不得放宽既有的审批、租户隔离、能力白名单或脱敏行为。
- 不得修改 `turn_event_collector` 对 terminal envelope 的逐字节一致性校验——该不变量在本次
  turn_state 精简中拦下过一版过宽实现。
- **不得为了追平对标对象而牺牲企业级定位。** 单用户本地优先架构的便利，多租户环境下往往是
  越权。

**过程性**

- 不得为了让测试变绿而改测试断言。冲突时先判定哪边对，理由写进提交信息。
- 不得记录未实际运行的验证。每条"已验证"附：命令、通过/失败计数、每个失败项的归因。
- 声称修复前，先有一个能复现该问题的失败测试。
- 每项改动可独立回滚。

**协作（本仓库多 agent 并发）**

- 状态还原只用 `git`，**不要用文件拷贝做快照**。本次审查中用 `cp` 还原一个正被并发编辑的文件，
  覆盖了对方改动，凭空产生 81 个失败，一度误判为代码缺陷。
- 改他人正在编辑的文件时，压到最小的单个 hunk。

---

## 第五部分：证据标准

**"已验证"必须包含**：实际执行的命令、通过/失败/跳过计数、每个失败项的归因（既有缺陷 /
环境问题 / 本次引入）。

**不接受**：把"非空输出"当能力成功；只有单平台 live 证据却声称跨平台；引用未随代码同步更新的
文档结论；用对标对象的 README 宣称代替读它的源码。

**并发工作时**：报告失败数前先归因。本次审查出现过 9 → 92 → 4 的波动，其中 92 那次完全是
还原方式错误造成的假象。

---

## 附：2026-08-13 最终证据

```text
分支        codex/hermes-openclaw-parity-hardening
Assistant   2423 passed, 1 skipped, 0 failed
全量非集成  6103 passed, 37 skipped, 0 failed
真实对标    AI 6/8；Hermes 5/8；OpenClaw 5/8；infra 0/24
raw summary sha256
            59178e2b372cf9939b251e988414f7db624b0a90ed9a29b3b4961df5aee57b62
manifest    5b943126f89319724d6ffc619bc8e0a97ee4e3e370c6e843ddb0f1959f4c1123
runner      eaca9b0732b25d3df7935fd93763b8d75046879bbc51f9155404615dc876e5fa
```

```bash
.venv/bin/python -m pytest tests/ -q --no-header --no-cov --ignore=tests/integration
```

Assistant 完整套件：

```bash
PYTHONPATH=apps/assistant-service/src:packages/ai-gateway-core/src \
  uv run --package assistant-service pytest -q --no-cov tests/services/assistant
```

结果级对标：

```bash
.venv/bin/python scripts/native_agent_parity_benchmark.py \
  --output-dir reports/benchmark/results/native-parity-20260813-final7
```

禁止将单题预检、失败的 `final6`、历史 replay 或非空输出替代上述完整 `final7` 汇总。更强的三次重复 E5 cohort 仍需新 suite ID 和新证据目录，不能覆盖本次最小验收语义。

---

## 附 2：2026-08-14 残留修复复核证据

在 08-13 对标交付之后，按本 runbook 的缺陷清单做了一轮"残留修复"（提交序列
`b67d476..df6fd89`，全部在 `main`）：P1 session-KB 绑定、working-memory settle 归档、
python_executable 守卫、requires_confirmation 盖章、异常门禁扩到全 ai-gateway-core + 站点迁移、
门禁接入 CI、死 RAG 分析集群删除、死中间件删除。全库回归（postgres 已启动）：

```text
分支        main
Assistant   2455 passed, 1 skipped, 0 failed
tests/database  52 passed, 0 failed（依赖本地 PostgreSQL 127.0.0.1:5432）
全量非集成  5966 passed, 6 skipped, 0 failed, 0 errors
```

```bash
# 需要先启动 postgres：docker compose up -d postgres
.venv/bin/python -m pytest tests/ -q --no-header --no-cov --ignore=tests/integration
PYTHONPATH=apps/assistant-service/src:packages/ai-gateway-core/src \
  uv run --package assistant-service pytest -q --no-cov tests/services/assistant
```

注：`tests/database` 与 3 个 `tests/services/eval/test_coding_parallel_fixture` 用例依赖 docker
daemon / 本地 PostgreSQL；daemon 未启动时分别报 50 个 ERROR（连接拒绝）与 3 个 FAILED
（sandbox image 无法 inspect），均为环境问题、非代码回归。启动 postgres 后全部归零。
