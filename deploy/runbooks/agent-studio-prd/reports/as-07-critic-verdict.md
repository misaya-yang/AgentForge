# AS-07 Independent Critic Verdict

**Phase:** AS-07 — Hosted App, Embed Widget, and Runtime API  
**Feature:** AS-F008  
**Critic:** fresh independent Critic `/root/as07_critic_iteration2`  
**Critic Verdict:** approved  
**Actor Report:** `docs/agent-studio-prd/reports/as-07-hosted-app-embed-widget-and-runtime-api-report.md`  
**Date:** 2026-07-19

## Inputs Reviewed

- The `prd-phase-harness` review/evidence protocol, repository `AGENTS.md`, AS-07 Phase contract, fixed plan, AS-F008 Oracle item, current loop state and agent handoff.
- Actor iteration-2 report, channel-security JSON, browser/network evidence and preserved iteration-1 verdict.
- Current production paths for shared quotas, Embed identity, Runtime idempotency, attachment persistence/resolution, Assistant attachment verification, citation projection/rendering, storage sharing, and deployment routing.
- Current negative/concurrency/browser/database tests covering the three iteration-1 findings.
- Independent SHA-256 recomputation of every entry in `source_fingerprints`: `24/24` matched current files; `0` missing and `0` mismatched.

## Independent Verification

| Check | Exact command | Result |
| --- | --- | --- |
| Runtime API and channel security | `uv run pytest -q --no-cov tests/api/test_agent_runtime_api.py tests/security/test_agent_channel_security.py` | exit 0; `24 passed`, `0 failed`, `0 skipped` |
| Real PostgreSQL channel runtime | `uv run pytest -q --no-cov tests/database/test_agent_channel_runtime.py` | exit 0; `3 passed`, `0 failed`, `0 skipped` |
| Hosted and Embed browser matrix | `corepack pnpm@10.33.0 -C web exec playwright test e2e/agent-hosted.spec.ts e2e/agent-embed.spec.ts --config playwright.opensource.config.ts` | exit 0; `8 passed`, `0 failed`, `0 skipped` |
| Embed deployment/header contract | `uv run pytest -q --no-cov tests/deployment/test_agent_embed_headers.py && bash scripts/new/test-agent-embed-headers.sh --config-only` | exit 0; `5 passed`, `0 failed`, `0 skipped`; config-only shell gate passed |

Independent total: `40 passed`, `0 failed`, `0 skipped`, plus the passing config-only shell gate. Pytest emitted only the existing Starlette `httpx` deprecation warning. Playwright emitted only the declared Node 24 versus requested Node 22 engine warning. The four durable browser screenshot SHA-256 values were identical before and after the 8-case run.

Before live PostgreSQL validation, the Critic confirmed that every running `ai-gateway-*` container is healthy and has `com.docker.compose.project.working_dir=/Users/yang/projects/AI--Platfform`. Gateway readiness also returned ready.

## Iteration-1 Finding Re-evaluation

| ID | Conclusion | Independent evidence |
| --- | --- | --- |
| C-01 | closed | `RedisAgentChannelLimiter` performs one Redis Lua decision over separate principal minute/day, trusted-IP minute/day and Publication minute/day buckets. All keys share a Publication hash tag, so the six-key operation remains atomic and shared across Gateway instances. Quota failure is fail-closed. Embed abuse identity is a deterministic HMAC of public ID, exact Origin and trusted client IP, so token renewal does not reset the principal. The passing security cases cover stable renewed subjects, two limiter instances, rotated principals sharing one IP and multiple principals contending for one Publication admission. |
| C-02 | closed | Runtime chat reserves `(tenant, Publication, token principal, idempotency key)` before quota charging, session binding or downstream execution. An existing `pending` or `failed` reservation returns stable 409 without re-execution; a `completed` reservation replays the persisted status, media type, session and exact SSE bytes. The passing API concurrency case asserts a single captured downstream invocation, the retry case asserts byte-for-byte replay, and the real-PostgreSQL case proves one winner across two concurrent clients plus terminal replay. |
| C-03 | closed | Hosted performs a real multipart upload and subsequently sends only the opaque artifact handle plus display metadata; browser requests contain no storage path. Disabled attachment policy prevents selection/upload. Gateway resolves the handle server-side for the exact tenant, Publication, principal, channel and expiry, then signs the resolved attachment for Assistant. Assistant accepts only the signed closed attachment shape and confines it to `/uploads/`; Gateway and Assistant use the same storage configuration and local shared volume or shared object store. Typed citation events are projected to bounded public metadata and rendered by React. The passing API/database/browser cases cover upload/resolve scope, opaque browser traffic, enabled/disabled UI and citation rendering. |

## Requirement Assessment

- **R1 Stable Hosted Delivery:** approved for AS-07. Stable public-ID delivery, access denial, streaming, attachment/citation behavior, feedback, responsive states and safe quota/disabled errors are covered by current source and passing focused/browser evidence.
- **R2 Origin-Isolated Embed:** approved for AS-07. Exact Origin enforcement, dedicated CSP/no-XFO routing, short-lived bound browser credentials, source/origin/protocol checks, focus/resize behavior and browser redaction remain covered.
- **R3 Scoped Runtime API:** approved for AS-07. Token scope/lifecycle, session isolation, attachment/feedback boundaries and corrected terminal idempotency are covered by focused and real-PostgreSQL evidence.
- **R4 Safe Anonymous Defaults and Rollback:** approved for AS-07. Anonymous memory/capability reduction, shared atomic abuse limits, stable Embed identity, fail-closed quota availability and immediate Publication/channel denial remain covered.

The Actor's full frontend lint/type/i18n/build, built-image header smoke and wider runtime regression receipts were reviewed but were not rerun by this Critic. Matching all 24 frozen fingerprints establishes that the reviewed iteration-2 source is the source named by the security evidence; it does not turn those Actor-only commands into independent receipts.

## Scope and Handoff Boundary

The aggregate worktree contains prior-phase/user-owned changes and has no clean AS-07-only commit baseline, so this review does not attribute every dirty file to AS-07. The Critic changed no product source, Oracle, loop state, continuity ledger or handoff; only this canonical verdict and its identical iteration-2 copy were written.

AS-F008 correctly remained `failing` during independent review. This approval permits the orchestrator to perform the prescribed Oracle/continuity/handoff writeback and supported phase claim check before unlocking AS-08. It is not itself that writeback or claim check.

## Whole-Demand Regression Assessment

AS-09 same-build whole-demand regression remains incomplete and pending. This verdict approves only AS-07 on the current 24-file fingerprint set and does not claim product-wide release readiness.

## Verdict Rationale

`approved`. All three iteration-1 findings are closed in the actual production paths and are exercised by current negative, concurrency, real-database and browser assertions. Every independently required command completed with zero failures and zero skips, and all 24 security fingerprints match the reviewed source.
