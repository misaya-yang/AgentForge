from __future__ import annotations

import json
import sys
from pathlib import Path

import pytest

from scripts.release import integration_gates


def _spec(command: list[str], *, required_env: list[str] | None = None) -> dict:
    return {
        "schema_version": integration_gates.SCHEMA,
        "gates": {
            "test": {
                "tier": "L2",
                "required_env": required_env or [],
                "steps": [{"id": "real-command", "command": command}],
            }
        },
    }


def _fresh_spec(command: list[str]) -> dict:
    required = ["FRESH_ADMIN_DSN", "FRESH_MIGRATOR_DSN"]
    return {
        "schema_version": integration_gates.SCHEMA,
        "gates": {
            "fresh": {
                "tier": "L3",
                "required_env": required,
                "fresh_database": {
                    "admin_dsn_env": required[0],
                    "migrator_dsn_env": required[1],
                },
                "steps": [{"id": "fresh-check", "command": command}],
            }
        },
    }


def test_missing_prerequisite_is_blocked_and_command_does_not_run(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    marker = tmp_path / "must-not-run"
    monkeypatch.delenv("REQUIRED_TEST_ENV", raising=False)
    receipt = tmp_path / "blocked.json"
    spec = _spec(
        [sys.executable, "-c", f"from pathlib import Path; Path({str(marker)!r}).touch()"],
        required_env=["REQUIRED_TEST_ENV"],
    )

    assert (
        integration_gates.run_gate("test", spec, root=tmp_path, dry_run=False, receipt_path=receipt)
        == 2
    )
    assert not marker.exists()
    assert json.loads(receipt.read_text())["result"] == "blocked"


def test_dry_run_is_never_recorded_as_pass(tmp_path: Path) -> None:
    receipt = tmp_path / "dry-run.json"

    assert (
        integration_gates.run_gate(
            "test",
            _spec([sys.executable, "-c", "raise SystemExit(99)"]),
            root=tmp_path,
            dry_run=True,
            receipt_path=receipt,
        )
        == 0
    )
    assert json.loads(receipt.read_text())["result"] == "dry-run"


def test_real_command_passes_and_skip_marker_fails_closed(tmp_path: Path) -> None:
    passed = tmp_path / "passed.json"
    skipped = tmp_path / "skipped.json"

    assert (
        integration_gates.run_gate(
            "test",
            _spec([sys.executable, "-c", "print('executed')"]),
            root=tmp_path,
            dry_run=False,
            receipt_path=passed,
        )
        == 0
    )
    assert json.loads(passed.read_text())["result"] == "pass"
    assert (
        integration_gates.run_gate(
            "test",
            _spec([sys.executable, "-c", "print('SKIPPED unavailable')"]),
            root=tmp_path,
            dry_run=False,
            receipt_path=skipped,
        )
        == 1
    )
    payload = json.loads(skipped.read_text())
    assert payload["result"] == "blocked"
    assert payload["unexpected_skips"] == 1


def test_checked_in_gate_manifest_has_real_commands_and_no_cycles() -> None:
    spec = integration_gates.load_spec(integration_gates.DEFAULT_SPEC)

    assert set(spec["gates"]) == {
        "platform-db",
        "agent-execution",
        "knowledge",
        "fresh-install",
        "rollback",
        "version-agreement",
        "all",
    }
    assert all(gate["steps"] for gate in spec["gates"].values())
    fresh = spec["gates"]["fresh-install"]
    assert fresh["fresh_database"] == {
        "admin_dsn_env": "AI_PLATFORM_FRESH_DATABASE_ADMIN_DSN",
        "migrator_dsn_env": "AI_PLATFORM_FRESH_DATABASE_MIGRATOR_DSN",
    }
    assert fresh["external_receipt"] == {
        "env": "AI_PLATFORM_FRESH_RUNTIME_RECEIPT",
        "gate": "fresh-runtime",
        "commands": [
            ["make", "quickstart"],
            ["make", "validate"],
            ["make", "status"],
            ["python3", "-m", "scripts.release.version_agreement_gate"],
        ],
    }
    commands = [step["command"] for step in fresh["steps"]]
    assert ["make", "quickstart"] not in commands
    assert [command[-1] for command in commands] == ["init-fresh", "verify", "fingerprint"]


def test_fresh_contract_requires_explicit_dsn_environment(tmp_path: Path) -> None:
    spec = _fresh_spec([sys.executable, "-c", "pass"])
    spec["gates"]["fresh"]["required_env"].remove("FRESH_ADMIN_DSN")
    path = tmp_path / "bad-fresh.json"
    path.write_text(json.dumps(spec), encoding="utf-8")

    with pytest.raises(
        integration_gates.IntegrationGateError,
        match="fresh database DSNs must be explicit required environment names",
    ):
        integration_gates.load_spec(path)


def test_default_receipt_path_is_portable_and_candidate_bound(tmp_path: Path) -> None:
    source_sha = "a" * 40

    path = integration_gates._default_receipt_path(tmp_path, "fresh-install", source_sha)

    assert path.relative_to(tmp_path).as_posix() == (
        f"reports/release/integration-gates/{source_sha}/fresh-install.json"
    )
    with pytest.raises(integration_gates.IntegrationGateError, match="candidate Git SHA"):
        integration_gates._default_receipt_path(tmp_path, "fresh-install", "not-a-sha")


def test_fresh_database_lifecycle_rewrites_dsn_and_cleans_up_after_failure(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    source_sha = "b" * 40
    receipt = tmp_path / "fresh.json"
    secret = "must-not-enter-receipt"
    monkeypatch.setenv("FRESH_ADMIN_DSN", f"postgresql://admin:{secret}@db/postgres")
    monkeypatch.setenv("FRESH_MIGRATOR_DSN", f"postgresql://migrator:{secret}@db/gateway")
    actions: list[tuple[str, str]] = []

    def fake_control(action: str, **kwargs: object) -> tuple[int, str]:
        actions.append((action, str(kwargs["database_name"])))
        return 0, f"{action} complete"

    monkeypatch.setattr(integration_gates, "_fresh_control", fake_control)
    command = [
        sys.executable,
        "-c",
        (
            "import os; "
            "dsn=os.environ['AI_GATEWAY_DATABASE_MIGRATOR_DSN']; "
            "assert '/arc08_fresh_' in dsn and not dsn.endswith('/gateway'); "
            "assert os.environ['DATABASE_URL'] == dsn; "
            "raise SystemExit(1)"
        ),
    ]

    assert (
        integration_gates.run_gate(
            "fresh",
            _fresh_spec(command),
            root=tmp_path,
            dry_run=False,
            receipt_path=receipt,
            identity=("release-test", source_sha),
        )
        == 1
    )

    payload = json.loads(receipt.read_text(encoding="utf-8"))
    assert payload["result"] == "fail"
    assert payload["source_git_sha"] == source_sha
    assert payload["fresh_environment"]["cleanup_result"] == "pass"
    assert [record["id"] for record in payload["steps"]] == [
        "fresh-database-create",
        "fresh-check",
        "fresh-database-drop",
    ]
    assert actions[0][0] == "create" and actions[-1][0] == "drop"
    assert actions[0][1] == actions[-1][1]
    assert secret not in receipt.read_text(encoding="utf-8")


def test_fresh_database_cleanup_failure_blocks_an_otherwise_passing_gate(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    source_sha = "c" * 40
    receipt = tmp_path / "cleanup-blocked.json"
    monkeypatch.setenv("FRESH_ADMIN_DSN", "postgresql://admin:secret@db/postgres")
    monkeypatch.setenv("FRESH_MIGRATOR_DSN", "postgresql://migrator:secret@db/gateway")

    def fake_control(action: str, **_kwargs: object) -> tuple[int, str]:
        return (2, "cleanup unavailable") if action == "drop" else (0, "created")

    monkeypatch.setattr(integration_gates, "_fresh_control", fake_control)

    assert (
        integration_gates.run_gate(
            "fresh",
            _fresh_spec([sys.executable, "-c", "pass"]),
            root=tmp_path,
            dry_run=False,
            receipt_path=receipt,
            identity=("release-test", source_sha),
        )
        == 2
    )
    payload = json.loads(receipt.read_text(encoding="utf-8"))
    assert payload["result"] == "blocked"
    assert payload["fresh_environment"]["cleanup_result"] == "blocked"


def test_fresh_database_target_is_generated_and_never_the_main_database() -> None:
    source_sha = "d" * 40
    name = integration_gates._fresh_database_name(source_sha)

    assert name.startswith(f"{integration_gates.FRESH_DATABASE_PREFIX}{source_sha[:12]}_")
    assert integration_gates._dsn_for_database(
        "postgresql://migrator:secret@127.0.0.1:5432/gateway?sslmode=disable",
        name,
    ).endswith(f"/{name}?sslmode=disable")
    with pytest.raises(integration_gates.IntegrationGateError, match="non-generated"):
        integration_gates._validate_fresh_database_name("gateway")


def test_external_fresh_runtime_receipt_is_candidate_bound_and_complete(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    source_sha = "e" * 40
    release_id = "platform-test-release"
    contract = {
        "env": "FRESH_RUNTIME_RECEIPT",
        "gate": "fresh-runtime",
        "commands": [["make", "quickstart"], ["make", "validate"], ["make", "status"]],
    }
    path = tmp_path / "fresh-runtime.json"
    monkeypatch.setenv(contract["env"], str(path))
    payload = {
        "schema_version": "ai-platform/integration-gate-receipt/v1",
        "gate": contract["gate"],
        "release_id": release_id,
        "source_git_sha": source_sha,
        "result": "pass",
        "unexpected_skips": 0,
        "steps": [
            {
                "command": contract["commands"][0],
                "exit_code": 0,
                "skip_markers": 0,
                "output_sha256": "1" * 64,
            }
        ],
    }
    path.write_text(json.dumps(payload), encoding="utf-8")

    with pytest.raises(
        integration_gates.IntegrationGateError,
        match="omits required quickstart checks",
    ):
        integration_gates._verify_external_receipt(
            contract,
            release_id=release_id,
            source_sha=source_sha,
        )

    payload["steps"] = [
        {
            "command": command,
            "exit_code": 0,
            "skip_markers": 0,
            "output_sha256": "2" * 64,
        }
        for command in contract["commands"]
    ]
    path.write_text(json.dumps(payload), encoding="utf-8")

    digest = integration_gates._verify_external_receipt(
        contract,
        release_id=release_id,
        source_sha=source_sha,
    )
    assert len(digest) == 64
