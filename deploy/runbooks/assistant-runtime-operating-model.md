# Assistant Runtime Operating Model

> Read-only runbook defining runtime health, failure categories, no-go thresholds,
> rollback behavior, owner surfaces, report locations, waiver policy, CI adoption
> policy, and the future read-only Assistant Runtime Doctor design.

**Schema:** `assistant-runtime-operating-model/v1`
**Owner phases:** AHR-01 through AHR-05
**Gate command:** `make verify-assistant-runtime-dev`

---

## 1. Runtime Health

A healthy Assistant runtime satisfies all of:

| Signal | Source | Threshold |
| --- | --- | --- |
| Turn contract envelope emitted | `assistant-turn-contract/v1` (AHR-01) | Every run exposes `terminal_envelope` and `context_snapshot` |
| Stream / non-stream parity | AHR-01 contract tests | Terminal state agrees across paths |
| Memory lifecycle discipline | `assistant-memory-lifecycle/v1` (AHR-02) | Completed-turn sync only; interrupted/failed turns skipped |
| Tool safety fail-closed | ExecutionGateway (AHR-03) | Risky tools denied without gateway approval |
| Trace / eval cockpit | `assistant-runtime-trajectory/v1` (AHR-04) | Trajectory metadata bounded; golden gate pass_rate = 1.0 |
| Regression gate green | `make verify-assistant-runtime-dev` (AHR-05) | All 5 groups pass |

## 2. Failure Categories

| Category | Severity | Example | Owner Phase |
| --- | --- | --- | --- |
| Turn contract drift | Critical | Missing `exit_reason` or `context_snapshot_id` on terminal event | AHR-01 |
| Memory leak / sync violation | Critical | Durable sync on interrupted turn | AHR-02 |
| Tool safety bypass | Critical | Risky tool executed without gateway approval | AHR-03 |
| Trace write failure | High | `runtime_trajectory` missing or unbounded | AHR-04 |
| Golden regression failure | High | Critical golden case fails | AHR-04 |
| Eval dashboard metric drift | Medium | `runtime_health` counters show unexpected zeros | AHR-04 |
| CI gate flake | Low | Intermittent timeout in non-critical test | AHR-05 |

## 3. No-Go Thresholds

The following are **release-blocking no-go** conditions:

1. Any critical-phase group (AHR-01 through AHR-04) fails in `make verify-assistant-runtime-dev`.
2. Eval golden gate `critical_pass_rate < 1.0`.
3. Eval golden gate `trajectory_pass_rate < 0.95`.
4. Any tool registered with `risk_level` medium/high that bypasses `ExecutionGateway`.
5. Memory durable sync on interrupted, cancelled, or failed turns.
6. Missing or unbounded `runtime_trajectory` in trace output.

## 4. Rollback Behavior

| Trigger | Rollback Action |
| --- | --- |
| Turn contract regression | Revert AHR-01 changes; restore `turn_contract.py` and `agent_loop.py` from git |
| Memory lifecycle regression | Revert AHR-02 changes; restore `source_store.py` and `memory/` from git |
| Tool safety regression | Revert AHR-03 changes; restore `tool_registry.py` and `execution_gateway.py` from git |
| Eval cockpit regression | Revert AHR-04 changes; restore `trace_writer.py` and eval UI from git |
| Runtime gate failure | Revert AHR-05 changes; restore `Makefile` and `scripts/assistant_runtime_regression.py` from git |

Rollback is always git-revert scoped; no production migration, deployment, or data mutation is involved.

## 5. Owner Surfaces

| Surface | Phase | Primary Files |
| --- | --- | --- |
| Turn contract | AHR-01 | `turn_contract.py`, `agent_loop.py`, `assistant_service.py` |
| Memory lifecycle | AHR-02 | `source_store.py`, `memory/`, `memory_service.py` |
| Tool safety | AHR-03 | `tool_registry.py`, `execution_gateway.py`, `audit/` |
| Eval cockpit | AHR-04 | `trace_writer.py`, `agent_trace_repository.py`, eval UI |
| Operating model | AHR-05 | `Makefile`, `scripts/assistant_runtime_regression.py`, this runbook |

## 6. Report Locations

| Report | Path | Format |
| --- | --- | --- |
| Eval golden gate | `reports/eval-regression/latest.json` | JSON |
| Eval golden gate | `reports/eval-regression/latest.md` | Markdown |
| Runtime regression gate | `reports/assistant-runtime-regression/latest.json` | JSON |
| Runtime regression gate | `reports/assistant-runtime-regression/latest.md` | Markdown |
| Phase reports | `deploy/runbooks/assistant-hermes-runtime-prd/reports/ahr-*.md` | Markdown |
| Harness state | `deploy/runbooks/assistant-hermes-runtime-prd/loop-state.json` | JSON |
| Feature oracle | `deploy/runbooks/assistant-hermes-runtime-prd/feature-oracle.json` | JSON |

## 7. Waiver Policy

A no-go threshold or failed group may only be waived when **all** conditions are met:

1. The user or release owner explicitly approves the waiver in writing.
2. The failure root cause is documented in the phase report or regression report.
3. Remaining evidence still proves the affected feature-oracle item.
4. The waiver is recorded in the `continuity-ledger.md` and `progress-log.md`.
5. The waiver does not mask a security or data-integrity risk.

Waiver is **never** automatic. A timeout, flake, or environment issue is a blocker, not a waiver.

## 8. CI Adoption Policy

The runtime regression gate follows a three-stage adoption path:

| Stage | Trigger | CI Status |
| --- | --- | --- |
| **Local / offline** | `make verify-assistant-runtime-dev` passes locally | Not in CI; developer-runs only |
| **Optional / manual** | Gate is stable across 3+ consecutive runs with no flakes | Added as non-blocking CI job |
| **CI-blocking** | Gate has 30+ day flake-free history; dependency risks understood | Promoted to required PR check |

Current stage: **Local / offline**.

Do not add `verify-assistant-runtime-dev` to required PR CI until the optional/manual stage is complete.

## 9. Read-Only Assistant Runtime Doctor (Future Design)

A future `make assistant-runtime-doctor` or equivalent command should:

- **Be read-only and offline.** No repair, no migration, no production access.
- **Cover these checks:**

| Check | Source | Secret-safe |
| --- | --- | --- |
| DB/Redis connectivity | Connection probe | Yes (no credentials logged) |
| assistant-service imports | Module import check | Yes |
| Trace writer metrics | Last N trace write outcomes | Yes (redacted) |
| Eval outbox status | Pending/failed eval outbox count | Yes |
| Memory index freshness | Newest indexed memory timestamp | Yes |
| Tool registry policy | Effective tool risk/permission summary | Yes (no args logged) |
| Sandbox availability | Docker/gVisor runtime probe | Yes |
| safe_fetch wiring | Registered safe_fetch call sites | Yes |
| TS contract drift | Eval API type-check vs backend schema | Yes |
| Redaction/export policy | Effective redaction config summary | Yes (no raw data) |

- **Output format:** Bounded JSON + human-readable Markdown under `reports/assistant-runtime-doctor/`.
- **Repair behavior:** Requires a separate phase approval; doctor itself is read-only.

## 10. Terminology Invariants

| Term | Definition | Source Phase |
| --- | --- | --- |
| run | A single assistant execution request with a unique `run_id` | AHR-01 |
| session | A conversation context containing multiple turns | AHR-01 |
| turn | A single request/response exchange within a session | AHR-01 |
| trace | Structured metadata captured during a run for observability | AHR-04 |
| checkpoint | Intermediate state snapshot for resume after interruption | AHR-01 |
| memory source | A named, typed, scored memory fragment with provenance | AHR-02 |
| transcript | Session-level replay state; not durable long-term memory | AHR-02 |
| runtime trajectory | Bounded sequence metadata linking context/tool/memory decisions | AHR-04 |
| execution gateway | Audited approval path for risky tool invocations | AHR-03 |
| golden case | A curated test case in the eval regression fixture | AHR-04 |
| gate | A pass/fail decision computed from metrics against thresholds | AHR-04, AHR-05 |
| waiver | Explicit user approval to bypass a failed gate with documented risk | AHR-05 |
| no-go | A release-blocking failure condition that prevents deployment | AHR-05 |
