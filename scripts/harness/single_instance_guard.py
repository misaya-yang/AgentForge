#!/usr/bin/env python3
"""Fail closed when Gateway or Agent Runtime can run concurrent owners.

The generated service-topology inventory is the machine-readable scale
authority.  This gate independently pins the two services that ARC-05/06 says
must remain single-instance, then checks the real Helm defaults, production
overrides, and templates.  Stateless APIs and durable workers may scale when
their topology record allows it.

Exit codes: 0 = contract intact, 1 = violations, 2 = gate error.
Evidence: tmp/gate-evidence/single-instance-guard.json
"""

from __future__ import annotations

import argparse
import importlib.util
import json
import re
import sys
import tempfile
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parents[2]
DEFAULT_EVIDENCE = ROOT / "tmp" / "gate-evidence" / "single-instance-guard.json"
DEFAULT_VALUES = Path("deploy/helm/ai-gateway/values.yaml")
PRODUCTION_VALUES = Path("deploy/helm/ai-gateway/values-production.yaml")
TEMPLATE_DIR = Path("deploy/helm/ai-gateway/templates")

# This independent pin prevents a topology+Helm change from blessing scale in
# the same diff.  Topology must agree; it cannot reclassify either service.
REQUIRED_SINGLE_INSTANCE = {
    "gateway": "gateway",
    "agent-runtime": "agentRuntime",
}
SCALABLE_CLASSES = {"stateless", "worker"}


class GuardInputError(RuntimeError):
    """The gate could not establish its inputs."""


def _parse_scalar(raw: str) -> Any:
    value = raw.strip()
    if value.lower() in {"true", "false"}:
        return value.lower() == "true"
    if re.fullmatch(r"-?[0-9]+", value):
        return int(value)
    if value.lower() in {"null", "~"}:
        return None
    return value.strip("\"'")


def yaml_scalar_paths(path: Path) -> dict[tuple[str, ...], Any]:
    """Read scalar paths from the repository's regular values files.

    This deliberately small reader keeps the L0 gate dependency-free.  It
    rejects tabs and duplicate scalar paths; lists and block scalars outside
    the scale fields are ignored.
    """

    if not path.is_file():
        raise GuardInputError(f"required Helm values file is missing: {path}")
    paths: dict[tuple[str, ...], Any] = {}
    stack: list[tuple[int, str]] = []
    for lineno, raw in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
        if "\t" in raw[: len(raw) - len(raw.lstrip())]:
            raise GuardInputError(f"{path}:{lineno}: tabs are not valid indentation")
        stripped = raw.strip()
        if not stripped or stripped.startswith(("#", "---")):
            continue
        match = re.match(r"^(?P<indent> *)(?P<key>[A-Za-z0-9_-]+):(?:\s*(?P<value>.*))?$", raw)
        if not match:
            continue
        indent = len(match.group("indent"))
        while stack and stack[-1][0] >= indent:
            stack.pop()
        key = match.group("key")
        value = (match.group("value") or "").split(" #", 1)[0].rstrip()
        current = (*[item[1] for item in stack], key)
        if value:
            if current in paths:
                raise GuardInputError(f"{path}:{lineno}: duplicate YAML scalar {'.'.join(current)}")
            paths[current] = _parse_scalar(value)
        else:
            stack.append((indent, key))
    return paths


def load_generated_topology(root: Path) -> dict[str, Any]:
    """Execute the current inventory generator instead of trusting a baseline."""

    inventory_dir = root / "scripts" / "inventory"
    generator = inventory_dir / "service_topology.py"
    if not generator.is_file():
        raise GuardInputError(f"service-topology generator is missing: {generator}")
    sys.path.insert(0, str(inventory_dir))
    try:
        spec = importlib.util.spec_from_file_location("_single_instance_topology", generator)
        if spec is None or spec.loader is None:
            raise GuardInputError("cannot load service-topology generator")
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        payload = module.build()
    except (OSError, RuntimeError, ValueError, json.JSONDecodeError) as exc:
        raise GuardInputError(f"cannot generate service topology: {exc}") from exc
    finally:
        sys.path.pop(0)
    if not isinstance(payload, dict):
        raise GuardInputError("service-topology generator returned a non-object")
    return payload


def topology_contracts(topology: dict[str, Any]) -> tuple[dict[str, dict], list[str]]:
    failures: list[str] = []
    raw_services = topology.get("services")
    if not isinstance(raw_services, list):
        return {}, ["generated service topology has no services list"]
    services: dict[str, dict] = {}
    for raw in raw_services:
        if not isinstance(raw, dict) or not isinstance(raw.get("service_id"), str):
            failures.append("generated service topology contains an invalid service record")
            continue
        service_id = raw["service_id"]
        if service_id in services:
            failures.append(f"generated service topology duplicates service_id {service_id!r}")
            continue
        services[service_id] = raw

    for service_id, helm_key in REQUIRED_SINGLE_INSTANCE.items():
        record = services.get(service_id)
        if record is None:
            failures.append(f"required single-instance service is absent: {service_id}")
            continue
        contract = record.get("scale_contract")
        if not isinstance(contract, dict):
            failures.append(f"{service_id}: scale_contract is missing")
            continue
        if contract.get("class") != "single-instance":
            failures.append(f"{service_id}: topology must classify it as single-instance")
        if contract.get("max_replicas") != 1:
            failures.append(f"{service_id}: topology max_replicas must be 1")
        if contract.get("hpa_allowed") is not False:
            failures.append(f"{service_id}: topology must forbid HPA")
        if contract.get("helm_values_key") != helm_key:
            failures.append(
                f"{service_id}: helm_values_key must be {helm_key!r}, "
                f"got {contract.get('helm_values_key')!r}"
            )

    for service_id, record in services.items():
        contract = record.get("scale_contract")
        if not isinstance(contract, dict):
            continue
        if contract.get("class") in SCALABLE_CLASSES and contract.get("hpa_allowed") is not True:
            failures.append(f"{service_id}: scalable class must explicitly allow HPA")
    return services, failures


def _component_present(values: dict[tuple[str, ...], Any], key: str) -> bool:
    return any(path and path[0] == key for path in values)


def validate_values(
    defaults: dict[tuple[str, ...], Any],
    production: dict[tuple[str, ...], Any],
) -> tuple[dict[str, Any], list[str]]:
    failures: list[str] = []
    facts: dict[str, Any] = {}
    effective_production = {**defaults, **production}
    for service_id, key in REQUIRED_SINGLE_INSTANCE.items():
        present = _component_present(defaults, key)
        facts[service_id] = {"helm_values_key": key, "present": present}
        if service_id == "gateway" and not present:
            failures.append("gateway: Helm defaults block is missing")
        if not present:
            continue  # Agent Runtime has no Helm workload yet; future additions are checked.
        for label, values in (("defaults", defaults), ("production", effective_production)):
            observed = {
                "replicaCount": values.get((key, "replicaCount")),
                "autoscaling.enabled": values.get((key, "autoscaling", "enabled")),
                "autoscaling.minReplicas": values.get((key, "autoscaling", "minReplicas")),
                "autoscaling.maxReplicas": values.get((key, "autoscaling", "maxReplicas")),
            }
            facts[service_id][label] = observed
            if observed["replicaCount"] != 1:
                failures.append(
                    f"{service_id}: {label} replicaCount must be 1, "
                    f"got {observed['replicaCount']!r}"
                )
            if observed["autoscaling.enabled"] is not False:
                failures.append(f"{service_id}: {label} must set autoscaling.enabled=false")
            for field in ("autoscaling.minReplicas", "autoscaling.maxReplicas"):
                if observed[field] != 1:
                    failures.append(
                        f"{service_id}: {label} {field} must be 1, got {observed[field]!r}"
                    )
    return facts, failures


def _mentions_service(document: str, service_id: str) -> bool:
    lowered = document.lower()
    if re.search(
        rf"(?m)^\s*app\.kubernetes\.io/component:\s*{re.escape(service_id)}\s*$",
        lowered,
    ):
        return True
    # Metadata names may include the shared chart fullname (which itself
    # contains "ai-gateway"). Match only the final component suffix so a
    # frontend/knowledge workload is not mistaken for the Gateway.
    return (
        re.search(
            rf"(?m)^\s*name:\s*[^\n]*-{re.escape(service_id)}\s*$",
            lowered,
        )
        is not None
    )


def validate_templates(root: Path, services: dict[str, dict]) -> tuple[list[str], list[str]]:
    template_root = root / TEMPLATE_DIR
    if not template_root.is_dir():
        return [], [f"Helm template directory is missing: {template_root}"]
    paths = sorted((*template_root.glob("*.yaml"), *template_root.glob("*.yml")))
    scanned = [str(path.relative_to(root)) for path in paths]
    failures: list[str] = []
    deployment_counts = dict.fromkeys(REQUIRED_SINGLE_INSTANCE, 0)

    for path in paths:
        text = path.read_text(encoding="utf-8")
        rel = path.relative_to(root)
        for key in REQUIRED_SINGLE_INSTANCE.values():
            if f".Values.{key}.replicaCount" in text:
                failures.append(f"{rel}: template must not honor {key}.replicaCount")
            if f".Values.{key}.autoscaling" in text:
                failures.append(f"{rel}: template must not honor {key}.autoscaling")

        for document in re.split(r"(?m)^---\s*$", text):
            kind_match = re.search(r"(?m)^kind:\s*([A-Za-z0-9]+)\s*$", document)
            if kind_match is None:
                continue
            kind = kind_match.group(1)
            for service_id in REQUIRED_SINGLE_INSTANCE:
                if not _mentions_service(document, service_id):
                    continue
                if kind == "HorizontalPodAutoscaler":
                    failures.append(f"{rel}: {service_id} must not render an HPA")
                if kind != "Deployment":
                    continue
                deployment_counts[service_id] += 1
                replicas = re.findall(r"(?m)^\s*replicas:\s*([^#\n]+?)\s*$", document)
                if replicas != ["1"]:
                    failures.append(
                        f"{rel}: {service_id} Deployment must declare exactly literal replicas: 1"
                    )
                if re.search(r"(?ms)^\s*strategy:\s*$.*?^\s*type:\s*Recreate\s*$", document) is None:
                    failures.append(
                        f"{rel}: {service_id} Deployment must use Recreate to prevent rollout overlap"
                    )

    for service_id, key in REQUIRED_SINGLE_INSTANCE.items():
        contract = (services.get(service_id) or {}).get("scale_contract") or {}
        if contract.get("helm_values_key") != key:
            continue
        if service_id == "gateway" and deployment_counts[service_id] != 1:
            failures.append(
                f"gateway: expected exactly one protected Helm Deployment, "
                f"found {deployment_counts[service_id]}"
            )
    return scanned, failures


def evaluate_contract(root: Path, topology: dict[str, Any]) -> dict[str, Any]:
    services, topology_failures = topology_contracts(topology)
    defaults = yaml_scalar_paths(root / DEFAULT_VALUES)
    production = yaml_scalar_paths(root / PRODUCTION_VALUES)
    values_facts, values_failures = validate_values(defaults, production)
    templates, template_failures = validate_templates(root, services)
    failures = [*topology_failures, *values_failures, *template_failures]
    return {
        "gate": "single-instance-guard",
        "tier": "L0-static",
        "protected_services": sorted(REQUIRED_SINGLE_INSTANCE),
        "values": values_facts,
        "templates_scanned": templates,
        "failures": failures,
        "result": "pass" if not failures else "fail",
    }


def run(root: Path, evidence_path: Path) -> int:
    try:
        result = evaluate_contract(root, load_generated_topology(root))
    except (GuardInputError, OSError, ValueError) as exc:
        print(f"GATE ERROR: {exc}", file=sys.stderr)
        return 2
    evidence_path.parent.mkdir(parents=True, exist_ok=True)
    evidence_path.write_text(json.dumps(result, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    if result["failures"]:
        for failure in result["failures"]:
            print(f"FAIL: {failure}", file=sys.stderr)
        return 1
    print(
        "OK: Gateway and Agent Runtime remain fail-closed single-instance; "
        f"evidence={evidence_path.relative_to(root)}"
    )
    return 0


def _selftest() -> int:
    topology = {
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
                "scale_contract": {"class": "stateless", "hpa_allowed": True},
            },
            {
                "service_id": "agent-capability-worker",
                "scale_contract": {"class": "worker", "hpa_allowed": True},
            },
        ]
    }
    failures = 0
    with tempfile.TemporaryDirectory() as temporary:
        root = Path(temporary)
        values = root / DEFAULT_VALUES
        production = root / PRODUCTION_VALUES
        templates = root / TEMPLATE_DIR
        values.parent.mkdir(parents=True)
        templates.mkdir(parents=True)
        values.write_text(
            "gateway:\n  replicaCount: 1\n  autoscaling:\n    enabled: false\n"
            "    minReplicas: 1\n    maxReplicas: 1\n"
            "frontend:\n  replicaCount: 4\n  autoscaling:\n    enabled: true\n",
            encoding="utf-8",
        )
        production.write_text("gateway:\n  replicaCount: 1\n", encoding="utf-8")
        deployment = templates / "gateway.yaml"
        deployment.write_text(
            "apiVersion: apps/v1\nkind: Deployment\nmetadata:\n  name: x-gateway\n"
            "spec:\n  replicas: 1\n  strategy:\n    type: Recreate\n",
            encoding="utf-8",
        )
        (templates / "frontend-hpa.yaml").write_text(
            "kind: HorizontalPodAutoscaler\nmetadata:\n  name: x-frontend\n"
            "spec:\n  maxReplicas: {{ .Values.frontend.autoscaling.maxReplicas }}\n",
            encoding="utf-8",
        )
        clean = evaluate_contract(root, topology)
        if clean["result"] != "pass":
            failures += 1
            print(f"[FAIL] scalable services were rejected: {clean['failures']}")
        else:
            print("[ok] safe singleton plus scalable stateless/worker topology passes")

        unsafe = values.read_text(encoding="utf-8").replace("replicaCount: 1", "replicaCount: 2", 1)
        values.write_text(unsafe, encoding="utf-8")
        result = evaluate_contract(root, topology)
        if not any("replicaCount must be 1" in item for item in result["failures"]):
            failures += 1
            print("[FAIL] replicaCount=2 was not rejected")
        else:
            print("[ok] replicaCount=2 is rejected")

        values.write_text(unsafe.replace("replicaCount: 2", "replicaCount: 1", 1), encoding="utf-8")
        self_blessed = json.loads(json.dumps(topology))
        self_blessed["services"][0]["scale_contract"]["class"] = "stateless"
        result = evaluate_contract(root, self_blessed)
        if not any("must classify it as single-instance" in item for item in result["failures"]):
            failures += 1
            print("[FAIL] topology self-bless was not rejected")
        else:
            print("[ok] topology cannot self-bless Gateway scale")

        deployment.write_text(
            "kind: Deployment\nmetadata:\n  name: x-gateway\n"
            "spec:\n  replicas: {{ .Values.gateway.replicaCount }}\n"
            "  strategy:\n    type: RollingUpdate\n---\n"
            "kind: HorizontalPodAutoscaler\nmetadata:\n  name: x-gateway\n",
            encoding="utf-8",
        )
        result = evaluate_contract(root, topology)
        expected = ("must not honor gateway.replicaCount", "must not render an HPA", "must use Recreate")
        if not all(any(fragment in item for item in result["failures"]) for fragment in expected):
            failures += 1
            print(f"[FAIL] unsafe templates were not fully rejected: {result['failures']}")
        else:
            print("[ok] values-driven replicas, HPA, and rollout overlap are rejected")

    if failures:
        print(f"SELFTEST FAILED: {failures} case(s)", file=sys.stderr)
        return 1
    print("SELFTEST OK: single-instance guard negative cases detected")
    return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=ROOT)
    parser.add_argument("--evidence-out", type=Path, default=DEFAULT_EVIDENCE)
    parser.add_argument("--selftest", action="store_true")
    args = parser.parse_args(argv)
    if args.selftest:
        return _selftest()
    return run(args.root.resolve(), args.evidence_out.resolve())


if __name__ == "__main__":
    raise SystemExit(main())
