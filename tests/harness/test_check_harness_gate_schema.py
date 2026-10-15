from __future__ import annotations

from pathlib import Path

import pytest

from scripts.harness.check_harness import gate_schema_violations


def _gate_lines(
    *,
    evidence: str | None = "tmp/gate-evidence/unit-gate.log",
    ci_job: str | None = "unit-job",
    required_on: str = "[change, release]",
    resource: str = "offline",
    skip: str = "never",
    entrypoint: str = "make: unit-gate",
) -> list[str]:
    fields = [
        "gates:",
        "  unit:",
        f"    {entrypoint}",
        "    triggers: [src/**]",
        "    tier: L1",
        f"    required_on: {required_on}",
        f"    resource: {resource}",
        f"    skip: {skip}",
        "    timeout: 10",
    ]
    if evidence is not None:
        fields.append(f"    evidence: {evidence}")
    if ci_job is not None:
        fields.append(f"    ci_job: {ci_job}")
    return fields


def _workflow(tmp_path: Path, *, job_body: str = "        run: make unit-gate\n") -> Path:
    path = tmp_path / "ci.yml"
    path.write_text(
        "name: test\n"
        "jobs:\n"
        "  unit-job:\n"
        "    runs-on: ubuntu-latest\n"
        "    steps:\n"
        "      - name: gate\n"
        f"{job_body}",
        encoding="utf-8",
    )
    return path


def test_complete_gate_with_real_ci_entrypoint_passes(tmp_path: Path) -> None:
    violations = gate_schema_violations(
        _gate_lines(),
        {"unit-gate"},
        _workflow(tmp_path),
    )

    assert violations == []


@pytest.mark.parametrize(
    ("evidence", "ci_job", "message"),
    [
        (None, "unit-job", "evidence must be explicit and non-empty"),
        (
            "tmp/gate-evidence/unit-gate.log",
            None,
            "ci_job must be explicit",
        ),
    ],
)
def test_evidence_and_ci_job_are_mandatory(
    tmp_path: Path,
    evidence: str | None,
    ci_job: str | None,
    message: str,
) -> None:
    violations = gate_schema_violations(
        _gate_lines(evidence=evidence, ci_job=ci_job),
        {"unit-gate"},
        _workflow(tmp_path),
    )

    assert any(message in violation for violation in violations)


@pytest.mark.parametrize("resource", ["offline", "hosted-service"])
def test_change_required_never_skip_gate_cannot_hide_behind_manual(
    tmp_path: Path,
    resource: str,
) -> None:
    violations = gate_schema_violations(
        _gate_lines(ci_job="manual", resource=resource),
        {"unit-gate"},
        _workflow(tmp_path),
    )

    assert any("change-required gate cannot use ci_job 'manual'" in item for item in violations)
    assert any("needs a real CI job" in item for item in violations)


def test_non_change_release_gate_may_use_explicit_manual_sentinel(tmp_path: Path) -> None:
    violations = gate_schema_violations(
        _gate_lines(ci_job="manual", required_on="[release, manual]"),
        {"unit-gate"},
        _workflow(tmp_path),
    )

    assert violations == []


def test_ci_job_must_exist_and_execute_the_declared_target(tmp_path: Path) -> None:
    missing_job = gate_schema_violations(
        _gate_lines(ci_job="missing-job"),
        {"unit-gate"},
        _workflow(tmp_path),
    )
    wrong_command = gate_schema_violations(
        _gate_lines(),
        {"unit-gate"},
        _workflow(tmp_path, job_body="        run: make unit-gate-placeholder\n"),
    )

    assert any("is not a job" in item for item in missing_job)
    assert any("does not execute its make entrypoint 'unit-gate'" in item for item in wrong_command)


@pytest.mark.parametrize(
    "job_body",
    [
        "        run: |\n          # make unit-gate\n          make unrelated-gate\n",
        "        run: make unit-gate --dry-run\n",
        "        run: make unit-gate || true\n",
        "        run: make unit-gate -f /dev/null\n",
        "        run: make unit-gate | true\n",
        "        run: MAKEFLAGS=-n make unit-gate\n",
        "        run: >-\n          make unit-gate\n          --dry-run\n",
        "        run: |\n          make unit-gate \\\n            --dry-run\n",
        "        run: |\n          set +e\n          make unit-gate\n          true\n",
    ],
)
def test_ci_job_cannot_claim_a_nonexecuting_or_error_swallowing_make_target(
    tmp_path: Path,
    job_body: str,
) -> None:
    violations = gate_schema_violations(
        _gate_lines(),
        {"unit-gate"},
        _workflow(tmp_path, job_body=job_body),
    )

    assert any(
        "does not execute its make entrypoint 'unit-gate'" in item
        for item in violations
    )


def test_inline_run_step_executes_the_declared_target(tmp_path: Path) -> None:
    workflow = tmp_path / "ci.yml"
    workflow.write_text(
        "name: test\n"
        "jobs:\n"
        "  unit-job:\n"
        "    runs-on: ubuntu-latest\n"
        "    steps:\n"
        "      - run: make unit-gate\n",
        encoding="utf-8",
    )

    assert gate_schema_violations(
        _gate_lines(),
        {"unit-gate"},
        workflow,
    ) == []


def test_shell_gate_requires_every_command_in_the_ci_job(tmp_path: Path) -> None:
    lines = _gate_lines(
        entrypoint='shell: "pnpm type-check && pnpm test"',
    )
    missing_test = gate_schema_violations(
        lines,
        set(),
        _workflow(tmp_path, job_body="        run: pnpm type-check\n"),
    )

    assert any("does not execute its shell entrypoint" in item for item in missing_test)


@pytest.mark.parametrize(
    "job_body",
    [
        "        run: |\n          pnpm type-check\n          pnpm test || true\n",
        "        run: |\n          pnpm type-check\n          pnpm test-placeholder\n",
        "        run: |\n          pnpm type-check\n          # pnpm test\n",
    ],
)
def test_shell_gate_rejects_swallowed_suffixed_or_commented_commands(
    tmp_path: Path,
    job_body: str,
) -> None:
    violations = gate_schema_violations(
        _gate_lines(entrypoint='shell: "pnpm type-check && pnpm test"'),
        set(),
        _workflow(tmp_path, job_body=job_body),
    )

    assert any("does not execute its shell entrypoint" in item for item in violations)
