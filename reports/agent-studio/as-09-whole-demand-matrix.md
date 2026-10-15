# AS-09 Whole-Demand Regression Matrix

**Phase:** AS-09 — Terminal Whole-Demand Release Gate
**Feature:** AS-F010
**Accepted run:** 2026-07-19T19:25:20.438149Z to 2026-07-19T19:33:21.542797Z
**Actor result:** passed; independent terminal release Critic approved; supported completion claim check passed
**Release decision:** ready-but-not-deployed

## Immutable Aggregate Receipt

`make verify-agent-studio` executed every required entry in
`tests/fixtures/agent-studio/regression_manifest.json` from one source
snapshot. The final result is
`reports/agent-studio/agent-studio-regression-v1-result.json`.

| Receipt | Observed value |
| --- | --- |
| Aggregate status | `passed` |
| Required / executed / passed / failed | `39 / 39 / 39 / 0` |
| Per-gate skipped count | `0` for all 39 gates |
| Source SHA-256 | `2ffa4684a3055b123b51b779eef9321e0821c1940c2942c10b34a2c054f14115` |
| Source stable | `true` |
| Manifest SHA-256 | `6630592d04cf04b60e1dba1f42068fdfd3bd19a049c36dfff0ad3aafa057dc1d` |
| Result SHA-256 | `1ac99751038f037060fd0ae633c19e9544d31daa25586038bc8abc77e0ee8a1b` |
| Log integrity | all 39 recorded log SHA-256 values independently matched their files |

## Oracle-to-Terminal Evidence

| Oracle | Whole-demand behavior | Same-run gates | Result |
| --- | --- | --- | --- |
| AS-F001 | Assistant-compatible capability baseline, allowlist and isolation | AS-00 `branch-baseline`, `capability-tests`, `assistant-isolation`, `lint` | 4/4 passed |
| AS-F002 | Tenant-safe Agent persistence, migrations, CRUD/RBAC and Gateway compatibility | AS-01 `migration-contract`, `agent-api`, `lint`, `gateway-regression` | 4/4 passed |
| AS-F003 | Signed Runtime Envelope, immutable resolution, session/trace lifecycle and fail-closed runtime | AS-02 `gateway-envelope`, `resolver-isolation`, `trace-session`, `runtime-gate`, `lint` | 5/5 passed |
| AS-F004 | MCP and Connector registry/runtime/security with Assistant compatibility | AS-03 `mcp-api-runtime`, `mcp-security`, `lint`, `assistant-regression` | 4/4 passed |
| AS-F005 | Immutable Skill entrypoints and Knowledge binding/provenance | AS-04 `skill-api-isolation`, `skill-runtime`, `knowledge-binding`, `lint` | 4/4 passed |
| AS-F006 | Multi-Agent directory/create/Studio, custom prompt/configuration and isolated Draft/Version Preview | AS-05 `frontend-static`, `agent-studio-e2e`, `existing-route-e2e`, `preview-contract` | 4/4 passed; Agent Studio 25/25 and full OSS browser 41/41 |
| AS-F007 | Eval, version publication, atomic channel promotion and rollback | AS-06 `publish-api-atomicity`, `agent-eval`, `frontend`, `runtime-regression` | 4/4 passed; publish/API/PostgreSQL 35/35 and release browser 10/10 |
| AS-F008 | Hosted, Embed and Runtime API delivery/security | AS-07 `runtime-api`, `frontend-build`, `embed-header-contract`, `built-nginx-header-smoke`, `channel-browser`, `runtime-regression` | 6/6 passed; channel browser 8/8 and built-image header smoke passed |
| AS-F009 | Metrics, audit, quotas, governance, deletion, migration and compatibility | AS-08 `operations-governance`, `aggregate-manifest`, `analytics-frontend`, `compatibility` | 4/4 passed; operations 24/24, manifest 5/5, analytics 5/5 |
| AS-F010 | One compatible-build whole-demand release evaluation with release/deployment separation | the complete immutable 39-gate run plus this matrix, build receipt, decision and fresh release Critic | passed; Critic approved and supported completion claim check exited 0 |

## Terminal Cross-Cutting Checks

| Boundary | Same-run observation | Result |
| --- | --- | --- |
| Browser routes and viewports | Agent Studio, Eval/Publish, Hosted/Embed, Analytics and existing Assistant/Knowledge/Eval/Share routes executed at their specified desktop/tablet/mobile viewports | passed |
| Accessibility and interaction | axe-covered surfaces, keyboard/focus flows, mobile overflow checks and reduced-motion behavior executed in the browser suites | passed |
| Built Hosted/Embed headers | an actual frontend Docker image was built; Hosted retained `SAMEORIGIN` / self-only framing while Embed used dynamic exact-Origin `frame-ancestors`, no XFO and `no-store` | passed |
| Origin and browser credential safety | allowed/rejected Origin fixtures, protocol/source validation, short-lived Embed credential handling and redacted request surfaces | passed |
| Migration safety | additive/reentrant migration contracts through 081, database constraints and operations migration coverage | passed |
| Publish and rollback | idempotent publish, atomic promotion, pinned sessions, valid channel-history rollback and invalid-target rejection | passed |
| Feature rollback | Agent Studio flag-off removes Agent entry points while Assistant, Knowledge, Eval and Share remain mounted | passed |
| Built-in Assistant compatibility | AHR groups passed with 33/77/10/98 tests plus golden gate; credentialed isolation passed 6/6 with zero skips | passed |
| Security, privacy and tenant isolation | authorization, MCP/Connector credentials, runtime token/origin controls, recursive redaction, quotas, legal hold and deletion isolation executed | passed |
| Source integrity | source hash before and after all gates was identical; generated reports were excluded but source/tests/manifest were covered | passed |

## Candidate History and Repair Boundary

The first candidate ran all 39 gates on stable source
`3518b0df0ff14c41f4ff05d8976b9abbe6c6e06cd26d7b3dbd62a7f16beda58a`
and stopped at 37/39. The failures were returned to AS-01 and AS-02: a legacy
MCP authorization test mounted the new registry router, and frozen/golden AS-02
evidence had not incorporated intentional Knowledge configuration fields. The
narrow repairs passed 14/14 and 45/45 focused tests, Ruff and JSON validation.
A fresh repair Critic approved only those owning-Phase fixes and explicitly
required a new full aggregate.

The second post-repair candidate reached 38/39 on stable source
`2ffa4684a3055b123b51b779eef9321e0821c1940c2942c10b34a2c054f14115`.
Its sole failure was Docker Hub TLS handshake timeout while resolving
`docker/dockerfile:1.7` for the built-image header gate. A targeted retry passed,
but neither that partial aggregate nor the retry is used as terminal evidence.

The accepted third candidate reran all 39 gates from the same post-repair source
hash, including the Docker build/header gate, and passed 39/39. No application
source, migration, test, frontend/deployment configuration or aggregate
manifest changed during that accepted run.

## Harness and Runtime Boundary

The mandated legacy command with `--strict --quality-score` was executed and
exited 1 because the installed lightweight validator explicitly does not offer
legacy strict certification for this v2 Harness. The supported compatibility
structure validation then exited 0 with `100/100`; it is metadata validation,
not product evidence. Product completion rests on the zero-skip 39-gate
aggregate and independent Critic, not on that diagnostic score.

All eight local services were repository-owned and healthy before the accepted
run; migrations 071 through 081 were present with none pending. Gateway and
Assistant used a provider-free test Stub only for the credentialed aggregate.
Both were recreated afterward, resynchronized with the same local source, and
verified healthy with `ASSISTANT_E2E_STUB_LLM=false`. No provider API key was
read, printed, changed or invented, and no external provider or deployment is
claimed.
