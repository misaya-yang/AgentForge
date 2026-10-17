# 05 Web console (frontend)

status: proposed · domain_id: platform-upgrade-2026-10 · last_verified: 2026-10-16 (HEAD `22d9ce0`)

## 1. Role

The console is the product surface for operators, builders and end users. It covers
administration (users, roles, tenants, providers, models, services, API keys, quotas), Agent
Studio and publications, the Assistant workbench, Knowledge (datasets, documents, retrieval
testing), Eval, the Playground, dashboards, tasks, connections, Local Nodes, plugins and skills,
plus public agent embeds and share pages. It owns no server facts: it renders them, reconnects to
them and aborts its own requests.

## 2. Current architecture (SRC)

| Aspect | Current state |
| --- | --- |
| Size | 231 TSX files, 124.8k lines in `web/src`; 151 unit test files; 44 Playwright specs in `web/e2e` |
| Stack | React 19.2, Vite 8 (Rolldown), TypeScript ~6.0, pnpm, Node 26 engine. React Router 7, TanStack Query 5, zustand 5 ([package.json](../../../web/package.json)) |
| UI | antd 6 (imported in 45 files) alongside Radix primitives (11 files) styled with Tailwind 4, `cva`, `clsx` and `tailwind-merge`; lucide icons (130 files) and antd icons (8); `framer-motion` (28 files) |
| Data access | Hand-written API modules over axios (21 files). A fetch-based SSE client with a parser that keeps control frames ([sse.ts](../../../web/src/lib/sse.ts)). Thread events resume from `after_sequence` ([agentThreads.ts:132](../../../web/src/api/agentThreads.ts)). |
| Auth | Bearer JWT kept in `localStorage` ("remember me") or `sessionStorage` and attached by an interceptor and by the SSE client ([api.ts:51–85](../../../web/src/lib/api.ts), [useAuthStore.ts](../../../web/src/store/useAuthStore.ts)) (F-07) |
| i18n | i18next with `en-US` and `zh-CN`, namespaced files for agents, eval and knowledge, and a key-check script |
| Quality gates | ESLint 10, `tsc --noEmit`, Node test runner, Playwright with `@axe-core/playwright`, a bundle budget script. Four routes have budget exceptions: dashboard, agents, eval, knowledge ([check-bundle-budget.mjs:23](../../../web/scripts/check-bundle-budget.mjs)). |
| Build | `rollupOptions.output.manualChunks` keeps React, React DOM and the router in one vendor chunk ([vite.config.ts:45–53](../../../web/vite.config.ts)) |
| Serving | `nginx:1.27-alpine` with gzip, a CSP, `runtime-config.js` generated at container start, and proxying of `/api`, `/v1`, `/ws`, docs and embeds to the Gateway ([nginx.conf](../../../web/nginx.conf), [Dockerfile](../../../web/Dockerfile)) (F-37) |

## 3. Assessment

Strengths to keep: a current stack; route-level bundle budgets that fail the build;
accessibility checks in e2e; an i18n key check; tested API modules; an SSE client that keeps
control frames and resumes by cursor instead of guessing; a console that already distinguishes
unknown outcomes from failures.

Gaps, in order of consequence:

1. **Token in script-readable storage (F-07).** Any XSS can read a long-lived bearer token. The CSP
   is a partial mitigation, but `connect-src https: wss:` lets injected script send the token to
   any host.
2. **Outdated edge (F-37).** nginx 1.27 is an unsupported branch; the security headers are a 2020
   set.
3. **Two UI systems and dead dependencies (F-25).** antd plus Radix/Tailwind doubles styling
   approaches and bundle weight. Five unused packages and two date libraries remain.
4. **Hand-written API types (F-28).** 349 OpenAPI paths are mirrored by hand, which invites drift
   from `sdk/openapi.json`.
5. **Build and performance.** No React Compiler. `manualChunks` is deprecated under Vite 8's
   Rolldown. Four routes exceed their budgets. No real-user metrics.
6. **Streaming.** The default SSE timeout is five minutes for the whole stream
   ([sse.ts:16](../../../web/src/lib/sse.ts)), while the Gateway already uses idle budgets
   (evolution F6). Long agent turns depend on reconnect logic that should not be needed.
7. **Tooling skew.** `@types/node` ^22 against a Node 26 engine; TypeScript 6 while TypeScript 7
   is GA.

## 4. Target architecture

### 4.1 Sessions and browser security (U-WEB-01, with U-GW-03)

- **BFF sessions.** Login sets an HttpOnly `__Host-` session cookie (U-GW-03). The console stops
  storing tokens: requests use `credentials: "same-origin"`; the SSE client and WebSocket upgrades
  rely on the cookie; "remember me" becomes a server-side session lifetime.
- **CSRF.** A synchronizer token fetched at boot goes in a header on state-changing requests. The
  Gateway also checks `Sec-Fetch-Site` and `Origin`.
- **Step-up.** Passkey re-authentication before sensitive admin actions (keys, providers, SSO,
  role grants), with the server enforcing the step-up.
- **Headers.** A strict CSP: `connect-src 'self'` plus explicitly configured origins,
  `img-src 'self' data: blob:` plus configured CDNs, and Trusted Types
  (`require-trusted-types-for 'script'`) once DOMPurify and the markdown pipeline are wired to a
  policy. Add `Permissions-Policy`, `Cross-Origin-Opener-Policy: same-origin` and
  `Cross-Origin-Resource-Policy: same-origin`. Drop `X-XSS-Protection`. Set HSTS at the TLS edge.
  Replace the generated `runtime-config.js` with a JSON document fetched at boot, so the CSP needs
  no script exception.
- **Migration.** A dual-mode window: the Gateway accepts both bearer tokens and session cookies,
  with a per-tenant switch. Then the console drops bearer mode. SDKs and CLI keep tokens.

### 4.2 One design system (U-WEB-03)

- **antd 6 is the base.** It already carries the data-dense screens (tables, forms, date pickers),
  with design tokens and CSS-variable theming. Tailwind stays for layout utilities only. Migrate
  the 11 Radix-based components to antd equivalents and remove Radix, `cva` and `tailwind-merge`
  unless a Radix primitive has no antd counterpart.
- Remove `@langchain/core`, `@langchain/langgraph-sdk`, `marked`, `react-grid-layout` and
  `react-resizable`. Keep `dayjs` (antd's date library) and remove `date-fns`. Replace
  `framer-motion` with CSS transitions and the View Transitions API where possible, otherwise its
  successor package `motion`.
- Add a lint rule that blocks new imports from removed packages, so the consolidation sticks.

### 4.3 Generated, typed API layer (U-WEB-02, with U-CL-10)

- Generate TypeScript types and a fetch client from the OpenAPI 3.2 document that U-CL-10
  produces. Streaming events are typed from `itemSchema`. Generate TanStack Query option
  factories with query keys, so cache invalidation is typed too.
- Retire axios once the generated client covers its 21 call sites. Fetch gives streaming, abort
  and the same cookie semantics without the extra dependency.
- CI fails when the console uses an operation that is missing from, or changed in, the OpenAPI
  document.

### 4.4 Performance (U-WEB-04)

- **React Compiler 1.0.** `@vitejs/plugin-react` v6 dropped its internal Babel, so the compiler
  runs through `reactCompilerPreset` with `@rolldown/plugin-babel` (WEB; confirm against official
  docs when activated). Roll it out route by route behind the existing e2e suite.
- **Code splitting.** Move to `build.rolldownOptions.output.codeSplitting` groups (WEB) and bring
  the four exception routes under budget: lazy-load charts, KaTeX and markdown rendering, and the
  dashboard grid.
- **Delivery.** Precompressed Brotli and zstd assets served as static files, immutable caching for
  hashed assets, and `modulepreload` for route chunks on navigation intent.
- **TypeScript 7.** Use the native compiler for `--noEmit` type checks in CI and editors. Keep
  TypeScript 6 for typescript-eslint until TypeScript 7.1 ships a stable programmatic API (WEB).

### 4.5 Streaming client v2 (U-WEB-05)

One shared stream client for the Assistant, Playground and Knowledge QA:

- An idle budget (for example 45 s without bytes, matching the Gateway's keep-alives) instead of a
  five-minute whole-stream timeout.
- Automatic resume with `Last-Event-ID` or `after_sequence` and capped backoff with jitter.
- Correct pausing and resuming for background tabs, and visible states for "unknown outcome",
  "reconnecting" and "server terminal".

### 4.6 Observability (U-WEB-06)

- Real-user Core Web Vitals (INP, LCP, CLS) and route timings, sampled and exported to the OTel
  Collector through a same-origin endpoint.
- `traceparent` on API calls, so one trace spans browser → Gateway → Runtime.
- Client error reporting with source maps uploaded at build time but not served publicly. PII
  scrubbing on by default.

### 4.7 Accessibility and localization (U-WEB-07)

- WCAG 2.2 AA as a gate: axe checks run on every route in e2e, not a subset.
- ICU message format for plurals and dates. Locale namespaces load lazily.
- Right-to-left layout support, if the product targets Arabic content as the OCR prompts suggest.
  This is a product decision; antd and Tailwind both support `dir="rtl"`.

### 4.8 Workbench features (U-WEB-08)

Console surfaces for the platform capabilities in this plan. Where the vNext PRD already defines a
requirement, the work keeps that requirement's ID and owner.

| Surface | Depends on |
| --- | --- |
| Trace viewer for agent turns (model calls, tool calls, approvals, cost) | U-RT-09, U-GW-21 |
| Cost explorer and budgets | U-GW-01, U-GW-07, U-GW-08 |
| Routing policy editor with "explain this decision" | U-GW-06 |
| Guardrail policy editor and decision log | U-GW-09 |
| MCP registry with protocol revision, tool-schema diffs and approval | U-GW-10 |
| SSO/SCIM administration, passkeys, sessions | U-GW-04 |
| Knowledge parsing preview with DocIR regions; region citations; multimodal results | U-KN-04, U-KN-06, U-KN-09 |
| Audit log explorer (policy decisions, admin actions) | U-GW-13 |

### 4.9 Serving container (U-WEB-09)

- Move to the nginx 1.30 stable line on an unprivileged base: non-root, read-only root file
  system, `cap_drop: [ALL]`, `tmpfs` for cache directories (U-PE-22).
- In the single-node profile, evaluate serving the static assets from the Gateway's HTTP crate
  (precompressed files through `tower-http`). That would remove a hop and a container. Kubernetes
  keeps an ingress or CDN.

## 5. Technology upgrades

| Item | Current | Target | Notes |
| --- | --- | --- | --- |
| React | 19.2 | 19.2 + React Compiler 1.0 | Route-by-route rollout |
| Vite | 8 with `rollupOptions.manualChunks` | 8 with `rolldownOptions.output.codeSplitting` | Deprecated option removed |
| TypeScript | ~6.0 | 7.0 native for type checks; 6.x for linting until 7.1 | WEB |
| UI | antd 6 + Radix + Tailwind + `cva` | antd 6 + Tailwind layout utilities | F-25 |
| HTTP | axios + fetch | Generated fetch client | U-WEB-02 |
| Motion | `framer-motion` | CSS / View Transitions; `motion` where needed | — |
| Node types | `@types/node` ^22 | ^26 | Matches the engine |
| Edge | `nginx:1.27-alpine` | nginx 1.30.x unprivileged | F-37 |

## 6. Work packages

| ID | Work package | Prio | Phase | Acceptance |
| --- | --- | --- | --- | --- |
| U-WEB-01 | Cookie sessions, CSRF, step-up, strict CSP and Trusted Types (F-07, F-37) | P1 | Ph1–Ph2 | No token readable from script; CSRF and Origin negative tests; CSP report-only for one release, then enforced with zero violations on e2e |
| U-WEB-02 | Generated, typed API client from OpenAPI 3.2 | P2 | Ph2 | No hand-written request types left; CI fails on undocumented operations |
| U-WEB-03 | One design system; dead dependencies removed (F-25) | P2 | Ph2 | Radix, `cva`, `date-fns` and unused packages removed; lint rule blocks reintroduction |
| U-WEB-04 | React Compiler, `codeSplitting`, budgets with no exceptions, precompression, TS 7 type checks | P2 | Ph2 | Every route within budget; type-check time reduced (measured) |
| U-WEB-05 | Streaming client v2 with idle budgets and resume | P2 | Ph2 | Hour-long turn completes with no client timeout; network-flap e2e passes |
| U-WEB-06 | Real-user metrics, browser trace propagation, error reporting | P2 | Ph2 | INP and LCP p75 dashboards; browser-to-Runtime trace links |
| U-WEB-07 | WCAG 2.2 AA gate on all routes; ICU messages; RTL if adopted | P3 | Ph2 | axe passes on every route in e2e |
| U-WEB-08 | Workbench surfaces for the new platform capabilities | P2 | Ph2–Ph3 | Each surface ships with its backend package and an e2e spec |
| U-WEB-09 | nginx 1.30 unprivileged image and container hardening (F-37) | P1 | Ph0–Ph1 | Image scan clean of criticals; container runs read-only and non-root |

## 7. Verification gates

- Security: CSRF, Origin and Fetch Metadata negative tests; a CSP violation budget of zero in e2e;
  session fixation and logout tests; a check that no token or secret reaches `localStorage`,
  `sessionStorage` or any URL.
- Contract: generated client compiled against the current `sdk/openapi.json`; shared SSE fixtures
  from Stage E replayed through the console stream client.
- Performance: the bundle budget with no exceptions; Lighthouse CI on key routes; RUM p75 INP below
  200 ms (target to validate).
- Accessibility: axe on every route; keyboard-only journeys for login, Assistant and approvals.
