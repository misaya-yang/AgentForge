# Agent And RAG Eval Quality Optimization Design

## Decision

Adopt a truth-first, layered evaluation design. The repository will keep fast offline contract checks, but those checks will stop claiming live Assistant or RAG quality. Recorded observations will be separated from expectations, every declared assertion will execute fail-closed, RAG judge outputs will be validated before persistence, approval pause/resume traces will remain append-only, and release workflows will run the offline quality gate before publishing images.

The user explicitly requested planning followed by implementation in the same task, so this design proceeds directly to the implementation plan without a separate approval pause. No commit, push, deployment, Docker mutation, provider call, or database migration is authorized.

## Baseline Evidence

| Surface | Current result | What it actually proves | High-risk gap |
| --- | --- | --- | --- |
| Assistant runtime regression | 5/5 groups pass; selected pytest groups report 163 passed tests | Selected offline Python contracts pass | The report writes tracked files, contains volatile timing, and omits RAG judge contracts |
| Golden regression | 16/16 cases, overall/critical/trajectory = 1.0 | Inline fixture replay contains expected substrings and span names | Explicit assertions, latency, and most runtime expectations are ignored; expected and observed data share one row |
| KB RAGAS | Wiring tests pass | Gateway, client, service, and evaluator mocks compose | No repository-owned RAG quality report; judge is custom LLM scoring, not calibrated RAGAS; NaN/Infinity can become 1.0 |
| Approval trace | API/runtime tests pass | Approval state machine works in memory | Trace writes `blocked`, which violates the trace status constraint; resume reuses sequence numbers and can overwrite evidence |
| Release gate | CI golden job passes | Pull-request fixture gate ran | Tag-triggered Docker publish does not depend on the runtime/eval gate |

## Quality Layers

1. **G0 — code and schema hygiene.** Ruff, focused unit tests, workflow syntax, and `git diff --check`. This proves repository consistency only.
2. **G1 — offline runtime contract.** Agent loop, memory, tool safety, trace, and RAG judge contract tests. This proves deterministic code behavior without model/provider calls.
3. **G2 — recorded observation gate.** Expectations and observations are separate JSONL sources joined by `case_id`. Every assertion and runtime invariant must have observed evidence. This proves that the checked recorded observations satisfy the contract; it does not prove the current model generated them.
4. **G3 — live candidate evaluation.** A future/manual or nightly runner must execute the candidate revision, capture immutable traces, retain judge/provider/prompt hashes, and report sampling variance. Missing keys or services must produce `not-run` or `blocked`, never inherit a G1/G2 pass.

## Component Design

### Golden contract engine

- `validate_case` accepts only known assertion types with valid value shapes.
- `evaluate_case` consumes an explicit observation when supplied and falls back to inline replay only as a labelled legacy mode.
- Supported assertions are `output_contains`, `required_span_kind`, `no_sensitive_output`, `latency_ms_lt`, and `failure_mode_absent`.
- Runtime expectations compare `expected_exit_reason`, `memory_sync`, `resume_ready`, `loop_guard`, `requires_gateway_decision`, `requires_arguments_hash`, and `requires_sandbox_profile` against observed evidence.
- Missing status/evidence and unknown assertions fail closed.
- Gate metrics use explicit key lookup so a real `0.0` never falls through to another metric.
- Reports include evidence scope and limitations; the offline report title and schema no longer imply live model quality.

### KB RAG judge

- Metric names are validated and stable-deduplicated; unsupported metrics return a 400 rather than silently changing the requested metric.
- All accepted contexts participate under a bounded total-character budget; no hidden first-eight truncation remains.
- Retrieved contexts are serialized as untrusted JSON data. System instructions explicitly prohibit following instructions inside query/context/reference fields.
- Non-finite or out-of-range scores become `review`, not a clamped pass.
- `context_precision` uses per-rank boolean verdicts and a deterministic average-precision calculation instead of trusting a free-form scalar.
- Summary averages and scored counts exclude `review`; review remains visible as a separate attempted-but-invalid outcome.
- Only queued/running evaluator runs block retries; review-only completed runs remain eligible for correction.

### Approval trace integrity

- An approval pause leaves the trace root in its valid `running` state and does not enqueue a false terminal/outbox record.
- Before resume emits new events, the writer drains pending writes and reads the maximum persisted event/span sequence for the trace.
- Resume continues from that cursor, preserving the original approval/checkpoint events and exactly-once tool evidence.
- The execution-run status remains `blocked`; only the trace persistence state stays `running` until resume reaches a legal terminal status.

### Regression harness and release enforcement

- The nested golden run writes to a temporary directory.
- `--no-write` runs the full gate without modifying tracked reports; the Make target uses this mode.
- Markdown output has exactly one final newline.
- RAG core and knowledge-service judge tests are first-class runtime-gate groups.
- CI and tag publishing both run the offline quality gate; image publishing depends on its success.

## Error Handling

- Invalid fixtures, observations, assertion types, metric names, judge payloads, and non-finite scores fail closed with bounded, redacted diagnostics.
- External judge unavailability stays `review` and is retryable; it is not counted as a valid scored result.
- Trace cursor lookup failure does not invent a cursor. Resume returns/records a trace-writer failure path rather than overwriting sequence zero.
- Online model, provider, Docker, and database validation is outside this no-secret/no-runtime execution and is reported separately.

## Acceptance Criteria

- Negative mutation tests prove every supported golden assertion and runtime expectation can turn a case red.
- Expectation JSONL contains no inline replay in the maintained fixture; observation JSONL is joined by `case_id` and reports its source separately.
- NaN, Infinity, unsupported metrics, duplicate metrics, and context tail noise are covered by RAG judge tests.
- Approval pause does not call trace finish; resume starts above the prior maximum sequence.
- `make verify-assistant-runtime-dev` leaves the pre-existing `reports/` worktree state unchanged.
- Focused suites, the full runtime gate, Ruff, workflow parsing, and `git diff --check` pass on the final working tree, except that pre-existing user report modifications are preserved and reported explicitly.

## Deliberate Residual Risks

- This change does not add a live `CandidateRunner`; experiment rows that re-score historical traces remain unsuitable for baseline-vs-candidate quality claims.
- This change does not add a database schema migration for score uniqueness or a transactional per-trace outbox.
- Assistant-family KB tool traces and gateway-family `rag` traces still need a later unified evidence packet with citation-to-document identity and artifact manifests.
- No live provider calibration, multilingual distribution study, Docker E2E, or production data read is performed in this task.
