# AS-09 Terminal Whole-Demand Release Gate Actor Report

**Phase:** AS-09 — Terminal Whole-Demand Release Gate  
**Feature:** AS-F010  
**Status:** Actor gates passed; independent release Critic approved; post-Critic claim check passed; `ready-but-not-deployed`  
**Date:** 2026-07-20  
**Actor:** primary implementation agent

## Outcome

The final compatible-build aggregate passed all 39 AS-00 through AS-08
required gates with zero skips and no source change during execution. Its
manifest SHA-256 is
`6630592d04cf04b60e1dba1f42068fdfd3bd19a049c36dfff0ad3aafa057dc1d`,
matching the AS-08-approved artifact. Its stable source SHA-256 is
`2ffa4684a3055b123b51b779eef9321e0821c1940c2942c10b34a2c054f14115`.
The result and all 39 hashed per-gate logs are durable under
`reports/agent-studio/`.

The Actor decision is `ready-but-not-deployed`. Local implementation,
security/privacy, migration, rollback, built headers, accessibility and existing
Assistant compatibility have terminal evidence. External-provider quality,
production Secret/OAuth/egress configuration, monitoring access/window and
deployment authorization remain explicit deployment-readiness items. No
external provider call or deployment is claimed.

## Plan and No-Feature Boundary

- Fixed plan:
  `docs/agent-studio-prd/reports/as-09-terminal-whole-demand-release-gate-plan.md`.
- The first aggregate failures were returned to their owning AS-01/AS-02
  phases and independently reviewed. The final AS-09 candidate began only after
  those repairs and ran on a new stable source hash.
- The accepted run did not edit application source, migrations, tests,
  frontend/deployment configuration or the aggregate manifest. AS-09 writes
  evidence only.
- The Actor does not transition AS-F010 and does not claim the full completion
  gate. Those actions remain post-Critic orchestrator work.

## Required Validation

| Gate | Exact command | Actual result |
| --- | --- | --- |
| whole-demand-aggregate | `make verify-agent-studio` | exit 0; 39 required, 39 executed, 39 passed, 0 failed; every gate `skipped_count=0`; `source_stable=true` |
| harness-structure (mandated legacy form) | `python3 /Users/yang/.codex/skills/prd-phase-harness/scripts/validate_harness_prd.py docs/agent-studio-prd --strict --quality-score` | exit 1; installed lightweight validator explicitly says legacy strict certification is unsupported for a v2 Harness; no strict claim made |
| harness-structure (supported compatibility form) | `python3 /Users/yang/.codex/skills/prd-phase-harness/scripts/validate_harness_prd.py docs/agent-studio-prd --quality-score` | exit 0; structure validation passed, diagnostic compatibility score 100/100; metadata only |
| approved-live-smoke | `bash scripts/new/test-agent-studio-release.sh --live-smoke` | deferred under the PRD waiver: no external-provider or deployment credential was read or used |

The user explicitly authorized skipping obsolete overly strict ritual checks;
the unsupported legacy flag is recorded rather than relabeled as passed. No
non-waivable product, security, migration, browser/header, rollback,
accessibility or Assistant gate was skipped.

## Aggregate Groups

| Phase | Gates | Result |
| --- | ---: | --- |
| AS-00 | 4 | 4/4 passed |
| AS-01 | 4 | 4/4 passed |
| AS-02 | 5 | 5/5 passed |
| AS-03 | 4 | 4/4 passed |
| AS-04 | 4 | 4/4 passed |
| AS-05 | 4 | 4/4 passed |
| AS-06 | 4 | 4/4 passed |
| AS-07 | 6 | 6/6 passed |
| AS-08 | 4 | 4/4 passed |

Durable feature mapping and cross-cutting evidence are in
`reports/agent-studio/as-09-whole-demand-matrix.md`. Machine-readable build,
manifest, result and candidate-history facts are in
`reports/agent-studio/as-09-build-and-manifest.json`.

## Terminal Browser, Header, Migration and Assistant Evidence

- Agent Studio browser: 25/25; full open-source route regression: 41/41.
  Directory/create/Studio/Preview covered populated, loading, empty, error,
  permission, Owner/Editor/Viewer, keyboard/focus, reduced-motion and exact
  desktop/tablet/mobile states.
- Release browser: 10/10. Saved-Draft Eval freshness, durable cancellation,
  publish idempotency, promotion, session pinning and rollback executed.
- Hosted/Embed browser: 8/8. Public/private Hosted, attachments/citations,
  allowed/rejected Origin, launcher/inline protocol, focus and mobile behavior
  executed.
- Built header gate built an actual frontend image and passed. Hosted retained
  self-only/SAMEORIGIN framing; Embed received dynamic exact-Origin
  `frame-ancestors`, no XFO and `no-store`. Vite headers were not used as
  production proof.
- Analytics/admin browser: 5/5. Metrics, redacted traces, audits, quotas,
  governance roles, error/empty states and flag-off compatibility executed.
- Migration and PostgreSQL suites ran inside AS-01, AS-06 and AS-08 gates;
  additive/reentrant migrations through 081, atomic release races, legal hold,
  deletion receipt replay and rollback invariants passed.
- Built-in Assistant AHR passed 33/77/10/98 plus golden gate; credentialed
  Gateway/Assistant isolation passed 6/6 with no skips. The final local runtime
  was restored healthy with Stub disabled.

## Candidate History

1. Candidate 1 was stable but failed 2/39. It is not completion evidence. The
   MCP legacy authorization router mismatch and AS-02 frozen/golden evidence
   drift were routed to their owning phases; focused 14/14 and 45/45 reruns,
   Ruff and JSON validation passed. A fresh Critic approved only those repairs.
2. Candidate 2 passed 38/39. Its sole failure was an external Docker Hub TLS
   handshake timeout while resolving the Dockerfile frontend for the built
   header gate. A targeted retry passed, but the partial run plus retry is not
   accepted as terminal evidence.
3. Candidate 3 reran the complete manifest, including the built-image gate, on
   stable source and passed 39/39. Only this candidate supports this report.

The repair Critic is preserved at
`docs/agent-studio-prd/reports/as-09-preflight-regression-repair-critic.md`.

## Runtime, Security and Secret Boundary

Before the accepted run, all eight `ai-gateway-*` services were healthy and
their Compose `working_dir` labels matched
`/Users/yang/projects/AI--Platfform`. Migrations 071 through 081 were present
with none pending. A disposable ignored local account and provider-free Stub
made the credentialed isolation nodes execute without skips. No API key was
read, printed, changed, invented or placed in evidence.

After the aggregate, Gateway and Assistant were recreated with
`ASSISTANT_E2E_STUB_LLM=false`, the same full local Python trees were
resynchronized, both services became healthy, and the Gateway `/health`
endpoint returned healthy version 2.0.0. This is local-development evidence,
not production availability.

## Release Decision and Residual Risk

The detailed decision is
`reports/agent-studio/as-09-release-decision.md`. Deployment remains blocked
until named owners supply and validate production Qwen readiness, Secret
Store/OAuth/egress controls, monitoring credentials/window, explicit rollout
authorization and quantitative rollback triggers. No production mutation or
deployment occurred.

If a later rollout fails, disable public channels and Agent Studio entry
points, revoke Runtime tokens/grants, repoint Publications to the last healthy
Versions and roll back application code while preserving additive schemas,
immutable release history, audit and trace evidence. Schema rollback remains
forward-only.

## Critic Handoff

A fresh independent terminal release Critic must inspect the Phase contract,
all prior Oracle/critic completion evidence, manifest/result/log hashes, the
accepted 39-gate logs, browser/header/migration/rollback/Assistant evidence,
the repair boundary and this `ready-but-not-deployed` decision. AS-F010 remains
`failing` until that Critic approves. Only then may the orchestrator update the
Oracle/runtime artifacts and execute the post-Critic completion claim check.

## Post-Critic Orchestrator Addendum

The fresh terminal Critic approved with no blocking findings and independently
matched the result, manifest, current source and all 39 log hashes plus the
browser/header/migration/rollback/security/Assistant evidence. The orchestrator
then transitioned AS-F010 to `passing` and ran both documented validator forms:

- `python3 /Users/yang/.codex/skills/prd-phase-harness/scripts/validate_harness_prd.py docs/agent-studio-prd --strict --completion-gate --quality-score`
  exited 1 because the installed lightweight validator explicitly does not
  support legacy strict certification for v2; it also noted that
  `--completion-gate` is a deprecated alias for `--claim-check`. This result is
  not counted as a pass.
- `python3 /Users/yang/.codex/skills/prd-phase-harness/scripts/validate_harness_prd.py docs/agent-studio-prd --completion-gate --phase AS-09 --quality-score`
  exited 0: structure validation passed, claim metadata was internally
  consistent and the diagnostic score was 100/100. The tool explicitly states
  that it does not execute or validate cited evidence.

AS-09 / AS-F010 is therefore locally verified under the supported Harness
contract. The terminal product evidence remains the accepted 39/39 zero-skip
aggregate plus the independent Critic; deployment remains unclaimed.
