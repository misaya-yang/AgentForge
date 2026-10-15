#!/usr/bin/env python3
"""Resolve and validate ARC-06 compact/full/scale Compose modes."""

from __future__ import annotations

import argparse
import json
import sys
from copy import deepcopy
from pathlib import Path
from typing import Any

import yaml

ROOT = Path(__file__).resolve().parents[2]
MANIFEST = ROOT / "src/core/data/service_topology.json"
MODES = ("compact", "full", "scale")
SINGLETONS = ("gateway", "agent-runtime")
WORKERS = ("knowledge-worker", "agent-capability-worker")


class TopologyModeError(RuntimeError):
    pass


def load_manifest(path: Path = MANIFEST) -> dict[str, Any]:
    try:
        payload = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        raise TopologyModeError(f"cannot read topology manifest: {exc}") from exc
    if payload.get("schema") != "ai-gateway/service-topology/v1":
        raise TopologyModeError("unexpected topology schema")
    if payload.get("default_mode") != "full":
        raise TopologyModeError("full must remain the default until compact adoption passes")
    services = payload.get("services")
    if not isinstance(services, list) or not services:
        raise TopologyModeError("topology services must be a non-empty list")
    by_id: dict[str, dict[str, Any]] = {}
    for service in services:
        service_id = service.get("service_id") if isinstance(service, dict) else None
        if not isinstance(service_id, str) or service_id in by_id:
            raise TopologyModeError("topology service ids must be unique strings")
        by_id[service_id] = service
        resolutions = service.get("modes")
        if not isinstance(resolutions, dict) or set(resolutions) != set(MODES):
            raise TopologyModeError(f"{service_id} must resolve compact/full/scale")
        for mode, resolution in resolutions.items():
            if not isinstance(resolution, dict):
                raise TopologyModeError(f"{service_id}:{mode} resolution is malformed")
            present = resolution.get("present")
            replicas = resolution.get("replicas")
            if type(present) is not bool or type(replicas) is not int or replicas < 0:
                raise TopologyModeError(f"{service_id}:{mode} presence/replicas are invalid")
            if present != (replicas > 0):
                raise TopologyModeError(f"{service_id}:{mode} presence and replicas disagree")
    for service_id, service in by_id.items():
        dependencies = set(service.get("required_deps") or []) | set(
            service.get("optional_deps") or []
        )
        unknown = dependencies - set(by_id)
        if unknown:
            raise TopologyModeError(f"{service_id} has unknown dependencies: {sorted(unknown)}")
    for singleton in SINGLETONS:
        service = by_id.get(singleton)
        if service is None or service.get("scale_support") != "single-instance" or any(
            service["modes"][mode]["replicas"] != 1 for mode in MODES
        ):
            raise TopologyModeError(f"{singleton} must remain single-instance")
    if by_id.get("knowledge-service", {}).get("modes", {}).get("compact", {}).get(
        "runtime_role"
    ) != "all":
        raise TopologyModeError("compact mode requires Knowledge all role")
    if by_id.get("knowledge-worker", {}).get("modes", {}).get("compact", {}).get("present"):
        raise TopologyModeError("compact mode cannot start knowledge-worker")
    for worker in WORKERS:
        service = by_id.get(worker)
        if service is None or service.get("scale_support") != "worker":
            raise TopologyModeError(f"{worker} must declare the worker scale contract")
        if service["modes"]["scale"]["replicas"] < 2:
            raise TopologyModeError(f"scale mode requires two {worker} replicas")
    for job in ("gateway-init", "migrate"):
        if by_id.get(job, {}).get("lifecycle") != "one-shot":
            raise TopologyModeError(f"{job} must resolve as one-shot")
    return payload


def _read_compose(path: Path) -> dict[str, Any]:
    try:
        payload = yaml.safe_load(path.read_text(encoding="utf-8"))
    except (OSError, yaml.YAMLError) as exc:
        raise TopologyModeError(f"cannot parse {path.name}: {exc}") from exc
    if not isinstance(payload, dict) or not isinstance(payload.get("services"), dict):
        raise TopologyModeError(f"{path.name} must declare services")
    return payload


def _merge(base: dict[str, Any], override: dict[str, Any]) -> dict[str, Any]:
    merged = deepcopy(base)
    for key, value in override.items():
        if isinstance(value, dict) and isinstance(merged.get(key), dict):
            merged[key] = _merge(merged[key], value)
        else:
            merged[key] = deepcopy(value)
    return merged


def _replicas(service_id: str, service: dict[str, Any]) -> int:
    deploy = service.get("deploy") or {}
    value = deploy.get("replicas", 1) if isinstance(deploy, dict) else None
    if type(value) is not int or value < 1:
        raise TopologyModeError(f"{service_id} has an invalid Compose replica count")
    return value


def _validate_compose(
    mode: str,
    *,
    root: Path,
    manifest: dict[str, Any],
    files: list[str],
) -> None:
    base = _read_compose(root / files[0])["services"]
    overlay = _read_compose(root / files[1])["services"]
    unknown = set(overlay) - set(base)
    if unknown:
        raise TopologyModeError(f"{files[1]} declares unknown services: {sorted(unknown)}")
    by_id = {row["service_id"]: row for row in manifest["services"]}
    missing = set(base) - set(by_id)
    if missing:
        raise TopologyModeError(f"base Compose services missing from topology: {sorted(missing)}")
    merged = {
        service_id: _merge(value, overlay.get(service_id, {}))
        for service_id, value in base.items()
    }
    gateway_mode = (merged["gateway"].get("environment") or {}).get(
        "AI_PLATFORM_TOPOLOGY_MODE"
    )
    if gateway_mode != mode:
        raise TopologyModeError(f"{files[1]} does not stamp the Gateway topology mode")

    for service_id, contract in by_id.items():
        service = merged.get(service_id)
        resolution = contract["modes"][mode]
        if service is None:
            if resolution["present"] and contract["lifecycle"] != "external":
                raise TopologyModeError(f"{service_id}:{mode} is missing from Compose")
            continue
        profiles = service.get("profiles") or []
        if resolution["present"] and profiles:
            raise TopologyModeError(f"{service_id}:{mode} is active but hidden behind a profile")
        if not resolution["present"]:
            if not profiles:
                raise TopologyModeError(f"{service_id}:{mode} must be disabled by a profile")
            continue
        actual = _replicas(service_id, service)
        if actual != resolution["replicas"]:
            raise TopologyModeError(
                f"{service_id}:{mode} Compose replicas {actual} "
                f"do not match topology {resolution['replicas']}"
            )
        expected_role = resolution.get("runtime_role")
        environment = service.get("environment") or {}
        if expected_role is not None and (
            not isinstance(environment, dict)
            or environment.get("KNOWLEDGE_RUNTIME_ROLE") != expected_role
        ):
            raise TopologyModeError(f"{service_id}:{mode} requires runtime role {expected_role!r}")
        if actual > 1 and contract["scale_support"] == "worker" and service.get("container_name"):
            raise TopologyModeError(f"{service_id} cannot scale with a fixed container_name")
        if contract["lifecycle"] == "one-shot" and service.get("restart") != "no":
            raise TopologyModeError(f"{service_id} must use restart: no")


def resolve_mode(mode: str, *, root: Path = ROOT) -> dict[str, Any]:
    manifest = load_manifest(root / "src/core/data/service_topology.json")
    compose_modes = manifest.get("compose_modes") or {}
    if mode not in compose_modes:
        raise TopologyModeError(f"unknown topology mode {mode!r}")
    files = compose_modes[mode].get("files")
    if files != ["docker-compose.yml", f"docker-compose.{mode}.yml"]:
        raise TopologyModeError(f"topology mode {mode} must name base plus its overlay")
    for relative in files:
        if not (root / relative).is_file():
            raise TopologyModeError(f"topology mode {mode} file is missing: {relative}")
    _validate_compose(mode, root=root, manifest=manifest, files=files)

    services: list[dict[str, Any]] = []
    for service in manifest["services"]:
        resolution = service["modes"][mode]
        if resolution["present"]:
            services.append(
                {
                    "service_id": service["service_id"],
                    "bounded_context": service["bounded_context"],
                    "lifecycle": service["lifecycle"],
                    "scale_support": service["scale_support"],
                    "replicas": resolution["replicas"],
                    **(
                        {"runtime_role": resolution["runtime_role"]}
                        if "runtime_role" in resolution
                        else {}
                    ),
                }
            )
    return {
        "schema": "ai-gateway/topology-resolution/v1",
        "mode": mode,
        "topology_revision": manifest["revision"],
        "compose_files": files,
        "services": sorted(services, key=lambda row: row["service_id"]),
    }


def service_ids(mode: str, scope: str, *, root: Path = ROOT) -> list[str]:
    rows = resolve_mode(mode, root=root)["services"]
    infrastructure = {
        row["service_id"]
        for row in rows
        if row["bounded_context"] == "infrastructure" and row["lifecycle"] == "long-running"
    }
    if scope == "infrastructure":
        selected = infrastructure
    elif scope == "app":
        selected = {
            row["service_id"]
            for row in rows
            if row["service_id"] not in infrastructure and row["service_id"] != "gateway-init"
        }
    else:
        selected = {row["service_id"] for row in rows}
    return sorted(selected)


def service_replicas(mode: str, service_id: str, *, root: Path = ROOT) -> int:
    manifest = load_manifest(root / "src/core/data/service_topology.json")
    known = {row["service_id"] for row in manifest["services"]}
    if service_id not in known:
        raise TopologyModeError(f"unknown topology service {service_id!r}")
    rows = {row["service_id"]: row for row in resolve_mode(mode, root=root)["services"]}
    return int(rows.get(service_id, {}).get("replicas", 0))


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--mode", choices=MODES, default="full")
    parser.add_argument("--check-all", action="store_true")
    output = parser.add_mutually_exclusive_group()
    output.add_argument("--service-ids", choices=("all", "app", "infrastructure"))
    output.add_argument("--service-replicas")
    args = parser.parse_args(argv)
    try:
        if args.service_ids:
            print(" ".join(service_ids(args.mode, args.service_ids)))
            return 0
        if args.service_replicas:
            print(service_replicas(args.mode, args.service_replicas))
            return 0
        modes = MODES if args.check_all else (args.mode,)
        resolved = [resolve_mode(mode) for mode in modes]
    except TopologyModeError as exc:
        print(f"TOPOLOGY ERROR: {exc}", file=sys.stderr)
        return 2
    print(json.dumps(resolved if args.check_all else resolved[0], indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
