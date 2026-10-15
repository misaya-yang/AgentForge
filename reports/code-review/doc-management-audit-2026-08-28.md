# 文档管理排查报告（2026-08-28）

> 范围：`docs/`、根级文档、`deploy/runbooks/`、`reports/` 索引一致性、链接健康、状态一致性、工作区文档卫生。
> 方法：全量 md 清点（排除 node_modules/.venv/.git）；相对链接脚本检查（docs/ + runbooks/ + reports/ + 根级，共 154 条）；`make harness-check`；每个 `loop-state.json` 与 `docs/README.md` Programs 表逐条对照。
> 权威来源：程序状态以各自 `loop-state.json` 为准（`docs/harness/workflow.md` 约定）。

## 基线（健康项）

- `docs/README.md` 索引与磁盘文件**一一对应**：harness 6、design 4、ADR 4、plans 7、research 1、archive 7，无遗漏无多余。
- 根级 10 个 md（README/DEPLOY/CONTRIBUTING/SECURITY/CHANGELOG/RELEASE/SUPPORT/CODE_OF_CONDUCT/AGENTS/CLAUDE）链接全通。
- `make harness-check` 通过（31 targets、15 required docs、74 links、12 programs inspected）。
- 归档区 7 个文件全部在索引中登记，符合「archive 只读、按月存放」约定。

## 发现

### F1 — 断链 1 处（gate 未覆盖）

`deploy/runbooks/agent-kb-eval-optimization-20260802/README.md:135` 链接
`apps/assistant-service/docs/api/openai-responses-ingress.md`。目标目录已不存在
（`apps/` 现仅 `islamic-content-service`、`knowledge-service`、`local-node`；Python 执行面已随
Rust 切换删除）。支持矩阵其实已内联在该 README 上表；实现现为 `src/api/v1/responses.py`。
harness-check 只解析 74 条链接（budgeted docs），未覆盖 runbook README，故此断链漏网。

### F2 — 索引漏登一个已存在 10 天的程序

`deploy/runbooks/sota-performance-dual-gate/`（SPD-00→04，首次提交 2026-08-18，e1fd53a）
不在 `docs/README.md` Programs 表（该表 2026-08-26 verified 时已漏）。当前状态：SPD-04，
实现收尾完成，**blocked** —— live qwen3.7-plus thinking-low TTFT p50 9.28s 超 3.41s gate；
三个八任务稳定性 cohort 未按最小收尾要求跑。

### F3 — Programs 表状态漂移（索引 vs loop-state.json）

| 程序 | 索引（2026-08-26） | loop-state 实况 |
| --- | --- | --- |
| `agent-contract-unification` | **active** | **superseded**（by agent-runtime-full-rust-cutover）；2026-08-14 scaffold 后从未开工 |
| `agent-runtime-single-kernel` | **active** — checkpoint in progress | **superseded**（同上）；CHR-05 source-lock checkpoint 后移交 Rust 迁移 |
| `agent-runtime-full-rust-cutover` | FRC-06 收尾 | FRC-06 未闭合：差「commit/push rollback evidence、验证 main==origin/main、标记 FRC-06 done」 |
| `assistant-general-agent-harness` / `assistant-hermes-runtime-prd` / `assistant-runtime-optimization` | 终态描述 | 终态 + loop-state 均标记 superseded by Rust kernel migration（措辞可补齐） |
| `platform-plane-restructure` | authored, not started | 与当前 loop-state 一致（active PPR-00，iter 0，next=Start PPR-00）；但工作树有未提交 PPR-00 探针产物，且**有另一会话正在实时修改该程序**（本次排查中 loop-state 两次读取间发生变化，mtime 2026-08-28 09:37）——本报告不触碰其任何文件 |

### F4 — plans 状态问题

- `plans/assistant-upgrade-plan-2026-08.md`：**自标「已被取代」**（被 lighten plan 取代，仅留实测数据）。按 `docs/README.md` 写作约定 §6 应移入 `docs/archive/2026-08/`。
- `plans/assistant-harness-lighten-plan-2026-08.md`：实施契约指向 AGA 程序，AGA 已 7/7 完成并被 Rust 迁移取代 → 计划实际已交付，但未标状态。
- `plans/sota-performance-optimization-2026-08.md`：状态行「active — 计划已写，尚未提升为 runbook」过时。交接提示词已执行（证据 `reports/performance/sota-spo-remaining-2026-08.md`，2026-08-17），现行性能程序为 SPD。注：SPD 内部未交叉引用 SPO，非正式「提升」关系，措辞按「现行程序」而非「SPO 的 runbook」。
- `plans/sota-performance-claude-handoff.md`：一次性交接提示词，已执行完毕，属历史文档。
- `plans/kb-rag-optimization-plan.md`（2026-07-20）：**无状态行**、无对应 runbook/report 可证明进度；子项 BM25 v2 已部分落地（shadow writes，代码中有 `lexical_v2`），但主计划交付状态从文件不可考 → 需 owner 标注或归档。
- `plans/knowledge-bm25-v2-shadow-rollout.md`：状态明确（仅 shadow，live 仍 lexical_v1），无问题。
- `plans/rust-expansion-and-service-topology-2026-08.md`：与索引一致；正在被另一会话修改（工作树 M），不触碰。

### F5 — reports/ 组织

- 索引 Reports 段列 7 个 area 目录；磁盘另有 **`security/`**（SECRET_LEAK_REVIEW_2026-08-17）与 **`agent-runtime/`**（full-rust-cutover-final-2026-08-25 + rollback-rehearsal json）未列出。
- 顶层 3 个散件：`2026_phase1_to_phase3_code_review_and_audit_report.md`、`system-manual-browser-audit-2026-08-20.md`（均应属 `code-review/`）、`repro_eval_label.py`（复现脚本，混在证据区）。
- 索引中「Latest / Current working backlog」指针仍有效。

### F6 — 工作区卫生（均 gitignored，不进 git，但占盘并污染全库搜索）

- `.agents/`（1.4MB，约 190 个 md）：一次多代理编排的遗留（auditor/explorer/orchestrator/reviewer/worker 的 BRIEFING/DISPATCH/handoff/progress），`.gitignore:78` 已忽略。
- `.claude/worktrees/kb-upgrade`（32MB）：**locked** worktree；分支 `worktree-kb-upgrade` 已完全合入 main 与 rust-0828（`git log main..worktree-kb-upgrade` 为空），工作树无未提交内容 → 纯遗留。副作用：全库 `find *.md` 计数被镜像翻倍（984 vs 实际约 500）。
- `tmp/`（82MB）：约定内的 scratch，含截图、探针脚本、外部 clone（`codex-harness-upstream`）、自标 stale 的 `agent-runtime-overlay-stale-copy`。可选清理。

> **验收时状态更新（同日稍后）：** `kb-upgrade` 此后新增了知识库 PRD 提交，且
> `kb-rag-upgrade` 成为另一个 locked 活跃 worktree。上面的“纯遗留/可删除”判断只描述
> 本次排查当时的瞬时状态，现已失效；两个 worktree 均不得按本报告清理，必须重新核对
> 分支包含关系、工作树状态和对应 Claude session。

## 已执行的整理（本次）

1. 修复 F1 断链：改指上表 + `src/api/v1/responses.py`（最小改动，终态程序 README）。
2. F2/F3：`docs/README.md` Programs 表补 SPD 行、更正 ACU/CHR/FRC 状态、AGA/AHR/ARO 补 superseded 标注、PCH 行的后续指针补 SPD；Verified 日期 → 2026-08-28。
3. F5a：Reports 段补 `security/`、`agent-runtime/` 两个 area 目录。

## 待确认后执行

- 归档 `assistant-upgrade-plan-2026-08.md` → `docs/archive/2026-08/`（自标 superseded）。
- 给 lighten / SPO / handoff / kb-rag 四个 plan 补状态行（涉及交付/承接判断，需 owner 确认措辞）。
- 移动 reports/ 顶层 3 个散件到 area 目录。
- 删除 `.agents/`；unlock + remove `kb-upgrade` worktree 并删已合并分支；`tmp/` 清理。

## 长期建议（防复发）

- **gate 覆盖缺口**：harness-check 的链接解析只覆盖 budgeted docs（74 条），未覆盖 `deploy/runbooks/` 的 README/phase 文件 —— F1 断链因此存活。建议把 runbook 顶层文档纳入链接检查（改 gate，非改本文件，需另行评审）。
- Programs 表漂移的根因是程序终态/superseded 时不更新索引。建议在 `docs/harness/workflow.md` 的 program 收尾清单中加一条「更新 `docs/README.md` Programs 表」。
