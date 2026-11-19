# P1-05 — 所有入口的容量/重试语义一致；依赖P1-02/04

- PHASE_ID: P1-05
- FEATURE_ID: P1-F006
- DEPENDS_ON: P1-04

## Outcome

所有入口的容量/重试语义一致；依赖P1-02/04

## Scope

In:

- 需求：GW-02,GW-03。
- Owner范围：`src/core/gateway`, `src/proxy`, `src/adapters`, `src/connectors`相关HTTP调用边界。Act前以work-packages.yml的文件allowlist为准。

Out:

- 后一期功能、上游kernel算法重写、无关重构、未授权提交/推送/生产变更。

## Done when

- [ ] S13/S14；run/provider容量各1、多轮不死锁；stream绕限反例、请求发送后断连unknown
- [ ] 直接/隔离/指定live证据及review真实完成；未跑live不标通过。

## Verify

| Check | Command or observation | Proves |
| --- | --- | --- |
| Direct baseline | `make gateway-unit-gate` | 当前门禁覆盖的直接行为；不替代下列真实场景 |
| Outcome | S13/S14；run/provider容量各1、多轮不死锁；stream绕限反例、请求发送后断连unknown | 本包需求的真实成功/失败结果 |

S场景与证据层级见[门禁分册](../../../docs/plans/agent-platform-vnext-2026-09/06-evaluation-and-release-gates.md)；精确命令和结果写本包receipt。

## Stop or confirm

- 超出owned paths或公共/数据合同发生冲突，先更新范围/裁决。
- 回滚类别：R0；保持既有资源维度；无安全回滚时不得release。
- 本地环境与既有凭证使用已授权；禁止输出秘密、未授权Git/生产写操作。
