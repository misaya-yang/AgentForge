# Platform upgrade plan 2026-10

本计划交付完整的 Rust-native AgentForge。**当前执行规模是 6 个模块、3 个交付批次**；
原 109 项工作包保留为需求和验收对照，不再作为 109 次独立启动、评审、编译和交接流程。

- 执行记录：[native-enterprise-upgrade/loop-state.json](../../../deploy/runbooks/native-enterprise-upgrade/loop-state.json)。
- 项目约定：[AGENTS.md](../../../AGENTS.md)；执行方法：[task harness](../../harness/README.md)。
- 沿用 [vNext PRD](../agent-platform-vnext-2026-09/README.md) 和[跨服务架构边界](../../design/cross-service-architecture-evolution-2026-10-12.md)。
- 2026-10-18 收敛：从当前代码接着完成；删除旧排期、重复现状评审和过程性前置任务。

## 0. How to read this plan

先选下面的完整交付模块，再阅读该模块的接线要求。分项文档中的原设计细节按需展开，
原工作包表用于核对功能/验收；[01 初审资料](01-baseline-and-findings.md) 不是当前任务清单。
当前是否已经实现只看代码和账本，不能由两天前的“缺失”描述推断。

## 1. Executive summary

先把 Gateway、执行引擎、知识库、产品入口和数据底座的所有必需实现接通，再用一套集成
候选集中编译、验证接口并运行真实服务。最后完成原定安全、故障恢复、性能和发布验收。
不新增微服务来体现模块拆分，不反复恢复旧评审，不让文档或局部代码质量工作代替功能交付。

## 2. Scope

| 完整交付模块 | 需求映射 | 一次交付的结果 | 详细接线 |
| --- | --- | --- | --- |
| Gateway | U-GW-01…24（24 项） | 身份、模型路由/准入/计费、后台角色、MCP/A2A/平台能力形成完整入口 | [02 Gateway](02-gateway.md) |
| Runtime / Worker / SDK execution | U-RT、U-CW、U-SW（21 项） | 正常执行、工具审批、隔离、持久终态、观察和恢复完整成立 | [03 执行](03-agent-execution.md) |
| Knowledge | U-KN-01…14（14 项） | 入库、发布、检索、引用、ACL 与后台模型调用贯通 | [04 Knowledge](04-knowledge.md) |
| 产品入口 | U-WEB、U-CL（19 项） | 控制台、Local Node、CLI、desktop 和 SDK 消费同一真实接口 | [05 Web](05-web-console.md)、[06 客户端](06-clients-and-edge.md) |
| 数据平台 | U-DATA（14 项） | 完整结构、索引、角色/RLS、连接池和存储/恢复工具由业务实际使用 | [07 数据](07-data-platform.md) |
| 构建、部署与运维 | U-PE（17 项） | 一个构建流程形成正确镜像，服务可部署、观测，并具备原定发布/回滚能力 | [08 平台工程](08-platform-engineering.md) |

这些模块是交付边界，不是要求同时运行的进程或容器数量。共享契约、清单和锁文件由主任务
统一整合；若以后启用代理，按完整模块划定互不冲突的代码归属和验证责任，不再派并行审计角色。

## 3. Current-state scorecard

当前状态只在执行账本中维护。原评审的源码统计、版本差距和风险记录已归入 [01](01-baseline-and-findings.md)。
本计划不维护第二份进度表，也不要求 agent 每次开工重新打分、全仓盘点或核查所有历史产物。

## 4. Architectural principles for this upgrade

- 保留公开协议、事件语义、签名/计费规则、租户隔离、现有数据与未知副作用处理。
- 在自有代码完成类型、职责和调用边界；沿用开源 Codex harness，上游变更/投稿按单独授权处理。
- 每个持久事实有明确写入权；服务验证者不持有签发秘密，权限按当前身份验证。
- 数据库变化通过 native authority；新开发库初始化当前完整结构，历史冻结输入不改写。
- 复用既有进程角色、镜像、构建缓存和平台组件；只有原计划所需的隔离边界才增加运行单元。
- 原定技术版本、兼容窗口、性能/质量门槛和候选项触发条件仍在分项设计与验收表中生效。

## 5. Target architecture (end of Ph3)

保留 Gateway control、Agent execution、Knowledge 三个服务上下文及其产品入口。
Gateway 管身份、策略、模型与账务；Runtime 管 Agent 执行；Worker 管有权限/审批边界的能力；
Knowledge API/Worker 管知识数据。数据服务和遥测按已有部署配置共享，不按计划表新增常驻服务。
详细边界以[项目架构](../../harness/architecture.md)及分项设计为准。

基础设计已收敛为现有三个上下文、明确的数据写入权和统一公开契约：产品层次见
[产品边界](../../harness/platform-architecture.md)，源码归属、调用方向、数据库初始化与
部署选择见[项目架构](../../harness/architecture.md)。后续开发直接补现有模块的真实缺口，
不重新选择整体架构、不按工作包新增服务；条件技术仍按原触发条件处理。

## 6. Roadmap

| 批次 | 主任务 | 交付条件 |
| --- | --- | --- |
| **A：完成所有必需源码与接线** | 按六个模块连续完成实现、配置、调用方和必要回归代码；对已实现功能只补真实缺口 | 必需功能没有占位实现或断开的调用链；各模块明确需要统一运行的验证及原条件项 |
| **B：统一编译与接口验证** | 用现有受控流程集中编译 Native 与 Runtime/Worker，运行受影响行为/协议测试，处理实际阻塞 | 构建和必需接口/行为检查通过；修复后仅重跑受影响检查，复用仍适用结果 |
| **C：真实服务联调与原定验收** | 打包并启动所需真实服务，完成主流程和安全/恢复/质量/性能/发布场景 | 原必需验收成立；未运行、受阻和条件未触发项如实区分，不能用代码或编译代替 |

A 中的独立模块可以并行实现；共享构建和有状态联调按本机资源串行。只受实际契约/数据依赖
约束，不再等待旧 Ph0/Ph1 的整阶段“验收放行”才能写下一模块。Ph0…Ph3 保留在原表中作为
来源标签，旧日历排期和逐包审核流程退出当前执行。

统一联调按真实调用链覆盖，结果关联多个需求 ID：

| 联调链 | 共同验证的内容 |
| --- | --- |
| 登录 → 会话 → Agent → 工具 → 终态/重连 | Web、Gateway、Runtime、Worker、审批、撤销、租户与恢复 |
| 文档上传 → 解析/嵌入 → 发布 → 检索/引用 | 模型面、Knowledge 前后台、索引/对象存储、代际与 ACL |
| LLM API → 路由/准入 → provider → 结算 | 协议、预算、缓存、guardrails、流式错误和计费 |
| Local Node/CLI/desktop/SDK → 平台任务 | 设备身份、共享接口、控制/停止、客户端重连及平台适配 |
| 数据/服务故障 → 恢复/回滚 → 再访问 | 角色与 RLS、持久化、备份代际、未知副作用、兼容性与 RPO/RTO |

每条链一个验证负责人，一份适用结果覆盖其中的模块和需求；不再为每个工作包重启一套环境。
分项表中的质量、安全和发布专用场景仍在 C 中执行，未被主流程覆盖的不能省略。

## 7. Decision backlog (proposed ADRs)

复用已经落实的决定。只有真正改变产品范围、兼容性或资源投入的未决选择才需要决策，
普通实现不等待 ADR 编号。[原 12 个决策主题](01-baseline-and-findings.md#proposed-adr-topics) 作为背景保留。

条件项遵守各自原条件，包括语义缓存、上游 memories、按需 OS computer use、PG19、分析层、
Lite 检索，以及 executable Wasm/其他候选技术。条件未触发时不默认开发、部署或采购；
P3 并不自动等于可选，必需的 P3 功能仍要完成。

## 8. Success metrics

These are **targets to validate**, not measurements. Ph0 establishes baselines with the
[enterprise performance contract](../../design/enterprise-performance-contract.md); each target
is confirmed or revised by ADR before it gates a release.

| Area | Metric | Target |
| --- | --- | --- |
| Gateway overhead | Added latency against a deterministic mock provider, non-streaming, p99 | ≤ 30 ms |
| Gateway overhead | First-byte overhead for streaming, p95 | ≤ 50 ms |
| Gateway hot path | Valkey round trips per admitted request | ≤ 2 |
| Agent | Platform share of time-to-first-token (provider time reported separately), p95 | ≤ 300 ms |
| Knowledge | Interactive retrieval without rerank / with rerank, p95 | ≤ 400 ms / ≤ 1.2 s |
| Knowledge | Golden-set Recall@20 and nDCG@10 | No regression on any profile promotion; multimodal target set after baseline |
| Reliability | Unexplained terminal-state mismatches in the fault-injection suite | 0 |
| Reliability | RPO / RTO (single region) | ≤ 5 min / ≤ 30 min, drilled each quarter |
| Security | Verifier services holding signing secrets | 0 |
| Security | Critical CVEs in released images; images signed and verified at deploy | 0; 100 % |
| Security | Cross-tenant negative suite | 100 % pass on every release |
| Delivery | PR CI wall time p50; full multi-arch release build | ≤ 20 min; ≤ 45 min |
| Cost | Requests with an immutable price receipt; prompt-cache hit ratio reported per tenant/model | 100 %; reported |
| Tool execution | Read-capability p95 under mixed load (long sandbox runs in parallel) compared with reads alone | Within 10 % |

## 9. Risks and mitigations

实际阻塞优先处理：不能编译、主流程中断、协议不兼容、权限/凭据问题、数据错误及恢复不确定。
历史风险表见 [01](01-baseline-and-findings.md#review-risks)，不再由每个代理重复审计。

非阻塞的命名、排版、文件拆分、旧模块名称、标签整理、独立 ADR 写作和推测性优化，合并到
相关模块或贯通后的收尾；它们不成为新的阶段或环境准备任务。原需求中的契约迁移仍需完成，
尤其不能提前删除 v1 兼容性、导入窗口或旧数据支持。

## 10. Delivery order

执行 §6 的 A → B → C，完成检查点后继续下一项。账本仅在模块、验证结果或阻塞变化时更新；
不新建逐包计划、冻结、审计、交接或恢复报告。相关实现与环境未变化，不重跑已适用结果。
整体完成仍以原必需验收为准；计划整理本身不代表代码或服务已完成。

## 11. External references (checked 2026-10-16)

[原技术调研与引用](01-baseline-and-findings.md#external-references-2026-10-16) 已集中到背景资料。
只有本次实现确实依赖一个尚未确定的外部事实时才查询，不周期性重做版本调研。
