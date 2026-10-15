# AS-05 Agent Studio Frontend and Preview Actor Report

**Phase:** AS-05 — Agent Studio Frontend and Preview

**Feature:** AS-F006

**Status:** Passed — iteration-3 Critic approved; supported claim check exit 0

**Date:** 2026-07-19

**Actor:** primary implementation agent

## Summary

AS-05 now provides an additive, feature-flagged Agent directory, three-step
Draft-only creation flow, complete V1 configuration Studio and isolated saved
Draft or immutable historical Version Preview. It preserves the existing
Assistant route and visual shell. Owners can create, copy, archive and edit;
Editors can edit; Viewers remain server-bounded and read-only. Controlled
templates copy non-secret prompt/default values only and never copy
capabilities, Knowledge authority, credentials, sessions or memory.

The Studio covers identity, welcome/suggested content, instructions, model and
thinking mode, native/MCP/Skill/Connector capability facts, per-Dataset
Knowledge retrieval settings, memory/safety policy and honest disabled future
Eval/Publish/Channels surfaces. Capability rows expose authoritative source,
risk, health/setup, principal, version and schema facts. Compatibility checks
block unsupported tools or image retrieval instead of silently saving an
unexecutable combination.

Save behavior is revision- and server-truthful: one `If-Match` Draft PUT writes
name, description, spec, normalized resource bindings and the next revision in
one PostgreSQL transaction. Clean, dirty, saving, saved, validation, 409
conflict and network failure are distinct. A failed mutation cannot leave an
earlier metadata PATCH behind; local non-secret edits survive retries, stale
revisions offer Copy/Reload/Reload-and-reapply, and edits made during an
in-flight batch remain dirty for a later save.

Preview consumes only closed Gateway APIs. The Agent runtime SSE route now
projects event-type-specific public fields on the server and drops unknown
events, raw tool results/arguments/metadata/output files, context chunks and
arbitrary error text before any browser receives them. It names the exact saved Draft
revision or immutable Version, shows effective model/resource counts, isolates
session history per target, confirms target switches, rebuilds after Draft
revision changes, restores per-target history and exposes controlled tool/RAG
events without protected prompt, credential, signed Snapshot or arbitrary
internal-result payloads. The bounded AS-05 backend unblocker adds historical
Version Preview resolution without creating a Publication or entering AS-06.

All four required Phase gates pass on the iteration-3 source with zero skips. Real axe checks, the
desktop/tablet/mobile state matrix, deterministic Preview golden and a real
repository-owned stack flow also pass. The live stack has no configured usable
provider, so it correctly returns `Model unavailable`; this is failure-closed
runtime evidence, not an external-provider success claim. AS-F006 remains
`failing` until a new independent Critic approves and the supported claim
check passes. Those conditions are now satisfied: the fresh iteration-3 Critic approved C01-C06 and
independently reran every required gate with zero skips. The supported AS-05
claim check then exited zero with structure score 100/100; it validates claim
metadata only and is not substituted for the product evidence above.

## Plan Followed and Scope

- Fixed plan:
  `docs/agent-studio-prd/reports/as-05-agent-studio-frontend-and-preview-plan.md`.
- Deviation from product architecture: none. The only backend addition is the
  plan-documented closed historical-Version Preview seam required by R3.
- Per the user's explicit direction, the obsolete unsupported Harness
  `--strict` option is not treated as a product gate. No current test,
  accessibility, security, compatibility, rendered-browser or Critic gate is
  waived.
- AS-06 Eval/Publish behavior, Hosted `/a/:publicId`, Embed, Runtime API tokens,
  deployment, commit, push and provider-key changes remain out of scope.

## Iteration-1 Critic Remediation

The first independent verdict is preserved at
`docs/agent-studio-prd/reports/as-05-critic-verdict-iteration-1.md` and remains
`changes_requested`. Iteration 2 implemented a response to every recorded
finding; the second Critic accepted C01-C03 and requested stronger C04/C05
execution evidence, which iteration 3 supplies without waiving or deleting any
finding.

| Finding | Final disposition after iteration 3 | Direct evidence |
| --- | --- | --- |
| AS05-C01 partial metadata/Draft save | Replaced Studio's metadata PATCH followed by Draft PUT with one revision-checked repository transaction. The API, browser harness and real PostgreSQL test prove 409/422/503 leave both metadata and Draft unchanged; a PostgreSQL trigger forces failure after the metadata UPDATE and proves transaction rollback; the live test refreshes after a combined metadata/spec save. | `tests/api/test_agents_api.py`, focused real-PostgreSQL repository test, deterministic save/reload/reapply/in-flight cases, live test |
| AS05-C02 arbitrary Agent SSE payload | Replaced recursive deny-list filtering with a closed event-type projection. Unknown events are dropped; tool, Knowledge, text and error events expose only their explicit public scalar fields. A raw-SSE test injects nested authorization/API-key/client-secret/password-shaped data and proves none reaches the response. Generic `/chat/stream` retains its existing rich contract. | `tests/services/assistant/test_agent_runtime_resolver.py` 11/11 |
| AS05-C03 Compose flag not wired | Base Compose now passes `VITE_AGENT_STUDIO_ENABLED` to the frontend runtime entrypoint. Unit mapping/default/false checks pass, and a real container was recreated with false: Agent navigation/routes disappeared while `/assistant` remained usable; true was then restored and the live Agent flow reran. | runtime-config tests 5/5; live rollback 1/1; final runtime config true |
| AS05-C04 mobile Copy/Archive missing | Mobile rows now keep a distinct Studio link and 44px action button with the same Owner/Editor/Viewer menu policy as desktop. Owner Copy/Archive and Editor/Viewer denials pass at 390px. | deterministic mobile role-action test and `directory-role-actions-mobile-390x844.png` |
| AS05-C05 keyboard/focus/Editor evidence | Tests now use real Tab, Enter, Space, Shift+Tab and Escape; assert Drawer focus containment and return; and explicitly prove Editor save/Preview plus owner-only archive denial. | deterministic keyboard/Drawer and Editor cases; real axe scans |

## Iteration-2 Critic Remediation

The second independent verdict is preserved at
`docs/agent-studio-prd/reports/as-05-critic-verdict-iteration-2.md` with
`changes_requested`. It independently closed C01-C03, then identified three
evidence-execution gaps. Iteration 3 corrects all three without changing the
product architecture or enlarging any timeout.

| Finding | Iteration-3 disposition | Direct evidence |
| --- | --- | --- |
| AS05-C04 durable mobile menu evidence | The 390x844 case waits for the Ant overlay to reach visible opacity, asserts both disabled Viewer items, verifies the overlay's left/right bounds and document width, then captures a viewport image with animations allowed. | `directory-role-actions-mobile-390x844.png`; focused and full Agent E2E |
| AS05-C05 Editor execution proof | The Editor case now executes the atomic Draft save, proves revision 8→9 and persisted description with zero metadata PATCH, creates Draft r9 Preview, sends a message through the isolated SSE path, and retains Copy-enabled/Archive-denied assertions. | deterministic Editor case; harness mutation/session counters |
| AS05-C06 mislabeled desktop evidence | Every capture helper call now asserts that its `-WIDTHxHEIGHT` filename matches `page.viewportSize()`. The four previously default-1280 cases explicitly set 1440x900 and were regenerated at width 1440; full-page height may truthfully exceed 900. | regenerated create-failure, Draft Preview, Version Preview and degraded-Viewer images; focused 5/5 and full 25/25 runs |

## Main Change Groups

| File/group | Result |
| --- | --- |
| `web/src/router.tsx`, `web/src/layouts/AppLayout.tsx`, `web/src/config/runtime.ts` | Additive feature-flagged `/agents`, `/agents/new`, `/agents/:agentId` routes and navigation; existing Assistant remains independent |
| `web/src/api/agents.ts`, `web/src/types/agents.ts` | Closed typed list/create/copy/archive/atomic Draft/Version/catalog/Preview clients and stable error mapping |
| `web/src/pages/agents/*` | Directory, creation flow, Studio sections, conflict-safe saving, responsive drawer/tabs and isolated Preview |
| Agent locale bundles and `web/src/i18n/index.ts` | Synchronized English/Chinese Agent copy included in the repository i18n checker |
| `src/api/schemas/agents.py`, Agent runtime route/schema and `agent_repository.py` | Atomic metadata/Draft transaction plus redacted tenant/ACL-scoped immutable Version Preview resolution; no Publication mutation |
| Assistant Agent runtime stream route | Closed server-side event projection; generic Assistant stream remains unchanged |
| `docker-compose.yml` and frontend runtime config | Environment-driven true/false Agent Studio rollback without image rebuild or Assistant removal |
| `web/e2e/agent-studio.spec.ts` | 25 deterministic route/state/viewport/a11y/save/Preview/security cases, executed Editor authorization, overlay containment and viewport-label-enforced durable screenshots |
| `web/e2e/agent-studio-live.spec.ts` | Real local combined metadata/Draft save with reload, Preview, 1440/1024/390, finally-delete and flag-off Assistant compatibility flow |
| `web/e2e/eval-trace.spec.ts` | Stabilizes the existing mobile overflow-tab interaction with keyboard activation and semantic selected-state assertion; keeps a 15-second action timeout instead of masking the failure with a larger total timeout |
| AS-05 evidence | Actor report, browser matrix, Preview golden and rendered deterministic/live screenshots |

## Requirement Results

| Requirement | Actor result | Evidence |
| --- | --- | --- |
| R1 complete Agent information architecture | passed | All three routes, create/copy/archive including mobile role actions, every V1 Studio section, English/Chinese copy and actual runtime feature-flag fallback pass in 25 deterministic cases |
| R2 truthful conflict-safe editing | passed | One transaction covers metadata/spec/revision; 409/422/503 zero-partial-write, first-field focus, reload/reapply, retry and in-flight second-batch preservation all pass in memory, real PostgreSQL and live browser evidence |
| R3 isolated explainable Preview | passed | Draft r8 and Version 7 have separate sessions/effective specs; target switch is confirmed; server-projected tool/RAG events, raw-SSE redaction, clear failure taxonomy and Version resolution tests pass |
| R4 responsive accessible quality | passed | 1440x900, 1024x768 and 390x844 screenshots; zero serious/critical axe findings; real keyboard traversal/focus trap/return, labels, reduced motion, console/network and overflow checks pass |

## Required Validation Evidence

| Gate | Exact command | Final Actor result |
| --- | --- | --- |
| Frontend static | `corepack pnpm@10.33.0 -C web lint && corepack pnpm@10.33.0 -C web type-check && corepack pnpm@10.33.0 -C web i18n:check && corepack pnpm@10.33.0 -C web build` | exit 0; lint 0 errors and 17 inherited warnings; TypeScript pass; English/Chinese keys synchronized; production build pass |
| Agent Studio E2E | `corepack pnpm@10.33.0 -C web exec playwright test e2e/agent-studio.spec.ts --config playwright.opensource.config.ts` | exit 0: 25 passed, 0 failed, 0 skipped in 1.1m |
| Existing route E2E | `corepack pnpm@10.33.0 -C web e2e:opensource` | exit 0: 31 passed, 0 failed, 0 skipped in 1.1m |
| Preview contract | `uv run pytest -q --no-cov tests/api/test_agents_api.py && uv run --package assistant-service pytest -q --no-cov tests/services/assistant/test_agent_runtime_resolver.py` | exit 0: 9 passed + 11 passed, 0 failed, 0 skipped |
| Supported phase claim | `python3 /Users/yang/.codex/skills/prd-phase-harness/scripts/validate_harness_prd.py docs/agent-studio-prd --claim-check --phase AS-05 --quality-score` | exit 0 after the truthful Oracle/loop transition; metadata internally consistent; structure score 100/100. The first invocation correctly failed while loop state was still `running` and is not counted as a pass. |

The existing-route command initially returned 28/29 because its long Eval test
clicked a mobile Ant tab while that overflow tab was being repositioned. Trace
timings proved all prior work completed in about 9 seconds and the first click
then waited roughly 113 seconds without selecting the tab. The test now
activates the focused tab through its keyboard contract, asserts
`aria-selected=true`, then clicks the visible custom segmented label. The
focused node passed in 7.9 seconds and the exact full regression subsequently
passed 29/29. The total timeout was not enlarged.

The local host uses Node 24.14.0 while `web/package.json` requests Node
`^22.12.0`; pnpm reports an engine warning. The production Docker build uses
the supported Node 22 base and passed. Vite also reports the existing large
shared UI chunk warning; the Agent chunk is about 80.81 kB minified and the
warning is not an AS-05 functional failure.

## Supplemental Validation and Live Evidence

| Check | Actor result |
| --- | --- |
| Python static check | `uv run ruff check` over the changed Agent runtime schema/route/repository/tests exited 0: `All checks passed!` |
| Compose and PostgreSQL supplement | Runtime entrypoint/Compose mapping 5/5 plus a real PostgreSQL atomic repository test 1/1 passed |
| Live Agent Studio | Remote Playwright against repository-owned containers passed after final true restore: one combined metadata/Draft save, reload persistence, Preview boundary, three viewports and `finally` delete |
| Live rollback flag | Frontend recreated from Compose with `VITE_AGENT_STUDIO_ENABLED=false`; actual runtime config returned false and browser 1/1 proved Agent surfaces absent plus existing Assistant usable. Frontend was restored to true and the live Agent test passed again. |
| Cleanup | PostgreSQL returned `0` live rows matching `AS05 Live %` after the live test |
| Compose ownership | All eight `ai-gateway-*` containers report `/Users/yang/projects/AI--Platfform` in `com.docker.compose.project.working_dir` |
| Health and memory | 8/8 services healthy after serial frontend build/recreate and bounded Gateway/Assistant hot update; final sample about 763 MiB total |
| Rendered inspection | Latest deterministic and live desktop/tablet/mobile images were opened with direct image inspection; no clipping, unreadable content or unreachable primary action remained |
| Working-tree whitespace | `git diff --check` exited zero after evidence writeback |

The first run of the expanded PostgreSQL atomic-save case failed because its
isolated Dataset fixture lacked the current `is_deleted` column required by
the production resolver. The fixture was corrected to the production minimum;
the exact real-PostgreSQL case then passed. The final keyboard run likewise
exposed a case-sensitive zero-match locator (`Blank Agent` versus the rendered
`Blank agent`); the helper now fails fast on non-unique or missing targets, the
test uses the exact accessible prefix, and both the 25-case Agent suite and
31-case existing-route suite pass without enlarging the timeout.

No API key, provider token, database password, JWT or generated `.env` value was
printed or changed. The live browser setup uses generated local development
credentials only; they remain in ignored Playwright artifacts and are not
copied into evidence.

## Browser, Accessibility, and Security Evidence

- Durable matrix:
  `reports/agent-studio/as-05-browser-matrix.md`.
- Deterministic screenshots:
  `reports/agent-studio/as-05/matrix/` (32 state/viewport images).
- Live screenshots:
  `reports/agent-studio/as-05/studio-desktop.png`,
  `studio-tablet.png`, and `studio-mobile.png`.
- Preview golden:
  `reports/agent-studio/as-05-preview-golden.json`.
- `assertNoBlockingA11yIssues` invokes real `@axe-core/playwright` analysis.
  Directory, create, Studio/Preview and mobile scans returned zero serious or
  critical violations.
- Keyboard/focus checks use real Tab/Enter/Space/Shift+Tab/Escape and cover Create,
  first invalid field, Drawer focus containment/return, dialog semantics and
  Test-in-Preview focus transfer. Editor save/Preview and owner-only archive
  denial are explicit. Reduced-motion
  animation duration and all required horizontal-overflow assertions pass.
- Happy-path page error, console error and unexpected API response listeners
  remained clean. Deliberate negative responses are scoped to their explicit
  403/409/422/500/503 tests.
- The controlled template assertion rejects credential/token/secret/OAuth
  fields and proves capabilities/Knowledge are empty. Icon URL input accepts
  public HTTPS only and rejects userinfo, query and fragment components.
- Preview requests contain only normal session/message input plus closed target
  identity. Browser code does not submit trusted model, Prompt, capability,
  Snapshot, secret or Publication overrides. Tool event rendering is a closed
  projection rather than arbitrary result JSON; the server drops non-public
  fields before serialization, and the raw-SSE credential-shaped regression
  proves the boundary independently of React rendering.

## Visual Fidelity and Copy Diff

The implementation follows the existing protected AppLayout, Assistant and
Knowledge form language and the four generated AS-05 concepts beneath
`/Users/yang/.codex/generated_images/019f7317-747b-7e41-98de-91658f1f5287/`.
Generated images were treated as design specifications only; all shipped text
and controls are real responsive code.

### Five-point mismatch ledger

| Area | Initial mismatch | Final disposition |
| --- | --- | --- |
| Directory density | Early cards did not expose enough ownership/status facts | Desktop uses a compact table and mobile uses fact-preserving cards; both retain actions and empty/error states |
| Disabled actions | Disabled Continue/Save could read visually as active | Disabled primary actions now use an explicit neutral treatment in addition to `disabled` semantics |
| Mobile navigation | Section drawer was captured mid-close and capability rows crowded controls | Drawer evidence waits for close; a separate open-Drawer capture exists; rows stack and facts wrap at 390px |
| Preview truth | Early empty copy did not sufficiently distinguish saved state | Header, info banner, effective summary and unsaved warning name Draft rN or Version N before a session starts |
| Policy facts | Long memory/safety and principal/schema facts risked overflow | Facts wrap into responsive blocks while retaining labels/icons and no horizontal scroll |

### Above-the-fold copy diff

| Concept intent | Final product copy | Reason |
| --- | --- | --- |
| Agent Studio catalog | `Agents` + `Create, configure, test, and publish reusable agents.` | Matches existing navigation noun style and keeps future publication honest |
| Generic new Agent CTA | `Create agent` / `Create blank agent` | Distinguishes the directory action from controlled starters |
| Generic saved badge | `Draft · revision N` plus `All changes saved`, `Unsaved changes`, `Saving…` or `Saved` | Makes server revision and mutation state explicit |
| Generic tester panel | `Preview runs the saved Draft rN` or saved `Version N` | Prevents users from assuming unsaved fields are running |
| Generic failure | Stable configuration/resource/permission/provider/runtime labels | Supports actionable recovery without leaking internal payloads |

## Rollback and Residual Risk

- Setting the Agent Studio runtime frontend flag false removes navigation and
  all Agent routes without deleting backend data or changing `/assistant`.
  This was executed against the built container and then restored to true.
- Historical Version Preview is additive and closed. Removing its route/client
  leaves saved Draft Preview and immutable data intact; it creates no
  Publication or channel pointer.
- The local provider is unavailable, so no external answer quality, token
  accounting or provider latency claim exists. The UI and runtime failure
  boundary is verified; real-provider readiness remains a later environment
  concern.
- The existing shared UI chunk remains large. The AS-05 Agent chunk is bounded,
  but route-level performance/load measurement remains aggregate release work.
- Whole-demand same-build regression, publication, rollback, Hosted, Embed,
  Runtime API and final Assistant compatibility are still owned by AS-06 through
  AS-09. This Actor result cannot substitute for those gates.

## Independent Critic and Handoff

Both preserved `changes_requested` verdicts remain durable. The fresh
iteration-3 canonical Critic independently closed AS05-C01 through C06,
reran the four exact required commands and supplemental transaction/Compose
checks, and returned `approved`. The orchestrator then transitioned AS-F006 and
the supported phase claim check exited zero with structure score 100/100.

AS-F006 is passing and AS-06 is dependency-unlocked. AS-06 receives stable
Draft revision state, atomic save behavior, immutable Version Preview selectors,
closed API errors, isolated Preview sessions and browser fixtures. This is not
a claim that Eval/Publish/Rollback, Hosted, Embed, Runtime API, deployment or
AS-09 whole-demand regression is complete.
