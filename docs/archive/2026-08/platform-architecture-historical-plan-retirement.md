# Platform architecture historical plan retirement

**Lifecycle:** archived evidence index; not an execution plan.

The only architecture implementation authority for this closeout is
[`platform-architecture-convergence-prd-2026-08.md`](../../plans/platform-architecture-convergence-prd-2026-08.md).
The machine-readable source for this table is
[`historical-plan-retirement.json`](../../../deploy/release/historical-plan-retirement.json).

| Historical document | Lifecycle | Why it cannot be resumed |
| --- | --- | --- |
| `assistant-upgrade-plan-2026-08.md` | superseded | Targets the deleted Python Assistant runtime. |
| `assistant-harness-lighten-plan-2026-08.md` | superseded | Its product observations remain evidence; execution moved to the Rust kernel. |
| `kb-rag-optimization-plan.md` | superseded | The later RAG program absorbed its remaining implementation work. |
| `rust-expansion-and-service-topology-2026-08.md` | superseded | ADR-008 and the convergence PRD own the accepted topology. |
| `sota-performance-optimization-2026-08.md` | superseded | Historical analysis and measurements only. |
| `sota-performance-claude-handoff.md` | archived | One-time handoff already consumed. |
| `rust-0828-full-acceptance-and-kb-integration-test-plan-2026-08-28.md` | archived | Its dated acceptance evidence is complete and immutable. |

`superseded` and `archived` do not mean deleted. Git history and the original documents retain
their evidence; they simply cannot direct new implementation. The queued Agent Runtime upstream
upgrade is a separate successor program and begins only after this convergence candidate is closed.
