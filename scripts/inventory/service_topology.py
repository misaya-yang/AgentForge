"""Generate the service-topology inventory from its single manifest."""

from __future__ import annotations

import ast
import json
from pathlib import Path
from typing import Any

from _common import REPO_ROOT, base_envelope, sha256_file, walk_files

TOPOLOGY_MANIFEST = REPO_ROOT / "src/core/data/service_topology.json"
COMPOSE_FILES = (
    "docker-compose.yml",
    "docker-compose.compact.yml",
    "docker-compose.full.yml",
    "docker-compose.scale.yml",
    "docker-compose.dev.yml",
    "docker-compose.build.yml",
)
HTTP_METHODS = {"get", "post", "put", "delete", "patch", "options", "head", "websocket"}
REQUIRED_SERVICE_FIELDS = {
    "service_id",
    "display_name",
    "bounded_context",
    "process_role",
    "lifecycle",
    "exposure",
    "required_deps",
    "optional_deps",
    "state_owner",
    "scale_support",
    "health_contract",
    "image_artifact",
    "modes",
}


class TopologyManifestError(RuntimeError):
    """The topology manifest is incomplete or contradicts deployment facts."""


def parse_compose_services(path: Path) -> dict[str, dict[str, Any]]:
    services: dict[str, dict[str, Any]] = {}
    if not path.is_file():
        return services
    in_services = False
    current: str | None = None
    for raw in path.read_text(encoding="utf-8").splitlines():
        stripped = raw.strip()
        if not stripped or stripped.startswith("#"):
            continue
        indent = len(raw) - len(raw.lstrip())
        if indent == 0:
            in_services = stripped == "services:"
            current = None
            continue
        if in_services and indent == 2 and stripped.endswith(":"):
            current = stripped[:-1]
            services[current] = {"container_name": None}
            continue
        if current and indent > 2 and stripped.startswith("container_name:"):
            services[current]["container_name"] = stripped.split(":", 1)[1].strip()
    return services


def load_manifest(path: Path = TOPOLOGY_MANIFEST) -> dict[str, Any]:
    try:
        payload = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        raise TopologyManifestError(f"cannot read topology manifest {path}: {exc}") from exc
    if payload.get("schema") != "ai-gateway/service-topology/v1":
        raise TopologyManifestError("unexpected service-topology schema")
    if payload.get("default_mode") != "full":
        raise TopologyManifestError("full must remain the default until compact adoption evidence")
    groups = {row.get("id") for row in payload.get("groups", []) if isinstance(row, dict)}
    services = payload.get("services")
    if not isinstance(services, list) or not services:
        raise TopologyManifestError("service-topology services must be a non-empty list")
    seen: set[str] = set()
    for service in services:
        if not isinstance(service, dict) or set(service) != REQUIRED_SERVICE_FIELDS:
            raise TopologyManifestError("every topology service must declare the exact field contract")
        service_id = str(service["service_id"])
        if service_id in seen:
            raise TopologyManifestError(f"duplicate topology service {service_id}")
        seen.add(service_id)
        if service["bounded_context"] not in groups:
            raise TopologyManifestError(f"unknown bounded context for {service_id}")
        modes = service["modes"]
        if not isinstance(modes, dict) or set(modes) != {"compact", "full", "scale"}:
            raise TopologyManifestError(f"{service_id} must resolve compact/full/scale")
        for mode, resolution in modes.items():
            if not isinstance(resolution, dict) or not isinstance(resolution.get("present"), bool):
                raise TopologyManifestError(f"{service_id}:{mode} resolution is malformed")
            replicas = resolution.get("replicas")
            if not isinstance(replicas, int) or replicas < 0:
                raise TopologyManifestError(f"{service_id}:{mode} replicas is invalid")
            if resolution["present"] is not (replicas > 0):
                raise TopologyManifestError(f"{service_id}:{mode} presence and replicas disagree")
    by_id = {row["service_id"]: row for row in services}
    for singleton in ("gateway", "agent-runtime"):
        if by_id[singleton]["scale_support"] != "single-instance" or any(
            row["replicas"] != 1 for row in by_id[singleton]["modes"].values()
        ):
            raise TopologyManifestError(f"{singleton} must remain single-instance")
    if by_id["knowledge-service"]["modes"]["compact"].get("runtime_role") != "all":
        raise TopologyManifestError("compact Knowledge must use runtime role all")
    if by_id["knowledge-worker"]["modes"]["compact"]["present"]:
        raise TopologyManifestError("compact mode must not start knowledge-worker")
    for worker in ("knowledge-worker", "agent-capability-worker"):
        if by_id[worker]["modes"]["scale"]["replicas"] < 2:
            raise TopologyManifestError(f"scale mode must declare at least two {worker} replicas")
    for job in ("gateway-init", "migrate"):
        if by_id[job]["lifecycle"] != "one-shot":
            raise TopologyManifestError(f"{job} must be a one-shot job")
    return payload


def gateway_route_surface() -> dict[str, Any]:
    snapshot = REPO_ROOT / "sdk/openapi.json"
    if not snapshot.is_file():
        return {"available": False, "source": "sdk/openapi.json"}
    spec = json.loads(snapshot.read_text(encoding="utf-8"))
    paths = spec.get("paths", {})
    return {
        "available": True,
        "source": "sdk/openapi.json",
        "sha256": sha256_file(snapshot),
        "path_count": len(paths),
        "operation_count": sum(
            1
            for methods in paths.values()
            if isinstance(methods, dict)
            for method, operation in methods.items()
            if method in HTTP_METHODS and isinstance(operation, dict)
        ),
    }


def knowledge_route_surface() -> dict[str, Any]:
    routes = 0
    files: set[str] = set()
    for relative in walk_files(
        (".py",), roots=("apps/knowledge-service/src/knowledge_service/api",)
    ):
        path = REPO_ROOT / relative
        try:
            tree = ast.parse(path.read_text(encoding="utf-8"))
        except (OSError, SyntaxError, UnicodeDecodeError):
            continue
        found = 0
        for node in ast.walk(tree):
            if not isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
                continue
            found += sum(
                1
                for decorator in node.decorator_list
                if isinstance(decorator, ast.Call)
                and isinstance(decorator.func, ast.Attribute)
                and decorator.func.attr in HTTP_METHODS
            )
        if found:
            files.add(relative.as_posix())
            routes += found
    return {"route_file_count": len(files), "route_decorator_count": routes}


def build() -> dict[str, Any]:
    manifest = load_manifest()
    compose = {name: parse_compose_services(REPO_ROOT / name) for name in COMPOSE_FILES}
    base_services = set(compose["docker-compose.yml"])
    manifest_services = {row["service_id"] for row in manifest["services"]}
    missing = sorted(base_services - manifest_services)
    if missing:
        raise TopologyManifestError(f"base Compose services missing from topology: {missing}")
    helm_keys = {"gateway": "gateway", "agent-runtime": "agentRuntime"}
    services = []
    for source in manifest["services"]:
        scale_class = source["scale_support"]
        services.append(
            {
                **source,
                "scale_contract": {
                    "class": scale_class,
                    "max_replicas": 1 if scale_class == "single-instance" else None,
                    "hpa_allowed": scale_class in {"stateless", "worker"},
                    **(
                        {"helm_values_key": helm_keys[source["service_id"]]}
                        if source["service_id"] in helm_keys
                        else {}
                    ),
                },
            }
        )
    return {
        **base_envelope("service-topology"),
        "topology_schema": manifest["schema"],
        "topology_revision": manifest["revision"],
        "default_mode": manifest["default_mode"],
        "groups": manifest["groups"],
        "services": services,
        "compose_modes": manifest["compose_modes"],
        "compose_files": {
            name: {"exists": (REPO_ROOT / name).is_file(), "services": sorted(rows)}
            for name, rows in compose.items()
        },
        "gateway_routes": gateway_route_surface(),
        "knowledge_routes": knowledge_route_surface(),
        "manifest_sha256": sha256_file(TOPOLOGY_MANIFEST),
    }


if __name__ == "__main__":
    from _common import write_json

    output = write_json("service-topology.json", build())
    print(f"wrote {output}")
