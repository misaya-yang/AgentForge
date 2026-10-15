# AS-05 Independent Critic Verdict — Iteration 2

**Phase:** AS-05 — Agent Studio Frontend and Preview  
**Feature:** AS-F006  
**Critic:** `/root/as05_critic_iteration_2` — fresh independent subagent  
**Verdict:** `changes_requested`  
**Date:** 2026-07-19

The Actor report was used only as a navigation aid. This verdict comes from
direct source/test inspection, independent execution of every required Phase
command, supplemental PostgreSQL/runtime-config tests, focused Playwright
trace inspection, and direct inspection of the rendered evidence.

## Independently Rerun Gates

| Gate | Iteration-2 Critic result |
| --- | --- |
| `corepack pnpm@10.33.0 -C web lint && corepack pnpm@10.33.0 -C web type-check && corepack pnpm@10.33.0 -C web i18n:check && corepack pnpm@10.33.0 -C web build` | exit 0; lint 0 errors/17 warnings; type-check, i18n and production build passed |
| `corepack pnpm@10.33.0 -C web exec playwright test e2e/agent-studio.spec.ts --config playwright.opensource.config.ts` | exit 0; 25 passed, 0 failed, 0 skipped in 1.1m |
| `corepack pnpm@10.33.0 -C web e2e:opensource` | exit 0; 31 passed, 0 failed, 0 skipped in 1.1m |
| `uv run pytest -q --no-cov tests/api/test_agents_api.py && uv run --package assistant-service pytest -q --no-cov tests/services/assistant/test_agent_runtime_resolver.py` | exit 0; 9 passed + 11 passed, 0 failed, 0 skipped |

The host used Node 24.14.0 while the package requests Node `^22.12.0`; pnpm
reported the known engine warning. Vite also reported the existing large
shared-chunk warning. Neither changed an exit code.

Supplemental independent checks also passed:

- `uv run pytest -q --no-cov tests/scripts/test_web_runtime_config_entrypoint.py tests/database/test_agent_studio_migrations.py::test_repository_enforces_tenant_revision_version_and_hash_only_token_contracts`
  returned 6 passed, 0 failed, 0 skipped. This includes five runtime-config /
  Compose mapping cases and the real PostgreSQL trigger rollback case.
- A focused one-case Playwright run for mobile role actions passed 1/1 with
  tracing enabled. The trace placed the Viewer menu at `top: 560px` and
  `right: 21px` in a 390x844 viewport and showed both disabled menu items.
- `git diff --check` exited zero.

The green commands do not repair the remaining evidence gaps described below.

## Iteration-1 Finding Disposition

### AS05-C01 — closed

The Studio no longer sends metadata `PATCH` followed by a Draft `PUT`. One
`If-Match` Draft request now carries optional `name`/`description`; the
repository locks the authorized Agent and Draft and updates metadata, resource
bindings, Draft revision/spec and audit record inside one PostgreSQL
transaction. The browser fixture records zero metadata PATCH requests.

Independent evidence:

- the required Agent API suite passed 9/9, including 409/422/503 unchanged
  metadata/Draft state and a successful whole-batch retry;
- the supplemental real-PostgreSQL case passed and forces a Draft UPDATE
  exception after the metadata UPDATE, then proves metadata and revision rolled
  back;
- deterministic browser cases retain local edits across validation, conflict,
  storage failure, reapply and in-flight second-batch behavior.

### AS05-C02 — closed

The Agent-only Assistant stream now uses an explicit event-type allowlist and
field projection. Unknown events are dropped; text, tool, Knowledge and error
events expose only their documented public scalars. Arbitrary result,
arguments, metadata, output files and raw error text are not serialized. The
generic `/chat/stream` route remains unchanged.

The required 11/11 resolver suite includes both a raw SSE injection containing
nested authorization/API-key/client-secret/password-shaped data and a generic
Assistant rich-event compatibility test. Both passed independently.

### AS05-C03 — closed at the implementation/deterministic boundary

Base `docker-compose.yml` passes
`VITE_AGENT_STUDIO_ENABLED: "${VITE_AGENT_STUDIO_ENABLED:-true}"` to the
frontend entrypoint. The five supplemental mapping/default/false tests passed,
and the required browser suite proved Agent navigation/routes disappear while
the existing Assistant composer remains available when false is injected.

Per the Critic assignment, containers were not modified or restarted. The
Actor's reported real false/true container recreation was therefore inspected
but not independently reenacted and is not represented as a Critic runtime
receipt. This boundary does not negate the source, entrypoint, Compose and
browser evidence above.

### AS05-C04 — implementation fixed; required durable evidence still incomplete

Mobile rows now have separate Studio links and 44px action buttons using the
same role policy as desktop. At 390x844, Owner Copy and Archive execute, Editor
Copy is enabled/Archive disabled, and Viewer Copy/Archive are disabled. The
required and focused tests passed.

However, the test captures `directory-role-actions-mobile-390x844.png` with
animations disabled while the Ant menu is still in `appear-start`; the
resulting durable image shows only the highlighted action button, not the
Editor/Viewer disabled menu items. The case also does not assert the open
overlay's bounding box or horizontal containment. A temporary focused trace
showed the actual overlay in bounds, but the Phase requires durable rendered
mobile evidence, not a Critic-only temporary trace. C04 is therefore not yet
fully closed.

Required correction: wait for the menu transition before capture (or capture
without forcing it back to the start state), visibly record the disabled
items, and assert the open overlay remains within the 390px viewport with no
document horizontal overflow.

### AS05-C05 — keyboard/focus fixed; Editor execution proof incomplete

The suite now uses real Tab/Enter for the list, Tab/Space and Tab/Enter in
creation, keyboard confirmation in the archive dialog, and
Tab/Shift+Tab/Enter/Space/Escape for the mobile Drawer. It asserts Drawer focus
containment, Escape close, trigger focus return, section selection, first field
error focus and Preview composer focus. These flows passed without direct
`.focus()` shortcuts in the Agent suite.

The new Editor case is still assertion-only at the mutation/runtime boundary:
it fills Description and checks that Save and New session are enabled, then
navigates away without clicking Save or starting Preview
(`web/e2e/agent-studio.spec.ts:730-742`). The Actor report calls this “Editor
save/Preview” evidence, but neither request executes. Owner-only Archive denial
is checked. C05 remains partially open because enabled UI is not proof that the
Editor path successfully crosses the server-backed save and Preview seams.

Required correction: in the Editor fixture, execute Save and verify the Draft
batch/revision changed, execute New session and verify an isolated Preview
session was created, then retain the Copy-enabled/Archive-denied assertions.

## New Finding

### AS05-C06 — medium — desktop viewport evidence is mislabeled

Several files whose names and matrix rows claim `desktop-1440x900` were
captured under Playwright's default Desktop Chrome 1280x720 viewport because
their tests never call `page.setViewportSize`:

- `preview-draft-events-desktop-1440x900.png` is 1280x720;
- `preview-version-desktop-1440x900.png` is 1280x720;
- `studio-degraded-viewer-desktop-1440x900.png` is 1280x763 (full-page output);
- `create-api-failure-desktop-1440x900.png` is 1280x1340 (full-page output).

The missing explicit viewport is visible in
`web/e2e/agent-studio.spec.ts:575-590,679-727`. This is not the benign case
where a 390x844 or 1440x900 viewport produces a taller full-page raster; those
tests explicitly set the viewport and their taller image height is truthful.
Here the width itself proves that the claimed 1440 viewport was not used.

R4 and the Phase browser contract require truthful 1440x900 Studio state
coverage, including degraded/Viewer and Preview evidence. A 1280 desktop run
does support general desktop behavior but cannot be relabeled as the exact
required viewport.

Required correction: explicitly set 1440x900 before each claimed 1440 capture,
refresh the affected screenshots/matrix/report, or rename non-required default
desktop evidence truthfully and add the missing exact 1440 captures. Do not
force full-page raster height to 900; only the actual viewport and evidence
label must be truthful.

## Requirement Assessment

| Requirement | Independent assessment |
| --- | --- |
| R1 complete Agent information architecture | Product source substantially satisfies the additive Agent directory/create/Studio IA and preserves `/assistant`; mobile actions exist, but C04's required rendered containment receipt is incomplete. |
| R2 truthful conflict-safe editing | Passed at source, API, deterministic browser and real PostgreSQL transaction boundaries. C01 is closed. |
| R3 isolated explainable Preview | Passed at closed target/session, immutable Version, server projection, raw-SSE redaction and generic Assistant compatibility boundaries. C02 is closed. No external-provider answer-quality claim is made. |
| R4 responsive accessible quality | Real axe, reduced motion, core 1440/1024/390 layouts and keyboard/Drawer flows pass. C04's durable open-menu evidence, C05's executed Editor save/Preview path and C06's truthful exact viewport evidence remain incomplete. |

## Visual, Security, and Scope Boundaries

- Direct image inspection covered populated/empty/error/permission directory,
  creation, conflict, Draft/Version Preview, degraded Viewer, tablet, mobile
  Drawer/Configure/Preview and live provider-unavailable states. The product
  follows the existing console visual language and no generic marketing
  surface or visible primary-layout horizontal clipping was found.
- Full-page screenshots legitimately exceed viewport height. This verdict
  rejects only false viewport-width labels and the hidden mobile menu receipt.
- The closed Agent stream and client requests do not submit trusted prompt,
  model, capability, Snapshot, Publication or Secret overrides. No API key,
  provider token, generated credential or `.env` value was read, printed or
  changed during review.
- The historical Version Preview seam is the fixed-plan backend exception. No
  AS-06 Eval/Publish product behavior, Hosted page, Embed, Runtime API token,
  deployment, commit or push was introduced by the inspected AS-05 work.
- The Critic did not modify or restart Docker and does not promote Actor live
  evidence into independent live-runtime evidence.

## Decision and Handoff

`changes_requested` is required. C01, C02 and C03 are closed, and the core C04
and C05 implementation work is present, but the non-waivable responsive and
permission evidence is not yet truthful/complete. Green prescribed commands
support only the behavior they actually observe.

AS-F006 must remain `failing`; AS-05 is not complete and AS-06 remains locked.
After correcting C04, C05 and C06, rerun all four exact Phase commands with
zero skips, refresh only affected evidence, and request a new fresh independent
Critic. Do not run the completion gate from this verdict.
