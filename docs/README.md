# Documentation Index

This page is navigation, not a mandatory reading list. Start from the user's task and
[`AGENTS.md`](../AGENTS.md); open only the owning code and documentation you need.

**Updated:** 2026-10-16

---

## Start here

| You are | Read |
| --- | --- |
| An AI agent about to change code | [`AGENTS.md`](../AGENTS.md); use the [task harness](harness/README.md) when workflow detail is needed |
| Using Codex, Claude Code or another agent | The same [`AGENTS.md`](../AGENTS.md); no separate agent rulebook |
| Finding current Rust source | [Native workspace](../rust/platform-native/Cargo.toml), [Runtime overlay](../rust/agent-runtime-overlay/), [root layout](../README.md) |
| Deployment and provider operations | [Operator guide](deployment/operator-guide.md) |
| Current upgrade implementation and acceptance | [Existing execution ledger](../deploy/runbooks/native-enterprise-upgrade/loop-state.json) |
| Running the project for the first time | [`README.md`](../README.md) → `make doctor` → `make quickstart` |
| Deploying to a server | [`DEPLOY.md`](../DEPLOY.md) |
| Contributing a change | [`CONTRIBUTING.md`](../CONTRIBUTING.md), [`harness/workflow.md`](harness/workflow.md) |
| Reporting a vulnerability | [`SECURITY.md`](../SECURITY.md) |

## Layout

| Directory | Holds | Lifecycle |
| --- | --- | --- |
| [`harness/`](harness/README.md) | The agent harness: contract, architecture, commands, workflow | Living — must stay true, gated by `make harness-check` |
| [`architecture/`](architecture/) | ADRs — one file per boundary decision | Immutable once accepted; superseded by a new ADR |
| [`design/`](design/) | Durable design intent that outlives any one plan | Living |
| [`plans/`](plans/) | Plans for work that is scoped but not yet a program | Deleted or archived once delivered |
| [`research/`](research/) | External comparisons and background reading | Dated; archived when stale |
| [`archive/<yyyy-mm>/`](archive/) | Superseded snapshots kept for provenance | Never edited; read-only history |
| [`../deploy/runbooks/`](../deploy/runbooks/) | Multi-phase/context-spanning programs; `loop-state.json` is authoritative | Living while active, then terminal |
| [`../reports/`](../reports/) | Evidence: reviews, benchmarks, regression output | Append-only, dated |
| [`../web/e2e/`](../web/e2e/README.md) | The only home for Playwright specs, helpers, and fixtures | Living |

Rule of thumb: **design** says why, **plans** say what next, **runbooks** track a program in flight,
**reports** are evidence of something that happened, **archive** is what used to be true.

## Harness

| Doc | Purpose |
| --- | --- |
| [`harness/README.md`](harness/README.md) | Task routing, complete-module execution, one validation owner and result reuse |
| [`harness/platform-architecture.md`](harness/platform-architecture.md) | The product law: kernel / contract / surfaces / extensions, and the admission table |
| [`harness/architecture.md`](harness/architecture.md) | Topology, module ownership, dependency direction, contracts that must not drift |
| [`harness/commands.md`](harness/commands.md) | Canonical command for every routine action; which gate to run for what |
| [`harness/workflow.md`](harness/workflow.md) | Task loop, definition of done, program convention, git rules |
| [`harness/work-packages.md`](harness/work-packages.md) | Single-owner package contract for architecture and repository-wide changes |
| [`harness/runtime-and-secrets.md`](harness/runtime-and-secrets.md) | Mandatory before any Docker, deploy, or E2E action |
| [`harness/integration-and-rollback.md`](harness/integration-and-rollback.md) | Singleton-runtime ownership, verification ladder, integration and rollback |
| [`harness/repository-quality.md`](harness/repository-quality.md) | Dead code/docs/tests, dependencies, large files and browser-evidence lifecycle |

## Design

| Doc | Subject |
| --- | --- |
| [`design/enterprise-console-workbench.md`](design/enterprise-console-workbench.md) | Permission-aware console workbench, navigation, live usage and accessibility contract |
| [`design/agent-design-manifesto.md`](design/agent-design-manifesto.md) | Design intent for the general agent runtime |
| [`design/general-agent-product-contract.md`](design/general-agent-product-contract.md) | Product, UX, safety, eval, and release contract for the general Agent |
| [`design/agent-design-manifesto-critique.md`](design/agent-design-manifesto-critique.md) | Counter-arguments against the manifesto; read with it, not instead of it |
| [`design/agent-optimization-spec.md`](design/agent-optimization-spec.md) | Optimization targets for the agent loop |
| [`design/assistant-runtime-trace-reconciliation.md`](design/assistant-runtime-trace-reconciliation.md) | Durable, read-only reconciliation of terminal Agent Runtime traces after Gateway restarts |

## Architecture decisions

- [Tenant account administration](architecture/tenant-account-administration.md)

| ADR | Subject |
| --- | --- |
| [`ADR-004`](architecture/ADR-004-bounded-plugin-subagent-delegation.md) | Bounded plugin subagent delegation |
| [`ADR-005`](architecture/ADR-005-model-capability-profiles.md) | Tenant-configurable model capability profiles |
| [`ADR-006`](architecture/ADR-006-agent-runtime-single-kernel.md) | Agent Runtime as the single target Agent kernel |
| [`ADR-007`](architecture/ADR-007-agent-runtime-data-boundaries.md) | Gateway model plane, ThreadStore, and capability-service boundaries |
| [`ADR-009`](architecture/ADR-009-independent-cli-local-runtime.md) | Independent CLI local Runtime and provider ownership |
| [`ADR-010`](architecture/ADR-010-phase1-tenant-identity-and-knowledge-authorization.md) | Authenticated tenant preconditions, epoch 001 Knowledge ACL/privileges, Gateway actor/deletion confirmation and epoch 2 compatibility |
| [P-ADR-1 (proposed)](architecture/proposals/rust-native-platform-baseline.md) | Rust-native implementation baseline; precise successor scope for ADR-008 language clauses |
| [`ADR-011`](architecture/ADR-011-phase1-shared-contracts-and-metrics-ownership.md) | Reviewed pure pricing/schema-revision/retrieval contracts; metrics collector remains Gateway-owned with explicit core transport injection |

Write a new ADR when a change alters a dependency boundary, adds a service, or changes a contract
listed in [`harness/architecture.md`](harness/architecture.md) §6.

## Plans

| Plan | Subject |
| --- | --- |
| [`plans/platform-upgrade-2026-10/README.md`](plans/platform-upgrade-2026-10/README.md) | **adopted for execution** — 109 work packages in four phases; scope and acceptance contract from the 2026-10-16 review. Current package status and evidence stay in the `native-enterprise-upgrade` ledger. |
| [现有功能完善 PRD](plans/product-feature-completion-2026-09-23/README.md) | **PRD v1.0 / 待产品评审** — 对13个现有模块进行产品分析，定义功能需求、优先级、正常/异常验收及R0～R5迭代；本轮仅规划，未启动开发。 |
| [Codex内核更新与Gateway兼容优化](plans/codex-harness-refresh-2026-09-22.md) | **active / 实现与集成中** — 从`94cbbdda`升级到指定快照`279ba894`；用户授权多代理代码更新、优化/review及本地Docker/内置浏览器验收。状态以执行账本为准。 |
| [`plans/agent-platform-vnext-2026-09/README.md`](plans/agent-platform-vnext-2026-09/README.md) | **active / 一期实现** — 三期升级PRD；一期已在 `codex/agent-platform-vnext-phase1` 启动，P1-01首批租户边界代码与验证见执行receipt；二/三期仍为计划。 |
| [`plans/sota-performance-optimization-2026-08.md`](plans/sota-performance-optimization-2026-08.md) | **superseded as execution instruction** — 证据保留；现行 provider/stability blocker 在 `sota-performance-dual-gate`。 |
| [`plans/sota-performance-claude-handoff.md`](plans/sota-performance-claude-handoff.md) | **archived intent, pending move** — 已消费的一次性交接提示词，不得再次执行。 |
| [`plans/assistant-upgrade-plan-2026-08.md`](plans/assistant-upgrade-plan-2026-08.md) | **superseded** — Assistant runtime upgrade snapshot retained for measured evidence; current direction is the lighten plan below. |
| [`plans/assistant-harness-lighten-plan-2026-08.md`](plans/assistant-harness-lighten-plan-2026-08.md) | **superseded** — Python AgentLoop 已删除；后续优化以 Rust Runtime/Worker 为准。 |
| [`plans/kb-rag-optimization-plan.md`](plans/kb-rag-optimization-plan.md) | **superseded pending RAG merge** — 当前 RAG PRD/增补在验收 worktree；旧计划不再指导实现。 |
| [`plans/rust-expansion-and-service-topology-2026-08.md`](plans/rust-expansion-and-service-topology-2026-08.md) | **superseded by queued architecture convergence** — 仅保留调查与迁移判据。 |
| [`plans/knowledge-bm25-v2-shadow-rollout.md`](plans/knowledge-bm25-v2-shadow-rollout.md) | **active design contract, pending move to `docs/design/`** — BM25 v2 safety/rollout，不是程序状态。 |
| [`plans/rust-0828-full-acceptance-and-kb-integration-test-plan-2026-08-28.md`](plans/rust-0828-full-acceptance-and-kb-integration-test-plan-2026-08-28.md) | **archived evidence, pending move** — 已完成的 Rust 0828 验收计划，不得重新执行。 |
| [`plans/platform-architecture-convergence-prd-2026-08.md`](plans/platform-architecture-convergence-prd-2026-08.md) | **active closeout** — 核心候选 `9aaa9cab` 当前栈实机已通过；托管 Rust CI、多架构、fresh-machine 与冻结镜像回切证据仍未完成。 |
| [`plans/agent-runtime-codex-harness-upgrade-prd-2026-08.md`](plans/agent-runtime-codex-harness-upgrade-prd-2026-08.md) | **implemented; retained as upgrade contract** — upstream `94cbbdda` 已同步并进入 `main`；后续修复由下列计划接管。 |
| [升级后核心修复：一期执行记录](../deploy/runbooks/agent-platform-vnext/README.md) | **completed_local** — 原本地草稿已由一期接管；工具授权、租户隔离、终态恢复、Python sandbox、成对制品与 CLI 流式边界的本地验收已完成，外部发行义务见执行账本。 |

## Research

- [`research/ai-assistant-harness-4oss-comparison-research-2026-08.md`](research/ai-assistant-harness-4oss-comparison-research-2026-08.md) — comparison against four open-source assistant harnesses.

## Programs

All programs live in [`../deploy/runbooks/`](../deploy/runbooks/). Status comes from each program's
`loop-state.json`, never from prose. The table below is a dated navigation snapshot, not a second
state authority; lifecycle conflicts in older ledgers are a named ARC-00 cleanup. Verified
2026-08-30 against the active program ledgers:

| Program | Terminal phase | Status |
| --- | --- | --- |
| **`platform-architecture-convergence`** | ARC-08 | **active closeout** — ARC-00A/B、01/01B、02、03、04、05、07 verified；ARC-06 awaiting final live；ARC-00C/02B/08 blocked on hosted Rust CI/multi-arch, fresh-machine and missing frozen rollback images. |
| **`platform-plane-restructure`** | PPR-00 → PPR-09 | **PPR-00 evidence complete; future phases paused for convergence** — PPR-01 构建经济性与 PPR-08 provider 实验将被提取，PPR-02～07/09 由 queued architecture successor ADR裁决；当前不要按旧 next_action 开工。 |
| **`agent-runtime-full-rust-cutover`** | FRC-00 → FRC-06 | **closeout-only** — Rust 切换、验收与回滚演练已有报告；只允许核对 ledger/receipt/SHA 后终结，不得继续实现。Python AgentLoop 已删除 |
| **`agent-runtime-single-kernel`** | CHR-00 → CHR-06 | **superseded** — CHR-05 source-lock checkpoint 后由 `agent-runtime-full-rust-cutover`（Rust kernel 迁移）接替；无新 PRD 不得重启。运行流量从未移动。 |
| **`performance-correctness-hardening`** | PCH-00 → PCH-07 | **superseded by full Rust cutover** — 保留验证证据；不得从旧 Python 执行路径继续。剩余质量差距需新 Rust 计划。 |
| **`sota-performance-dual-gate`** | SPD-00 → SPD-04 | **blocked（SPD-04）** — Grok hardening 批次集成与性能/质量双门禁；SPD-00→03 已过，实现收尾完成。blockers：live TTFT p50 9.28s 超 3.41s gate；三个八任务稳定性 cohort 未跑。 |
| **`agent-contract-unification`** | ACU-00 → ACU-06 | **superseded** — 2026-08-14 scaffold 后从未开工，由 `agent-runtime-full-rust-cutover` 接替；无新 PRD 不得重启。原目标：assistant 成为 `AgentSpec` 实例、公共 runtime API 为唯一面契约（见 [`harness/platform-architecture.md`](harness/platform-architecture.md)） |
| **`product-convergence`** | PC-04 | **verified** — review findings #1–#5+#8: first-run onboarding, connectors as the only Confluence story, ai-quiz plugin, nav groups, DEFAULT_MODEL. Report: [`reports/code-review/product-convergence-2026-08.md`](../reports/code-review/product-convergence-2026-08.md) |
| `agent-studio-prd` | AS-09 | verified, no blockers |
| `agent-trace-eval-prd` | ATE-04 | verified, terminal — expansions need a new program |
| `assistant-general-agent-harness` | AGA-06 | 7/7 phases done; superseded by Rust kernel migration |
| `assistant-hermes-runtime-prd` | AHR-05 | passed, requirement chain complete; superseded by Rust kernel migration |
| `assistant-runtime-optimization` | ARO-05 | verified, complete; superseded by Rust kernel migration |
| `eval-quality-optimization` | — | design + plan + verification report, no loop state |
| `agent-kb-eval-optimization-20260802` | — | README only |

Standalone runbooks in the same directory:

- [`codex-harness-refresh-2026-09-22/`](../deploy/runbooks/codex-harness-refresh-2026-09-22/README.md) — 新一轮Codex更新的执行owner；多代理实现、统一Docker/浏览器验收，目标与证据边界见主计划。
- [`agent-platform-vnext/`](../deploy/runbooks/agent-platform-vnext/README.md) — 一期唯一执行owner；`loop-state.json`与工作包receipt记录当前进度，前序执行owner已移交，历史证据及未完义务保留。
- `assistant-runtime-operating-model.md` — runtime health, failure categories, no-go thresholds, rollback.
- `assistant-runtime-trust-hardening.md` — trust boundary hardening.
- `assistant-local-os-product-contract.md` — local OS agent product contract.
- `open-source-env-readiness-todo.md` — open-source release readiness (checked by CI).

## Reports

Latest architecture requirements evidence: [`../reports/architecture/agent-platform-vnext-2026-09-07.md`](../reports/architecture/agent-platform-vnext-2026-09-07.md) — 当前源码发现、真实静态门禁、有限内置浏览器验收及版本限制；不是软件升级完成或SOTA证明。

`../reports/` holds evidence, organised by area: `code-review/`, `benchmark/`, `eval-regression/`,
`agent-studio/`, `assistant-runtime-regression/`, `assistant-local-os/`, `performance/`,
`security/`, `agent-runtime/`.

Latest performance planning evidence: [`../reports/performance/sota-microservice-review-2026-08-17.md`](../reports/performance/sota-microservice-review-2026-08-17.md)
— five-agent read-only review of gateway, assistant, knowledge, web/SDK, and the data plane (2026-08-17).

Current working backlog: [`../reports/code-review/codebase-hygiene-scan-2026-08-13.md`](../reports/code-review/codebase-hygiene-scan-2026-08-13.md)
— dead code, oversized files, and comment drift, worked P0 → P1 → P2. Re-run its `rg` cross-checks
before deleting anything; the report is a snapshot, not a live view.

Latest: [`../reports/code-review/product-convergence-2026-08.md`](../reports/code-review/product-convergence-2026-08.md)
— product-convergence program final verification (findings #1–#5 + #8, connectors, ai-quiz plugin).

## Archive

[`archive/`](archive/) holds superseded snapshots by month. They are provenance, not guidance —
do not act on them without re-verifying against the current code.

| Archived | Why |
| --- | --- |
| `2026-07/PLATFORM_REVIEW_2026-07-14.md` | Platform review superseded by the 2026-08-13 hygiene scan |
| `2026-07/comprehensive-code-review-optimization.md` | Same |
| `2026-07/optimization-report-comprehensive.md`, `optimization-report-grok-build.md` | Point-in-time optimization reports |
| `2026-07/前端UI全面升级优化总纲.md` | Frontend UI upgrade outline, delivered |
| `2026-08/HANDOFF-2026-08-12.md` | Session handoff that depended on a directory outside this repository |
| `2026-08/assistant-module-deep-review-2026-08-12.md` | Superseded by the 2026-08-13 whole-repo scan |

## Writing docs here

1. One topic per file; put it in the directory whose lifecycle matches.
2. Open with what the doc is and when it was last verified.
3. Link to code with repo-relative paths so links survive on every machine.
4. State status plainly — `active`, `superseded by X`, `archived`. A doc with no status decays silently.
5. Update the owning doc in the same change as the code. Add the file to this index if it is new.
6. When a doc stops being true, move it to `archive/<yyyy-mm>/` — do not leave it to be believed.

- [Native model discovery](design/native-model-discovery.md)
