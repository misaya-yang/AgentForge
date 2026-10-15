from __future__ import annotations

import copy
from pathlib import Path

import pytest

from scripts.harness.single_instance_guard import (
    DEFAULT_VALUES,
    PRODUCTION_VALUES,
    TEMPLATE_DIR,
    GuardInputError,
    evaluate_contract,
    load_generated_topology,
    topology_contracts,
    yaml_scalar_paths,
)


def _topology() -> dict:
    return {
        "services": [
            {
                "service_id": "gateway",
                "scale_contract": {
                    "class": "single-instance",
                    "max_replicas": 1,
                    "hpa_allowed": False,
                    "helm_values_key": "gateway",
                },
            },
            {
                "service_id": "agent-runtime",
                "scale_contract": {
                    "class": "single-instance",
                    "max_replicas": 1,
                    "hpa_allowed": False,
                    "helm_values_key": "agentRuntime",
                },
            },
            {
                "service_id": "frontend",
                "scale_contract": {
                    "class": "stateless",
                    "max_replicas": None,
                    "hpa_allowed": True,
                    "helm_values_key": "frontend",
                },
            },
            {
                "service_id": "agent-capability-worker",
                "scale_contract": {
                    "class": "worker",
                    "max_replicas": None,
                    "hpa_allowed": True,
                    "helm_values_key": "agentCapabilityWorker",
                },
            },
        ]
    }


def _write_contract(root: Path) -> None:
    defaults = root / DEFAULT_VALUES
    production = root / PRODUCTION_VALUES
    templates = root / TEMPLATE_DIR
    defaults.parent.mkdir(parents=True)
    templates.mkdir(parents=True)
    defaults.write_text(
        "gateway:\n"
        "  replicaCount: 1\n"
        "  autoscaling:\n"
        "    enabled: false\n"
        "    minReplicas: 1\n"
        "    maxReplicas: 1\n"
        "frontend:\n"
        "  replicaCount: 4\n"
        "  autoscaling:\n"
        "    enabled: true\n"
        "    minReplicas: 2\n"
        "    maxReplicas: 10\n",
        encoding="utf-8",
    )
    production.write_text(
        "gateway:\n"
        "  replicaCount: 1\n"
        "  autoscaling:\n"
        "    enabled: false\n"
        "    minReplicas: 1\n"
        "    maxReplicas: 1\n"
        "frontend:\n"
        "  replicaCount: 8\n",
        encoding="utf-8",
    )
    (templates / "gateway-deployment.yaml").write_text(
        "apiVersion: apps/v1\n"
        "kind: Deployment\n"
        "metadata:\n"
        "  name: release-gateway\n"
        "spec:\n"
        "  replicas: 1\n"
        "  strategy:\n"
        "    type: Recreate\n",
        encoding="utf-8",
    )
    (templates / "frontend-hpa.yaml").write_text(
        "apiVersion: autoscaling/v2\n"
        "kind: HorizontalPodAutoscaler\n"
        "metadata:\n"
        "  name: release-frontend\n"
        "spec:\n"
        "  maxReplicas: {{ .Values.frontend.autoscaling.maxReplicas }}\n",
        encoding="utf-8",
    )


def test_safe_singletons_allow_manifest_scalable_services(tmp_path: Path) -> None:
    _write_contract(tmp_path)

    result = evaluate_contract(tmp_path, _topology())

    assert result["result"] == "pass"
    assert result["failures"] == []
    assert result["values"]["gateway"]["production"]["replicaCount"] == 1
    assert result["values"]["agent-runtime"] == {
        "helm_values_key": "agentRuntime",
        "present": False,
    }


@pytest.mark.parametrize(
    ("file_name", "old", "new", "expected"),
    [
        ("defaults", "replicaCount: 1", "replicaCount: 2", "defaults replicaCount"),
        ("defaults", "enabled: false", "enabled: true", "defaults must set autoscaling"),
        ("production", "maxReplicas: 1", "maxReplicas: 3", "production autoscaling.maxReplicas"),
    ],
)
def test_values_scale_attempts_fail_closed(
    tmp_path: Path,
    file_name: str,
    old: str,
    new: str,
    expected: str,
) -> None:
    _write_contract(tmp_path)
    path = tmp_path / (DEFAULT_VALUES if file_name == "defaults" else PRODUCTION_VALUES)
    path.write_text(path.read_text(encoding="utf-8").replace(old, new, 1), encoding="utf-8")

    result = evaluate_contract(tmp_path, _topology())

    assert result["result"] == "fail"
    assert any(expected in failure for failure in result["failures"])


@pytest.mark.parametrize(
    ("field", "value", "expected"),
    [
        ("class", "stateless", "must classify it as single-instance"),
        ("max_replicas", 2, "max_replicas must be 1"),
        ("hpa_allowed", True, "must forbid HPA"),
        ("helm_values_key", "frontend", "helm_values_key must be 'gateway'"),
    ],
)
def test_topology_cannot_self_bless_gateway_scale(
    field: str,
    value: object,
    expected: str,
) -> None:
    topology = copy.deepcopy(_topology())
    topology["services"][0]["scale_contract"][field] = value

    _services, failures = topology_contracts(topology)

    assert any(expected in failure for failure in failures)


def test_template_hpa_values_replica_and_rollout_overlap_are_rejected(tmp_path: Path) -> None:
    _write_contract(tmp_path)
    template = tmp_path / TEMPLATE_DIR / "gateway-deployment.yaml"
    template.write_text(
        "kind: Deployment\n"
        "metadata:\n"
        "  name: release-gateway\n"
        "spec:\n"
        "  replicas: {{ .Values.gateway.replicaCount }}\n"
        "  strategy:\n"
        "    type: RollingUpdate\n"
        "---\n"
        "kind: HorizontalPodAutoscaler\n"
        "metadata:\n"
        "  name: release-gateway\n",
        encoding="utf-8",
    )

    result = evaluate_contract(tmp_path, _topology())

    assert any("must not honor gateway.replicaCount" in item for item in result["failures"])
    assert any("must not render an HPA" in item for item in result["failures"])
    assert any("must use Recreate" in item for item in result["failures"])


def test_yaml_reader_rejects_duplicate_scale_scalar(tmp_path: Path) -> None:
    values = tmp_path / "values.yaml"
    values.write_text("gateway:\n  replicaCount: 1\n  replicaCount: 2\n", encoding="utf-8")

    with pytest.raises(GuardInputError, match="duplicate YAML scalar gateway.replicaCount"):
        yaml_scalar_paths(values)


def test_repository_generated_topology_has_pinned_singletons() -> None:
    root = Path(__file__).resolve().parents[2]

    services, failures = topology_contracts(load_generated_topology(root))

    assert failures == []
    assert services["gateway"]["scale_contract"]["class"] == "single-instance"
    assert services["agent-runtime"]["scale_contract"]["max_replicas"] == 1
