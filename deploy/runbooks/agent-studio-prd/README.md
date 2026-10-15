# Agent Studio 产品与交付 Harness

**状态：** 需求与执行 Harness 已就绪；业务实现尚未开始  
**日期：** 2026-07-17  
**Owner：** Product / Frontend / Gateway / Assistant / Knowledge / Security  
**目标：** 将现有通用 AI Assistant 封装为可配置、可版本化、可评测，并可通过托管页、嵌入组件和 API 发布的独立 Agent 产品。

## Harness Intent

本目录既是完整产品需求包，也是可由后续 Coding Agent 分阶段执行的长期 Harness。产品主张、架构不变量、UI、权限、数据、MCP、Skills、知识库、评测、发布和回滚都必须由仓库事实和持久证据驱动，不能依赖当前聊天上下文。

核心文档：

| 文档 | 用途 |
| --- | --- |
| [`product-requirements.md`](product-requirements.md) | 用户、范围、功能/非功能需求、验收场景、非目标 |
| [`architecture-contract.md`](architecture-contract.md) | 当前实现结论、目标架构、数据模型、API、运行授权和迁移不变量 |
| [`ux-spec.md`](ux-spec.md) | 路由、页面/状态矩阵、Studio、Preview、发布、Hosted/Embed 与可访问性 |
| [`source-packet.md`](source-packet.md) | 用户请求、代码证据、竞品研究、差距和来源可信度 |
| [`phase-manifest.md`](phase-manifest.md) | 10 个实施阶段、依赖、风险、验证和交接索引 |
| [`reports/planning-critic-verdict.md`](reports/planning-critic-verdict.md) | 独立规划评审、七项修订闭环与最终 approved verdict |

## 产品决策摘要

1. **不是把所有工具改成 MCP。** 当前能力来自平台原生工具、模型原生联网、知识库代理、Skills、Connector 和 MCP；统一层是 Capability Catalog + Version Binding + Policy Intersection。
2. **稳定身份与不可变发布分离。** `Agent -> Draft -> immutable Version -> Publication`；发布和回滚只原子移动 Publication 指针。
3. **会话固定 Version。** Publication 升级只影响新会话；进行中的会话不静默换配置。
4. **Gateway 是唯一运行解析权威。** 外部请求只提交 Agent/Publication 身份和允许的会话输入；Gateway 解析 Snapshot 后签发 Runtime Envelope，Assistant 只验证并执行，拒绝客户端伪造的能力字段。
5. **知识库采用“固定绑定、活内容、记 revision”。** V1 不复制整份知识库，每次 Trace 记录内容 revision fingerprint；它用于 provenance，不承诺确定性重放。
6. **MCP V1 只开放远程 Streamable HTTP。** Secret 只保存引用；service-account 与 user-delegated grant 明确 owner/scope/audience/revoke/channel 规则，并启用 SSRF/OAuth/schema hash/超时/熔断门禁。
7. **V1 聚焦单 Agent 应用。** 任意 DAG、多 Agent 编排、Marketplace、自定义域名、Agent-as-MCP 和租户任意代码执行明确排除。
8. **现有 Assistant 保持兼容。** 新表与字段采用加法迁移，`__builtin_assistant__` 作为回退路径，功能开关关闭时行为不变。

## Coding Agent Loading Protocol

被分配某个 Phase 时：

1. 打开 `deploy/runbooks/agent-studio-prd/context-profile.json`。
2. 打开 `deploy/runbooks/agent-studio-prd/loop-state.json`。
3. 只打开目标 Phase 文件；目标未知时才使用 `phase-manifest.md` 定位。
4. 按 Phase 的 `context.primary_context` 打开最多 4 个 hot-path 源码/文档项。
5. `README`、完整 source packet、oracle、progress、handoff、ledger、历史 reports 和 next-window prompt 默认延迟加载，只有 context trigger 命中时才打开相应小节。
6. 编辑前先写 Phase Plan；仅在 `likely_edit_paths` 内做最小需求变更。
7. 先运行 Phase 指定 baseline，再实现、测试、做浏览器/运行时/安全/评测检查。
8. Actor 写完成报告后，由独立 Critic 读取文件和证据给出 verdict；自评不是完成证据。
9. 退出前更新 loop-state、progress、handoff、report、oracle evidence、source-packet 代码事实和 continuity ledger。
10. 只有依赖 Phase `passed` 或有用户明确 waiver，下一 Phase 才可开始。

定位命令：

```bash
rg -n "PHASE_ID: AS-XX" deploy/runbooks/agent-studio-prd
```

结构校验：

```bash
python3 /Users/yang/.codex/skills/prd-phase-harness/scripts/validate_harness_prd.py \
  deploy/runbooks/agent-studio-prd --quality-score
```

Phase 完成声明必须额外运行：

```bash
python3 /Users/yang/.codex/skills/prd-phase-harness/scripts/validate_harness_prd.py \
  deploy/runbooks/agent-studio-prd --claim-check --phase AS-XX --quality-score
```

## Long-Running Runtime Protocol

- Loop 类型是 goal；每轮只选择一个 Phase 和一个 Feature Oracle 项。
- 循环固定为 `observe -> select -> execute -> verify -> record -> decide`，每个 Phase 最多 3 次无新证据尝试。
- Phase 报告、Critic verdict 和验证日志是权威证据；聊天、终端滚屏和隐式记忆都不是。
- 命中 blocker、边界外修改、缺审批、同一失败重复或尝试上限时停止并写证据。
- 代码事实、API/Schema 决策和跨 Phase 影响写入 `continuity-ledger.md`，不能只写在某份临时报告。
- 任何 Phase 改变既定产品范围或架构不变量时，必须先更新 PRD/契约、manifest 和受影响 Phase，而不是直接编码。

## Source Packet

`source-packet.md` 已记录：

- 用户原始意图和范围。
- 当前 checkout 的 Assistant、Tool、MCP、Skills、Knowledge、UI、Session、Trace/Eval 代码事实。
- Dify、Flowise 和 MCP 官方资料中的可借鉴能力与明确差异。
- 当前 `main` 落后 `origin/main` 8 个提交的基线风险。

AS-00 必须先在目标实现分支重新核对这些事实；本规划不能替代实现时的 branch rebaseline。

## Runtime Artifacts

| Artifact | 作用 |
| --- | --- |
| `context-profile.json` | 冷启动文件预算、Actor/Critic/Builder 上下文策略 |
| `loop-contract.json` | Goal loop、尝试上限、done/stop 条件 |
| `loop-state.json` | 当前 Phase、Feature、迭代、阻塞和下一动作 |
| `feature-oracle.json` | 端到端行为预言机；实现 Agent 只能更新 status/evidence/notes |
| `progress-log.md` | 跨窗口时间线、当前状态和干净退出证据 |
| `agent-handoff.md` | Planner/Actor/Critic 文件化交接 |
| `continuity-ledger.md` | 跨 Phase 接口、不变量、代码事实和变更影响 |
| `next-window-prompt.md` | 新窗口可直接使用的目标 Prompt |

## Current System Shape

当前系统由 React Web、Gateway、Assistant Service、Knowledge Service、PostgreSQL/Redis/Qdrant 和内部 docgen 服务组成。Assistant Service 已负责对话、会话、AgentLoop、工具、RAG、记忆和 Trace，但用户侧只有单一内置 Assistant 页面。

已确认的关键约束：

- `AssistantConfig` 支持 system prompt、模型、KB、联网、文件、策略、Skills 等请求配置。
- Web 端只把写作风格转为临时 system prompt，所有会话使用 `service_id="__builtin_assistant__"`。
- ToolRegistry 是当前统一执行抽象；`tools_enabled` 尚未在 streaming tool selection 中强制生效。
- MCP Manager/配置/API 存在，但当前 Assistant 组合根没有初始化，Gateway 对应 state 为空。
- Skills API/版本表存在，但 DB 加载只重建元数据，且现有持久化调用签名/错误处理存在需 AS-00 复核的缺陷。
- 知识库已支持多 Dataset 和 revision fingerprint 基础，但没有 Agent Version 绑定。
- Trace/Eval 体系足以复用，但缺 Agent/Version/Publication 强类型维度。

完整证据和路径见 `source-packet.md` 与 `architecture-contract.md`。

## Assumptions and Decisions

- 默认主要模型仍遵守仓库约束：DashScope/Qwen，当前默认 `qwen3.7-plus`；Agent Studio 不把 OpenAI Key 作为就绪门槛。
- 产品对象属于 Tenant；个人 Agent 仍通过 ACL 表达，不能绕开 Tenant isolation。
- Agent Version 配置不可变，但安全撤权和资源删除可使历史 Version 不可运行。
- Publication 是渠道稳定入口，同一 Agent 可有 Hosted/Embed/API 多个独立策略。
- Preview 与 Production 分离会话、配额和 Trace namespace。
- 本轮只创建 PRD/Harness，不执行数据库迁移、Docker、部署、提交或推送。

## Phase Order

| Phase | 核心结果 | 主要风险 |
| --- | --- | --- |
| AS-00 | 重新基线 Runtime/能力/分支，固化可执行 Capability Contract | 依据过期或错误架构开始实现 |
| AS-01 | Agent/Draft/Version/Publication/ACL 数据模型与 CRUD | 租户泄漏、不可回滚 Schema |
| AS-02 | Gateway Resolver、签名 Runtime Envelope、Prompt/能力/会话/Trace 隔离 | 双重解析漂移、客户端伪造能力、跨 Agent 污染 |
| AS-03 | MCP/Connector Registry、credential principal、Secret/OAuth/SSRF、发现与健康 | 主体混淆、Secret 泄漏、SSRF、schema 漂移 |
| AS-04 | instruction-only Skill Version 与规范化 KB Dataset/revision 绑定 | 任意 entrypoint、全局缓存泄漏、知识越权/漂移 |
| AS-05 | Agent 目录、Studio、Preview 和可访问响应式 UI | 表单失控、移动端/状态遗漏 |
| AS-06 | Eval Gate、不可变发布、Diff、Promotion 与 Rollback | 未评测配置进入生产 |
| AS-07 | Hosted、新窗口、专用 Embed 路由/动态 CSP、Widget 与 Runtime API | 匿名滥用、Origin/Token 泄漏、生产响应头阻断嵌入 |
| AS-08 | 分析、审计、配额、保留、兼容迁移与版本化全量回归 manifest | 可运维性不足、回归集合漏项、现有 Assistant 回归 |
| AS-09 | 禁止功能修补的 whole-demand 终局验收与独立 release verdict | 跨构建拼接证据、遗漏 Oracle、带病发布 |

依赖链为 `AS-00 -> AS-01 -> AS-02 -> {AS-03, AS-04} -> AS-05 -> AS-06 -> AS-07 -> AS-08 -> AS-09`。AS-03/AS-04 可在 AS-02 契约通过后由不同分支并行，AS-05 必须等待两者都通过。

## Roadmap Cohesion

- AS-00 输出能力来源、策略和运行时基线，约束后续所有 Phase。
- AS-01/02 先建立领域和运行隔离，再接外部 MCP/Skill/KB，避免 UI 先于安全边界。
- AS-05 只消费已稳定 API；不在浏览器保存 Secret 或自行计算权限。
- AS-06 让发布对象成为不可变、可追溯 Version，AS-07 才开放外部渠道；live 知识库内容只保证 revision provenance，不保证确定性重放。
- AS-08 固化运维/治理和版本化 aggregate manifest；AS-09 才在同一兼容构建中跨全部 Oracle 做 whole-demand regression，并验证关闭功能开关后的内置 Assistant。

## Shared Harness Rules

- 阅读代码后写作；遵守仓库既有 FastAPI/Pydantic/Repository/React/i18n 模式。
- 不新增依赖，除非 Phase 报告说明现有能力无法满足并记录批准。
- 不以 `service_id` 复用 Agent Identity；不以 JSON metadata 替代需要查询/索引的核心列。
- 不在前端、Agent Spec、日志、截图和 Trace 中保存 Secret。
- 不运行重建镜像、Docker、迁移、部署、清理、生产外部服务或破坏性 Git 命令，除非对应 Phase 获得明确批准并满足仓库 Docker ownership 规则。
- 只在真实运行后报告测试通过；无法运行必须记录精确命令、阻塞和剩余风险。
- 每个实现 Phase 都需要最小变更说明与独立 Critic 证据。

## Global Non-Goals

- 不在本需求内实现 DAG/Workflow 画布、多 Agent 编排、模板市场、付费 Marketplace。
- 不允许租户任意 `stdio` MCP、任意代码/容器执行或浏览器持有服务端 Token。
- 不为每个 Version 复制知识库全文/向量。
- 不修改当前 Assistant 的外部 API 或现有会话语义，除非后续独立迁移需求明确批准。
- 不在本轮做业务代码、迁移、Docker/E2E、部署、Commit 或 Push。

## Global Compliance Gates

- Tenant + ACL 双重授权、最小权限、默认拒绝。
- Prompt Injection、MCP SSRF/OAuth audience、Secret redaction、Origin/CSP、API Token hash/rotation。
- 匿名流量限流、配额、附件限制、持久记忆禁用和高风险工具默认关闭。
- PII、数据保留、删除、审计和 Trace 脱敏。
- WCAG 2.2 AA、键盘、焦点、对比度、i18n 和 reduced motion。
- 加法迁移、幂等、回滚、功能开关与旧 Assistant 回归。

## Standard Verification Commands

Phase 文件只选择与改动相关的最小集合；以下是仓库已确认的候选命令：

```bash
uv run ruff check <changed-python-paths>
uv run pytest -q --no-cov <targeted-tests>
uv run --package assistant-service pytest -q --no-cov <assistant-tests>
make test-isolation
make verify-assistant-runtime-dev
make verify-eval-dev
make eval-regression-gate
corepack pnpm@10.33.0 -C web lint
corepack pnpm@10.33.0 -C web type-check
corepack pnpm@10.33.0 -C web build
corepack pnpm@10.33.0 -C web i18n:check
corepack pnpm@10.33.0 -C web e2e:opensource
make validate-example-config
```

涉及 DB 的 Phase 还需运行目标 migration contract tests；不得为了验证默认执行真实迁移或 Docker rebuild。

## Required Browser or Runtime Checks

- UI Phase 必须验证 `/agents`、Studio、Publish Sheet、Hosted 和 Embed fixture 的 loading/empty/error/permission/degraded/normal 状态。
- 必测视口：1440x900、1024x768（Studio）和 390x844；不得有横向溢出或不可达操作。
- 必须有键盘与 axe 证据、console/network error 摘要和截图路径。
- Agent Phase 必须有 golden questions、Trace、有效能力集合、拒绝路径、KB provenance 和 Prompt Injection 用例。
- 外部 MCP 可用性不是本地测试前提；使用 mock server 覆盖协议、安全、schema 变化和熔断，真实 OAuth smoke 需单独批准。

## External Inputs and Approvals

| 输入 | 需要时点 | 未提供时行为 |
| --- | --- | --- |
| 目标实现分支/与 `origin/main` 同步决定 | AS-00 | 阻止以过期代码事实解锁 AS-01 |
| credential principal/grant 策略、Secret Store 和 OAuth callback/egress 策略 | AS-03 | 只完成接口与 mock，不连接真实服务 |
| 生产 Eval Dataset/阻断阈值 | AS-06 | 只能完成框架，不能正式发布 |
| Public/Embed 配额、保留、隐私文案及生产 header smoke 批准 | AS-07/08 | 保持私有/禁用 public 渠道，不声明嵌入可用 |
| Docker/迁移/部署批准与 release owner/监控窗口 | AS-09 或首次 live 验证前 | 只运行离线、静态和 mock 验证，不给终局发布 verdict |

## New Window Prompt

使用 `next-window-prompt.md`。新窗口默认从 AS-00 开始；只有报告、Critic 和 completion gate 都证明 AS-00 `passed` 后，才能把 loop-state 推进到 AS-01。
