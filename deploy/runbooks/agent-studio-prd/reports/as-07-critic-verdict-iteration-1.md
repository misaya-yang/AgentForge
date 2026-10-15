# AS-07 Independent Critic Verdict

**Phase:** AS-07 — Hosted App, Embed Widget, and Runtime API  
**Feature:** AS-F008  
**Critic:** fresh independent Critic `/root/as07_critic`  
**Critic Verdict:** changes_requested  
**Actor Report:** `docs/agent-studio-prd/reports/as-07-hosted-app-embed-widget-and-runtime-api-report.md`  
**Date:** 2026-07-19

## Inputs Reviewed

- Phase contract and AS-F008 Oracle item.
- Actor report, `reports/agent-studio/as-07-channel-security.json`, `reports/agent-studio/as-07-browser-network.md`, actual AS-07 source/diff, tests, deployment configuration, SDK example, and all four screenshots.
- Independent SHA-256 recomputation: all 14 fingerprints recorded in the security JSON match current source (`14/14`).

## Findings

| ID | Severity | Requirement/gate | Finding | Required correction |
| --- | --- | --- | --- | --- |
| C-01 | high | R4; non-waivable rate/quota gate | `_enforce_channel_limits` stores counters only in one Gateway process and keys them only by `(publication_id, principal_id)`. It has neither an independent IP bucket nor a Publication-wide bucket. Embed issuance creates a fresh random `sub` for every `e1` token, so reloading the iframe changes the principal and bypasses both configured minute/day counters. The existing test exercises two requests from one principal and therefore does not prove the Actor claim of per-IP/principal plus Publication quotas. | Implement shared/atomic limits with separate IP, principal/token, and Publication dimensions. Keep the Embed abuse identity stable enough that reload/token renewal cannot reset quota. Add negative tests using two Embed tokens/principals for one IP and many principals for one Publication, including multi-worker behavior. |
| C-02 | high | R3; API idempotency | Confirmed from the production code path: `reserve_runtime_idempotency` returns `(False, existing_session_id)` for an identical replay, but `published_chat_stream` ignores `_created` and continues through rate charging, snapshot/session binding, and `_proxy_runtime_stream`. Thus the same key and same request can execute the model/tools a second time; only conflicting payloads are tested. | Make an identical replay return/replay the original terminal result, or otherwise ensure it cannot re-run model/tools or charge quota. Persist an execution state/result contract and add a same-key/same-body concurrency/retry test that asserts one downstream invocation. |
| C-03 | medium | R1; Oracle step 1; attachment/citation browser coverage | Hosted does not implement attachment selection: the paperclip button has no handler, and `streamPublicAgent` always sends `attachments: []`. The Hosted renderer also flattens SSE to text and has no citation model/rendering. The 7-case browser fixture sets `attachments: false` and emits text-only events, so the Actor's Hosted attachment/citation coverage is not established. Runtime API attachment scope and feedback boundaries do pass, but they do not close the Hosted requirement. | Implement or explicitly narrow the contract. For the current Phase/Oracle, wire allowed Hosted attachments through the approved artifact boundary, render citation events, and add browser/API assertions for enabled/disabled attachments and citations. |

## Requirement Coverage

- **R1:** partial. Stable Hosted ID, public/private behavior, streaming, feedback, responsive states and safe errors have passing focused/browser evidence; tenant-mode browser coverage, Hosted attachments and citations are incomplete.
- **R2:** substantially covered by exact-Origin schema/runtime checks, CSP/no-XFO configuration, restrictive sandbox, source/origin/version checks and browser redaction. The Critic did not independently rerun the built-image smoke.
- **R3:** token hashing/scope/expiry validation/rotation/revocation, session ownership, attachment scope, feedback and SSE have passing focused/real-PostgreSQL evidence; C-02 leaves idempotency unsafe.
- **R4:** anonymous session-only memory and no-write/high-risk filtering pass. Immediate disabled resolution is covered. C-01 leaves the mandatory abuse/cost boundary open.

## Test and Regression Assessment

Independently executed against the matching source:

| Check | Result |
| --- | --- |
| Runtime API + channel security | `20 passed`, `0 failed`, `0 skipped` |
| Deployment header tests + config-only script | `5 passed`, `0 failed`, `0 skipped`; config contract passed |
| Hosted/Embed Playwright | `7 passed`, `0 failed`, `0 skipped`; Node 24 versus requested Node 22 engine warning only |
| Real PostgreSQL channel runtime | `2 passed`, `0 failed`, `0 skipped` |
| AHR no-write regression | `33 / 77 / 8 / 98` passed plus golden gate passed |
| Source fingerprints | `14/14` matched |

The exact frontend lint/type/i18n/build group, built-image header smoke, live isolation, and 41-case open-source browser regression were not independently rerun in this Critic pass. Their Actor results were reviewed but are not elevated to independent receipts. One initial local hash helper invocation failed because a zsh special variable replaced `PATH`; the corrected invocation produced the reported `14/14` result. No product test failed or skipped in the Critic commands listed above.

## Security, Privacy, and Failure Assessment

Public-ID lookup does not by itself grant private/tenant access; reusable `agt_` tokens are absent from Hosted/Embed source and captured requests; Runtime tokens are hashed and Publication/scoped; postMessage validates source, origin and protocol version; anonymous snapshots reduce memory and capabilities; session pin checks bind tenant, Publication, principal, channel and Version. These positive controls do not compensate for the quota bypass or duplicate idempotent execution, both of which can multiply cost and tool side effects.

## Minimal-Change Assessment

The AS-07 surfaces reviewed are consistent with the Phase boundary: additive migration/repository/routes, Hosted/Embed assets, deployment routing, SDK example and focused tests. The aggregate worktree contains prior-phase changes and no clean AS-07-only commit baseline, so this assessment cannot attribute every dirty file exclusively to AS-07.

## Rollback and Handoff Assessment

Resolution consults current Publication/token state and existing sessions remain Version-pinned across rollback. AS-F008 correctly remains `failing`; no Oracle transition, claim gate, or AS-08 unlock is authorized. Continuity/handoff writeback must follow only after C-01 through C-03 are corrected and a fresh Critic approves.

## Whole-Demand Regression Assessment

AS-09 same-build whole-demand regression remains pending. This verdict assesses only AS-07 and inherited focused regression.

## Verdict Rationale

`changes_requested`. C-01 violates a non-waivable AS-07 abuse/quota gate, and C-02 defeats Runtime API idempotency in a way that can duplicate model cost or tool side effects. C-03 is a smaller but real Oracle/browser completeness gap. Passing positive-path tests and matching hashes cannot approve the Phase until targeted negative tests close these paths.
