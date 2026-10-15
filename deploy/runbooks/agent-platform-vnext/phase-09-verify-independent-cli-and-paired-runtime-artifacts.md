# P1-09 — 已支持CLI可独立运行；Runtime/Worker来源及工具/stream边界可验证；依赖P1-02/03

- PHASE_ID: P1-09
- FEATURE_ID: P1-F010
- DEPENDS_ON: P1-08

## Outcome

已支持CLI可独立运行；Runtime/Worker来源及工具/stream边界可验证；依赖P1-02/03

## Scope

In:

- 需求：CL-01,CL-02,CL-06,CP-06。
- Owner范围：`sdk/cli`, Rust平台artifact hooks，`scripts/harness/agent_runtime_supply_chain.py`及现有构建脚本。Act前以work-packages.yml的文件allowlist为准。

Out:

- 后一期功能、上游kernel算法重写、无关重构、未授权提交/推送/生产变更。

## Done when

- [ ] S10/S29/S30/S32的Linux目标；native真实执行，不能只跑launcher Node tests
- [ ] 直接/隔离/指定live证据及review真实完成；未跑live不标通过。

## Verify

| Check | Command or observation | Proves |
| --- | --- | --- |
| Direct baseline | `make independent-cli-gate` | 当前门禁覆盖的直接行为；不替代下列真实场景 |
| Outcome | S10/S29/S30/S32的Linux目标；native真实执行，不能只跑launcher Node tests | 本包需求的真实成功/失败结果 |

S场景与证据层级见[门禁分册](../../../docs/plans/agent-platform-vnext-2026-09/06-evaluation-and-release-gates.md)；精确命令和结果写本包receipt。

## Stop or confirm

- 超出owned paths或公共/数据合同发生冲突，先更新范围/裁决。
- 回滚类别：R3；无安全回滚时不得release。
- 本地环境与既有凭证使用已授权；禁止输出秘密、未授权Git/生产写操作。
