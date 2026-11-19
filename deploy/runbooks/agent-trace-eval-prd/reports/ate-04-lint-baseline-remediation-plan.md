# ATE-04 Lint Baseline Remediation Plan

Status: implemented

## Why This Exists

ATE-04 was initially blocked by the required broad backend ruff gate:

```bash
uv run ruff check src/api/v1/eval.py src/api/schemas/eval.py packages/ai-gateway-core/src/ai_gateway_core/persistence/repositories/agent_trace_repository.py apps/assistant-service/src/assistant_service tests/api/test_eval_traces.py tests/services/assistant/test_agent_trace_capture.py
```

The first-wave trace/eval touched scope passes ruff and the functional regression suite passes. The failing part is the pre-existing assistant-service package lint baseline.

## Initial Baseline

- Total ruff findings: 636
- Files affected: 123
- Findings with ruff fixes available: 531
- Findings without automatic fixes: 105

Top rule categories:

| Rule | Count | Meaning |
| --- | ---: | --- |
| `UP045` | 167 | Optional type annotations should use `X | None` |
| `W291` | 166 | trailing whitespace |
| `I001` | 74 | unsorted imports |
| `F401` | 54 | unused imports |
| `ARG002` | 44 | unused method arguments |
| `SIM102` | 24 | collapsible nested `if` |
| `UP037` | 17 | quoted annotations |
| `W293` | 16 | blank line with whitespace |
| `SIM105` | 11 | suppressible exception |
| `UP035` | 8 | deprecated typing imports |

Top affected files:

| File | Findings | Main rules |
| --- | ---: | --- |
| `apps/assistant-service/src/assistant_service/core/skills/docx/scripts/office/validators/base.py` | 39 | `W291`, `SIM102`, `SIM114`, `UP015` |
| `apps/assistant-service/src/assistant_service/core/skills/pptx/scripts/office/validators/base.py` | 39 | `W291`, `SIM102`, `SIM114`, `UP015` |
| `apps/assistant-service/src/assistant_service/core/skills/xlsx/scripts/office/validators/base.py` | 39 | `W291`, `SIM102`, `SIM114`, `UP015` |
| `apps/assistant-service/src/assistant_service/core/docgen/ir/base.py` | 18 | `UP045`, `UP037`, `UP007` |
| `apps/assistant-service/src/assistant_service/core/docgen/sandbox/local.py` | 18 | `UP045`, `UP035`, `SIM105` |
| `apps/assistant-service/src/assistant_service/core/docgen/sandbox/docker_backend.py` | 16 | `UP045`, `I001`, `UP035`, `SIM105` |
| `apps/assistant-service/src/assistant_service/core/docgen/service.py` | 15 | `UP045`, `I001`, `UP035`, `F401` |
| `apps/assistant-service/src/assistant_service/core/skills/pdf/scripts/fill_pdf_form_with_annotations.py` | 14 | `W293`, `W291`, `I001`, `UP015` |
| `apps/assistant-service/src/assistant_service/core/docgen/ir/pptx.py` | 12 | `UP045`, `UP007` |
| `apps/assistant-service/src/assistant_service/core/docgen/planners/layout_rules.py` | 12 | `ARG002`, `UP045`, `B005` |

## Safe Auto-Fix Dry Run

A no-worktree-impact dry run was executed by copying `apps/assistant-service/src/assistant_service` to a temporary directory and running:

```bash
uv run ruff check <tmp>/assistant_service --fix --exit-zero
uv run ruff check <tmp>/assistant_service --output-format=json
```

Result:

- Ruff reported `684 errors (551 fixed, 133 remaining)` inside the temporary copy.
- Remaining findings affect 50 files.
- Remaining top rules:
  - `ARG002`: 44
  - `SIM102`: 24
  - `F401`: 15
  - `SIM105`: 11
  - `E402`: 7
  - `E721`: 6
  - `invalid-syntax`: 6
  - `B007`: 5
- Remaining top files:
  - `core/tools/confluence_tool.py`: 10
  - `core/content/structured_output.py`: 7
  - `core/docgen/planners/layout_rules.py`: 7
  - `core/tools/pptx_generator_tool.py`: 7
  - `core/files/file_strategy.py`: 6
  - `core/models/model_registry.py`: 5
  - `core/quality/domain_policies.py`: 5
  - `core/tools/document_generator_tool.py`: 5

Interpretation at the time of reproduction: a cleanup pass could safely start with normal ruff auto-fixes, but it still needed manual review. Because ATE-04 could not pass while the required broad gate failed, the cleanup was executed inside ATE-04 and recorded as release-gate remediation rather than new trace functionality.

## Implemented Unblock Path

1. Ran the cleanup path without a waiver because ATE-04 could not pass until the broad lint gate was cleared.
2. Applied safe automatic fixes, inspected the remaining lint classes, then applied reviewed unsafe mechanical fixes.
3. Preserved interface signatures with narrow `noqa` markers for callback/override unused arguments.
4. Manually fixed Python 3.10 syntax, optional dependency probes, re-export metadata, type comparisons, and loop variable binding.
5. Re-ran the exact ATE-04 broad ruff command successfully.

Initial cleanup pass:

```bash
uv run ruff check apps/assistant-service/src/assistant_service --fix
```

Regression after cleanup:

```bash
uv run --extra dev --extra test pytest -q --no-cov tests/services/assistant/test_agent_trace_capture.py tests/services/assistant/test_agentloop_streaming_first_contract.py tests/integration/test_assistant_isolation_contract.py
```

The exact ATE-04 broad ruff command now passes with `All checks passed!`; whole-demand pytest passes with `28 passed, 2 skipped`; frontend e2e passes with `4 passed`.

## Guardrails

- Do not mix lint cleanup with new trace functionality.
- Do not run unsafe ruff fixes without review.
- Do not edit migrations, deployments, production config, or secrets.
- Preserve public assistant-service API signatures unless a failing lint rule can be resolved without contract change.
- Keep `docs_project/` untouched; it is unrelated untracked user content.
