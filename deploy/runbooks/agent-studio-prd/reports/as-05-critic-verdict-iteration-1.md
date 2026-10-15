# AS-05 Independent Critic Verdict — Iteration 1 (Preserved)

**Phase:** AS-05 — Agent Studio Frontend and Preview

**Feature:** AS-F006

**Critic:** `/root/as05_critic` — fresh independent subagent

**Verdict:** `changes_requested`

**Date:** 2026-07-19

This immutable iteration record preserves the first independent Critic's
release-blocking findings before Actor remediation. The canonical verdict at
`docs/agent-studio-prd/reports/as-05-critic-verdict.md` contained the complete
review at the time this snapshot was created.

## Independently Rerun Required Gates

| Gate | Iteration-1 result |
| --- | --- |
| Frontend lint/type/i18n/build | exit 0; lint 0 errors/17 warnings; remaining commands passed |
| Agent Studio E2E | 23 passed, 0 failed, 0 skipped in 54.1s |
| Existing route E2E | 29 passed, 0 failed, 0 skipped in 53.5s |
| Agent API + Assistant resolver | 8 passed + 8 passed, 0 failed, 0 skipped |

The green prescribed commands did not falsify the source-level gaps below.

## Preserved Findings

### AS05-C01 — high — partial metadata/Draft save

One user-visible Save performed an unversioned Agent metadata PATCH before the
revision-checked Draft PUT. A later 422, 409 or 503 could therefore commit
name/description while the UI reported the whole operation as conflict/error
and retained stale saved metadata. The deterministic fixture executed the same
order without replaying or asserting server metadata. Required correction:
make the operation atomic or explicitly and truthfully represent versioned
partial success, and add stateful negative/reload/in-flight tests.

### AS05-C02 — high — arbitrary Agent SSE payload enters browser

React ignored one arbitrary `data.result`, but the Assistant Agent SSE route
used a recursive key denylist and could still transmit arbitrary AgentLoop
`result`, `metadata` or argument dictionaries, including nested
credential-shaped values. Required correction: server-side closed projection
per public Agent event type and raw-SSE negative tests; preserve the generic
Assistant stream contract outside Agent runtime.

### AS05-C03 — high — Compose rollback flag not wired

The frontend entrypoint consumed `VITE_AGENT_STUDIO_ENABLED`, but the default
open-source frontend Compose service did not pass it. Direct browser fixture
injection proved only the React guard, not the documented one-click rollback.
Required correction: wire the non-secret flag through every supported frontend
startup path and test generated runtime config plus Agent-off/Assistant-on
behavior.

### AS05-C04 — medium — mobile directory loses required actions

Desktop rows offered Open/Copy/Archive while the 390px replacement row was only
a Studio link. Required correction: add an accessible mobile action menu with
the same server-backed Owner/Editor/Viewer semantics and confirmation flow,
then test Copy/Archive/Viewer denial at 390x844 without overflow.

### AS05-C05 — medium — keyboard/focus and Editor evidence incomplete

The suite directly called `.focus()` and used pointer clicks for the mobile
Drawer, so it did not prove keyboard traversal, activation, focus placement/
trap, Escape close or focus return. No explicit Editor browser scenario ran.
Required correction: add real Tab/Shift+Tab/Enter/Space/Escape flows for list,
create/dialog/Drawer and an Editor scenario proving edit/Preview access plus
Owner-only Archive denial.

## Iteration-1 Boundary

- Real axe, responsive screenshots, i18n, reduced motion, closed Preview target
  identifiers, Version resolution and no-hot-swap behavior otherwise received
  positive evidence.
- No API key, provider token or generated credential was read or changed.
- No AS-06 product behavior, deployment, commit, push or completion gate was
  authorized.
- AS-F006 correctly remained `failing`; AS-06 remained locked.
