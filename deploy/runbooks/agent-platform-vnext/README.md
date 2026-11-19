# Agent Platform vNext 一期执行 runbook

status: active
domain_id: agent-platform-vnext
owner: primary Codex session
last_verified: 2026-09-07
prerequisite: 用户已授权一期实现、本地Docker与内置浏览器验收，验收后本地提交及合并
successor: null

分支：`codex/agent-platform-vnext-phase1`；实现基线：`26dfbbc2c2e03c4daa1eef6f64dc7cf79e395c3f`。
需求入口是[主PRD](../../../docs/plans/agent-platform-vnext-2026-09/README.md)，范围见
[工作包分册](../../../docs/plans/agent-platform-vnext-2026-09/04-delivery-phases-and-work-packages.md)
与本目录 `work-packages.yml`。`loop-state.json` 是唯一机器执行状态；最终状态和现场收据由主session更新。

## 当前事实

P1-00～P1-10的本地集成实现及验收已完成。实际结果、现场修复、源码/镜像对应关系与明确未验证项见[统一验收报告](../../../reports/architecture/phase1-local-acceptance-2026-09-07/README.md)。早期P1-01收据移至`receipts/P1-01-first-batch.yml`保留为历史；当前每包收据引用统一证据，机器状态按本地验收范围更新。

合同决策见 [ADR-010](../../../docs/architecture/ADR-010-phase1-tenant-identity-and-knowledge-authorization.md) 和 [ADR-011](../../../docs/architecture/ADR-011-phase1-shared-contracts-and-metrics-ownership.md)。数据库epoch为2，禁止回退至未实现租户ACL隔离的旧代码。公开发行仍需要补齐下面列明的外部证据，不能据本地完成宣称RELEASE_100。

## Authorization and acceptance scope

用户后续指令覆盖早期单writer/只读子代理限制：两个子代理可直接修改互不重叠的代码；主session负责共享入口、
数据库manifest/指纹、部署、集成、最终review与Git操作。子代理不提交、不切分支、不操作Docker。
允许使用既有本地数据库和专用E2E账号；秘密只在执行时读取，不输出或写入文档。禁止host Cargo。

本轮P1-10按用户最新授权，以当前机器的Docker部署、内置浏览器/API核心旅程和必要故障回归收尾；
通过后可本地提交并合并main，不push、不公开发布。结论须明确local-only范围。
原PRD的fresh-machine、完整冻结release unit/数据库回切、公开registry、外部CI及其他OS/架构发行证据
保留为未验证，不冒充已通过，也不据此无限扩大本轮任务。迁移安全与权限隔离要求不因验收范围调整而放宽。

## Operating Rules

1. 读取AGENTS、HANDOFF与loop-state，核对实际Git状态并保留已有dirty。
2. 继续主session的统一部署/回归清单，不重新审计全仓、不恢复前序程序的旧next_action。
3. 涉及容器前核对Compose owner、源码/镜像identity、schema epoch；遵守runtime-and-secrets规则。
4. 将实际命令、结果、失败/跳过和适用范围写入收据；由主session更新loop-state最终状态。

## Non-goals

不扩展二/三期，不新建桌面产品、多实例HA或学习路由，不重写codex-harness内核，不做无关LOC整理。
前序ARC/Runtime/CLI/RAG程序只移交执行owner，其历史结果和未验证发行义务保留。
设计阶段有限现场观察见[调查报告](../../../reports/architecture/agent-platform-vnext-2026-09-07.md)，不代表当前源码全栈通过。

## Goal

完成主PRD的一期可信运行闭环；真实代码/契约/测试/活栈证据决定状态。

## Phase Map

| Phase | Feature | Contract |
| --- | --- | --- |
| P1-00 | P1-F001 | [P1-00](phase-00-freeze-phase-one-contracts-and-evidence.md) |
| P1-01 | P1-F002 | [P1-01](phase-01-enforce-evaluation-identity-and-dataset-authorization.md) |
| P1-02 | P1-F003 | [P1-02](phase-02-unify-tool-authorization-and-stream-recovery.md) |
| P1-03 | P1-F004 | [P1-03](phase-03-bound-capability-execution-and-cancellation.md) |
| P1-04 | P1-F005 | [P1-04](phase-04-bind-model-pricing-and-evaluator-profiles.md) |
| P1-05 | P1-F006 | [P1-05](phase-05-unify-admission-and-retry-semantics.md) |
| P1-06 | P1-F007 | [P1-06](phase-06-recover-evaluation-jobs-after-worker-interruption.md) |
| P1-07 | P1-F008 | [P1-07](phase-07-validate-knowledge-configuration-and-lifecycle.md) |
| P1-08 | P1-F009 | [P1-08](phase-08-make-health-truthful-and-close-import-exceptions.md) |
| P1-09 | P1-F010 | [P1-09](phase-09-verify-independent-cli-and-paired-runtime-artifacts.md) |
| P1-10 | P1-F011 | [P1-10](phase-10-validate-integrated-phase-one-release-and-rollback.md) |
