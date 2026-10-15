# Phase 03 - Context, Knowledge, tools, MCP, artifacts, and office capabilities

- PHASE_ID: CHR-03
- FEATURE_ID: CHR-F004
- DEPENDS_ON: CHR-02

## Outcome

Agent discovers and uses read-only platform context and capabilities without a second loop or keyword routing.

## Scope

In:

- Context/TurnInput/Tool/MCP/TurnItem contributors, Knowledge, memory, attachments, citations, artifacts, and office capability normalization.

Out:

- Consequential writes, production canary, or legacy-loop deletion.

## Done when

- [ ] Tool discovery is metadata-driven and all capability reads are tenant-scoped and revision-bound.
- The private capability plane is `POST /internal/v1/capabilities/catalog` and
  `POST /internal/v1/capabilities/invoke`; it accepts only an active Runtime
  lease and read-only schemas. Runtime dynamic tool calls are resolved through
  this boundary, never by a second Agent loop.
- [ ] Knowledge, research, coding, attachment, and office-read Agent Eval scenarios pass.
- [ ] Existing affected behavior still passes its smallest relevant regression check.

## Verify

| Check | Command or observation | Proves |
| --- | --- | --- |
| Read-only capability gate | `make agent-runtime-readonly-gate` | Context, tools, MCP, evidence, citations, and artifacts work with no authority or tenant leak. |

Use the smallest check that can falsify the outcome. Add runtime, browser, migration, or reviewer evidence only when the outcome requires it.

## Stop or confirm

- Stop when a dependency is incomplete, the same failure repeats without a new hypothesis, or the iteration cap is reached.
- Set `waiting_confirmation` only for a real authority boundary.
- Do not enable write-capable tool schemas until CHR-04 policy hooks are active.
