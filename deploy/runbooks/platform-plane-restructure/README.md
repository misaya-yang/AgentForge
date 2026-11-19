# 平台平面化重构（PPR）

> **状态：** PPR-00 complete；其余阶段 paused，等待 RAG 合入后的 successor architecture ADR。
> 执行状态只看 `loop-state.json`，当前不得按旧 phase next action 开工。
> **创建：** 2026-08-26；执行契约修订：2026-08-27。

## Goal

Establish attributable performance evidence, choose the least complex topology that meets the measured load, and adopt Rust or new deployment units only when parity, resource, operational, review, and rollback gates all pass without changing public contracts.

## Architecture

逻辑架构固定为五个应用 plane：**Edge、Control、Data、Index、Governance**。PostgreSQL、Redis、Qdrant 和对象存储是共享基础设施底座，不计作第六个应用 plane。逻辑 plane 不等于容器；是否成为独立部署单元必须经过 PPR-02 的 ADR 决策和对应阶段的采用门。

## Non-goals

- 不追求全 Rust，也不以“代码更现代”作为收益。
- 不预设必须拆 Edge、Governance 或 Rust model plane。
- 不重写鉴权、RBAC、配额、计费、Studio、Eval 评分或检索质量策略。
- 不改变 OpenAPI、`agent-event/v2`、Capability Contract V2 或四端 SDK 行为。
- 不把供应商 TTFT 改善归因给本地重构。
- 不在本程序中删除历史会话数据。

## Authorization

- Coding agent 可以执行只读检查、文档和代码编辑、非破坏性本地测试，并且一次只处理 `loop-state.json` 指定的一个 feature。
- 任何 Docker、部署或 E2E 操作前必须先读 `docs/harness/runtime-and-secrets.md`，核对 Compose ownership；凭据只在执行时读取，绝不打印或写入证据。
- 更换运行镜像基础家族、运行有实际费用的供应商实验、发布内部端口、修改默认模型/产品门槛、操作共享环境、执行真实数据迁移、切流或回滚演练，都必须把当前 phase 设为 `waiting_confirmation` 并提出一个具体问题。
- 不得 force-push、删除分支、清理 Docker、删除数据或修改公共契约，除非用户另行明确授权。
- 独立评审是本程序的完成条件，不得由实现者自批；需要评审的阶段只有在 `review` 变为 `approved` 或有具名 waiver 后才能 `done`。

## Source of Truth

| 资产 | 权威范围 |
| --- | --- |
| `loop-state.json` | 当前 phase/feature、依赖、状态、review、证据和下一步 |
| `phase-NN-*.md` | 单阶段 outcome、边界、采用门、验证和停止条件 |
| `feature-oracle.json` | 可证伪验收断言和允许的负向结论 |
| `HANDOFF.md` | 最新一次可替换交接，不保存历史 |
| `architecture-contract.md` | plane 定义和不可漂移规则 |
| `product-requirements.md` | 产品目标、备选方案、指标与风险；不记录进度 |
| `docs/plans/rust-expansion-and-service-topology-2026-08.md` | 背景调查与迁移判据；不记录进度 |

Git 历史保存旧交接。不要新建第二份进度表、actor ledger 或重复的 phase manifest。

## Phase Map

| Phase | Outcome | Depends on | Adoption | Required review |
| --- | --- | --- | --- | --- |
| PPR-00 | 冻结可加和的本地/供应商 timing schema、统计协议和并发资源基线 | — | required foundation | performance methodology |
| PPR-01 | 缩小运行镜像、建立单一 pin 权威和 crate 级构建身份 | PPR-00 | required foundation | supply chain/security |
| PPR-02 | ADR-008 比较保留现状、只拆 Governance、拆 Edge+Governance，并建立可执行 plane gate | PPR-00, PPR-01 | decision, not preselected | architecture/security |
| PPR-03 | 仅在 ADR 与负载证据支持时拆 Edge | PPR-02 | measured-not-adopted allowed | auth/quota/security |
| PPR-04 | 单次供应商请求下影子对拍 Rust model projector，收益不足则保留 Python | PPR-02 | measured-not-adopted allowed | protocol/accounting/security |
| PPR-05 | 仅在 noisy-neighbor 证据成立时隔离 Governance | PPR-02 | measured-not-adopted allowed | architecture/eval |
| PPR-06 | 独立扩展 knowledge worker；仅在 CPU 受限时采用原生内核 | PPR-02 | measured-not-adopted allowed | data/retrieval |
| PPR-07 | 独立、可逆地统一会话存储；失败则保留 legacy read path | PPR-02 | migration may be deferred | database/migration |
| PPR-08 | 随机交错的供应商 A/B；默认模型或门槛变化需产品批准 | PPR-00 | negative conclusion allowed | product/eval |
| PPR-09 | 只覆盖实际采用的部署单元，串行发布并演练回滚 | PPR-03…PPR-08 | required closeout | release/security |

PPR-03～PPR-08 是基础阶段之后的独立轨道，不得因为编号相邻而伪造依赖。单个 coding agent 仍然一次执行一个 active feature。

## Operating Rules

每次冷启动严格按以下顺序：

1. 读根目录 `AGENTS.md`、`loop-state.json`、`HANDOFF.md` 和 active phase。
2. 检查 `git status --short`、当前分支、upstream 和最近提交，保护用户未提交改动。
3. 运行 `./deploy/runbooks/platform-plane-restructure/init.sh`，再运行 active phase 最小基线检查。
4. 只做一个 `observe -> act -> verify -> decide` 循环；失败后只有假设或输入发生变化才能重试。
5. 采用门必须在实现前写进阶段证据；不得根据实现结果事后降低门槛。
6. 负向结论必须保留原实现、记录数字和原因；它不是 waiver，也不能伪装成已采用。
7. 结果满足 phase、oracle 和真实运行路径后才把 feature `passes` 设为 `true`。
8. 更新 `loop-state.json` 和替换 `HANDOFF.md`；证据写已有测试、receipt 或报告路径，不为“有东西可引用”而制造报告。

所有阶段共享的架构与回归门见 `architecture-contract.md`。当前没有 active phase；PPR-00 仅保留为冻结证据，PPR-01 及后续阶段处于 paused，等待 RAG 合入后的 successor ADR。不得从旧 phase 文件自行恢复执行。
