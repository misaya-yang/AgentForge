# P1-04 — 模型/价格身份、Eval候选/judge模型与权限固定；依赖P1-01/02

- PHASE_ID: P1-04
- FEATURE_ID: P1-F005
- DEPENDS_ON: P1-03

## Outcome

模型/价格身份、Eval候选/judge模型与权限固定；依赖P1-01/02

## Scope

In:

- 需求：GW-01,EV-01。
- Owner范围：`src/services/llm/model_service.py`, `services/billing`, `services/eval`, core当前usage owner、contracts、相应迁移。Act前以work-packages.yml的文件allowlist为准。

Out:

- 后一期功能、上游kernel算法重写、无关重构、未授权提交/推送/生产变更。

## Done when

- [ ] S12/S40；双租户/双provider同名模型、失败同步、改价不改历史；judge工具负例在CP-01修复后验证
- [ ] 直接/隔离/指定live证据及review真实完成；未跑live不标通过。

## Verify

| Check | Command or observation | Proves |
| --- | --- | --- |
| Direct baseline | `make gateway-unit-gate` | 当前门禁覆盖的直接行为；不替代下列真实场景 |
| Outcome | S12/S40；双租户/双provider同名模型、失败同步、改价不改历史；judge工具负例在CP-01修复后验证 | 本包需求的真实成功/失败结果 |

S场景与证据层级见[门禁分册](../../../docs/plans/agent-platform-vnext-2026-09/06-evaluation-and-release-gates.md)；精确命令和结果写本包receipt。

## Stop or confirm

- 超出owned paths或公共/数据合同发生冲突，先更新范围/裁决。
- 回滚类别：R1；历史价格snapshot保留；无安全回滚时不得release。
- 本地环境与既有凭证使用已授权；禁止输出秘密、未授权Git/生产写操作。
