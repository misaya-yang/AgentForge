#!/usr/bin/env python3
"""Export all declared native-only operations without claiming the full Gateway gate.

The independently compiled Rust producer consumes shared native request declarations.
The wrapper composes retained Python Knowledge declarations only beneath the real
native streaming wildcard mapping; opaque requests/responses remain explicit gaps.
The publication is read only AFTER production, for explicit ownership/coverage checks.
No schema is filled from the publication. Dynamic response members, unconstrained error bodies and common operations
remain gaps. This command does not replace verify-native-openapi-contract.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import subprocess
import sys
from pathlib import Path

from jsonschema import Draft202012Validator, SchemaError

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))
from scripts.harness import knowledge_proxy_contract as knowledge  # noqa: E402
from scripts.harness.openapi_profiles import load_ownership  # noqa: E402

CONTRACT = "rust/platform-native/crates/platform-gateway-foundation/src/native_contract.rs"
RESPONSES = "rust/platform-native/crates/platform-gateway-foundation/src/native_responses.rs"
COMMON = "sdk/native-common-contracts.json"
COMMON_SOURCES = {
    "native_api_keys_contract.rs",
    "self_service_api_keys_api.rs",
    "self_service_api_keys.rs",
    "self_service_api_keys_store.rs",
    "http_content_type.rs",
    "service_access_resolver.rs",

    'native_connectors_contract.rs',
    'connector_user_api.rs',
    'connector_user_service.rs',
    'connector_user_store.rs',
    'connector_user_types.rs',
    '../../platform-auth/src/admission.rs',
    '../../platform-gateway/src/migration_host/bootstrap/foundation.rs',

    "native_invocation_task_contract.rs",
    "invocation_http.rs",
    "task_http.rs",
    "task_manager.rs",
    "task_actor.rs",
    "task_durable_worker.rs",
    "gateway_dispatcher.rs",

    "../../platform-auth/src/contract.rs",
    "../../platform-auth/src/http.rs",
    "../../platform-auth/src/lib.rs",
    "../../platform-auth/src/accounts.rs",
    "../../platform-auth/src/sessions.rs",
    "../../platform-gateway/src/migration_host/foundation.rs",

    "session_storage.rs",
    "../../platform-auth/src/api_keys.rs",
    "../../platform-auth/src/service.rs",
    "../../platform-gateway/src/migration_host/bootstrap/graph.rs",
    "../../platform-gateway/src/migration_host/bootstrap/foundation_backends.rs",
    "../../platform-gateway/src/migration_agent/control.rs",
    "../../platform-gateway-integrations/src/migration_adapters/sdk_session_cleanup.rs",
    "sessions_api.rs",
    'native_sessions_contract.rs', '../../platform-gateway/src/lib.rs', '../../platform-gateway/src/session_history.rs',
    "native_roles_contract.rs", "roles_api.rs", "native_services_contract.rs", "services_api.rs",
    "protocol.rs", "request_validation.rs", "service_registry.rs", "service_config_api.rs",
    "native_users_contract.rs", "users_api.rs", "users_store.rs", "users_store/guard.rs",
    "rate_config_api.rs", "usage_parser.rs",
}
METHODS = {"get", "post", "put", "patch", "delete", "head", "options"}


def digest(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def operations(document: dict) -> dict[str, str]:
    return {f"{method.upper()} {path}": op.get("operationId")
            for path, item in document["paths"].items()
            for method, op in item.items() if method in METHODS}


def common_implementations(published: dict, ownership: dict, root: Path) -> tuple[dict, set]:
    """Validate independent native implementations without altering Python projection."""
    manifest = json.loads((root / COMMON).read_text())
    if set(manifest) != {"version", "scope", "operations", "source_evidence"} or manifest["version"] != 1 or manifest["scope"] != "common-native-implementations":
        raise ValueError("unknown common native implementation manifest")
    expected = {}
    for entry in manifest["operations"]:
        if set(entry) != {"path", "method", "operation_id"}:
            raise ValueError("invalid common native operation")
        path, method = entry["path"], entry["method"]
        key = f"{method.upper()} {path}"
        if method not in METHODS or path not in ownership["python_paths"] or key in expected:
            raise ValueError("common native operation overlaps or leaves Python ownership")
        if published["paths"][path].get(method, {}).get("operationId") != entry["operation_id"]:
            raise ValueError("common native operation ID differs from publication")
        expected[key] = entry["operation_id"]
    evidence = manifest["source_evidence"]
    if not expected or not isinstance(evidence, dict) or not evidence:
        raise ValueError("missing common native implementation/source evidence")
    for relative, proofs in evidence.items():
        source = (root / relative).resolve()
        if not source.is_relative_to((root / "rust/platform-native").resolve()):
            raise ValueError("invalid common native source")
        text = source.read_text()
        if not isinstance(proofs, list) or not proofs or any(not isinstance(proof, str) or not proof or proof not in text for proof in proofs):
            raise ValueError("stale common native dispatcher/schema evidence")
    return expected, set(evidence)


def bounded_success_schema(schema: dict, method: str, path: str) -> bool:
    """Keep object envelopes and the two explicitly owned Services/Tasks list shapes.

    Its two source-owned item variants must retain actual object declarations.
    This does not infer item fields from publication or exempt other operations.
    """
    if method == "get" and path == CONNECTOR_AVAILABLE:
        items = schema.get("items", {})
        expected = {
            "provider": {"type": "string"}, "display_name": {"type": "string"},
            "description": {"type": ["string", "null"]}, "icon_url": {"type": ["string", "null"]},
            "enabled": {"type": "boolean"}, "connected": {"type": "boolean"},
            "status": {"type": ["string", "null"]},
        }
        return (schema.get("type") == "array" and items.get("type") == "object"
                and items.get("properties") == expected and items.get("additionalProperties") is False
                and set(items.get("required", [])) == set(expected))
    if method == "get" and path == "/api/v1/api-keys":
        items = schema.get("items", {})
        fields = {"key_id", "key_prefix", "name", "description", "user_id",
                  "derived_user_id", "scopes", "roles", "tier", "rate_limit",
                  "is_active", "created_at", "last_used_at", "expires_at", "use_count"}
        return (schema.get("type") == "array" and items.get("type") == "object"
                and set(items.get("properties", {})) == fields
                and set(items.get("required", [])) == fields
                and items.get("additionalProperties") is False)
    if method == "get" and path == "/api/v1/tasks":
        items = schema.get("items")
        return (schema.get("type") == "array" and isinstance(items, dict)
                and items.get("type") == "object" and bool(items.get("properties"))
                and items.get("additionalProperties") is False
                and set(items.get("required", [])) == set(items["properties"]))
    if method == "get" and path == "/api/v1/services":
        try:
            # Existing project dependency; validates against the built-in meta-schema
            # without resolving any instance-data references or fetching sources.
            Draft202012Validator.check_schema(schema)
        except SchemaError:
            return False
        items = schema.get("items")
        variants = items.get("anyOf") if isinstance(items, dict) else None
        return (schema.get("type") == "array" and isinstance(variants, list)
                and len(variants) == 2
                and all(isinstance(item, dict) and item.get("type") == "object"
                        and isinstance(item.get("properties"), dict) and item["properties"]
                        and isinstance(item.get("required"), list)
                        and set(item["required"]) == set(item["properties"])
                        and item.get("additionalProperties") is False for item in variants))
    return schema.get("type") == "object" and bool(schema.get("properties"))


CONNECTOR_CALLBACK = "/api/v1/connectors/callback/{provider}"
CONNECTOR_MINE = "/api/v1/connectors/mine"
CONNECTOR_AVAILABLE = "/api/v1/connectors/available"
CONNECTOR_PATHS = {CONNECTOR_CALLBACK, CONNECTOR_MINE, CONNECTOR_AVAILABLE,
                   "/api/v1/connectors/auth/{provider}", "/api/v1/connectors/{provider}"}


def verify_connector_operation(path: str, method: str, op: dict) -> None:
    """Check only the five existing source-owned personal connector declarations."""
    callback = path == CONNECTOR_CALLBACK
    expected_security = [] if callback else [{"NativeBearerAuth": []}, {"NativeBoundApiKey": []}]
    if op.get("security") != expected_security:
        raise ValueError("connector authority declaration differs")
    responses = op.get("responses", {})
    if responses.get("default", {}).get("x-native-undeclared") is not True:
        raise ValueError("connector default response gap hidden")
    errors = {"400", "404", "422", "500", "503"} if callback else (
        {"400", "401", "404", "500", "503"} if path.endswith("/auth/{provider}")
        else {"401", "500", "503"})
    if set(responses) != errors | {"default", "302" if callback else "200"}:
        raise ValueError("connector observed response statuses differ")
    for status in errors:
        expected_content = {"application/json": {"schema": {}}}
        if status == "500":
            expected_content["text/plain"] = {"schema": {"type": "string"}}
        if responses[status].get("content") != expected_content:
            raise ValueError("connector observed error media or body narrowed")
    parameters = op.get("parameters", [])
    provider = {"name": "provider", "in": "path", "required": True, "schema": {"type": "string"}}
    expected_parameters = [provider] if "{provider}" in path else []
    if callback:
        expected_parameters += [
            {"name": "code", "in": "query", "required": False, "schema": {"type": "string"}},
            {"name": "state", "in": "query", "required": False, "schema": {"type": "string", "default": ""}},
            {"name": "error", "in": "query", "required": False, "schema": {"type": "string"}},
        ]
        redirect = responses.get("302", {})
        if (op.get("x-native-response-schema") != "redirect-source-owned"
                or any(key.startswith("2") for key in responses)
                or "content" in redirect
                or redirect.get("headers") != {"Location": {"required": True, "schema": {"type": "string"}}}):
            raise ValueError("missing source-owned connector redirect declaration")
    if parameters != expected_parameters:
        raise ValueError("connector parameter declaration differs")
    if path == CONNECTOR_MINE:
        schema = responses.get("200", {}).get("content", {}).get("application/json", {}).get("schema")
        if (op.get("x-native-response-schema") != "opaque-row-array-source-owned"
                or schema != {"type": "array", "items": {}}):
            raise ValueError("connector raw row projection must remain opaque")


def verify(document: dict, root: Path = ROOT) -> dict:
    marker = document.get("x-native-contract", {})
    if (marker.get("scope") != "native-only" or marker.get("complete") is not False
            or marker.get("responses_declared") is not False
            or marker.get("success_responses_declared") is not False
            or marker.get("bounded_success_responses_declared") is not True):
        raise ValueError("producer must explicitly declare bounded/incomplete response coverage")
    if marker.get("source_sha256") != digest(root / CONTRACT):
        raise ValueError("compiled producer does not match current native contract source")
    if marker.get("response_source_sha256") != digest(root / RESPONSES):
        raise ValueError("compiled producer does not match current native response source")
    if set(marker.get("common_source_sha256", {})) != COMMON_SOURCES:
        raise ValueError("missing common native compiled-source bindings")
    for name in COMMON_SOURCES:
        if marker["common_source_sha256"][name] != digest(root / Path(CONTRACT).parent / name):
            raise ValueError("compiled producer differs from common native source")
    published = json.loads((root / "sdk/openapi.json").read_text())
    ownership = load_ownership(published, root / "sdk/openapi-profiles.json", root)
    expected = {f"{method.upper()} {entry['path']}": operation_id
                for entry in ownership["native_only"]
                for method, operation_id in entry["operations"].items()}
    common, common_sources = common_implementations(published, ownership, root)
    forwarded = knowledge.verify_native_forwarding(document, root)
    owner_report = knowledge.validate_composition(document, root)
    expected.update(common)
    actual = operations(document)
    if actual != expected:
        raise ValueError("native operation ownership differs: " + json.dumps({
            "missing": sorted(set(expected) - set(actual)),
            "unexpected": sorted(set(actual) - set(expected)),
            "changed_ids": sorted(key for key in expected.keys() & actual.keys()
                                  if expected[key] != actual[key])}))
    if set(document["paths"]) != ({entry["path"] for entry in ownership["native_only"]} | {key.split(" ", 1)[1] for key in common}):
        raise ValueError("native path ownership differs")
    for path, item in document["paths"].items():
        for method, op in item.items():
            if method not in METHODS:
                continue
            if path in CONNECTOR_PATHS:
                verify_connector_operation(path, method, op)
                if path in {CONNECTOR_CALLBACK, CONNECTOR_MINE}:
                    continue
            responses = op.get("responses", {})
            success = responses.get("200", {}).get("content", {}).get("application/json", {}).get("schema", {})
            if f"{method.upper()} {path}" in forwarded:
                continue  # Already checked as an opaque streamed owner response, never bounded.
            if method == "post" and path == "/api/v1/stream":
                stream_content = responses.get("200", {}).get("content", {})
                sse = op.get("x-native-sse", {})
                chunk = sse.get("data_schema", {})
                if (op.get("x-native-response-schema") != "bounded-sse-source-owned"
                        or set(stream_content) != {"text/event-stream"}
                        or stream_content["text/event-stream"].get("schema") != {"type": "string"}
                        or chunk.get("type") != "object" or not chunk.get("properties")
                        or chunk.get("additionalProperties") is not False
                        or set(chunk.get("required", [])) != set(chunk["properties"])
                        or sse.get("error_event") != "error" or sse.get("error_data") != "text"
                        or sse.get("opening") != "first-body-poll"
                        or responses.get("default", {}).get("x-native-undeclared") is not True):
                    raise ValueError("missing source-owned invocation SSE declaration")
                Draft202012Validator.check_schema(chunk)
                continue
            if (op.get("x-native-response-schema") != "bounded-source-owned"
                    or not bounded_success_schema(success, method, path)
                    or responses.get("default", {}).get("x-native-undeclared") is not True):
                raise ValueError(f"missing bounded success/default response declaration: {method} {path}")
    all_operations = operations(published)
    inputs = {CONTRACT, RESPONSES, COMMON, "sdk/openapi-profiles.json", "sdk/openapi.json"} | common_sources
    inputs.update(knowledge.NATIVE_ROOT + source for source in knowledge.NATIVE_SOURCES)
    inputs.update(document["x-python-knowledge-owner"]["source_sha256"])
    for entry in ownership["native_only"]:
        inputs.add(entry["source"])
        inputs.update(entry.get("shared_source_evidence", {}))
    return {"result_kind": "bounded-native-source-export", "result": "pass",
            "full_native_gate_satisfied": False, "native_runtime_verified": False,
            "native_paths": len(document["paths"]), "native_operations": len(actual),
            "published_operations": len(all_operations),
            "common_native_operations": sorted(common),
            "python_projection_unchanged": True,
            "missing_common_operations": sorted(set(all_operations) - set(actual)),
            "undeclared_response_operations": sorted(forwarded | {"GET " + CONNECTOR_MINE}),
            "forwarded_response_operations": sorted(forwarded),
            "bounded_response_operations": sorted(set(actual) - forwarded - {"GET " + CONNECTOR_MINE}),
            "opaque_row_response_operations": ["GET " + CONNECTOR_MINE],
            "redirect_response_operations": ["GET " + CONNECTOR_CALLBACK],
            "bounded_sse_operations": ["POST /api/v1/stream"],
            "default_response_gaps": sorted(actual),
            "dynamic_row_schemas_remain_open": True,
            **owner_report,
            "source_sha256": {name: digest(root / name) for name in sorted(inputs)}}


def declaration_drift(previous: dict, current: dict) -> list[str]:
    """Conservative review gate: no request/parameter/auth/response constraint is ignored.

    This deliberately rejects additive changes too. It reports drift, not a claim
    that every difference is backwards-incompatible. A previous partial export
    cannot serve as the full published baseline.
    """
    if previous.get("x-native-contract", {}).get("scope") != "native-only":
        raise ValueError("previous document must be a bounded native-source export")
    return [section for section in ("openapi", "paths", "components",
                                    "x-native-knowledge-forwarding", "x-python-knowledge-owner")
            if previous.get(section) != current.get(section)]


def safe_output(path: Path, root: Path = ROOT) -> None:
    if path.is_symlink() or (path.exists() and path.stat().st_nlink != 1):
        raise ValueError("output must not be a symlink or hardlinked file")
    resolved = path.resolve()
    # Export artifacts belong outside source; this also protects publication aliases.
    if resolved.is_relative_to(root.resolve()):
        raise ValueError("native export output must be outside the repository")
    published = root / "sdk/openapi.json"
    if path.exists() and path.samefile(published):
        raise ValueError("output aliases the published contract")


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--producer", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--coverage-output", type=Path, required=True)
    parser.add_argument("--knowledge-tokenizer-cache", type=Path,
                        help="verified offline cl100k_base cache; exporter never fetches assets")
    parser.add_argument("--previous", type=Path, help="prior source export; any declared constraint drift fails for review")
    args = parser.parse_args(argv)
    try:
        safe_output(args.output)
        safe_output(args.coverage_output)
        if args.output.resolve() == args.coverage_output.resolve():
            raise ValueError("schema and coverage outputs must be distinct")
        for output in [args.output, args.coverage_output]:
            if output.resolve() == args.producer.resolve() or (output.exists() and output.samefile(args.producer)):
                raise ValueError("output aliases producer executable")
            if (args.knowledge_tokenizer_cache is not None
                    and output.resolve().is_relative_to(args.knowledge_tokenizer_cache.resolve())):
                raise ValueError("output overlaps verified tokenizer cache")
        exports = []
        for _ in range(2):
            result = subprocess.run([str(args.producer.resolve()), "--native-only"],
                                    check=True, capture_output=True, timeout=30)
            if len(result.stdout) > 8 * 1024 * 1024:
                raise ValueError("native producer output is oversized")
            exports.append(result.stdout)
        if exports[0] != exports[1]:
            raise ValueError("native producer is not byte-deterministic across processes")
        document = json.loads(exports[0])
        # Own FastAPI declarations are produced independently; never fill from SDK publication.
        document = knowledge.compose(document, knowledge.read_owner_declarations(ROOT, args.knowledge_tokenizer_cache))
        candidate = (json.dumps(document, sort_keys=True, indent=2) + "\n").encode()
        report = verify(document)
        if args.previous is not None:
            drift = declaration_drift(json.loads(args.previous.read_text()), document)
            if drift:
                raise ValueError("native declaration drift requires review: " + ", ".join(drift))
            report["previous_declarations_identical"] = True
        report["determinism_processes"] = 2
        report["producer_sha256"] = digest(args.producer)
        report["candidate_sha256"] = hashlib.sha256(candidate).hexdigest()
        for output in [args.output, args.coverage_output]:
            output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_bytes(candidate)
        args.coverage_output.write_text(json.dumps(report, indent=2) + "\n")
        print(f"Exported {len(document['paths'])} native source paths; full native contract remains incomplete.")
        return 0
    except (OSError, ValueError, TypeError, KeyError, subprocess.SubprocessError) as error:
        print(f"Native export failed: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
