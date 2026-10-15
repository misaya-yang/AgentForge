# ATE-00 Baseline Trace Architecture Plan

## Selected Scope

- Phase: ATE-00 Baseline Trace Architecture
- Feature oracle item: ATE-F001
- Implementation scope: runbook evidence only
- Runtime code scope: no edits to `src/`, `apps/`, `packages/`, `database/`, or `web/`

## Todo List

1. Verify ATE-00 hot-path files and primary context.
2. Run strict harness validation, placeholder scan, docs-ignore proof, and repo manifest proof.
3. Write the actor report with validation evidence and minimal-change scope.
4. Write a separate critic artifact with `Critic Verdict`.
5. Update only ATE-F001 status, evidence, and notes in `feature-oracle.json`.
6. Update source packet, continuity ledger, progress log, agent handoff, and loop state for the ATE-01 unlock decision.
7. Run the ATE-00 completion gate.

## Requirement Mapping

| Gate | Evidence |
| --- | --- |
| First-wave AI Assistant boundary | Actor report plus source packet and continuity ledger writeback |
| LangGraph Proxy and RAG future boundaries | Actor report plus continuity ledger writeback |
| Docs persistence proof | `git check-ignore -v docs/agent_trace_eval_prd docs/general_ai_assistant_next_gen/README.md` |
| Harness readiness | strict validator output |
| Placeholder absence | placeholder scan output with expected exit 1 |
| Repository validation command discovery | manifest proof command output |
| Independent review | separate critic artifact |

## Minimal-Change Boundary

ATE-00 changes only PRD harness files under `deploy/runbooks/agent-trace-eval-prd`. Runtime implementation begins in ATE-01 after completion gate approval.

## Validation Commands

```bash
python3 validate_harness_prd.py deploy/runbooks/agent-trace-eval-prd --strict --quality-score
rg -n '\b(T[D]O|T[B]D)\b|\{\{[^}]+\}\}' deploy/runbooks/agent-trace-eval-prd
git check-ignore -v docs/agent_trace_eval_prd docs/general_ai_assistant_next_gen/README.md
rg --files -g 'pyproject.toml' -g 'Makefile' -g 'package.json' -g 'pnpm-lock.yaml' -g '.github/workflows/*'
```
