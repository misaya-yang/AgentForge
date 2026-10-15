# AS-07 Hosted / Embed Browser and Network Evidence

**Date:** 2026-07-19
**Phase:** AS-07 / AS-F008
**Actor result:** passed, pending independent Critic

## Executed Browser Matrix

| Surface | Viewport/state | Executed assertion | Result |
| --- | --- | --- | --- |
| Hosted | 1440x900 public | stable public ID, welcome/suggestion, SSE merge, feedback, axe, no overflow, request redaction | passed |
| Hosted | 390x844 public | keyboard send, new chat, responsive composer, no horizontal overflow | passed |
| Hosted | attachment and citation contract | enabled picker uploads a file, sends only its opaque handle, renders citation events and removes staged files; disabled policy prevents selection | passed |
| Hosted | quota/provider-safe error | stable alert with no traceback or resolved Snapshot | passed |
| Hosted | private anonymous | server 401 drives `/login`; public ID does not bypass auth | passed |
| Embed launcher | 1280x800 allowed origin | iframe initializes, streams, closes, and returns focus to launcher; axe passes | passed |
| Embed inline | 390x844 allowed origin | iframe resizes, wrong protocol message is ignored, no horizontal overflow | passed |
| Embed | rejected origin | 403 `AGENT_EMBED_ORIGIN_FORBIDDEN`; Embed script never initializes | passed |

Exact command:

`corepack pnpm@10.33.0 -C web exec playwright test e2e/agent-hosted.spec.ts e2e/agent-embed.spec.ts --config playwright.opensource.config.ts`

Final result: `8 passed`, zero failed, zero skipped.

## Response Header Matrix

| Request | Required production behavior | Executed evidence |
| --- | --- | --- |
| `/a/header-test` | `X-Frame-Options: SAMEORIGIN`; CSP `frame-ancestors 'self'` | built frontend image smoke passed |
| `/embed/agents/header-test` with allowed Origin | no XFO; exact Origin in `frame-ancestors`; `Cache-Control: no-store` | built frontend plus dynamic Gateway fixture passed |
| `/embed/agents/:publicId` missing/wrong Origin | 403, no Embed initialization | API/security and Playwright passed |
| Nginx/Helm config | dedicated Embed location proxies dynamic Gateway headers and does not inherit static XFO/CSP | 5 deployment tests plus config-only script passed |

The built-image smoke used an isolated temporary image, network, frontend
container and deterministic Gateway header fixture. Cleanup targets only those
ephemeral names. It does not rely on Vite response headers.

## Browser Credential and Data Boundary

- The browser receives a short-lived `e1` Embed token bound to the exact public
  ID, parent Origin, subject and expiry. It never receives a reusable `agt_`
  Runtime API token.
- The Embed token travels in `X-Agent-Embed-Token`, not query parameters,
  localStorage or sessionStorage.
- Loader and iframe both validate `event.source`, `event.origin` and
  `agent-embed/v1`; neither uses wildcard postMessage targets.
- Captured Hosted/Embed requests contain no `agt_`, `resolved_spec`, signed
  Snapshot, internal Prompt, `GATEWAY_ASSISTANT_SHARED_SECRET`, upstream URL,
  credential value or stack trace.
- Hosted attachment upload returns an opaque artifact UUID. The subsequent chat
  request contains that handle and filename only; neither request exposes an
  internal `/uploads/` path. Disabled attachment policy leaves the picker
  inaccessible and prevents an upload request.
- Citation SSE events are deduplicated and rendered as safe title/dataset
  metadata; no server filesystem or signed Snapshot data is rendered.
- Anonymous Hosted/Embed resolution forces session-only memory and removes
  mutating or high-risk capability bindings before the signed Runtime Envelope.

## Accessibility and Render Review

The executed helper reports no serious/critical axe violations for the Hosted
shell and launcher host page. Keyboard Enter sends Hosted chat, New chat resets
the session, and closing the launcher returns focus. Reduced-motion behavior is
implemented by CSS/media queries and exercised by the Widget protocol/source
checks. The four durable screenshots were visually inspected after the final
8/8 run:

- `as-07-screenshots/hosted-desktop.png`
- `as-07-screenshots/hosted-mobile.png`
- `as-07-screenshots/embed-launcher.png`
- `as-07-screenshots/embed-mobile-inline.png`

## Honest Evidence Boundary

The deterministic browser suite mocks application API payloads so it can
falsify UI, attachment/citation, protocol, origin, focus, error and redaction behavior. Production
header truth is separately covered by the built Nginx/Gateway smoke, while
repository, Redis, shared-storage and real-PostgreSQL tests cover token,
idempotency, quota, attachment resolution and runtime authorization.
No external model/provider call or public deployment is claimed.
