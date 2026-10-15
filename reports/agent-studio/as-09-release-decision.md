# AS-09 Release Decision

**Decision:** `ready-but-not-deployed`
**Status:** local implementation and whole-demand gates passed; independent terminal release Critic approved; supported completion claim check passed
**Date:** 2026-07-20

## Decision Basis

The accepted terminal run executed the immutable AS-08 manifest on one stable
source snapshot and passed all 39 required gates with zero skips. The manifest
hash matched the approved AS-08 hash, all recorded logs matched their SHA-256
receipts, and the source hash was unchanged across the run. The same run covered
Agent Studio management and Preview, MCP, Connectors, Skills, Knowledge,
evaluation, publication and rollback, Hosted, Embed, Runtime API, operations,
governance and existing Assistant compatibility.

The local stack was repository-owned and healthy, migrations through 081 were
present with none pending, and the post-test Gateway/Assistant runtime was
restored healthy with the provider-free Stub disabled. No deployment or
external provider success is implied.

## Deployment Readiness Items

| Residual item | Owner | Required before deployment |
| --- | --- | --- |
| Qwen production readiness and model-quality smoke | release owner / model platform | supply a usable runtime credential without committing or printing it; validate the configured Qwen model and agreed production Eval thresholds |
| Production Secret Store, Connector OAuth and MCP egress controls | platform security | bind production secret references, redirect URLs, egress allowlists and audit/rotation procedures |
| Monitoring credentials and observation window | SRE / release owner | name dashboards/alerts, establish a monitoring window and confirm alert access |
| Rollout authorization and rollback trigger | product/release owner | approve rollout percentage, owner and window; define quantitative stop/rollback thresholds |
| Production data operations | data owner / SRE | approve any production migration execution and retention/deletion procedures; no destructive rollback is permitted |

These items block deployment authorization, not the local implementation
decision. The PRD waiver permits external-provider smoke and deployment to stay
deferred for `ready-but-not-deployed`; local security, privacy, migration,
header, rollback, accessibility and Assistant gates were not waived.

## Rollback Position

No deployment occurred, so no production rollback is necessary. If a later
authorized rollout fails, disable public channels and Agent Studio entry points,
revoke Runtime tokens/grants, repoint Publications to the last healthy Version,
and roll back application code while retaining additive schemas, immutable
Versions, Publication history, audits and traces. Schema correction is
forward-only; do not use `DROP`, `TRUNCATE` or volume reset.

## Evidence Boundary

- Deterministic browser fixtures prove UI states, roles, accessibility,
  interaction and redaction; built Nginx/Gateway smoke proves production header
  behavior; PostgreSQL/Redis and authenticated local HTTP tests prove storage,
  concurrency and authorization boundaries.
- Provider-free Stub execution proves transport and runtime contracts, not model
  quality or provider availability.
- The installed Harness validator cannot issue legacy `--strict`
  certification for the v2 Harness. Its supported structure mode passed
  `100/100`; neither score substitutes for the 39-gate product result.
- AS-F010 is `passing` after the fresh independent release Critic approved the
  terminal artifacts and the supported post-Critic completion claim check
  exited 0. This does not authorize deployment.
