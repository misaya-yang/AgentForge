"""Compose owning Python declarations beneath native Knowledge wildcard contracts.

No SDK publication is a producer input. Concrete owner routes are supporting
metadata, never new top-level published operations or complete response claims.
"""

from __future__ import annotations

import ast
import hashlib
import importlib.metadata
import inspect
import json
import os
import subprocess
import sys
from pathlib import Path
from urllib.parse import urlparse

ROOT = Path(__file__).resolve().parents[2]
NATIVE_ROOT = "rust/platform-native/crates/platform-gateway/src/"
NATIVE_SOURCES = {
    "migration_host/knowledge_proxy/contract.rs",
    "migration_host/knowledge_proxy/mod.rs",
    "migration_host/knowledge_proxy/forwarding.rs",
    "migration_host/knowledge_proxy/management.rs",
    "migration_host/knowledge_proxy/impact.sql",
    "migration_host/application.rs",
    "migration_host/foundation.rs",
    "migration_content/access.rs",
}
OWNER_ROOT = "apps/knowledge-service/src"
CORE_ROOT = "packages/ai-gateway-core/src"
CONTRACTS_ROOT = "packages/ai-gateway-contracts/src"
METHODS = {"get", "post", "put", "patch", "delete"}
PREFIXES = ("/api/v1/knowledge", "/v1/knowledge")
UPSTREAM = "/api/v1/knowledge"
SCHEMA_PREFIX = "KnowledgeOwner__"
# The private provenance iterator is reviewed against this exact locked source.
# Schema generation still uses public get_openapi on the untouched include tree.
FASTAPI_ROUTE_DISCOVERY = {
    "package": "fastapi", "version": "0.137.1",
    "source_sha256": {
        "fastapi/routing.py": "4e6f0dad3cb6a5e1c8d5c71802ea7d4ef7ed81a161c845f8c0c7cf05b391e5cb",
        "fastapi/openapi/utils.py": "6c8b9a55e968c38240de845626a406c548e9c7e2cca495c69e9a1571ced3576d",
    },
}


def digest(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def _composition_digest(document: dict) -> str:
    value = {
        "operations": document["x-python-knowledge-owner"]["forwarded_operations"],
        "export_dependencies": document["x-python-knowledge-owner"]["export_dependencies"],
        "schemas": {key: schema for key, schema in document["components"]["schemas"].items()
                    if key.startswith(SCHEMA_PREFIX)},
    }
    return hashlib.sha256(json.dumps(value, sort_keys=True, separators=(",", ":")).encode()).hexdigest()


def tokenizer_declaration(source: str) -> tuple[str, str]:
    """Read, but never execute, the tokenizer asset declaration in its wheel."""
    function = next(node for node in ast.parse(source).body
                    if isinstance(node, ast.FunctionDef) and node.name == "cl100k_base")
    loads = [node for node in ast.walk(function) if isinstance(node, ast.Call)
             and isinstance(node.func, ast.Name) and node.func.id == "load_tiktoken_bpe"]
    if len(loads) != 1 or len(loads[0].args) != 1:
        raise ValueError("unsupported tokenizer asset declaration")
    url = ast.literal_eval(loads[0].args[0])
    expected = ast.literal_eval(next(k.value for k in loads[0].keywords if k.arg == "expected_hash"))
    if (urlparse(url).scheme != "https" or urlparse(url).netloc != "openaipublic.blob.core.windows.net"
            or urlparse(url).path != "/encodings/cl100k_base.tiktoken"
            or len(expected) != 64 or any(c not in "0123456789abcdef" for c in expected)):
        raise ValueError("unreviewed tokenizer asset identity")
    return url, expected


def tokenizer_input(cache: Path) -> dict:
    """Verify the offline asset against the exact installed tokenizer declaration."""
    distribution = importlib.metadata.distribution("tiktoken")
    source = Path(distribution.locate_file("tiktoken_ext/openai_public.py"))
    url, expected = tokenizer_declaration(source.read_text())
    asset = cache / hashlib.sha1(url.encode()).hexdigest()
    if (not asset.is_file() or asset.is_symlink() or asset.stat().st_nlink != 1
            or not 0 < asset.stat().st_size <= 4 * 1024 * 1024
            or digest(asset) != expected):
        raise ValueError("missing or corrupt offline Knowledge tokenizer asset; prepare the verified cache before export")
    return {"package": "tiktoken", "version": distribution.version,
            "source_sha256": digest(source), "asset_sha256": expected,
            "asset_bytes": asset.stat().st_size, "url": url}


def verify_native_forwarding(document: dict, root: Path) -> set[str]:
    marker = document.get("x-native-knowledge-forwarding", {})
    if (marker.get("version") != 1 or marker.get("complete") is not False
            or marker.get("runtime_verified") is not False
            or marker.get("public_prefixes") != list(PREFIXES)
            or marker.get("upstream_prefix") != UPSTREAM
            or set(marker.get("methods", [])) != {m.upper() for m in METHODS}
            or marker.get("response_mode") != "streamed-status-content-type-body"):
        raise ValueError("missing or changed native Knowledge forwarding declaration")
    if set(marker.get("source_sha256", {})) != NATIVE_SOURCES:
        raise ValueError("missing native Knowledge compiled-source bindings")
    for relative, expected in marker["source_sha256"].items():
        if digest(root / NATIVE_ROOT / relative) != expected:
            raise ValueError(f"native Knowledge producer/source mismatch: {relative}")
    expected_transforms = {
        ("DELETE", "datasets/{dataset_id}", "account-password-confirmation"),
        ("GET", "datasets/{dataset_id}/impact", "gateway-owned"),
    }
    actual_transforms = {(v.get("method"), v.get("suffix"), v.get("kind"))
                         for v in marker.get("transforms", [])}
    if actual_transforms != expected_transforms:
        raise ValueError("native Knowledge transform ownership changed")
    operations = set()
    for prefix in PREFIXES:
        path = prefix + "/{path}"
        for method in METHODS:
            op = document.get("paths", {}).get(path, {}).get(method, {})
            if (op.get("x-native-knowledge-proxy") is not True
                    or op.get("x-native-response-schema") != "streamed-owner-declared-or-opaque"
                    or op.get("security") != [{}, {"NativeBearerAuth": []},
                                               {"NativeKnowledgeApiKey": []}]
                    or op.get("responses", {}).get("default", {}).get("x-native-undeclared") is not True):
                raise ValueError(f"native Knowledge wildcard contract changed: {method} {path}")
            success = op.get("responses", {}).get("200", {}).get("content", {}).get("application/json", {}).get("schema")
            if success != {}:
                raise ValueError("wildcard must not invent a common Knowledge success shape")
            for status in ("200", "422", "default"):
                content = op["responses"].get(status, {}).get("content", {})
                if (content.get("*/*", {}).get("schema") != {}
                        or any(media.get("schema") != {} for media in content.values())):
                    raise ValueError("opaque Knowledge responses must preserve arbitrary media without typed guarantees")
            operations.add(f"{method.upper()} {path}")
    return operations


def route_discovery_input() -> dict:
    """Fail closed when the locked framework's private context adapter changes."""
    distribution = importlib.metadata.distribution("fastapi")
    if (distribution.version != FASTAPI_ROUTE_DISCOVERY["version"]
            or any(digest(Path(distribution.locate_file(path))) != expected
                   for path, expected in FASTAPI_ROUTE_DISCOVERY["source_sha256"].items())):
        raise ValueError("Knowledge route discovery requires reviewed FastAPI version/source")
    return {**FASTAPI_ROUTE_DISCOVERY,
            "source_sha256": dict(FASTAPI_ROUTE_DISCOVERY["source_sha256"])}


def _schema_references(value) -> set[str]:
    """Collect only the schema references supported by the composer."""
    if isinstance(value, list):
        return set().union(*(_schema_references(item) for item in value))
    if not isinstance(value, dict):
        return set()
    references = set()
    if "$ref" in value:
        reference = value["$ref"]
        prefix = "#/components/schemas/"
        if not isinstance(reference, str) or not reference.startswith(prefix):
            raise ValueError("Knowledge owner declaration has an unsupported reference")
        references.add(reference[len(prefix):])
    # Mapping targets need separate rewriting semantics; do not silently lose them.
    if isinstance(value.get("discriminator"), dict) and value["discriminator"].get("mapping"):
        raise ValueError("Knowledge owner has an unreviewed discriminator mapping")
    for item in value.values():
        references.update(_schema_references(item))
    return references


def _select_owner_document(document: dict, selected: set[tuple[str, str]]) -> dict:
    if set(document.get("components", {})) - {"schemas"}:
        raise ValueError("Knowledge owner has unreviewed component kinds")
    public_paths = {path: item for path, item in document["paths"].items()
                    if path.startswith(UPSTREAM + "/")}
    actual = {(method, path) for path, item in public_paths.items() for method in item}
    if actual != selected:
        raise ValueError("Knowledge owner route/schema coverage differs")
    definitions = document.get("components", {}).get("schemas", {})
    required = _schema_references(public_paths)
    schemas = {}
    while required:
        name = required.pop()
        if name in schemas:
            continue
        if name not in definitions:
            raise ValueError("Knowledge owner declaration has a missing schema reference")
        schemas[name] = definitions[name]
        required.update(_schema_references(definitions[name]) - schemas.keys())
    return {**document, "paths": public_paths,
            "components": {"schemas": {name: schemas[name] for name in sorted(schemas)}}}


def mounted_owner_declarations(routes: list) -> tuple[dict, list]:
    """Preserve real include contexts, mount order and public schema closure."""
    route_discovery_input()
    from fastapi.openapi.utils import get_openapi
    from fastapi.routing import APIRoute, _iter_routes_with_context

    public = []
    selected = set()
    for original, context in _iter_routes_with_context(routes):
        if not isinstance(original, APIRoute):
            continue
        effective = context if context is not None else original
        if not effective.include_in_schema or not effective.path_format.startswith(UPSTREAM + "/"):
            continue
        for method in sorted(effective.methods):
            key = (method.lower(), effective.path_format)
            if key in selected or key[0] not in METHODS:
                raise ValueError("duplicate or unsupported public Knowledge operation")
            selected.add(key)
        public.append(effective)
    if not public:
        raise ValueError("Knowledge owner has no mounted public declarations")
    # FastAPI owns prefix/dependency/response/schema inheritance. Never feed bare
    # original routes or private effective-context objects back into get_openapi.
    document = get_openapi(title="Retained Python Knowledge owner declarations",
                           version="source", openapi_version="3.1.0", routes=routes)
    return _select_owner_document(document, selected), public


def owner_document(root: Path = ROOT) -> dict:
    """Import real routers only; never import main/create_app or enter a lifespan."""
    cache = os.environ.get("TIKTOKEN_CACHE_DIR")
    if not cache:
        raise ValueError("TIKTOKEN_CACHE_DIR must identify a verified offline tokenizer cache")
    dependencies = tokenizer_input(Path(cache))
    dependencies["route_discovery"] = route_discovery_input()
    from fastapi import APIRouter
    from knowledge_service.api import route_registry

    registry = Path(route_registry.__file__).resolve()
    if not registry.is_relative_to((root / OWNER_ROOT).resolve()):
        raise ValueError("Knowledge owner imports are not from the selected source tree")
    target = APIRouter()
    route_registry.include_required_api_routes(target)
    document, routes = mounted_owner_declarations(target.routes)
    selected = []
    for route in routes:
        source = Path(inspect.getsourcefile(route.endpoint)).resolve()
        if not source.is_relative_to((root / OWNER_ROOT).resolve()):
            raise ValueError("public Knowledge route is not owned by Knowledge source")
        for method in sorted(route.methods):
            key = (method.lower(), route.path_format)
            selected.append({"method": key[0], "path": key[1],
                             "source": str(source.relative_to(root)),
                             "handler": route.endpoint.__name__})
    sources = {str(p.relative_to(root)): digest(p)
               for base in (OWNER_ROOT, CORE_ROOT, CONTRACTS_ROOT)
               for p in sorted((root / base).rglob("*.py"))}
    sources["scripts/harness/knowledge_proxy_contract.py"] = digest(
        root / "scripts/harness/knowledge_proxy_contract.py")
    return {"document": document, "route_order": selected, "source_sha256": sources,
            "export_dependencies": dependencies}


def read_owner_declarations(root: Path, tokenizer_cache: Path | None = None) -> dict:
    if tokenizer_cache is None:
        raise ValueError("provide --knowledge-tokenizer-cache for inert Knowledge export")
    dependency_path = Path(importlib.metadata.distribution("fastapi").locate_file("")).resolve()
    exports = []
    for seed in ("31", "83"):
        # Export needs source/dependencies, never deployment credentials/configuration.
        env = {"PATH": os.defpath, "LANG": "C.UTF-8"}
        env.update(PYTHONDONTWRITEBYTECODE="1", PYTHONHASHSEED=seed,
                   TIKTOKEN_CACHE_DIR=str(tokenizer_cache.resolve()),
                   PYTHONPATH=os.pathsep.join([*(str(root / p) for p in (OWNER_ROOT, CORE_ROOT, CONTRACTS_ROOT, ".")), str(dependency_path)]))
        result = subprocess.run(
            [sys.executable, "-S", "-m", "scripts.harness.knowledge_proxy_contract", "--owner-document"],
            cwd=root, env=env, check=True, capture_output=True, timeout=120)
        if len(result.stdout) > 8 * 1024 * 1024:
            raise ValueError("Knowledge owner declarations are oversized")
        exports.append(result.stdout)
    if exports[0] != exports[1]:
        raise ValueError("Knowledge declarations differ across hash-seed processes")
    return json.loads(exports[0])


def _rewrite_refs(value):
    if isinstance(value, list):
        return [_rewrite_refs(item) for item in value]
    if not isinstance(value, dict):
        return value
    result = {key: _rewrite_refs(item) for key, item in value.items()}
    if "$ref" in result:
        prefix = "#/components/schemas/"
        if not isinstance(result["$ref"], str) or not result["$ref"].startswith(prefix):
            raise ValueError("Knowledge owner declaration has an unsupported reference")
        result["$ref"] = prefix + SCHEMA_PREFIX + result["$ref"][len(prefix):]
    return result


def compose(document: dict, owner: dict, root: Path = ROOT) -> dict:
    """Add owner declarations to a verified native document without filling gaps."""
    verify_native_forwarding(document, root)
    if "x-python-knowledge-owner" in document:
        raise ValueError("Knowledge declarations already composed")
    if not owner.get("source_sha256") or not owner.get("route_order"):
        raise ValueError("Knowledge owner source evidence is missing")
    dependencies = owner.get("export_dependencies", {})
    if (dependencies.get("package") != "tiktoken" or not dependencies.get("version")
            or len(dependencies.get("source_sha256", "")) != 64
            or len(dependencies.get("asset_sha256", "")) != 64
            or not 0 < dependencies.get("asset_bytes", 0) <= 4 * 1024 * 1024):
        raise ValueError("Knowledge offline tokenizer input evidence is missing")
    if dependencies.get("route_discovery") != FASTAPI_ROUTE_DISCOVERY:
        raise ValueError("Knowledge route discovery dependency evidence is missing or changed")
    for relative, expected in owner["source_sha256"].items():
        source = (root / relative).resolve()
        if not source.is_relative_to(root.resolve()) or digest(source) != expected:
            raise ValueError("Knowledge owner source changed during export")
    declarations = owner["document"]
    if set(declarations.get("components", {})) - {"schemas"}:
        raise ValueError("Knowledge owner has unreviewed component kinds")
    schemas = document.setdefault("components", {}).setdefault("schemas", {})
    for name, schema in declarations.get("components", {}).get("schemas", {}).items():
        target = SCHEMA_PREFIX + name
        if target in schemas:
            raise ValueError("Knowledge owner schema namespace collision")
        schemas[target] = _rewrite_refs(schema)
    forwarded = []
    seen = set()
    opaque_success = []
    undeclared_body = []
    for route in owner["route_order"]:
        method, path = route["method"], route["path"]
        key = f"{method.upper()} {path}"
        if not path.startswith(UPSTREAM + "/") or method not in METHODS or key in seen:
            raise ValueError("invalid Knowledge owner forwarding target")
        seen.add(key)
        operation = declarations["paths"][path][method]
        suffix = path[len(UPSTREAM) + 1:]
        transform = next((v for v in document["x-native-knowledge-forwarding"]["transforms"]
                          if v["method"].lower() == method and v["suffix"] == suffix), None)
        if transform and transform["kind"] == "gateway-owned":
            raise ValueError("Gateway-owned impact cannot be composed as Python passthrough")
        forwarded.append({**route,
                          "public_paths": [prefix + "/" + suffix for prefix in PREFIXES],
                          "request_mapping": transform["kind"] if transform else "passthrough-after-native-admission",
                          "operation": _rewrite_refs(operation)})
        responses = operation.get("responses", {})
        for status, response in responses.items():
            if status.startswith("2"):
                contents = response.get("content", {})
                if not contents or any(not value.get("schema") for value in contents.values()):
                    opaque_success.append(key)
                    break
        if method in {"post", "put", "patch", "delete"} and "requestBody" not in operation:
            undeclared_body.append(key)
    actual = {f"{method.upper()} {path}" for path, item in declarations["paths"].items()
              for method in item if method in METHODS}
    if actual != seen:
        raise ValueError("Knowledge owner route/schema coverage differs")
    document["x-python-knowledge-owner"] = {
        "profile": "native-gateway-python-knowledge", "source_sha256": owner["source_sha256"],
        "declaration_only": True, "complete": False, "runtime_verified": False,
        "owner_hash_seed_processes": 2, "forwarded_operations": forwarded,
        "export_dependencies": dependencies,
        "opaque_success_operations": sorted(set(opaque_success)),
        "undeclared_request_body_operations": sorted(undeclared_body),
        "limitations": "Only owner FastAPI/Pydantic declarations are composed. Runtime validators, dynamic JSON, streaming/media bodies, owner ACL, errors and deployment identity remain authoritative and not exhaustively represented. Internal routes and Gateway-owned impact are not republished. Native wildcard declarations and transforms still apply.",
    }
    document["x-python-knowledge-owner"]["declarations_sha256"] = _composition_digest(document)
    return document


def validate_composition(document: dict, root: Path) -> dict:
    marker = document.get("x-python-knowledge-owner", {})
    if (marker.get("profile") != "native-gateway-python-knowledge"
            or marker.get("declaration_only") is not True
            or marker.get("complete") is not False or marker.get("runtime_verified") is not False
            or marker.get("owner_hash_seed_processes") != 2
            or not marker.get("forwarded_operations") or not marker.get("source_sha256")):
        raise ValueError("Knowledge owner composition is absent or overclaims coverage")
    if marker.get("export_dependencies", {}).get("route_discovery") != FASTAPI_ROUTE_DISCOVERY:
        raise ValueError("Knowledge route discovery dependency evidence is missing or changed")
    if marker.get("declarations_sha256") != _composition_digest(document):
        raise ValueError("Knowledge owner composed declarations were modified")
    for relative, expected in marker["source_sha256"].items():
        source = (root / relative).resolve()
        if not source.is_relative_to(root.resolve()) or digest(source) != expected:
            raise ValueError("Knowledge owner composition/source mismatch")
    return {"knowledge_owner_operations": len(marker["forwarded_operations"]),
            "knowledge_owner_opaque_success_operations": marker["opaque_success_operations"],
            "knowledge_owner_undeclared_request_body_operations": marker["undeclared_request_body_operations"],
            "knowledge_owner_deployment_verified": False}


def install_offline_audit() -> None:
    """Install before third-party/site initialization, including guarded pytest."""
    def _offline_export_audit(event, args):
        def terminal():
            # Imports may catch ordinary exceptions. A denial must never be hidden
            # by an optional-import fallback and followed by successful JSON output.
            os.write(2, f"Knowledge declaration export denied audit event: {event}\n".encode())
            os._exit(79)

        if event.startswith("socket.") or event in {
            "subprocess.Popen", "os.system", "os.fork", "os.forkpty",
            "os.exec", "os.posix_spawn", "os.spawn",
        }:
            terminal()
        if event == "open" and isinstance(args[0], (str, bytes)):
            path = os.fsdecode(args[0])
            name = Path(path).name
            if (name == ".env" or name.startswith(".env.") or name in {".netrc", ".git-credentials"}
                    or "/.aws/" in path or "/.config/gcloud/" in path):
                terminal()

    sys.addaudithook(_offline_export_audit)


if __name__ == "__main__":
    if sys.argv[1:] != ["--owner-document"]:
        raise SystemExit("Usage: python -S -m scripts.harness.knowledge_proxy_contract --owner-document")
    install_offline_audit()
    print(json.dumps(owner_document(), sort_keys=True, separators=(",", ":")))
