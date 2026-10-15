# Agent And RAG Eval Quality Optimization Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make Agent and RAG evaluation fail-closed, evidence-scoped, reproducible, append-only across approval resume, and enforced before tagged image publication.

**Architecture:** Preserve the existing eval APIs and storage shapes while hardening their boundaries. Expectations and observations become separate inputs; deterministic contract checks stay offline; the LLM RAG judge is treated as an untrusted producer whose outputs are validated and locally aggregated; trace resume continues an existing sequence without writing an invalid terminal state.

**Tech Stack:** Python 3.12+, pytest, FastAPI/Pydantic, asyncpg-style repositories, uv, Make, GitHub Actions.

## Global Constraints

- Work only in `the project root`.
- Preserve the two pre-existing user modifications under `reports/assistant-runtime-regression/`; do not restore or overwrite them.
- Do not commit, push, deploy, mutate Docker, call external providers, print secrets, or add a database migration.
- Preserve public API response fields; additive fields are allowed only when tests prove backward compatibility.
- Every production behavior change follows RED → verify expected failure → GREEN → focused regression.
- Unknown assertions, unknown metrics, missing observations, and non-finite judge values fail closed or become explicit `review`; never silently pass.

---

### Task 1: Make the golden gate execute evidence instead of trusting fixture self-report

**Files:**
- Modify: `src/services/eval/golden.py`
- Modify: `scripts/eval_golden.py`
- Modify: `tests/services/eval/test_golden_regression_gate.py`
- Modify: `tests/fixtures/eval/golden/assistant_regression_v1.jsonl`
- Create: `tests/fixtures/eval/observations/assistant_regression_v1.jsonl`
- Modify: `src/services/eval/trace_feedback.py`
- Test: `tests/services/eval/test_trace_feedback.py`

**Interfaces:**
- Produces: `load_observations(path) -> dict[str, dict[str, Any]]`.
- Produces: `validate_observations(cases, observations) -> dict[str, Any]`.
- Changes compatibly: `evaluate_case(case, observation=None)` and `evaluate_cases(cases, observations=None)`.
- CLI: `eval_golden.py gate EXPECTATIONS --observations OBSERVATIONS [--baseline-report REPORT]`.

- [ ] **Step 1: Add failing tests for ignored and malformed evidence**

```python
def test_runtime_and_latency_mismatches_fail() -> None:
    case = golden_case(
        assertions=[{"type": "latency_ms_lt", "value": 100}],
        runtime={"expected_exit_reason": "approval_denied", "memory_sync": "skipped"},
    )
    result = evaluate_case(
        case,
        {"status": "succeeded", "output_preview": "ok", "span_kinds": [],
         "total_latency_ms": 101, "exit_reason": "succeeded", "memory_sync": "written"},
    )
    assert result["passed"] is False
    assert "latency_ms_lt" in " ".join(result["failures"])
    assert "expected_exit_reason" in " ".join(result["failures"])


def test_unknown_assertion_and_missing_observation_fail_closed() -> None:
    case = golden_case(assertions=[{"type": "unknown_rule"}])
    assert validate_cases([case])["valid"] is False
    result = evaluate_case(golden_case(), None)
    assert result["passed"] is False
    assert "missing replay observation" in result["failures"]


def test_zero_critical_rate_is_not_replaced_by_pass_rate() -> None:
    gate = apply_gate({
        "overall_score": 1.0,
        "pass_rate": 1.0,
        "trajectory_pass_rate": 1.0,
        "critical_pass_rate": 0.0,
    })
    assert gate["status"] == "fail"
```

- [ ] **Step 2: Verify RED**

Run: `uv run pytest -q --no-cov tests/services/eval/test_golden_regression_gate.py`

Expected: failures show ignored assertion/runtime evidence, unknown assertions accepted, missing replay defaulting to success, and `critical_pass_rate=0.0` falling back to `pass_rate`.

- [ ] **Step 3: Implement typed assertions, runtime checks, and external observations**

```python
SUPPORTED_ASSERTIONS = {
    "output_contains",
    "required_span_kind",
    "no_sensitive_output",
    "latency_ms_lt",
    "failure_mode_absent",
}


def evaluate_case(
    case: dict[str, Any],
    observation: dict[str, Any] | None = None,
) -> dict[str, Any]:
    replay = observation if isinstance(observation, dict) else _case_replay(case)
    if not replay:
        return failed_case_result(case, "missing replay observation")
    failures = evaluate_assertions(case.get("assertions") or [], replay)
    failures.extend(evaluate_runtime_expectations(case, replay))
    failures.extend(evaluate_implicit_compat_expectations(case, replay))
    return build_case_result(case, replay, failures)
```

The implementation must use the shared `ai_gateway_core.security.redaction.redact_trace_text` to detect whether serialization changes under redaction. It must never default a missing replay status to `succeeded`.

- [ ] **Step 4: Split maintained expectations and observations**

Move each `metadata.replay` object from `tests/fixtures/eval/golden/assistant_regression_v1.jsonl` into one line of `tests/fixtures/eval/observations/assistant_regression_v1.jsonl`:

```json
{"case_id":"assistant.runtime.approval_denial","replay":{"status":"succeeded","output_preview":"The action was blocked because approval is required before running the tool.","span_kinds":["lifecycle","tool_execution"],"exit_reason":"approval_denied","gateway_decision":"denied"}}
```

Add the missing bounded evidence required by the existing expectations: `gateway_decision`, `arguments_hash_present`, `sandbox_profile`, and `loop_guard`. Label the report scope `recorded_offline_observation`; do not call it live quality.

- [ ] **Step 5: Align trace-feedback assertions with the typed registry**

Change generated cases from `no_secret_leak` to `no_sensitive_output` and from `failure_mode_regression` to `failure_mode_absent`. Proposed cases remain unevaluable until an observation exists.

- [ ] **Step 6: Verify GREEN and CLI behavior**

Run:

```bash
uv run pytest -q --no-cov \
  tests/services/eval/test_golden_regression_gate.py \
  tests/services/eval/test_trace_feedback.py

uv run python scripts/eval_golden.py gate \
  tests/fixtures/eval/golden/assistant_regression_v1.jsonl \
  --observations tests/fixtures/eval/observations/assistant_regression_v1.jsonl \
  --output /tmp/eval-quality-golden.json \
  --markdown /tmp/eval-quality-golden.md
```

Expected: tests pass; the CLI reports `evidence_scope=recorded_offline_observation`, 16 joined observations, every assertion/runtime check evaluated, and no repository report file changed.

### Task 2: Harden KB RAG judge inputs, score validity, precision calculation, and retry semantics

**Files:**
- Modify: `apps/knowledge-service/src/knowledge_service/services/eval/ragas_eval_service.py`
- Modify: `tests/services/knowledge/test_ragas_eval_service.py`
- Modify: `packages/ai-gateway-core/src/ai_gateway_core/eval/evaluator_executor.py`
- Modify: `tests/services/eval/test_evaluator_executor.py`
- Modify: `packages/ai-gateway-core/src/ai_gateway_core/persistence/repositories/agent_trace_repository.py`
- Modify: `tests/services/eval/test_kb_ragas_service.py`

**Interfaces:**
- Preserves: `KBRagasEvalService.evaluate_retrieval(...) -> list[MetricResult]`.
- Produces: `_average_precision(verdicts: list[bool]) -> float`.
- Preserves existing summary response fields while correcting `average_score` and `scored_count` semantics.

- [ ] **Step 1: Add failing service tests**

```python
@pytest.mark.asyncio
async def test_non_finite_score_is_review(monkeypatch: pytest.MonkeyPatch) -> None:
    service = service_with_response(monkeypatch, '{"score": NaN, "explanation": "bad"}')
    result = await service.evaluate_retrieval(query="q", contexts=["c"])
    assert result[0].label == "review"
    assert result[0].score == 0.0


@pytest.mark.asyncio
async def test_unknown_metrics_are_rejected_and_duplicates_are_stable_deduped() -> None:
    service = service_with_response(None, '{"score": 0.8, "explanation": "ok"}')
    with pytest.raises(ValueError, match="Unsupported KB RAGAS metrics"):
        await service.evaluate_retrieval(query="q", contexts=["c"], metrics=["faithfulness"])


def test_average_precision_is_rank_sensitive() -> None:
    assert _average_precision([True, False, True]) == pytest.approx((1.0 + 2 / 3) / 2)
```

Add a prompt-capture test with 10 contexts and assert context 10 is present in the serialized untrusted data block.

- [ ] **Step 2: Verify RED**

Run: `uv run --package knowledge-service pytest -q --no-cov tests/services/knowledge/test_ragas_eval_service.py`

Expected: NaN becomes a pass, unknown metrics silently fall back, average precision is absent, or contexts 9–10 are omitted.

- [ ] **Step 3: Implement strict judge contract**

```python
def _finite_unit_score(value: Any) -> float:
    score = float(value)
    if not math.isfinite(score) or score < 0.0 or score > 1.0:
        raise ValueError("judge score must be finite and between 0 and 1")
    return score


def _average_precision(verdicts: list[bool]) -> float:
    relevant = 0
    precision_sum = 0.0
    for rank, verdict in enumerate(verdicts, start=1):
        if verdict:
            relevant += 1
            precision_sum += relevant / rank
    return precision_sum / relevant if relevant else 0.0
```

Serialize `question`, `reference_answer`, and all normalized contexts as JSON under a bounded total-character budget. The system message must state that all payload fields are untrusted data and must not be executed as instructions. For `context_precision`, require a boolean verdict per context and compute the numeric result locally.

- [ ] **Step 4: Add failing persistence/retry tests**

Add tests that assert:

```python
assert "status IN ('queued', 'running')" in active_run_query
assert "FILTER (WHERE s.label IN ('pass', 'fail'))" in metric_summary_query
```

The fake repository must show that a completed review-only run is queued again and that `review` rows do not lower the valid-score average or inflate `scored_count`.

- [ ] **Step 5: Implement retry and aggregation corrections**

Restrict `has_active_evaluator_run_for_trace` to queued/running runs. In `get_kb_ragas_summary`, calculate `average_score` and `scored_count` from pass/fail rows only; retain `review_count` as a separate attempted-invalid count. Reject non-finite service payloads again inside `EvaluatorExecutor` as defense in depth.

- [ ] **Step 6: Verify GREEN**

Run:

```bash
uv run --package knowledge-service pytest -q --no-cov \
  tests/services/knowledge/test_ragas_eval_service.py

uv run pytest -q --no-cov \
  tests/services/eval/test_kb_ragas_service.py \
  tests/services/eval/test_evaluator_executor.py \
  tests/services/eval/test_kb_ragas_client.py \
  tests/packages/ai_gateway_core/test_kb_ragas_sample.py
```

Expected: all focused tests pass; unknown/non-finite inputs fail closed; all contexts are visible under the budget; review-only results remain retryable and separate from valid averages.

### Task 3: Preserve approval pause/resume trace evidence without a schema migration

**Files:**
- Modify: `apps/assistant-service/src/assistant_service/core/agent/agent_loop.py`
- Modify: `apps/assistant-service/src/assistant_service/core/trace_writer.py`
- Modify: `tests/services/assistant/test_agentloop_streaming_first_contract.py`
- Modify: `tests/services/assistant/test_agent_trace_capture.py`

**Interfaces:**
- Produces: `AssistantTraceWriter.resume_sequence(ctx) -> int`.
- Preserves execution-run status `blocked`; trace root remains `running` while approval is pending.

- [ ] **Step 1: Add failing pause/resume trace tests**

```python
@pytest.mark.asyncio
async def test_approval_pause_keeps_trace_open() -> None:
    writer = RecordingTraceWriter(resume_cursor=0)
    events = await run_confirming_loop(trace_writer=writer)
    assert any(event.event_type == "approval_required" for event in events)
    assert writer.finish_calls == []


@pytest.mark.asyncio
async def test_resume_continues_after_persisted_sequence() -> None:
    writer = RecordingTraceWriter(resume_cursor=41)
    await run_approved_resume(trace_writer=writer)
    assert writer.resume_sequence_calls == 1
    assert min(writer.recorded_sequences) >= 42
    assert writer.recorded_sequences == sorted(set(writer.recorded_sequences))
```

- [ ] **Step 2: Verify RED**

Run: `uv run --package assistant-service pytest -q --no-cov tests/services/assistant/test_agentloop_streaming_first_contract.py -k 'approval and trace'`

Expected: pause currently calls finish with `blocked`, and resume currently begins from sequence 1.

- [ ] **Step 3: Implement a persisted resume cursor**

```python
async def resume_sequence(self, ctx: AssistantTraceContext) -> int:
    await self.drain(timeout_s=self.write_timeout_s)
    row = await self.database.fetchrow(
        """SELECT GREATEST(
               COALESCE((SELECT MAX(sequence_no) FROM agent_trace_events WHERE trace_id = $1), 0),
               COALESCE((SELECT MAX(sequence_no) FROM agent_trace_spans WHERE trace_id = $1), 0)
           ) AS sequence_no""",
        ctx.trace_id,
    )
    return int((row or {}).get("sequence_no") or 0)
```

When resume is validated and `ctx.run_id` is replaced, initialize `ctx.trace_sequence_no` from `resume_sequence` before `start_trace`. If cursor lookup fails, record a trace-writer error and stop the resume path rather than overwriting existing evidence.

- [ ] **Step 4: Keep paused traces non-terminal**

In the AgentLoop `finally` block, call `_finish_trace` only when `ctx.approval_paused` is false. The approval event and checkpoint remain persisted; the trace root retains legal `running` status until resume reaches `succeeded`, `failed`, `cancelled`, or `timeout`.

- [ ] **Step 5: Verify GREEN and broader contracts**

Run:

```bash
uv run --package assistant-service pytest -q --no-cov \
  tests/services/assistant/test_agentloop_streaming_first_contract.py \
  tests/services/assistant/test_agent_trace_capture.py
```

Expected: pause/resume trace tests and the complete AgentLoop/trace contract files pass; execution gateway still reports blocked while paused and the tool still executes exactly once after approval.

### Task 4: Make the runtime gate read-only, truthful, RAG-aware, and release-blocking

**Files:**
- Modify: `scripts/assistant_runtime_regression.py`
- Create: `tests/scripts/test_assistant_runtime_regression.py`
- Modify: `Makefile`
- Modify: `.github/workflows/ci.yml`
- Modify: `.github/workflows/docker-publish.yml`

**Interfaces:**
- CLI: `assistant_runtime_regression.py gate --no-write`.
- Report: adds `evidence_scope`, `limitations`, bounded failure details, fixture/observation digests, and git revision metadata.

- [ ] **Step 1: Add failing harness tests**

```python
def test_no_write_does_not_touch_repository_reports(tmp_path: Path, monkeypatch) -> None:
    before = report_digests(REPO_ROOT / "reports")
    exit_code = main(["gate", "--no-write"])
    assert exit_code == 0
    assert report_digests(REPO_ROOT / "reports") == before


def test_markdown_has_one_final_newline(tmp_path: Path) -> None:
    write_reports(gate_result(), tmp_path / "gate.json", tmp_path / "gate.md")
    content = (tmp_path / "gate.md").read_bytes()
    assert content.endswith(b"\n")
    assert not content.endswith(b"\n\n")


def test_runtime_groups_include_rag_contracts() -> None:
    paths = {path for group in TEST_GROUPS for path in group["paths"]}
    assert "tests/services/knowledge/test_ragas_eval_service.py" in paths
    assert "tests/services/eval/test_kb_ragas_service.py" in paths
```

Use monkeypatched subprocess runners for `--no-write`; the test must not run the whole suite recursively.

- [ ] **Step 2: Verify RED**

Run: `uv run pytest -q --no-cov tests/scripts/test_assistant_runtime_regression.py`

Expected: no `--no-write` flag exists, nested golden output is hard-coded to tracked reports, Markdown has a blank EOF line, and RAG groups are absent.

- [ ] **Step 3: Implement read-only execution and truthful reports**

Use `tempfile.TemporaryDirectory()` for nested golden JSON/Markdown. `--no-write` prints the gate result without calling `write_reports`. Keep volatile timestamp/elapsed data only in the execution receipt, and label the verdict `offline_contract` with explicit `does_not_execute_live_model`, `does_not_measure_live_rag_distribution`, and `does_not_compare_candidate_runtime` limitations.

Add two groups:

```python
{
    "id": "ahr-04-rag-eval-core",
    "phase": "AHR-04",
    "runner": "pytest",
    "extra_args": [],
    "paths": [
        "tests/services/eval/test_kb_ragas_service.py",
        "tests/services/eval/test_kb_ragas_client.py",
        "tests/packages/ai_gateway_core/test_kb_ragas_sample.py",
    ],
},
{
    "id": "ahr-04-rag-judge-contract",
    "phase": "AHR-04",
    "runner": "pytest",
    "extra_args": ["--package", "knowledge-service"],
    "paths": ["tests/services/knowledge/test_ragas_eval_service.py"],
},
```

- [ ] **Step 4: Make local and release workflows use the gate**

Change `make verify-assistant-runtime-dev` to pass `--no-write`. In CI, pin `uv==0.9.27` and run the hardened golden command with the observation file plus the read-only runtime gate. Add an `offline-quality-gates` job to tag publishing and set `build-and-push.needs: offline-quality-gates`.

- [ ] **Step 5: Verify GREEN and workflow structure**

Run:

```bash
uv run pytest -q --no-cov tests/scripts/test_assistant_runtime_regression.py
make verify-assistant-runtime-dev
git status --short -- reports
uv run python - <<'PY'
from pathlib import Path
import yaml
for path in (Path('.github/workflows/ci.yml'), Path('.github/workflows/docker-publish.yml')):
    yaml.safe_load(path.read_text())
print('workflow yaml: pass')
PY
```

Expected: harness tests and runtime gate pass; `reports/` shows exactly the same two pre-existing modified files/digests as before Task 4; workflow YAML parses; Docker publish requires the quality job.

### Task 5: Whole-change verification and independent review

**Files:**
- Verify: all files changed by Tasks 1–4
- Create: `deploy/runbooks/eval-quality-optimization/verification-report.md`

**Interfaces:**
- Produces: a source-backed report separating verified offline facts, unrun online checks, residual risk, and exact commands.

- [ ] **Step 1: Run focused static checks**

```bash
uv run ruff check \
  src/services/eval/golden.py \
  scripts/eval_golden.py \
  scripts/assistant_runtime_regression.py \
  src/services/eval/trace_feedback.py \
  apps/knowledge-service/src/knowledge_service/services/eval/ragas_eval_service.py \
  packages/ai-gateway-core/src/ai_gateway_core/eval/evaluator_executor.py \
  packages/ai-gateway-core/src/ai_gateway_core/persistence/repositories/agent_trace_repository.py \
  apps/assistant-service/src/assistant_service/core/agent/agent_loop.py \
  apps/assistant-service/src/assistant_service/core/trace_writer.py \
  tests/services/eval/test_golden_regression_gate.py \
  tests/services/knowledge/test_ragas_eval_service.py \
  tests/scripts/test_assistant_runtime_regression.py
```

Expected: `All checks passed!`.

- [ ] **Step 2: Run focused and aggregate tests**

```bash
uv run pytest -q --no-cov \
  tests/services/eval/test_golden_regression_gate.py \
  tests/services/eval/test_trace_feedback.py \
  tests/services/eval/test_kb_ragas_service.py \
  tests/services/eval/test_kb_ragas_client.py \
  tests/services/eval/test_evaluator_executor.py \
  tests/packages/ai_gateway_core/test_kb_ragas_sample.py \
  tests/scripts/test_assistant_runtime_regression.py

uv run --package knowledge-service pytest -q --no-cov \
  tests/services/knowledge/test_ragas_eval_service.py

uv run --package assistant-service pytest -q --no-cov \
  tests/services/assistant/test_agentloop_streaming_first_contract.py \
  tests/services/assistant/test_agent_trace_capture.py

make verify-assistant-runtime-dev
```

Expected: zero failures. Warning counts are recorded rather than silently treated as quality passes.

- [ ] **Step 3: Verify worktree hygiene without overwriting user files**

```bash
git diff --check
git status --short
git diff -- reports/assistant-runtime-regression/latest.json \
  reports/assistant-runtime-regression/latest.md
```

Expected: no new whitespace errors; the original two user report modifications remain identifiable and are not regenerated by the new gate.

- [ ] **Step 4: Independent whole-diff review**

The reviewer checks requirement coverage, public-contract compatibility, secret handling, fail-closed behavior, async trace ordering, SQL aggregation semantics, workflow dependency direction, and test quality. Critical/important findings are fixed and re-reviewed before completion.

- [ ] **Step 5: Write verification report**

The report must include baseline vs optimized behavior, exact test counts, report-file preservation evidence, unrun live/Docker/provider checks, and the residual P1/P2 roadmap: CandidateRunner, immutable trace revisions, transactional outbox/completeness watermark, unified assistant/gateway RAG evidence packet, citation fidelity, and multilingual calibrated live datasets.
