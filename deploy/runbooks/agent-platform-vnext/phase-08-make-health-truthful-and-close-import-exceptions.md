# P1-08 — 状态/宣传真实，四个到期例外清零；依赖P1-04/06/07

- PHASE_ID: P1-08
- FEATURE_ID: P1-F009
- DEPENDS_ON: P1-07

## Outcome

状态/宣传真实，四个到期例外清零；依赖P1-04/06/07

## Scope

In:

- 需求：GW-04,UX-01,HX-03,OB-02。
- Owner范围：health/metrics owner、ProviderStatusCard/Dashboard/Login/Eval概览、四个allowlist源文件及owner模块。Act前以work-packages.yml的文件allowlist为准。

Out:

- 后一期功能、上游kernel算法重写、无关重构、未授权提交/推送/生产变更。

## Done when

- [ ] S15；零样本/采集错误/Gate未跑；import gate四项违规与entry均0，保留负向自测
- [ ] 直接/隔离/指定live证据及review真实完成；未跑live不标通过。

## Verify

| Check | Command or observation | Proves |
| --- | --- | --- |
| Direct baseline | `make architecture-boundary-gate` | 当前门禁覆盖的直接行为；不替代下列真实场景 |
| Outcome | S15；零样本/采集错误/Gate未跑；import gate四项违规与entry均0，保留负向自测 | 本包需求的真实成功/失败结果 |

S场景与证据层级见[门禁分册](../../../docs/plans/agent-platform-vnext-2026-09/06-evaluation-and-release-gates.md)；精确命令和结果写本包receipt。

## Stop or confirm

- 超出owned paths或公共/数据合同发生冲突，先更新范围/裁决。
- 回滚类别：R0；不可用显示unknown；无安全回滚时不得release。
- 本地环境与既有凭证使用已授权；禁止输出秘密、未授权Git/生产写操作。
