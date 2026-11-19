# Codex Harness 2026-09 更新执行入口

- status: completed_local
- domain_id: agent-runtime-upstream-sync
- owner: root integration session
- last_verified: 2026-09-22
- successor: none
- prerequisite: 无待执行前置；本次按用户最新范围收口，后续扩展须另行指定。

[主计划](../../../docs/plans/codex-harness-refresh-2026-09-22.md)定义目标/决策/优化与回滚；[验收矩阵](../../../docs/plans/codex-harness-refresh-2026-09-22-acceptance.md)定义C01～C20；[work-packages.yml](work-packages.yml)定义路径和顺序；[loop-state.json](loop-state.json)是唯一执行状态。

目标内核已编译、Docker已更新，主流程实机测试和最小修正的独立复测已完成；详见[实现报告](../../../reports/architecture/codex-harness-refresh-2026-09-22/implementation-verification.md)。本次为本地升级收口，未声称完整发行矩阵通过；历史工作包状态和后续义务保留在loop-state.json。

`bash deploy/runbooks/codex-harness-refresh-2026-09-22/init.sh`只读取当前Git/source-lock和计划状态，不构建或部署。
