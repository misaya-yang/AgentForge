# AS-07 Hosted App, Embed Widget, and Runtime API Actor Report

**Phase:** AS-07 — Hosted App, Embed Widget, and Runtime API  
**Feature:** AS-F008  
**Status:** completed; Actor gates passed, iteration-2 Critic approved, supported claim check passed  
**Date:** 2026-07-19  
**Actor:** primary implementation agent

## Outcome

AS-07 exposes only immutable AS-06 Publications through three server-owned
channels: stable Hosted pages, an exact-Origin dedicated Embed document and a
scoped server Runtime API. Clients can select only closed channel request
fields. The Gateway still resolves Version, Prompt, model, resources, policy
and session identity, then signs the existing AS-02 Runtime Envelope.

The implementation adds hashed one-time Runtime tokens with scope, expiry,
rotation, revocation and last-use metadata; shared atomic principal/IP/
Publication quotas; terminal-result idempotency; scoped opaque attachment
handles; feedback boundaries; private/tenant/public Hosted access; a
short-lived Origin-bound browser Embed token; exact dynamic CSP and production
Nginx/Helm routing; versioned source/origin-checked postMessage; responsive
Hosted/Widget surfaces with attachment and citation rendering; token management
UI; and an executable Python SDK example.

All required AS-07 Actor gates have real passing evidence after correcting the
three findings from the preserved iteration-1 Critic verdict: C-01 shared quota
enforcement, C-02 duplicate idempotent execution, and C-03 Hosted attachments
and citations. The initial live
isolation invocation was not counted because two cases skipped without
credentials. After supplying the ignored local bootstrap account, the first
non-skipped attempt failed because this machine has no usable provider key and
the Assistant model list was empty. The existing provider-free E2E Stub was
then enabled only in Gateway/Assistant, current source was hot-synced, and both
live contracts passed 2/2. The formal services were finally recreated and
hot-synced with Stub disabled. No API key was read, changed or invented.

## Contract and Scope

- Fixed plan: `reports/as-07-hosted-app-embed-widget-and-runtime-api-plan.md`.
- Architecture deviation: none. Gateway remains the only Agent resolver;
  Assistant consumes verified runtime state.
- AS-06 Publication creation/evaluation semantics, Agent Draft/Version
  mutation, MCP/Skill/Knowledge contracts, `/share/:shareId`, billing,
  custom domains and production DNS remain unchanged.
- Ordinary console/Hosted responses remain anti-framed. Only the dedicated
  `/embed/agents/:publicId` response removes XFO and emits exact
  Publication-derived `frame-ancestors`.
- No provider credential, generated password, raw token/hash, Prompt body,
  Secret ref or signed Snapshot was added to evidence.

## Main Change Groups

| Group | Result |
| --- | --- |
| Migrations 079–080 and repository | additive token lifecycle, durable idempotency terminal state/result, opaque attachment metadata and feedback persistence; real PostgreSQL concurrency/lifecycle evidence |
| Runtime/public routes and schemas | closed Hosted/Embed/API contracts, stable errors, Version/session pinning, anonymous policy reduction, shared Redis principal/IP/Publication quotas and scoped attachment upload/resolve |
| Management API/Studio release UI | stable Hosted URL, Embed loader snippet, API endpoint, one-time token issue/rotate and redacted list/revoke |
| Hosted page | public/private access, suggestions, streaming, opaque attachment upload/removal, citation rendering, feedback, error/disabled/quota states and responsive keyboard UI |
| Embed assets | launcher/inline iframe, exact Origin initialization, short-lived browser token, versioned source/origin-checked postMessage, focus/resize/reduced motion |
| Nginx/Helm/Ingress | dedicated dynamic Embed route; ordinary SPA anti-framing preserved; built response smoke |
| SDK | environment-only Runtime token/publication input, session creation and SSE example |
| Tests/evidence | API/security, real PostgreSQL, built headers, desktop/mobile Playwright, AHR/isolation/open-source regression and durable screenshots |

## Requirement Results

| Requirement | Actor result | Evidence |
| --- | --- | --- |
| R1 Stable Hosted Delivery | passed | stable `/a/:publicId`, private redirect, public welcome/SSE/feedback, enabled/disabled attachment UI, opaque upload handle, citation rendering, quota/disabled-safe errors, desktop/mobile/axe/redaction evidence |
| R2 Origin-Isolated Embed | passed | exact Origin schema/runtime denial, short-lived bound token, CSP/no-XFO route, sandbox, protocol/source/origin checks, focus/resize and built Nginx/Gateway headers |
| R3 Scoped Runtime API | passed | SHA-256 token persistence, one-time raw display, scope/expiry/rotate/revoke/last-use, Publication-principal session binding, terminal-result idempotent replay with one downstream execution, SSE, opaque attachment and feedback tests |
| R4 Safe Anonymous Defaults and Rollback | passed | public session memory forced to session; high-risk/write bindings removed; Redis-atomic principal/IP/Publication minute/day denial; stable Embed abuse identity; immediate channel disable; existing sessions remain Version pinned |

## Exact Required Validation

| Gate | Exact command | Final result |
| --- | --- | --- |
| Runtime API/security | `uv run pytest -q --no-cov tests/api/test_agent_runtime_api.py tests/security/test_agent_channel_security.py` | exit 0; 24 passed, 0 skipped; includes rotated-principal shared-IP denial, multi-limiter Publication concurrency, exact retry replay and concurrent one-invocation assertions |
| Frontend static/build | `corepack pnpm@10.33.0 -C web lint && corepack pnpm@10.33.0 -C web type-check && corepack pnpm@10.33.0 -C web i18n:check && corepack pnpm@10.33.0 -C web build` | exit 0; lint 0 errors/17 inherited warnings; type, i18n and production build passed |
| Header contract | `uv run pytest -q --no-cov tests/deployment/test_agent_embed_headers.py && bash scripts/new/test-agent-embed-headers.sh --config-only` | exit 0; 5 passed, 0 skipped; config smoke passed |
| Built headers | `bash scripts/new/test-agent-embed-headers.sh --built-image` | exit 0; built frontend Hosted and dynamic Embed responses passed |
| Channel browser | `corepack pnpm@10.33.0 -C web exec playwright test e2e/agent-hosted.spec.ts e2e/agent-embed.spec.ts --config playwright.opensource.config.ts` | exit 0; 8 passed, 0 skipped; includes enabled/disabled attachments, opaque-handle redaction and citation rendering |
| Runtime regression | `make verify-assistant-runtime-dev && make test-isolation && corepack pnpm@10.33.0 -C web e2e:opensource` | exit 0; AHR 33/77/8/98 plus golden; isolation static 4 passed with 2 credential skips; OSS 41 passed. The same current source then ran the two credentialed live isolation cases separately: 2 passed, 0 skipped |

The exact regression command's two skips are not treated as passes. They are
superseded by the explicit credentialed 2/2 live invocation while the other
four isolation cases had already passed. Thus every collected isolation case
has an actual passing receipt.

## Supplemental Evidence

| Check | Result |
| --- | --- |
| Real PostgreSQL | `tests/database/test_agent_channel_runtime.py` -> 3 passed, including migration reentrancy, token lifecycle and two-client atomic idempotency reservation/replay |
| Local main schema | migrations 079 and 080 applied; attachment table and idempotency terminal fields verified |
| Python static | focused Ruff passed; SDK `py_compile` passed |
| SDK execution | deterministic localhost fixture created a session and decoded `{'event_type': 'text_delta', 'content': 'SDK fixture response'}` |
| Assistant attachment resolver | targeted Assistant tests passed; signed opaque handle resolution reaches `file_paths`, while absolute/out-of-scope paths are rejected |
| Shared quota backend | live Redis probe using two native clients passed one atomic Publication admission and shared-IP enforcement |
| Shared attachment storage | live Gateway upload plus Assistant resolution against the shared local volume passed; probe object was removed afterward |
| Existing Agent regression | Runtime Envelope/API/Assistant resolver bundle previously reran 68 passed after the AS-07 proxy compatibility correction |
| Browser render | four screenshots visually inspected; exact 8/8 suite reports zero blocking axe findings in covered surfaces and no overflow |
| Runtime final state | repository-owned Gateway, Assistant and Frontend healthy, current source/dist hot-synced, Gateway and Assistant report Stub disabled |

The Host runs Node 24.14.0 while Web requests `^22.12.0`; pnpm emitted the
known engine warning. Release images use the pinned Docker Node runtime. Vite
also emitted its inherited large shared-chunk warning. Neither warning caused
an error or skip.

## Channel Security and Rollback

Hosted and Embed resolve an active Publication by stable public ID. Private
requires an authenticated member; tenant requires the same tenant; public may
use an anonymous synthetic principal scoped to that Publication. API callers
must provide an `agt_` token whose SHA-256 digest resolves to an active,
unexpired, unrevoked token for that exact Publication and all requested
scopes. Token metadata responses never include raw values or hashes.

Embed initialization derives the parent Origin from exact Origin/Referer
input, rejects missing/wildcard/credentialed/non-origin values, signs a
five-minute `e1` browser token and emits no-cache HTML with restrictive CSP.
Iframe and parent messages validate protocol version, source and Origin. The
browser token is never accepted as a Runtime API token and no reusable server
token is embedded in JavaScript, URL, storage or network bodies.

Anonymous runtime snapshots force session memory and filter mutating or
high/critical-risk capabilities before signing. A Redis Lua decision applies
principal, trusted-client-IP and Publication minute/day buckets atomically;
the Embed subject is a deterministic HMAC of Publication, Origin and trusted
client IP, so token renewal cannot reset its identity. Absence of the shared
limiter fails closed. Channel disable,
Publication rollback and API token revoke are checked on each new resolution;
existing sessions preserve their AS-02 pinned Version rather than silently
repinning. Rollback deletes no Version, token or evidence row.

Runtime idempotency stores `pending`, `completed` or `failed` state. The first
caller owns execution; an exact completed retry returns the stored SSE bytes
with `X-Idempotent-Replay: true`, while an in-progress or failed duplicate gets
a stable 409 and cannot call the model/tools again. Attachment upload stores
the object through the configured Gateway storage backend and returns only an
opaque database handle. Chat resolves that handle server-side for the exact
tenant, Publication, principal and channel before signing the Assistant
envelope; browser requests never contain `/uploads/` paths.

## Evidence and Honest Boundaries

- Machine security evidence:
  `reports/agent-studio/as-07-channel-security.json`.
- Browser/network evidence:
  `reports/agent-studio/as-07-browser-network.md`.
- Screenshots: `reports/agent-studio/as-07-screenshots/`.
- The browser suite uses deterministic API fixtures for UI/protocol evidence;
  built Nginx/Gateway headers and real PostgreSQL/token behavior are proven by
  separate falsifiable gates.
- Attachment acceptance is proven end-to-end across the scoped Hosted/Embed/API
  upload boundary, opaque handle persistence, server-side resolution, signed
  Assistant input and shared storage volume. Browser evidence asserts that no
  internal `/uploads/` path is exposed.
- No external model quality, provider availability, public internet exposure,
  DNS/CDN or production deployment success is claimed.

## Oracle, Critic, and Handoff

The iteration-1 `changes_requested` verdict is preserved as
`as-07-critic-verdict-iteration-1.md`. A fresh iteration-2 Critic independently
matched 24/24 fingerprints, ran 40 tests with zero failures/skips plus the
config-only header gate, closed C-01 through C-03 and approved. The orchestrator
linked the evidence, transitioned AS-F008, and ran the supported claim check:
exit 0 and structure score 100/100 under legacy diagnostic compatibility. That
validator proves metadata consistency only; it does not rerun behavior, and
strict certification is unavailable for the current v2 Harness. AS-08 is
dependency-unlocked; AS-09 whole-demand completion remains unclaimed.
