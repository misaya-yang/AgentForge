# AS-05 Agent Studio Frontend and Preview Plan

- **Phase:** AS-05 — Agent Studio Frontend and Preview
- **Feature Oracle:** AS-F006 only
- **Status:** fixed Phase-contract execution record; Actor iteration 1 in progress
- **Date:** 2026-07-18
- **Scope rule:** execute the existing AS-05 contract without replanning, shrinking, or entering AS-06 Eval/Publish product work

This artifact transcribes the approved AS-05 Phase and UX contract into an
execution record. It does not change the product architecture, dependency
order, acceptance gates, or terminal AS-09 release standard.

## Dependency and Runtime Baseline

- AS-03/AS-F004 and AS-04/AS-F005 are both `passing` with independent Critic
  approval and supported phase claim checks. The UI may consume their
  authoritative MCP/Connector/Skill/Knowledge resource state but cannot
  calculate or widen authorization.
- The current React/Vite console uses React Router, TanStack Query, Ant Design,
  Tailwind utility classes, Lucide icons, Zustand auth state and i18next. The
  protected `AppLayout`, Knowledge query/form patterns and mature Assistant
  streaming components remain the visual and implementation baseline.
- No Agent frontend paths, typed Agent client, Agent E2E fixture or browser
  matrix exists yet. Existing `/assistant`, `/knowledge`, `/playground` and
  `/eval` routes must remain unchanged except for bounded reusable extraction.
- Repository-owned Compose is 8/8 healthy around 725-727 MiB. Browser fixtures
  are deterministic and provider-free; Docker must remain serial and below the
  operator's 3.5 GiB ceiling. API keys, deployment, commit and push remain out
  of scope.

## Documented Contract Gap

The passed backend exposes saved-Draft Preview and published-channel runtime,
but no closed endpoint for running an arbitrary immutable historical Version.
AS-05 R3 and its acceptance/browser gates explicitly require both Draft and
Version Preview before AS-06 publication exists. The operator's broad local
implementation authorization is applied narrowly to this documented blocker:

- add an authenticated, tenant/ACL-scoped repository resolver and closed
  Version Preview session/stream request that accepts only `agent_version_id`
  plus normal session/message input;
- derive every model, prompt, capability, Knowledge and runtime field from the
  immutable Version; bind a new isolated session and reject revocation or
  cross-Agent/cross-tenant access;
- add API/repository/Envelope regression evidence;
- do not create or mutate a Publication, channel, Eval Gate, Runtime API token,
  Hosted route or Embed route.

Any broader backend API change remains a stop condition.

## Requirement-to-Change Map

| Contract | Bounded implementation | Primary evidence |
| --- | --- | --- |
| R1 complete Agent information architecture | Add feature-flagged navigation and `/agents`, `/agents/new`, `/agents/:agentId`; provide list/search/filter/create/copy/archive and Studio sections for overview, instructions, model, capabilities, Knowledge, memory/safety, and forward-looking disabled/deep-link entry surfaces. | route/state E2E, desktop/mobile screenshots, existing route regression |
| R2 truthful conflict-safe editing | Typed API/query layer with server revision/ETag; explicit clean/dirty/saving/saved/validation/network/409 states; preserve non-secret local edits, conflict comparison/reapply, first-error focus and Viewer read-only behavior. | deterministic fixture mutations, conflict/network/accessibility tests |
| R3 isolated explainable Preview | Separate Draft/Version selection, saved-revision labels, unsaved warning, explicit new-session rebuild on revision/version change, streaming events/effective capability summary/clear/Trace link without protected internals. | closed Preview API tests, provider-free Preview fixture/golden, browser interaction evidence |
| R4 responsive accessible quality | Existing visual language plus a complete concept-derived design system; desktop/tablet/mobile layouts, configuration/preview tabs, accessible section drawer/dialogs, visible focus, AA contrast, labels/errors, reduced motion and no horizontal overflow. | Browser-first screenshots, axe/keyboard/focus/reduced-motion matrix, lint/type/i18n/build |

## Design and Fidelity Contract

- Before coding the new surface, inspect rendered `/assistant` and `/knowledge`
  chrome, then generate complete Agent directory, creation, Studio desktop and
  Studio mobile visual concepts in the same white/neutral/primary visual
  system. Concepts are specifications, not marketing art or alternate product
  architecture.
- Extract tokens, typography, icon family, component variants, density,
  container rules and responsive transitions before implementation. Real form
  controls/text remain code-native; no static screenshot is shipped as UI.
- Browser/IAB is the first validation path. Final fidelity requires direct
  `view_image` inspection of both accepted concepts and latest rendered
  screenshots, a five-point mismatch ledger and above-the-fold copy diff.

## Bounded File Groups

- `web/src/router.tsx`, `web/src/layouts/AppLayout.tsx`, bounded runtime feature
  config and Agent navigation/locales.
- `web/src/pages/agents`, `web/src/components/agents`,
  `web/src/services/agents.ts`, `web/src/types/agents.ts` and any focused Agent
  query/form hooks.
- Existing Assistant components only through narrow reusable imports or
  extraction; no behavior rewrite of `/assistant`.
- `web/e2e/agent-studio.spec.ts`, `web/e2e/fixtures/agent-studio.ts` plus durable
  Phase browser matrix/screenshots.
- Minimal documented Version Preview unblocker in Agent repository/runtime
  schema/route/tests only.
- AS-05 plan/report/golden/browser evidence and Harness writeback.

## Fixed Execution Sequence

1. Run the pre-change frontend baseline; inspect existing rendered visual
   language and create the complete concept/design-system inventory.
2. Add exact TypeScript API models/client/query keys, stable error mapping and
   non-secret recovery state; add the minimal closed historical-Version Preview
   backend seam and contract tests.
3. Add feature-flagged routes/navigation and build the Agent directory plus
   three-step creation flow with loading/empty/filtered/error/permission and
   template exclusion states.
4. Build the Studio shell and sections with Owner/Editor/Viewer semantics,
   truthful resource facts, exact Draft revision state, conflict/network
   recovery and no client-side authorization assumptions.
5. Integrate isolated Draft/Version Preview with new-session behavior,
   effective capability/RAG/tool events, explicit saved-state labels, clear
   session and trace deep link while redacting internal prompt/secret fields.
6. Implement deterministic fixtures and the full route/state/viewport/axe/
   keyboard/console/network matrix; compare rendered screenshots against the
   concepts and repair every material mismatch.
7. Run every exact Phase command and existing-route regression, write the Actor
   report/browser matrix, freeze the source, and request a fresh independent
   visual/accessibility Critic. Keep AS-F006 `failing` until approval and the
   supported claim check.

## Required Validation Gates

| Gate | Exact Phase command | Required outcome |
| --- | --- | --- |
| Frontend static | `corepack pnpm@10.33.0 -C web lint && corepack pnpm@10.33.0 -C web type-check && corepack pnpm@10.33.0 -C web i18n:check && corepack pnpm@10.33.0 -C web build` | All four commands exit zero on final source. |
| Agent Studio E2E | `corepack pnpm@10.33.0 -C web exec playwright test e2e/agent-studio.spec.ts --config playwright.opensource.config.ts` | List/create/Studio/save/conflict/permission/degraded/Preview/responsive/accessibility fixtures pass with zero skips. |
| Existing route E2E | `corepack pnpm@10.33.0 -C web e2e:opensource` | Existing dynamic route and Eval trace suites pass. |
| Preview contract | `uv run pytest -q --no-cov tests/api/test_agents_api.py && uv run --package assistant-service pytest -q --no-cov tests/services/assistant/test_agent_runtime_resolver.py` | Existing Agent API/resolver contracts plus the minimal Version Preview seam pass. |

## Browser and Compliance Matrix

- `/agents`: 1440x900 and 390x844, loading/empty/populated/filtered-empty/API
  error/permission, keyboard order, no horizontal overflow.
- `/agents/new`: blank/template/validation/created/failure, dialog/sheet focus,
  no inaccessible-resource or credential copy.
- `/agents/:agentId`: 1440x900, 1024x768 and 390x844, clean/dirty/saving/
  saved/validation/409/network/degraded/Viewer, accessible section navigation.
- Preview: saved Draft rN and immutable Version N, unsaved warning, new session
  after selection/apply, effective capabilities/tool/RAG events, clear and
  Trace link, stable configuration/provider/runtime errors.
- Axe zero critical/serious on list/create/Studio; visible focus, associated
  errors, reduced motion, clean console/network receipts and no browser-held
  Secret/token/recovery fields.

## Rollback and Stop Conditions

- A runtime frontend feature flag removes Agent navigation/routes without
  deleting backend data; existing `/assistant` and its components remain the
  independent fallback.
- Stop on an undocumented backend contract beyond the Version Preview blocker,
  client authority calculation, hidden-control authorization, fake save,
  unrecoverable conflict/network state, session hot-swap, Secret persistence,
  critical/serious axe failure, mobile unreachable action, wrong Compose owner
  or memory near 3.5 GiB.
- AS-06 Eval/Publish, `/a/:publicId`, Embed, Runtime API tokens, deployment,
  commit and push are explicitly excluded.
