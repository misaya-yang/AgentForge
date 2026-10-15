# P1-03 — 代码执行与内部broker有硬资源/路径/取消边界；依赖P1-01/02

- PHASE_ID: P1-03
- FEATURE_ID: P1-F004
- DEPENDS_ON: P1-02

## Outcome

代码执行与内部broker有硬资源/路径/取消边界；依赖P1-01/02

## Scope

In:

- 需求：ID-05,CP-05。
- Owner范围：Rust `ai-platform-capability-worker`平台执行文件、Gateway相关capability broker、local-node边界tests。Act前以work-packages.yml的文件allowlist为准。

Out:

- 后一期功能、上游kernel算法重写、无关重构、未授权提交/推送/生产变更。

## Done when

- [ ] S09；真实隔离容器内总磁盘/inode、进程组回收、input/output、错误audience/SSRF负测
- [ ] 直接/隔离/指定live证据及review真实完成；未跑live不标通过。

## Verify

| Check | Command or observation | Proves |
| --- | --- | --- |
| Direct baseline | `make agent-runtime-write-gate` | 当前门禁覆盖的直接行为；不替代下列真实场景 |
| Outcome | S09；真实隔离容器内总磁盘/inode、进程组回收、input/output、错误audience/SSRF负测 | 本包需求的真实成功/失败结果 |

S场景与证据层级见[门禁分册](../../../docs/plans/agent-platform-vnext-2026-09/06-evaluation-and-release-gates.md)；精确命令和结果写本包receipt。

## Stop or confirm

- 超出owned paths或公共/数据合同发生冲突，先更新范围/裁决。
- 回滚类别：R3；安全修补前版不可用；无安全回滚时不得release。
- 本地环境与既有凭证使用已授权；禁止输出秘密、未授权Git/生产写操作。
