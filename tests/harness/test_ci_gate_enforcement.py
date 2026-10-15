from __future__ import annotations

import copy
import json
from pathlib import Path

import pytest

from scripts.harness import affected_gates as selector
from scripts.harness.ci_gate_enforcement import (
    evaluate,
    normalize_job_results,
    validate_workflow_wiring,
)


def _gates() -> dict[str, dict[str, str]]:
    return {
        "affected_gates": {
            "required_on": "[change, release]",
            "ci_job": "gate-enforcement",
            "evidence": "tmp/gate-evidence/affected-gates.json",
        },
        "frontend": {
            "required_on": "[change, release]",
            "ci_job": "frontend",
            "evidence": "tmp/gate-evidence/frontend.log",
        },
        "manual_live": {
            "required_on": "[manual]",
            "ci_job": "manual",
            "evidence": "tmp/gate-evidence/manual-live.log",
        },
    }


def _selection() -> dict:
    return {
        "result": "pass",
        "changed_paths": ["harness.yml", "web/src/App.tsx"],
        "ungated_paths": [],
        "selected": {
            "affected_gates": {
                "ci_job": "gate-enforcement",
                "matched_paths": ["harness.yml"],
            },
            "frontend": {
                "ci_job": "frontend",
                "matched_paths": ["web/src/App.tsx"],
            },
        },
    }


def test_success_requires_upstream_result_and_local_selector() -> None:
    outcome = evaluate(_selection(), _gates(), {"frontend": "success"})

    assert outcome["result"] == "pass"
    assert outcome["required_results"] == {
        "frontend": {"gates": ["frontend"], "result": "success"},
        "gate-enforcement": {
            "gates": ["affected_gates"],
            "result": "success",
        },
    }


def test_missing_failed_skipped_and_cancelled_results_fail_closed() -> None:
    for result in (None, "failure", "skipped", "cancelled"):
        job_results = {} if result is None else {"frontend": result}
        outcome = evaluate(_selection(), _gates(), job_results)

        assert outcome["result"] == "fail"
        assert any("required CI job 'frontend'" in failure for failure in outcome["failures"])


def test_manual_only_match_does_not_count_as_ci_coverage() -> None:
    selection = {
        "result": "pass",
        "changed_paths": ["src/new.py"],
        "ungated_paths": [],
        "selected": {
            "manual_live": {"ci_job": "manual", "matched_paths": ["src/new.py"]},
        },
    }

    outcome = evaluate(selection, _gates(), {})

    assert outcome["result"] == "fail"
    assert outcome["failures"] == [
        "changed path has no change-required CI-wired gate: src/new.py"
    ]


def test_changed_path_must_appear_in_selected_gate_matches() -> None:
    selection = _selection()
    selection["changed_paths"].append("web/src/Missing.tsx")

    outcome = evaluate(selection, _gates(), {"frontend": "success"})

    assert outcome["result"] == "fail"
    assert "changed path is absent from selected gate matches: web/src/Missing.tsx" in outcome[
        "failures"
    ]
    assert "changed path has no change-required CI-wired gate: web/src/Missing.tsx" in outcome[
        "failures"
    ]


def test_selection_cannot_change_the_harness_ci_job() -> None:
    selection = copy.deepcopy(_selection())
    selection["selected"]["frontend"]["ci_job"] = "fake-green-job"

    outcome = evaluate(selection, _gates(), {"frontend": "success"})

    assert outcome["result"] == "fail"
    assert any("ci_job drifted" in failure for failure in outcome["failures"])


def test_selected_gate_requires_explicit_evidence_and_ci_job() -> None:
    gates = _gates()
    del gates["frontend"]["evidence"]
    del gates["affected_gates"]["ci_job"]
    selection = _selection()
    selection["selected"]["affected_gates"]["ci_job"] = None

    outcome = evaluate(selection, gates, {"frontend": "success"})

    assert outcome["result"] == "fail"
    assert "selected gate 'frontend' has no explicit evidence" in outcome["failures"]
    assert "selected gate 'affected_gates' has no explicit ci_job" in outcome["failures"]


def test_selected_change_gate_cannot_use_manual_ci_sentinel() -> None:
    gates = _gates()
    gates["frontend"]["ci_job"] = "manual"
    selection = _selection()
    selection["selected"]["frontend"]["ci_job"] = "manual"

    outcome = evaluate(selection, gates, {})

    assert outcome["result"] == "fail"
    assert (
        "selected change-required gate 'frontend' cannot use ci_job 'manual'"
        in outcome["failures"]
    )


def test_final_job_cannot_self_satisfy_non_selector_gates() -> None:
    gates = _gates()
    gates["frontend"]["ci_job"] = "gate-enforcement"
    selection = _selection()
    selection["selected"]["frontend"]["ci_job"] = "gate-enforcement"

    outcome = evaluate(selection, gates, {})

    assert outcome["result"] == "fail"
    assert any("may satisfy only 'affected_gates'" in failure for failure in outcome["failures"])


def test_unknown_selected_gate_fails() -> None:
    selection = _selection()
    selection["selected"]["ghost"] = {"ci_job": "ghost", "matched_paths": []}

    outcome = evaluate(selection, _gates(), {"frontend": "success"})

    assert outcome["result"] == "fail"
    assert "selector returned unknown gate 'ghost'" in outcome["failures"]


def test_normalize_accepts_github_needs_and_simple_result_maps() -> None:
    github, github_errors = normalize_job_results(
        {
            "frontend": {"result": "success", "outputs": {}},
            "gateway-units": {"result": "failure", "outputs": {}},
        }
    )
    simple, simple_errors = normalize_job_results({"frontend": "success"})

    assert github == {"frontend": "success", "gateway-units": "failure"}
    assert not github_errors
    assert simple == {"frontend": "success"}
    assert not simple_errors


def test_normalize_rejects_malformed_job_results() -> None:
    results, errors = normalize_job_results({"frontend": {"outputs": {}}})

    assert results == {}
    assert errors == ["CI job 'frontend' has no string result"]


def test_final_ci_mode_rejects_an_unselected_upstream_failure() -> None:
    outcome = evaluate(
        _selection(),
        _gates(),
        {"frontend": "success", "script-contracts": "failure"},
        require_all_jobs=True,
    )

    assert outcome["result"] == "fail"
    assert "upstream CI job 'script-contracts' is 'failure', expected 'success'" in outcome[
        "failures"
    ]


def test_ci_workflow_jobs_execute_the_gates_they_claim() -> None:
    assert validate_workflow_wiring() == []


def test_agent_runtime_contract_job_provisions_postgres() -> None:
    workflow = Path(".github/workflows/ci.yml").read_text(encoding="utf-8")
    job_start = workflow.index("  agent-runtime-contracts:")
    job_end = workflow.index("\n  sdk-sse:", job_start)
    job = workflow[job_start:job_end]

    assert "services:\n      postgres:" in job
    assert "POSTGRES_DB: agent_runtime_contract_test" in job
    assert "- name: Configure Agent Runtime contract database" in job
    assert "make agent-runtime-single-kernel-gate" in job


def test_workflow_wiring_rejects_named_job_without_declared_entrypoint(
    tmp_path: Path,
) -> None:
    workflow = tmp_path / "ci.yml"
    workflow.write_text(
        "name: test\n"
        "jobs:\n"
        "  unit-job:\n"
        "    runs-on: ubuntu-latest\n"
        "    steps:\n"
        "      - run: make unrelated-gate\n",
        encoding="utf-8",
    )
    gates = {
        "unit": {
            "make": "unit-gate",
            "ci_job": "unit-job",
            "evidence": "tmp/gate-evidence/unit.log",
            "required_on": "[change]",
        }
    }

    issues = validate_workflow_wiring(workflow, gates)

    assert any(
        "CI job 'unit-job' does not execute gate 'unit' entrypoint: unit-gate" in issue
        for issue in issues
    )


def test_workflow_comment_cannot_claim_to_execute_a_gate(tmp_path: Path) -> None:
    workflow = tmp_path / "ci.yml"
    workflow.write_text(
        "name: test\n"
        "jobs:\n"
        "  unit-job:\n"
        "    runs-on: ubuntu-latest\n"
        "    steps:\n"
        "      - run: |\n"
        "          # make unit-gate\n"
        "          make unrelated-gate\n",
        encoding="utf-8",
    )
    gates = {
        "unit": {
            "make": "unit-gate",
            "ci_job": "unit-job",
            "evidence": "tmp/gate-evidence/unit.log",
            "required_on": "[change]",
        }
    }

    issues = validate_workflow_wiring(workflow, gates)

    assert any("does not execute gate 'unit' entrypoint" in issue for issue in issues)


@pytest.mark.parametrize(
    "run_value",
    [
        "make unit-gate --dry-run",
        "make unit-gate || true",
        "make unit-gate -f /dev/null",
        "make unit-gate | true",
        "MAKEFLAGS=-n make unit-gate",
        ">-\n          make unit-gate\n          --dry-run",
        "|\n          make unit-gate \\\n            --dry-run",
        "|\n          set +e\n          make unit-gate\n          true",
    ],
)
def test_workflow_wiring_rejects_nonexecuting_or_error_swallowing_make(
    tmp_path: Path,
    run_value: str,
) -> None:
    workflow = tmp_path / "ci.yml"
    workflow.write_text(
        "name: test\n"
        "jobs:\n"
        "  unit-job:\n"
        "    runs-on: ubuntu-latest\n"
        "    steps:\n"
        f"      - run: {run_value}\n",
        encoding="utf-8",
    )
    gates = {
        "unit": {
            "make": "unit-gate",
            "ci_job": "unit-job",
            "evidence": "tmp/gate-evidence/unit.log",
            "required_on": "[change]",
        }
    }

    issues = validate_workflow_wiring(workflow, gates)

    assert any("does not execute gate 'unit' entrypoint" in issue for issue in issues)


@pytest.mark.parametrize(
    "run_value",
    [
        "|\n          pnpm type-check\n          pnpm test || true",
        "|\n          pnpm type-check\n          pnpm test-placeholder",
        "|\n          pnpm type-check\n          # pnpm test",
    ],
)
def test_workflow_shell_gate_rejects_swallowed_suffixed_or_commented_commands(
    tmp_path: Path,
    run_value: str,
) -> None:
    workflow = tmp_path / "ci.yml"
    workflow.write_text(
        "name: test\n"
        "jobs:\n"
        "  unit-job:\n"
        "    runs-on: ubuntu-latest\n"
        "    steps:\n"
        f"      - run: {run_value}\n",
        encoding="utf-8",
    )
    gates = {
        "unit": {
            "shell": "pnpm type-check && pnpm test",
            "ci_job": "unit-job",
            "evidence": "tmp/gate-evidence/unit.log",
            "required_on": "[change]",
        }
    }

    issues = validate_workflow_wiring(workflow, gates)

    assert any("does not execute gate 'unit' entrypoint" in issue for issue in issues)


def test_fetch_depth_must_belong_to_the_architecture_checkout_step(
    tmp_path: Path,
) -> None:
    workflow = tmp_path / "ci.yml"
    workflow.write_text(
        "name: test\n"
        "jobs:\n"
        "  architecture-gates:\n"
        "    runs-on: ubuntu-latest\n"
        "    steps:\n"
        "      - uses: actions/checkout@v4\n"
        "      - name: fetch-depth: 0\n"
        "        run: make unit-gate\n",
        encoding="utf-8",
    )
    gates = {
        "unit": {
            "make": "unit-gate",
            "ci_job": "architecture-gates",
            "evidence": "tmp/gate-evidence/unit.log",
            "required_on": "[change]",
        }
    }

    issues = validate_workflow_wiring(workflow, gates)

    assert "CI job 'architecture-gates' checkout must set fetch-depth: 0" in issues


def test_empty_diff_still_writes_selector_evidence(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    harness = tmp_path / "harness.yml"
    evidence = tmp_path / "affected-gates.json"
    harness.write_text(
        "gates:\n"
        "  harness:\n"
        "    make: harness-check\n"
        "    triggers: [harness.yml]\n"
        "    tier: L0\n"
        "    required_on: [change]\n"
        "    resource: offline\n"
        "    skip: never\n"
        "    timeout: 5\n"
        "    evidence: tmp/gate-evidence/harness.json\n"
        "    ci_job: harness\n",
        encoding="utf-8",
    )
    monkeypatch.setattr(selector, "changed_paths_since", lambda _base: [])

    assert (
        selector.main(
            [
                "--base",
                "0" * 40,
                "--harness",
                str(harness),
                "--evidence-out",
                str(evidence),
            ]
        )
        == 0
    )
    payload = json.loads(evidence.read_text(encoding="utf-8"))
    assert payload["result"] == "pass"
    assert payload["changed_paths"] == []
    assert payload["selected"] == {}
