# AS-07 Hosted App, Embed Widget, and Runtime API Plan

- Phase: `AS-07`
- Feature: `AS-F008`
- Status: executing
- Source of truth: `docs/agent-studio-prd/phase-07-hosted-app-embed-widget-and-runtime-api.md`

This is the fixed execution transcription required by AS-07. It does not alter the
approved architecture, dependencies, scope, acceptance gates, or validation commands.

## Critical path

1. Extend the existing Publication repository and forward-only schema with public-ID
   resolution, exact channel/auth/origin enforcement, hashed one-time Runtime token
   create/list/rotate/revoke/authenticate, scoped idempotency, feedback, audit, and
   immediate disabled/revoked behavior.
2. Reuse the AS-02 signed Runtime Envelope and immutable session pinning for Hosted,
   Embed, and API callers. Force anonymous callers to session-only memory and remove
   high-risk/write capabilities; enforce attachment, feedback, rate, quota, and scope
   policy at the Gateway boundary.
3. Add the stable `/a/:publicId` Hosted route and the dedicated dynamic
   `/embed/agents/:publicId` document. Keep the iframe credential-free, use exact
   Publication origins, restrictive CSP/sandbox, and versioned postMessage checks.
4. Route the dedicated Embed document through Gateway in both Nginx and Helm while
   retaining anti-framing headers on Hosted, console, `/share/:shareId`, and
   `/assistant`.
5. Add the channel management surface, stable browser loader, SDK examples, API,
   security, deployment, multi-origin browser, accessibility, redaction, rollback,
   and compatibility fixtures named by the Phase contract.
6. Run every AS-07 required validation command once against the final source, repair
   only blocking failures, then request a fresh independent Critic and update the
   Feature Oracle, report, continuity artifacts, and supported claim gate.

## Evidence boundary

Provider-free local stubs may prove channel behavior without API keys. Production
DNS/live-provider work is out of scope unless separately approved; Origin/CSP,
postMessage, token secrecy/scope/revocation, anonymous safety, tenant isolation,
rate limits, built-image headers, and regression checks are not waivable.

