# P1-10 — 一期集成C80→R100与完整发布证据；依赖P1-01～09

- PHASE_ID: P1-10
- FEATURE_ID: P1-F011
- DEPENDS_ON: P1-09

## Outcome

一期本地集成验收；依赖P1-01～09。

用户于2026-09-07后续明确：本轮按当前机器的Docker部署、内置浏览器/API核心旅程和必要故障回归收尾，
通过后本地提交并合并main；不push、不公开发布。任何C80/R100结论须注明local-only范围。
原完整发行义务保留在下方，未执行不标PASS，不据此无限扩展当前任务。

## Scope

In:

- 需求：RL-01,RL-02。
- Owner范围：已声明的integration paths、release manifest/receipts、发布CI、当前runbook。Act前以work-packages.yml的文件allowlist为准。

Out:

- 后一期功能、上游kernel算法重写、无关重构、未授权提交/推送/生产变更。

## Done when

- [ ] 当前本地Docker与内置浏览器/API核心旅程、真实provider及必要失败/取消/恢复回归已完成并有对应收据。
- [ ] 本轮未执行的外部分发/冻结回滚义务明确记录为未验证；最终状态由主session更新。
- [ ] 直接/隔离/指定live证据及review真实完成；未跑live不标通过。

## Verify

| Check | Command or observation | Proves |
| --- | --- | --- |
| Direct baseline | `make platform-release-gate` | 当前门禁覆盖的直接行为；不替代下列真实场景 |
| Local outcome | 当前Docker/内置浏览器/API核心旅程与必要故障回归；命令和结果写receipt | 本轮授权范围内的实际成功/失败结果 |
| Retained release evidence | fresh-machine、完整冻结artifact/数据库回切、外部CI/公开registry、其他OS/arch | 本轮未执行则保持未验证，不从local outcome推导 |

S场景与证据层级见[门禁分册](../../../docs/plans/agent-platform-vnext-2026-09/06-evaluation-and-release-gates.md)；精确命令和结果写本包receipt。

## Stop or confirm

- 超出owned paths或公共/数据合同发生冲突，先更新范围/裁决。
- 回滚类别：R3/R4按迁移标记；无安全回滚时不得release。
- 本地环境与既有凭证使用已授权；禁止输出秘密、未授权Git/生产写操作。
