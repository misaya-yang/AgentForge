# Progress Log

## 2026-08-16 — PCH-00 iteration 1

- Read both source reports completely.
- Adopted each report's final review section as authoritative.
- Excluded all four REJECTED findings and applied CORRECTED constraints.
- Created phased implementation and verification contracts.
- Next: capture current baseline and add red tests for the first PCH-01 batch.

## 2026-08-16 — PCH-00 complete / PCH-01 iteration 2

- Baseline focused backend: 148 passed in 1.51s.
- Baseline Web build: ui 1394.91 kB / 422.71 kB gzip; dashboard 497.56 kB;
  build emitted the greater-than-500-kB warning.
- Reproduced the idempotency disconnect defect as a tight receive loop that starved timeout
  cancellation; terminated only the two exact pytest processes.
- Fixed disconnect/cancel cleanup, bounded no-Redis session cache, stale state cache reinsertion,
  artifact legacy-owner fail-open, cross-user artifact creation, unqualified conversation-share
  artifact SQL, and subagent task/context/event redaction.
- Batch gate: 54 passed in 0.96s.
- Next: H24/H25/M7/M8 and Web terminal/session epoch.

## 2026-08-16 — PCH-01/PCH-02 iteration 3

- Closed legacy approval budget restore and distinguished internal `TimeoutError` from the
  authoritative wall-clock deadline.
- Added bounded native/MCP cancellation, caller-cancellation propagation and unknown-side-effect
  fencing; suppressed duplicate final text after final-iteration tool preambles.
- Made Web terminal state first-wins, invalidated old streams with a session epoch, authenticated
  shared SSE fetches and classified playground transport errors as failures.
- Moved memory source reads off the event loop, bounded the document cache and skipped unchanged
  source indexing; reused checkpoint digests and reduced completed-Markdown reparsing.
- Focused Assistant/shared-core gate: 377 passed. Web type-check and build passed; targeted stream
  Playwright: 3 passed.

## 2026-08-16 — PCH-02/PCH-04 iteration 4

- Usage recorder now collapses duplicate idempotency identities and writes 100 rows through one
  production `fetch` call instead of one round trip per row.
- Trace writer now writes root/lifecycle once per trace and drains to a fixed point. A 25-delta
  regression records 27 DB calls instead of the former repeated root/lifecycle pattern.
- Web production build completed in 696 ms. The forced 1,394.91 kB `ui` chunk disappeared; the
  largest remaining chunk is dashboard at 510.38 kB / 140.17 kB gzip.
- First full suite exposed 15 regressions. All were reproduced and fixed; rerun passed
  6,176 with 23 explicit skips.

## 2026-08-16 — PCH-06 iteration 5

- Hot-updated the repository-owned Compose services and passed runtime status/health.
- Real Responses non-stream and SSE stream: 2 passed. Real Assistant x KB lifecycle and tenant
  isolation: 1 passed.
- Ten fresh simple Qwen turns all terminated successfully, but TTFT p50=4.369 s and p95=7.596 s;
  this fails the 3.41 s release ceiling.
- Native complex parity ran all eight tasks with zero infrastructure errors but passed only 3/8.
  This fails the 6/8 maturity threshold.
- Initial docgen live run repeatedly cancelled at 30 s through the discovery bridge and failed.
  Root cause was a second timeout layer in `RegistryToolInvoker` that ignored the target tool's
  306 s budget.

## 2026-08-16 — PCH-06 iteration 6

- Unified discovery-bridge timeout resolution across ToolRegistry and RegistryToolInvoker and
  added a nested timeout regression. Targeted MCP/tool safety: 60 passed.
- After hot update, real docgen generated, persisted, downloaded and validated a document in
  134.79 s. Real image generation and Quiz receipts both passed in 40.45 s total.
- Final Python suite: 6,177 passed, 23 skipped, 0 failed in 148.12 s. Assistant regression gate:
  5/5 groups. Isolation gate with credentials: 7 passed. Web open-source E2E: 48 passed.
- `make validate` passed with one documented local-password warning. PCH-06 remains NO-GO solely
  on measured TTFT and complex-task quality, not component or container availability.
- A source image rebuild of frontend was blocked before application compilation by a Docker Hub
  OAuth IPv6 timeout and missing local Node/Nginx base images. The already-built `web/dist` was
  hot-copied into the repository-owned frontend container, runtime-config was regenerated, the
  container index SHA matched the local build and the port-8081 health check passed. This update
  is intentionally ephemeral until a later image build succeeds.
- Removed the two exact disposable local E2E users created for this run, their two role mappings,
  and all four temporary credential/token files. No historical test account or volume was removed.

## 2026-08-16 — PCH-06 iteration 7

- Kept `thinking=low` and added dual latency evidence instead of disabling reasoning. Ten fresh
  turns passed: first lifecycle p50=0.016 s, first reasoning p50=3.146 s, text TTFT p50=3.925 s,
  total p50=3.976 s. The Web now records first meaningful response separately from first text and
  renders real reasoning/tool activity as soon as it arrives.
- Compared the local OpenCode/OpenClaw implementations. Their perceived-speed advantage comes
  from first-class reasoning/status streaming and model/provider variants; current local setup is
  only ~16 ms, while the provider takes ~3.15 s to emit first reasoning. A 50-token budget and
  qwen-plus/qwen3.6-flash probes did not improve the path reliably and were reverted.
- Diagnosed finance/engineering complex failures from persisted traces: a deterministic Docker
  sandbox exit was incorrectly converted to `SIDE_EFFECT_UNKNOWN`, preventing model repair.
  Code Executor now marks completed isolated results as known and feeds non-zero exit/stderr back
  to the model. Related tests passed and the two real tasks then passed 2/2 after approval/resume.
- Final fresh eight-task cohort completed raw 5/8 with zero infrastructure errors. Exact live
  unknown-effect output correctly proved commit, suppressed retry and cancelled the sibling, but
  the evaluator rejected `workflow_id` and `duplicate_risk=mitigated`. The generic validator and
  tests were fixed; the unchanged live output replays as pass, giving semantic 6/8. Security's R4
  denial and Research evidence/conclusion failures remain real. Medium Research still failed and
  raised text TTFT to 21.306 s, so longer thinking is not the fix.
- Python full suite passed with 6,209 collected / exit 0 (6,186 passed, 23 skipped). Assistant
  runtime gate 5/5, harness, validate, status, diff check and core changed-file Ruff passed.
  Trusted-local Code Executor status/smoke and an Agent live sandbox/artifact test passed.
- Web type-check, lint and build passed (795 ms; 0 lint errors, 10 existing warnings). Targeted
  Assistant telemetry Playwright passed. The unconfigured full Playwright run was 103 passed,
  4 skipped, 29 failed, 3 not run; failures were dominated by missing E2E_API_URL/login state and
  are recorded as not verified, not passed. Latest dist was hot-copied, runtime config regenerated,
  container/local index SHA matched, and frontend health passed.
- Verified and removed the exact disposable `agent-studio-e2e` user/role created for TTFT testing,
  then deleted its temporary credential file. The post-cleanup user count is zero; no historical
  account, runtime volume or unrelated file was removed.
- Restored the Code Executor safe default after live verification with `make code-executor-disable`,
  then reapplied current Python/shared-core source with `make hot-update`. Final status is
  enabled=false, backend=none, docker_socket=no, healthy; all Compose health checks and the
  frontend local/container index hash still pass.

## 2026-08-17 — PCH-07 iteration 8 handoff

- Reproduced the stop-window defect: an assistant `tool_use` without its matching `tool_result`
  was still serializable into an Anthropic request while the context validator classified the
  same transcript as invalid.
- Added idempotent cancelled/not-executed tool results for the whole proposed batch, paired
  checkpoints, provider-boundary orphan/missing/duplicate validation, and a terminal trace drain.
- Changed the main Assistant stop path to retain `run_started.task_id`, call the owner-checked
  cancel API first, consume tool/result terminal events for 2.5 seconds, and abort only as fallback.
- Fixed the cross-container ownership bug by proxying Gateway cancellation into the Assistant
  process TaskManager; cross-owner lookups remain 404 and audit logs contain only bounded digests.
- Focused evidence passed before the final handoff edits: provider boundaries 81/81,
  streaming-first contract 122/122 plus 2 targeted approval/cancellation cases after the added
  regression, Assistant runtime gate 5/5, cancellation/session tests 41/41 plus 4 targeted
  cross-container cases, and browser cancellation/approval set 4/4. Web type-check/lint/build,
  Ruff and harness passed at their recorded checkpoints; lint retained 10 pre-existing warnings.
  The final blocked/side-effect UI projection was production-built during hot update but is
  intentionally left for the combined post-Grok type/lint/browser gate.
- In-app browser exercised an approved 20-second Code Executor operation, stopped it in about
  0.3 seconds, observed paired tool result/end events, and then completed a same-session `web_fetch`
  without provider HTTP 400. Because cancellation happened after a medium-risk dispatch, the
  backend correctly kept the run blocked as `side_effect_unknown`; the final frontend projection
  change still needs the combined post-Grok browser gate.
- Code Executor was returned to `enabled=false`, `backend=none`, `docker_socket=no`; Assistant
  source was hot-updated after the recreation. No commit or push was made so the concurrent Grok
  performance/security changes can be reviewed and gated as one integration.

## 2026-08-17 — PCH-07 iteration 9 integration closure

- Closed the combined review findings: client-selected session IDs now use an atomic
  create-if-absent path; partial/final usage preserves final token and cost totals without a second
  request charge; realtime stream usage records once; incremental memory keeps vector lineage on
  failure; PDF rendering no longer shares PyMuPDF objects across worker threads; retrieval defaults
  cannot truncate a requested `top_k`; and Redis success headers report real remaining capacity.
- Removed the Playwright spec-to-spec import by consolidating the composer readiness checks into the
  owning Assistant suite. The container frontend gate completed 35/36 with one explicit skip because
  no live Playground service was configured; all Assistant cases passed, including tool cancellation
  and same-session follow-up.
- Combined verification passed: 477 Gateway/Assistant/security tests, 873 Knowledge/embedding/PDF
  tests, all changed/untracked Python Ruff, Web type-check/build, 6 Node tests, Harness, diff check,
  runtime validate/status, and repository-owned Compose health.
- In-app-browser acceptance logged in with the ignored E2E account, loaded the Assistant composer in
  178 ms, completed a real Qwen turn with first visible text in 7.918 s, stopped a running docgen tool
  into the conservative `SIDE_EFFECT_UNKNOWN` terminal, and completed a same-session follow-up with
  no provider HTTP 400 or interrupted-stream UI.
- The latest native-agent parity evidence remains an explicit quality gap rather than a correctness
  pass: AI Platform is 5/8 raw while OpenClaw is 7/8. This integration is release-safe for the tested
  contracts, but the broader complex-task quality target remains future optimization work.
