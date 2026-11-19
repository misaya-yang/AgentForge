# Assistant Runtime Optimization Operating Model

**Scope:** ARO-00 through ARO-05 assistant runtime optimization evidence.

**Status:** release evidence prepared, no deployment performed.

**Date:** 2026-06-29

## Runtime Decisions

| Area | Decision | Rollback Boundary |
| --- | --- | --- |
| Baseline contract | Assistant stream/event and traceparent contracts are regression-protected before runtime changes. | Revert `chat.py` trace field propagation or assistant SSE event constants only if replacement tests cover the same contract. |
| Middleware lifecycle | Reliability behavior uses `MiddlewareChain.on_stream_event` and `on_error`; non-default harness middlewares remain opt-in. | Disable/remove middleware chain entries without changing AgentLoop core control flow. |
| Human approval | `CONFIRM` approvals are persisted, argument-matched, and consumed once. | Fall back to deny/pending behavior; never replay consumed approval IDs. |
| Trace feedback | Trace feedback preview is non-mutating and proposes redacted dataset/profile changes only. | Disable `/eval/trace-feedback:preview` callers; no automatic profile apply exists. |
| Checkpoint/resume | Assistant checkpoints are additive, sanitized summaries/hashes; `/runs/{run_id}/resume` returns a non-executing plan. | Ignore checkpoint rows or disable resume route; side effects still pass through command/approval gates. |
| Cache/context metrics | `context_budget` carries prompt/tool identity hashes and utilization; usage carries cache token metrics. | Remove telemetry fields or ignore them in dashboards; prompt/model execution remains unchanged. |
| Adaptive routing | Adaptive model/reasoning routing and embedding tool selection are not enabled. | Keep default model/tool selector path unless eval evidence and rollback config are added. |

## Trusted Verification Commands

| Purpose | Command | Pass Signal |
| --- | --- | --- |
| Eval/trace/web dev bundle | `make verify-eval-dev` | Python checks/tests pass; golden gate status is `pass`; web lint has 0 errors; web type-check exits 0. |
| Open-source config | `make validate-example-config` | `Example configuration validation passed`. |
| ARO phase gate | `python3 validate_harness_prd.py deploy/runbooks/assistant-runtime-optimization --strict --completion-gate --phase <PHASE_ID> --quality-score` | `Harness validation passed` and quality score is excellent. |
| Full harness gate | `python3 validate_harness_prd.py deploy/runbooks/assistant-runtime-optimization --strict --completion-gate --quality-score` | `Harness validation passed` and all completed features cite actor and critic evidence. |

## SLO and No-Go Thresholds

| Signal | Release Threshold | No-Go Condition |
| --- | --- | --- |
| Golden eval overall score | `overall_score >= 0.85` | Any regression gate status other than `pass`. |
| Golden trajectory pass rate | `trajectory_pass_rate >= 0.95` | Trajectory pass rate below threshold. |
| Critical golden cases | `critical_pass_rate == 1.0` | Any critical case fails. |
| Trace payload privacy | No raw secrets, bearer tokens, raw prompts, full tool args, or unbounded payloads in trace/run/checkpoint evidence. | Sensitive payload appears in regression output, trace preview, checkpoint, or report. |
| Approval safety | Approval IDs are argument-matched and single-use. | A consumed approval can execute a tool again. |
| Checkpoint safety | Checkpoints store hashes/summaries, not raw prompt/message/tool-argument bodies. | Checkpoint payload includes raw user prompt, full message history, credentials, or full tool args. |
| Cache/context telemetry | Prompt-prefix hash, tool-schema hash, context utilization, and cache token metrics are observable from fixture/provider usage where available. | Metrics require credentials to parse, leak raw prompt/schema text, or imply unsupported savings. |
| Web release gate | Web lint exits with 0 errors and type-check exits 0. Existing warnings are not release blockers unless they touch changed runtime surfaces. | Web lint error, type-check failure, or changed UI regression without e2e/waiver. |

## Operating Rules

- Do not deploy from this harness. Deployment requires a separate explicit user request and environment-specific release plan.
- Do not run production migrations, destructive Docker commands, force-pushes, or live provider load tests without explicit approval.
- Treat cache/cost metrics as evidence surfaces. Do not claim savings until live provider metrics and golden quality evidence both pass.
- Keep trace feedback and harness/profile updates review-gated. Proposed dataset/profile changes must not auto-apply.
- Prefer focused changed-path lint when broad commands sweep unrelated historical lint; record the broad failure and focused substitute.

## Release Backlog

- Add dashboard panels for `context_budget.prompt_prefix_hash`, `context_budget.context_utilization`, `usage.cached_input_tokens`, approval pause rate, checkpoint blocked reason, and trace feedback failure modes.
- Add an eval-backed adaptive-routing experiment only after baseline golden, replay, and rollback gates are explicit.
- Retire unrelated broad lint debt separately so future phase contracts can use wider ruff scopes without waivers.
