# P1-02 — 模型工具授权、wire、事件终态与恢复一致；依赖P1-01安全边界

- PHASE_ID: P1-02
- FEATURE_ID: P1-F003
- DEPENDS_ON: P1-01

## Outcome

模型工具授权、wire、事件终态与恢复一致；依赖P1-01安全边界

## Scope

In:

- 需求：ID-04,CP-01,CP-02,CP-03,CP-04,UX-02。
- Owner范围：`src/services/agent_runtime`, `src/adapters/openai.py`, CLI `provider`, Rust平台tool/approval hooks，`web/src/api/agentThreads.ts`, `features/chat`, Studio preview。Act前以work-packages.yml的文件allowlist为准。

Out:

- 后一期功能、上游kernel算法重写、无关重构、未授权提交/推送/生产变更。

## Done when

- [ ] S04～S08；现有SSE/Agent contract+每adapter负向fixture；至少两wire、一root+child、150+事件、网络中断
- [ ] 直接/隔离/指定live证据及review真实完成；未跑live不标通过。

## Verify

| Check | Command or observation | Proves |
| --- | --- | --- |
| Direct baseline | `make agent-eval-core-gate` | 当前门禁覆盖的直接行为；不替代下列真实场景 |
| Outcome | S04～S08；现有SSE/Agent contract+每adapter负向fixture；至少两wire、一root+child、150+事件、网络中断 | 本包需求的真实成功/失败结果 |

S场景与证据层级见[门禁分册](../../../docs/plans/agent-platform-vnext-2026-09/06-evaluation-and-release-gates.md)；精确命令和结果写本包receipt。

## Stop or confirm

- 超出owned paths或公共/数据合同发生冲突，先更新范围/裁决。
- 回滚类别：R0/R1；不能回退权限修复；无安全回滚时不得release。
- 本地环境与既有凭证使用已授权；禁止输出秘密、未授权Git/生产写操作。
