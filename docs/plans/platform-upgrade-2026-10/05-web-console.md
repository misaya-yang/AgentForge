# 05 Web console (frontend)

按完整模块交付；进度只在[现有账本](../../../deploy/runbooks/native-enterprise-upgrade/loop-state.json)。
原始工作包用于需求与验收核对，执行顺序使用[主计划的三个批次](README.md#6-roadmap)。

## 1. 本模块要交付什么

把控制台接到真实后端，完成登录、会话、Agent 执行、知识库、治理和新能力页面的可用流程。

| 交付部分 | 原需求 ID | 连通的结果 |
| --- | --- | --- |
| 浏览器身份 | `U-WEB-01` | Cookie、CSRF、step-up、CSP/Trusted Types 与服务端身份策略联动。 |
| 接口与工作台 | `U-WEB-02`, `U-WEB-05`, `U-WEB-08` | 生成客户端、流式恢复及新能力页面与所属后端同批交付。 |
| 组件与页面能力 | `U-WEB-03`, `U-WEB-04`, `U-WEB-07` | 组件统一、性能预算、类型检查、国际化及无障碍一次集中收尾；保留原预算和门槛。 |
| 观测与服务边缘 | `U-WEB-06`, `U-WEB-09` | 浏览器 trace/性能数据与实际静态服务、镜像安全配置贯通。 |

以上部分在同一模块中连续完成，不分别创建小任务、审批、审核或交接记录。

## 2. 接线与真实依赖

使用已有设计系统与后端契约；缺少真实接口时先完成明确的消费逻辑，不杜撰接口或把 mock 页面当联调完成。与客户端计划共同使用同一 OpenAPI/事件契约。

## 3. 一次集中验证与收尾边界

集中运行受影响的页面/接口测试，再用统一浏览器批次验证真实登录、完整任务、断网恢复和关键错误状态。路由预算、原定全路由无障碍与安全负向检查保持覆盖，不对每个组件分别启动整套浏览器。

无行为收益的视觉微调、重复状态管理封装和逐组件美化后置；用户无法操作、会话泄露、事件丢失或接口错误即时修复。

<details>
<summary>设计约束与技术选择（实现涉及该边界时按需查阅）</summary>

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

</details>

## 6. Work packages

<details>
<summary>原始需求和验收对照；Prio / Phase 是原计划标签，不是逐项执行流程</summary>

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

</details>

## 7. Verification gates

以下场景在上述模块验证和最终验收批次中覆盖。同一场景由一个执行者运行一次，关联
多个需求即可；未变化的适用结果直接复用。关键边界和原门槛不能由重复的局部测试替代。

- Security: CSRF, Origin and Fetch Metadata negative tests; a CSP violation budget of zero in e2e;
  session fixation and logout tests; a check that no token or secret reaches `localStorage`,
  `sessionStorage` or any URL.
- Contract: generated client compiled against the current `sdk/openapi.json`; shared SSE fixtures
  from Stage E replayed through the console stream client.
- Performance: the bundle budget with no exceptions; Lighthouse CI on key routes; RUM p75 INP below
  200 ms (target to validate).
- Accessibility: axe on every route; keyboard-only journeys for login, Assistant and approvals.

<details>
<summary>已有实现说明与历史结果（不自动变成当前任务或当前验收）</summary>

## 8. Source and local integration checkpoint — 2026-10-18 (runtime acceptance pending)

The console now has a generated-client adapter in
[`generatedTransport.ts`](../../../web/src/api/generatedTransport.ts). It imports the native
producer's `sdk/typescript/generated.ts`, derives request/response/event types from operation
keys, encodes path parameters, supplies abort signals, and creates identity-scoped TanStack
read options. Provider CRUD and Console session operations use that client; auth, provider,
model, user, role, scheduled-task and plugin request types derive from the generated contract.
The generated Fetch client now owns wire encoding and dispatch through the Console BFF
authority adapter, preserving synchronous account fences, CSRF, passkey step-up, original
error identity and single-attempt mutation behavior. SSE wrappers retain the shared reader's full event
and control-frame envelope.

[`check-api-contract.mjs`](../../../web/scripts/check-api-contract.mjs) checks source operations
against generated `ApiOperations` and fails for undocumented or unresolved calls and writes
whose published body is `never`/`unknown`. Run it with Node 26; `--json` emits the exact source
locations and matched operations for the producer owner. There are no success exemptions.
The Console contract extender declares 204 concrete paths/225 operations from actual
native owners, including Eval, Knowledge, generic sessions, routing, Local Nodes, Quiz,
MCP, connectors and the browser telemetry collector. Its 59 schemas combine actual serde
DTO extraction with explicit native JSON validator declarations. It additionally publishes
178 source-verified query fields across 51 existing operations (required comparison and
cursor fields remain required). Query declarations cannot create an unmounted route.
The generated request aliases replace local body/query DTOs, including nested evaluation
cases, passkey credentials, session payloads, usage filters and provider configuration.
Editor/read projections remain explicit where their nullable values differ from native writes.

The actual native producer generated all four SDK artifacts with 590 operations/82 models
(source SHA `5b25f7d2cb336d333e13c0a907030fb22516f4660cebd75da4523754086f7ef6`).
The source gate checks 383 call alternatives, including aliased clients and injected telemetry
fetches, with zero undocumented/unresolved calls and zero missing request body schemas.
The build invokes this gate before bundling. Compile-only positive/negative request, query,
operation and browser-authority checks run under both TS7 and TS6.

The producer emits the actual Fetch codec/client, a compact operation-key table, multipart
media metadata, typed query option/key factories and stream frame types. The shared Console
adapter calls the generated client's raw-response method; existing response views remain at
its boundary. The same codec also handles raw Response consumers and SSE. The SSE reader
retains framing, resume/reset/control events, idle budgets and GET-only reconnect behavior;
its decoded Runtime V2 JSON view aliases the generated native projection model. Provider
queries consume generated options and keys while preserving the existing invalidation prefix.
Axios and its exclusive transitive dependencies are removed. pnpm 12 reconciled the lock
without package upgrades or patch changes, and frozen/offline verification passed.

The synchronous request snapshot, cookie/CSRF boundary, current-account fence, original
Error identity, one-attempt mutation semantics and pre-dispatch passkey step-up remain in
place. Draft serialization precedes the asynchronous proof, and ownership/review is checked
again immediately before dispatch. Fetch upload progress reports confirmed completion;
there are no fabricated intermediate upload percentages. OIDC begin and step-up start send
no invented body to handlers without JSON extraction. Generated binary metadata constrains
server-provided download URLs before an authenticated read; foreign origins, JSON routes,
traversal and encoded separators are rejected. Multipart conversion preserves File/Blob
bytes, names, repeated fields and the browser-owned boundary.

The focused transport and negative tests cover delayed proof/account changes, stale raw
Request materialization, original HTTP/network errors, unpublished operations and repeated
query parameters. The final actual SDK passes 2,228 Node tests (including the build compression
regressions), TS7 and TS6 app/node/contracts checks, API and i18n gates, and ESLint with no
errors (five existing hook warnings). The formerly failing HTTP 202/no-MIME receipt returns
undefined; an explicitly typed empty binary receipt remains a Blob.

The performance source uses the React Compiler preset through the Rolldown Babel plugin,
with annotation mode starting at Workbench. Router imports point at individual Agent and
Knowledge pages. Dashboard panels (including provider status), heavy Eval panels, and
Knowledge tabs load independently; stateful Knowledge tabs retain their subtree after their
first visit. Agent tables load with their data and release evaluation/channel panels load
when selected. The Eval route uses an explicit type-only trace-filter import, preventing an
otherwise unused trace-list module from loading Table and DatePicker eagerly. The budget gate derives
all lazy route chunks from the router, retains the original 330,000/220,000 gzip and 500,000
raw limits, and has no route exceptions. The fresh production build measured 328,660 bytes
public gzip and 160,788/202,832/214,617 bytes Assistant initial/restored/GFM gzip; every route
static increment stays below 500,000 raw bytes. Native TypeScript 7 checks app, node and
contract projects; TypeScript 6
remains the ESLint API and an explicit comparison command. No editor extension is assumed.

Production assets emit byte-identical Brotli, zstd and gzip representations. Compression reads
final written files in writeBundle, after Vite injects dependency maps; the earlier
generateBundle timing produced mismatched compressed JavaScript. All 345 current /assets
files passed byte comparisons for all three encodings (1,035 representations), with no source
maps. The nginx include
negotiates accepted encodings, retains explicit MIME types, varies on `Accept-Encoding`, falls
back without a stale encoding header, and blocks raw/compressed source maps. The Helm nginx
copy and new include ConfigMap/mount must match the frontend image before deployment. The
source gate preserves that equality requirement. Serving from Gateway remains an evaluation,
not an activated second static server. Eight Chromium fixture cases passed: sampled
telemetry and dynamic Knowledge/share/Quiz paths, Agent desktop/mobile directory keyboard
flows, owner analytics and release lifecycle rendering. These runs used a frontend-only Vite
server with its backend proxies removed and mocked API responses. Deployed edge behavior,
real authentication/WebAuthn and real backend acceptance remain separate runtime checks.

The last `tailwind-merge` use is removed. Shared primitive defaults live in the components
cascade layer; antd is placed before components and caller utilities, preserving overrides
without a runtime class merger. Import lint rules prohibit all retired dependencies.

ICU plural/date messages are active for all ten locale files. Tasks now joins the deferred
namespaces, retaining exact translation content and loading before its route; concurrent
loading, language changes and retry after chunk rejection are covered. Every mounted route has an explicit accessibility case in
[`all-routes-accessibility.spec.ts`](../../../web/e2e/all-routes-accessibility.spec.ts); the gate
checks WCAG 2.2 AA tags and fails every reported violation. It must run with valid route data
fixtures as well as unavailable/error states. The existence of this gate does not establish
that every route passed. RTL remains conditional on the product language decision.

Workbench and Settings expose permission-filtered links to trace, cost/quota, guardrail,
audit and Knowledge surfaces, plus new routing-policy, enterprise-identity and MCP-registry
pages. Routing publishes immutable tenant versions, compares exact readback, performs
separate activation with an expected pin, and reads server decisions. MCP selects a real,
enabled, unexpired connection and submits its server-reported principal type; tool hashes and
snapshot IDs support schema-change inspection. Registry metadata does not approve tool use.
Enterprise identity supports configured SSO, scoped SCIM credential issuance/revocation,
current-session metadata, passkey enrollment/revocation and current-session step-up. Login
has distinct SSO and passkey ceremonies; passkey login is never treated as step-up. Enrollment
requires existing recent MFA, rotates the cookie session, and displays recovery codes only
in memory for the resulting account generation. Password-only initial enrollment remains
blocked by server policy.

Remaining server-owned surface contracts are explicit: there is no passkey/other-session inventory
or administrative per-session revoke API (current-session logout remains supported), no MCP protocol-revision field or schema-approval/CAS endpoint, and no
new budget-policy API in the existing quota endpoints. The console does not invent these
operations. Region citations display only server-enriched DocIR identity, publication
generation, page/block, source hash and measured coordinate units/region; unavailable geometry
stays unavailable. A page overlay still requires measured page dimensions.


### Event observation contract

The two event feeds have different native parsers. Local Node
`GET /api/v1/local-nodes/{device_id}/events` returns JSON and reads only
`after_sequence` (default zero). It does not implement `limit`,
`before_sequence`, `cursor` or `Last-Event-ID`; the old copied pagination
parameters are removed from its source-owned declaration rather than treated as
implemented features. Existing Local Node consumers continue to use the real
query cursor.

Agent Runtime `GET /api/v2/agent/threads/{thread}/events` is SSE and keeps the
production `after_sequence`, `limit` and `turn_id` query fields. Gateway validates
`limit` in 1..1000 (public default 100), then forwards it to Runtime as the initial
replay page size. Runtime's internal standalone default is 200; that default does
not replace Gateway's explicitly forwarded value. Replay can continue across
pages and live observation continues after the initial replay, so limit is not a
cap on the complete stream. The Web durable-run snapshot retains limit=1000.

An explicit non-negative signed-64-bit `after_sequence`, including zero, takes
precedence over `Last-Event-ID`. Only when the query is absent does the native
parser validate one ASCII decimal header value, then fall back to zero. Used
malformed, negative, overflow or duplicate headers reject with Gateway's existing
422 error; a query-selected cursor can ignore such headers. Therefore the header
has a string OpenAPI type and conditional grammar in its description, not an
unconditional pattern that would reject requests the native handler accepts.
The query has no unconditional OpenAPI default that could cause a generated
client to synthesize zero and mask a supplied header. Sequence gaps remain legal
observation positions. Reconnection preserves the
admitted thread/turn and performs GET observation; it never creates another turn,
renews execution authority or replays an inference POST.

</details>
