# AS-09 Independent Terminal Release Critic Verdict

**Phase:** AS-09 — Terminal Whole-Demand Release Gate  
**Feature:** AS-F010  
**Critic:** fresh independent terminal release Critic `/root/as09_terminal_release_critic`  
**Critic Verdict:** `approved`  
**Actor Report:** `docs/agent-studio-prd/reports/as-09-terminal-whole-demand-release-gate-report.md`  
**Date:** 2026-07-20

## Blocking Findings

None.

## Inputs Reviewed

- Repository `AGENTS.md`, the complete AS-09 Phase contract and Critic Protocol,
  Phase manifest, active loop state and handoff.
- `feature-oracle.json` for AS-F001 through AS-F010, every canonical AS-00
  through AS-08 Actor report and independent Critic verdict, and the preserved
  finding history referenced by those final verdicts.
- The AS-09 fixed plan, preflight repair Critic, terminal Actor report,
  whole-demand matrix, build/manifest receipt and release decision.
- The versioned regression manifest, aggregate result and all 39 referenced
  per-gate logs.
- The browser specifications/helpers and rendered evidence for Agent Studio,
  Eval/Publish, Hosted/Embed and Analytics; the Nginx/Helm/Gateway header
  contract, built-image smoke script and accepted built-image log.
- Migration, Runtime Envelope, tenant/RBAC, MCP/Connector, Skill/Knowledge,
  publish/rollback, channel, governance/privacy and built-in Assistant/AHR test
  definitions most likely to disprove the release claim.

## Independent Receipt and Integrity Checks

| Check | Independent result |
| --- | --- |
| Aggregate receipt | `passed`; required/executed/passed/failed = `39/39/39/0`; every result has exit `0`, `skipped_count=0` and no failure reason |
| Result SHA-256 | Recomputed `1ac99751038f037060fd0ae633c19e9544d31daa25586038bc8abc77e0ee8a1b`; matches the AS-09 build receipt |
| Manifest SHA-256 | Recomputed `6630592d04cf04b60e1dba1f42068fdfd3bd19a049c36dfff0ad3aafa057dc1d`; matches the result and AS-08-approved artifact |
| Manifest completeness | All 39 manifest entries are required, unique and in exact Phase-contract order; the accepted result has the same ordered Phase/ID sequence; `scripts/agent_studio_regression.py --validate-only` exited `0` |
| Log integrity | All 39 referenced logs exist; every recorded log SHA-256 independently matches; no non-zero skipped/deselected/xfailed/did-not-run summary was found |
| Run chronology | All gate timestamps are sequential and contained by the aggregate interval; every log modification time is inside its recorded gate interval |
| Source integrity | Accepted source SHA-256 is `2ffa4684a3055b123b51b779eef9321e0821c1940c2942c10b34a2c054f14115`, `source_stable=true`, and an independent current recomputation still matches it |
| Frozen evidence | All `30/30` AS-08 source fingerprints still match; every file hash recorded by the narrow AS-01/AS-02 preflight repair Critic still matches |
| Branch baseline | `HEAD` and local `origin/main` both resolve to `945eb2225d644093802bf5f9d75ca4d9dbad6a8d` |
| Secret-shape scan | No OpenAI-style key, JWT, non-redacted bearer value, provider-key assignment or password value was found in the 39 logs and terminal AS-09 artifacts |

The first candidate's `37/39` result is not reused: its two failures were routed
to AS-01/AS-02, the narrow repairs were independently approved, and the accepted
run uses the new post-repair source hash. The second candidate's external Docker
Hub TLS failure and targeted retry are also not combined into completion
evidence. Only candidate 3's complete `39/39` stable-source rerun is accepted.

## Requirement and Whole-Demand Coverage

- **R1 / no-feature terminal evaluation:** the runner's before/after source
  fingerprint is identical, the current fingerprint still matches, the
  aggregate manifest is unchanged from AS-08, and the accepted result contains
  no partial or retried gate substitution. AS-F010 remains `failing` and empty
  until this independent verdict is consumed, as required.
- **R2 / one compatible-build aggregate:** the exact manifest-contract test
  passed `5/5`; all AS-00 through AS-08 required gates ran in one aggregate.
  The manifest retains the named Runtime Envelope, credential-principal, Skill
  entrypoint, Knowledge provenance, publication atomicity, built-header and
  built-in Assistant gates.
- **Browser, viewports and accessibility:** accepted logs record Agent Studio
  `25/25`, full open-source route regression `41/41`, Eval/Publish `10/10`,
  Hosted/Embed `8/8` and Analytics `5/5`. The inspected specs execute exact
  desktop/tablet/mobile viewports, real Axe scans for serious/critical findings,
  keyboard/focus, mobile overflow, console/network and reduced-motion checks.
  Deterministic screenshot modification times fall inside accepted-run browser
  gate windows, and representative mobile Studio, mobile Publish, Hosted and
  Analytics renders were independently inspected.
- **Actual built Hosted/Embed headers:** the accepted gate log SHA-256 is
  `5773d6aab4f422cca74d0ef31cd63128efc70f1e30bc1ece96ca1a7a3e3c9f6c`.
  It records an actual frontend Docker build and manifest-list SHA-256
  `d3c522361b34355f6d075e8af12a2707a369a552950d45d6e56fdd255239b93b`.
  The executed smoke asserts Hosted `SAMEORIGIN` plus self-only framing and a
  dynamic Embed response with exact allowed-Origin `frame-ancestors`, no XFO
  and `no-store`. Vite headers are not used as production evidence.
- **Migration and rollback:** accepted migration/operations logs passed `16/16`
  and `24/24`. Inspected tests cover additive/reentrant migrations 071–081,
  composite tenant FKs, immutable Versions/events, feature-off preservation of
  Assistant/Knowledge/Eval/Share, atomic publish/idempotency, current-readiness
  rechecks, channel-history rollback, invalid-target rejection, pinned existing
  sessions, legal hold and retryable deletion.
- **Security, privacy and tenancy:** accepted gates exercise Runtime Envelope
  forgery/replay/body/session denial; tenant/RBAC isolation; MCP SSRF/OAuth and
  credential-principal separation; instruction-only exact Skill versions;
  Knowledge revision provenance and revocation; scoped Runtime tokens, exact
  Origin, shared quotas and safe anonymous defaults; recursive redaction,
  governance authorization and deletion isolation.
- **Built-in Assistant compatibility:** each accepted AHR invocation reports
  `33/77/10/98` tests plus the golden gate, and credentialed Gateway-to-Assistant
  isolation is `6/6` with zero skips. Current read-only runtime inspection found
  all eight Compose services healthy and owned by
  `/Users/yang/projects/AI--Platfform`; Gateway health is `healthy` version
  `2.0.0`, and both Gateway and Assistant have
  `ASSISTANT_E2E_STUB_LLM=false`.

## Harness Validation Boundary

The mandated legacy command with `--strict --quality-score` was independently
rerun and exited `1` with the validator's explicit statement that strict
certification is unsupported for a v2 Harness. Its displayed `80/100` is a
diagnostic result and is not treated as failure of the product evidence.

The supported compatibility command without `--strict` independently exited
`0`: `Harness structure validation passed`, diagnostic structure score
`100/100`. This checks structure/metadata only and does not substitute for the
accepted 39-gate outcome.

## Release, Rollback and Handoff Decision

The evidence supports **ready-but-not-deployed**. It does not support or claim
deployment, production Qwen/model quality, production Secret Store/OAuth/egress
configuration, monitoring access/window or rollout authorization. Those items
remain assigned to the release owner, model platform, platform security, SRE
and data owner in `reports/agent-studio/as-09-release-decision.md`.

No production rollback is presently required because no deployment occurred.
The documented future rollback is appropriately non-destructive: disable Agent
Studio/public channels, revoke tokens/grants, repoint Publications to the last
healthy Version, roll back application code and retain additive schema,
immutable history, audits and traces; schema correction remains forward-only.

AS-F010 **may transition to `passing`** after the orchestrator links the Actor
report and this verdict and updates the Oracle/runtime handoff artifacts. The
orchestrator **may then run the post-Critic supported completion claim check**
for AS-09:

`python3 /Users/yang/.codex/skills/prd-phase-harness/scripts/validate_harness_prd.py docs/agent-studio-prd --completion-gate --phase AS-09 --quality-score`

Because this is a v2 Harness, the check uses the supported legacy-compatible
form without claiming unavailable strict certification. It remains metadata
validation, not a deployment action or a replacement for the 39-gate evidence.

## Verification Boundary

I did not run a second aggregate, invoke an external provider, execute a live
migration, mutate Docker, deploy, commit or push. Independent work was limited
to repository/evidence inspection, cryptographic and chronology checks,
representative rendered-image review, the manifest validate-only command, both
documented Harness validation forms, and read-only runtime ownership/health
inspection.

## Verdict Rationale

`approved`. The accepted candidate is a complete, zero-skip, hash-consistent
39-gate rerun on one stable source snapshot; it includes actual built-response
headers, browser/accessibility, migration/rollback, security/privacy/tenant and
built-in Assistant evidence. The Actor preserves candidate failures, does not
pre-claim AS-F010 or the completion gate, and cleanly separates local readiness
from provider readiness and deployment authorization.
