# P1-00 — 建立唯一执行owner、现状/契约/制品/失败基线；无依赖

- PHASE_ID: P1-00
- FEATURE_ID: P1-F001
- DEPENDS_ON: none

## Outcome

建立唯一执行owner、现状/契约/制品/失败基线；无依赖

## Scope

In:

- 需求：HX-01,HX-02,HX-04,HX-05。
- Owner范围：`deploy/runbooks/agent-platform-vnext`, `docs/harness`, `scripts/harness/affected_gates.py`, `scripts/harness/ci_gate_enforcement.py`；只改已确认缺口。Act前以work-packages.yml的文件allowlist为准。

Out:

- 后一期功能、上游kernel算法重写、无关重构、未授权提交/推送/生产变更。

## Done when

- [ ] harness/import/core/singleton/OpenAPI；冻结S01～S15等必需oracles与镜像身份；标记当前event_stream源码漂移
- [ ] 直接/隔离/指定live证据及review真实完成；未跑live不标通过。

## Verify

| Check | Command or observation | Proves |
| --- | --- | --- |
| Direct baseline | `make harness-check` | 当前门禁覆盖的直接行为；不替代下列真实场景 |
| Outcome | harness/import/core/singleton/OpenAPI；冻结S01～S15等必需oracles与镜像身份；标记当前event_stream源码漂移 | 本包需求的真实成功/失败结果 |

S场景与证据层级见[门禁分册](../../../docs/plans/agent-platform-vnext-2026-09/06-evaluation-and-release-gates.md)；精确命令和结果写本包receipt。

## Stop or confirm

- 超出owned paths或公共/数据合同发生冲突，先更新范围/裁决。
- 回滚类别：R0；无安全回滚时不得release。
- 本地环境与既有凭证使用已授权；禁止输出秘密、未授权Git/生产写操作。
