# P1-07 — 普通知识生命周期可用，配置提前失败，目录/全量请求不截断；依赖P1-01

- PHASE_ID: P1-07
- FEATURE_ID: P1-F008
- DEPENDS_ON: P1-06

## Outcome

普通知识生命周期可用，配置提前失败，目录/全量请求不截断；依赖P1-01

## Scope

In:

- 需求：KB-02,KB-04,KB-05。
- Owner范围：KS dataset/retrieval config、目录API、worker/ingestion、`web/src/pages/knowledge`对应表单。Act前以work-packages.yml的文件allowlist为准。

Out:

- 后一期功能、上游kernel算法重写、无关重构、未授权提交/推送/生产变更。

## Done when

- [ ] S19/S21/S22；201库、10001段边界；坏parsing config；embedding失败保留旧generation
- [ ] 直接/隔离/指定live证据及review真实完成；未跑live不标通过。

## Verify

| Check | Command or observation | Proves |
| --- | --- | --- |
| Direct baseline | `make kb-unit-gate` | 当前门禁覆盖的直接行为；不替代下列真实场景 |
| Outcome | S19/S21/S22；201库、10001段边界；坏parsing config；embedding失败保留旧generation | 本包需求的真实成功/失败结果 |

S场景与证据层级见[门禁分册](../../../docs/plans/agent-platform-vnext-2026-09/06-evaluation-and-release-gates.md)；精确命令和结果写本包receipt。

## Stop or confirm

- 超出owned paths或公共/数据合同发生冲突，先更新范围/裁决。
- 回滚类别：R1/R2；无安全回滚时不得release。
- 本地环境与既有凭证使用已授权；禁止输出秘密、未授权Git/生产写操作。
