# platform-architecture-convergence

Single authoritative architecture program for this repository, activated
2026-08-29 after the KB RAG upgrade merged. Everything architecture-level that
came before it is terminal or superseded:

| Predecessor | Disposition |
| --- | --- |
| agent-runtime-full-rust-cutover (FRC) | terminal 2026-08-29 (final report + rollback receipt reconciled) |
| platform-plane-restructure (PPR) | superseded 2026-08-29 (PPR-00 evidence kept; PPR-01 → ARC-00C; PPR-08 → provider experiment backlog; PPR-02–07/09 superseded by ADR-008 T0) |
| performance-correctness-hardening (PCH), agent-contract-unification (ACU), assistant-runtime-optimization, agent-runtime-single-kernel, assistant-general-agent-harness, assistant-hermes-runtime-prd | superseded (recorded before this program) |

Governing documents:

- PRD: [`docs/plans/platform-architecture-convergence-prd-2026-08.md`](../../../docs/plans/platform-architecture-convergence-prd-2026-08.md)
- Successor/conformance decision: [`docs/architecture/ADR-008-bounded-contexts-no-new-services.md`](../../../docs/architecture/ADR-008-bounded-contexts-no-new-services.md) (Accepted)
- Package contract: [`docs/harness/work-packages.md`](../../../docs/harness/work-packages.md)
- Fact baselines: [`docs/architecture/baselines/2026-08-post-rag/`](../../../docs/architecture/baselines/2026-08-post-rag/) — regenerate with
  `python3 scripts/inventory/generate_baselines.py`, diff with `--verify`.

Execution model: the PRD plans sequential single-owner waves. On 2026-08-29
the user explicitly directed same-directory parallel execution of Wave 1+
packages under mutually exclusive owned paths, with commits performed by the
primary session. `work-packages.yml` records package states truthfully under
that deviation; the sequential rule of `docs/harness/work-packages.md` §1
still governs any package not explicitly authorized for parallel execution.

State files in this directory: `loop-state.json`, `work-packages.yml`,
`receipts/<package-id>.yml`.
