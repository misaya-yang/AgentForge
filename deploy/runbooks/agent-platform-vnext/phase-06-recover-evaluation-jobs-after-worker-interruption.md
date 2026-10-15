# P1-06 — Eval job可reclaim且结果语义诚实；依赖P1-01/02

- PHASE_ID: P1-06
- FEATURE_ID: P1-F007
- DEPENDS_ON: P1-05

## Outcome

Eval job可reclaim且结果语义诚实；依赖P1-01/02

## Scope

In:

- 需求：EV-02,EV-03。
- Owner范围：`src/services/eval`, core当前`eval/outbox_worker`, `agent_trace_repository`, 相应DB迁移与Web状态。Act前以work-packages.yml的文件allowlist为准。

Out:

- 后一期功能、上游kernel算法重写、无关重构、未授权提交/推送/生产变更。

## Done when

- [ ] S25；领取批次后kill，旧claim写拒绝，取消/超时/无效judge不伪成功
- [ ] 直接/隔离/指定live证据及review真实完成；未跑live不标通过。

## Verify

| Check | Command or observation | Proves |
| --- | --- | --- |
| Direct baseline | `make agent-eval-core-gate` | 当前门禁覆盖的直接行为；不替代下列真实场景 |
| Outcome | S25；领取批次后kill，旧claim写拒绝，取消/超时/无效judge不伪成功 | 本包需求的真实成功/失败结果 |

S场景与证据层级见[门禁分册](../../../docs/plans/agent-platform-vnext-2026-09/06-evaluation-and-release-gates.md)；精确命令和结果写本包receipt。

## Stop or confirm

- 超出owned paths或公共/数据合同发生冲突，先更新范围/裁决。
- 回滚类别：R1；旧running任务收养方案；无安全回滚时不得release。
- 本地环境与既有凭证使用已授权；禁止输出秘密、未授权Git/生产写操作。
