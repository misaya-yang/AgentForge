# AS-05 Independent Critic Verdict — Iteration 3

**Phase:** AS-05 — Agent Studio Frontend and Preview  
**Feature:** AS-F006  
**Critic:** `/root/as05_critic_iteration_3` — fresh independent subagent  
**Verdict:** `approved`  
**Date:** 2026-07-19

The Actor report was used only as a navigation aid. This verdict comes from
direct inspection of the frozen source, tests, both preserved
`changes_requested` verdicts, the rendered evidence and machine-readable
Preview receipt; fresh execution of every required Phase command; and
supplemental runtime-config, Compose, PostgreSQL, Ruff and diff-hygiene checks.
The Critic did not modify product source, tests, Actor evidence, Docker, the
Feature Oracle, loop state or continuity records.

## Independently Rerun Required Gates

| Gate | Iteration-3 Critic result |
| --- | --- |
| `corepack pnpm@10.33.0 -C web lint && corepack pnpm@10.33.0 -C web type-check && corepack pnpm@10.33.0 -C web i18n:check && corepack pnpm@10.33.0 -C web build` | exit 0; lint 0 errors/17 warnings; type-check, i18n and production build passed |
| `corepack pnpm@10.33.0 -C web exec playwright test e2e/agent-studio.spec.ts --config playwright.opensource.config.ts` | exit 0; 25 passed, 0 failed, 0 skipped in 1.1m |
| `corepack pnpm@10.33.0 -C web e2e:opensource` | exit 0; 31 passed, 0 failed, 0 skipped in 1.1m |
| `uv run pytest -q --no-cov tests/api/test_agents_api.py && uv run --package assistant-service pytest -q --no-cov tests/services/assistant/test_agent_runtime_resolver.py` | exit 0; 9 passed + 11 passed, 0 failed, 0 skipped |

The host used Node 24.14.0 while the package requests Node `^22.12.0`; pnpm
reported the known engine warning. Vite reported the existing large shared
chunk warning. Neither changed an exit code or invalidated an AS-05 behavior.

Supplemental independent checks also passed:

- `uv run pytest -q --no-cov tests/scripts/test_web_runtime_config_entrypoint.py tests/database/test_agent_studio_migrations.py::test_repository_enforces_tenant_revision_version_and_hash_only_token_contracts`
  returned 6 passed, 0 failed, 0 skipped: five runtime-config/Compose cases
  and the real PostgreSQL transaction-trigger rollback case.
- Focused Ruff checks over the Agent API/repository/Assistant projection and
  their tests returned `All checks passed!`.
- `git diff --check` exited zero.

No timeout was enlarged, no test was skipped or weakened, and the obsolete
unsupported legacy Harness `--strict` option was not substituted for product
evidence or treated as a required product gate.

## Preserved Finding Disposition

### AS05-C01 — closed

One revision-checked Draft `PUT` now carries optional Agent name/description
with the spec. The repository authorizes and locks the Agent and Draft, checks
the expected revision and resource bindings, then updates metadata, normalized
bindings, Draft revision/spec and audit record inside one PostgreSQL
transaction. The Studio sends zero metadata `PATCH` requests.

The required API suite independently passed 409, 422 and 503 unchanged-state
cases plus complete retry. The supplemental PostgreSQL case forced an
exception on the Draft UPDATE after the metadata UPDATE and proved name,
description and revision all rolled back. C01 did not regress.

### AS05-C02 — closed

The Agent-only Assistant stream uses an explicit event-type allowlist and
per-type scalar projection. Unknown events are dropped; arbitrary result,
arguments, metadata, output files, context chunks and raw errors are never
serialized. The malicious raw-SSE test passed, including nested
authorization/API-key/client-secret/password-shaped values. The generic
`/chat/stream` compatibility test also passed and retained its existing rich
event contract. C02 did not regress.

### AS05-C03 — closed

Base `docker-compose.yml` passes
`VITE_AGENT_STUDIO_ENABLED: "${VITE_AGENT_STUDIO_ENABLED:-true}"` into the
frontend runtime entrypoint. The five default/false/escaping/nginx/Compose
tests passed. The required browser test proved false removes Agent navigation
and routes while `/assistant` and its composer remain available; the complete
31-case regression passed.

Per the Critic assignment, containers were not changed or restarted. The
Actor's real false/true built-container receipt was inspected but is not
represented as an independently repeated runtime action. Source, entrypoint,
Compose and deterministic browser boundaries are independently green.

### AS05-C04 — closed

The 390x844 mobile directory now exposes distinct Studio and 44px action
controls with role-correct server-backed paths. The fresh run executed Owner
Copy/Archive, Editor Copy-enabled/Archive-denied and Viewer
Copy-denied/Archive-denied behavior.

Direct pixel and image inspection confirmed
`directory-role-actions-mobile-390x844.png` is exactly 390x844 and visibly
shows both disabled Viewer menu items. The persistent test waits until the Ant
overlay is fully visible, asserts its left edge is non-negative and its right
edge is at most 390px, then checks document scroll width does not exceed client
width before capture. C04 is fully closed.

### AS05-C05 — closed

The Editor scenario is now executable evidence rather than an enabled-control
assertion. It begins at Draft r8, edits Description, invokes the atomic Draft
`PUT`, observes Saved and `Draft · revision 9`, checks persisted description,
revision 9 and zero metadata PATCH requests, then creates an isolated Draft r9
Preview session, sends a message through the SSE path and receives the
projected response. It finally proves Editor Copy remains enabled while
owner-only Archive is denied.

The same fresh 25-case run exercised real Tab/Shift+Tab/Enter/Space/Escape
flows for directory, create, archive dialog and mobile Drawer, including focus
containment, Escape close, trigger focus return, keyboard section selection,
first-error focus and Preview composer focus. C05 is fully closed.

### AS05-C06 — closed

The evidence capture helper now parses every `-WIDTHxHEIGHT` basename and
fails unless it equals `page.viewportSize()`. The four previously mislabeled
files were regenerated and independently read as:

- `preview-draft-events-desktop-1440x900.png`: 1440x900;
- `preview-version-desktop-1440x900.png`: 1440x900;
- `studio-degraded-viewer-desktop-1440x900.png`: 1440x943;
- `create-api-failure-desktop-1440x900.png`: 1440x1340.

All four are 1440 pixels wide. The larger two heights are truthful full-page
rasters captured from a 1440x900 viewport, not false viewport labels. The
fresh required E2E run regenerated and validated these receipts. C06 is fully
closed.

## Requirement Assessment

| Requirement | Independent assessment |
| --- | --- |
| R1 complete Agent information architecture | Approved. The additive Agent directory, three-step Draft-only creation flow and complete V1 Studio sections are present across desktop/mobile, with role-correct actions and honest future Eval/Publish/Channel boundaries; `/assistant` remains independent. |
| R2 truthful conflict-safe editing | Approved. Atomic metadata/spec/revision persistence, validation, conflict, network recovery, reload/reapply and in-flight second-batch preservation passed at browser, API and real PostgreSQL boundaries. |
| R3 isolated explainable Preview | Approved. Saved Draft and immutable Version targets use separate sessions and effective specs; target changes require confirmation; Draft revision changes rebuild; closed tool/Knowledge events, failure taxonomy, clear session and Trace entry pass without protected internals. |
| R4 responsive accessible quality | Approved. Exact 1440x900, 1024x768 and 390x844 evidence, no horizontal overflow, reduced motion, visible keyboard/focus flows and real axe scans with zero serious/critical findings passed. |

## Visual, Permission, Security, Compatibility and Scope Boundaries

- Direct rendered inspection covered directory, controlled creation, conflict,
  Draft/Version Preview, degraded Viewer, tablet, mobile Drawer/Configure/
  Preview, the visible disabled mobile Viewer menu, and live
  provider-unavailable desktop/tablet/mobile evidence. The UI follows the
  existing console language; no generic marketing surface, clipped primary
  action or inaccessible required mobile action was found.
- UI role affordances mirror server-returned `caller_role`, but authorization
  remains enforced by the API/repository. Viewer editing and Copy/Archive are
  denied, Editor save/Preview/Copy works and Archive is denied, and Owner
  create/copy/archive/edit paths execute.
- Templates and recovery state contain only non-secret configuration. Browser
  Preview requests send closed target identity plus normal session/message
  input; the server performs runtime resolution and SSE projection. No API key,
  provider token, generated credential or `.env` value was read, printed or
  changed during this review.
- Agent Studio remains feature-flagged and additive. The existing Assistant,
  dynamic routes and Eval trace passed the same full regression. The historical
  Version Preview seam is the fixed-plan R3 exception; no AS-06 Eval Gate,
  publish/promotion/rollback UI, Hosted page, Embed widget, Runtime API token,
  deployment, commit or push was introduced by the inspected AS-05 surface.
- The local live screenshots correctly show `Model unavailable`; they support
  failure-closed UI behavior, not external-provider answer quality or
  production readiness. The Critic did not independently rerun live Docker.

## Decision and Handoff

`approved`. All six preserved findings are closed on the frozen iteration-3
source, all four exact required gates passed independently with zero skips,
the supplemental transaction/runtime-config checks passed, and no new
material AS-05 finding remains.

This verdict alone does not mutate the Oracle. AS-F006 must remain `failing`
until the orchestrator records the Actor and this independent Critic evidence
and the supported phase claim check exits zero. AS-06 remains locked until
that transition completes; no whole-demand completion claim is made here.
