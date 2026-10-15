"""Bounded native export cannot replace full coverage or conceal source/constraint drift."""
from __future__ import annotations

import copy
import json
import os
import shutil

import pytest

from scripts import snapshot_native_openapi as native
from scripts.harness import knowledge_proxy_contract as knowledge


@pytest.fixture
def source_tree(tmp_path):
    root = tmp_path / "source"
    ownership = json.loads((native.ROOT / "sdk/openapi-profiles.json").read_text())
    files = {"sdk/openapi.json", "sdk/openapi-profiles.json", native.CONTRACT, native.RESPONSES}
    common = json.loads((native.ROOT / native.COMMON).read_text())
    files.add(native.COMMON)
    files.update(common["source_evidence"])
    files.update(knowledge.NATIVE_ROOT + name for name in knowledge.NATIVE_SOURCES)
    files.add("scripts/harness/knowledge_proxy_contract.py")
    files.update(entry["source"] for entry in ownership["native_only"])
    for entry in ownership["native_only"]:
        files.update(entry.get("shared_source_evidence", {}))
    for relative in files:
        output = root / relative
        output.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(native.ROOT / relative, output)
    return root


@pytest.fixture
def document(source_tree):
    ownership = json.loads((source_tree / "sdk/openapi-profiles.json").read_text())
    paths = {}
    for entry in ownership["native_only"]:
        paths[entry["path"]] = {
            method: {"operationId": operation_id, "x-native-response-schema": "bounded-source-owned",
                     "responses": {"200": {"content": {"application/json": {"schema": {
                         "type": "object", "properties": {"fixture": {}}}}}},
                         "default": {"x-native-undeclared": True}}}
            for method, operation_id in entry["operations"].items()}
    common = json.loads((source_tree / native.COMMON).read_text())
    for entry in common["operations"]:
        paths.setdefault(entry["path"], {})[entry["method"]] = {
            "operationId":entry["operation_id"],"x-native-response-schema":"bounded-source-owned",
            "responses":{"200":{"content":{"application/json":{"schema":{"type":"object","properties":{"fixture":{}}}}}},"default":{"x-native-undeclared":True}}}
    paths["/api/v1/services"]["get"]["responses"]["200"]["content"]["application/json"]["schema"] = {
        "type": "array", "items": {"anyOf": [
            {"type": "object", "properties": {"fixture": {}}, "required": ["fixture"],
             "additionalProperties": False},
            {"type": "object", "properties": {"variant": {}}, "required": ["variant"],
             "additionalProperties": False},
        ]}}
    paths["/api/v1/tasks"]["get"]["responses"]["200"]["content"]["application/json"]["schema"] = {
        "type": "array", "items": {"type": "object", "properties": {"fixture": {}},
        "required": ["fixture"], "additionalProperties": False}}
    key_fields = {name: {} for name in (
        "key_id", "key_prefix", "name", "description", "user_id", "derived_user_id",
        "scopes", "roles", "tier", "rate_limit", "is_active", "created_at",
        "last_used_at", "expires_at", "use_count")}
    paths["/api/v1/api-keys"]["get"]["responses"]["200"]["content"]["application/json"]["schema"] = {
        "type": "array", "items": {"type": "object", "properties": key_fields,
        "required": list(key_fields), "additionalProperties": False}}
    stream = paths["/api/v1/stream"]["post"]
    stream["x-native-response-schema"] = "bounded-sse-source-owned"
    stream["responses"]["200"]["content"] = {"text/event-stream": {"schema": {"type": "string"}}}
    stream["x-native-sse"] = {"data_schema": {"type": "object", "properties": {"fixture": {}},
        "required": ["fixture"], "additionalProperties": False}, "error_event": "error",
        "error_data": "text", "opening": "first-body-poll"}
    # Synthetic connector wrapper fixtures, not production response evidence.
    for path in native.CONNECTOR_PATHS:
        method = "delete" if path == "/api/v1/connectors/{provider}" else "get"
        op = paths[path][method]
        op["security"] = [] if path == native.CONNECTOR_CALLBACK else [{"NativeBearerAuth": []}, {"NativeBoundApiKey": []}]
        errors = {"400", "404", "422", "500", "503"} if path == native.CONNECTOR_CALLBACK else (
            {"400", "401", "404", "500", "503"} if path.endswith("/auth/{provider}") else {"401", "500", "503"})
        for status in errors:
            op["responses"][status] = {"content": {"application/json": {"schema": {}}}}
        op["responses"]["500"]["content"]["text/plain"] = {"schema": {"type": "string"}}
        if "{provider}" in path:
            op["parameters"] = [{"name": "provider", "in": "path", "required": True, "schema": {"type": "string"}}]
    available = paths[native.CONNECTOR_AVAILABLE]["get"]
    fields = {"provider": {"type": "string"}, "display_name": {"type": "string"},
              "description": {"type": ["string", "null"]}, "icon_url": {"type": ["string", "null"]},
              "enabled": {"type": "boolean"}, "connected": {"type": "boolean"}, "status": {"type": ["string", "null"]}}
    available["responses"]["200"]["content"]["application/json"]["schema"] = {
        "type": "array", "items": {"type": "object", "properties": fields,
        "required": list(fields), "additionalProperties": False}}
    mine = paths[native.CONNECTOR_MINE]["get"]
    mine["x-native-response-schema"] = "opaque-row-array-source-owned"
    mine["responses"]["200"]["content"]["application/json"]["schema"] = {"type": "array", "items": {}}
    callback = paths[native.CONNECTOR_CALLBACK]["get"]
    callback["x-native-response-schema"] = "redirect-source-owned"
    del callback["responses"]["200"]
    callback["responses"]["302"] = {"headers": {"Location": {"required": True, "schema": {"type": "string"}}}}
    callback["parameters"] += [
        {"name": "code", "in": "query", "required": False, "schema": {"type": "string"}},
        {"name": "state", "in": "query", "required": False, "schema": {"type": "string", "default": ""}},
        {"name": "error", "in": "query", "required": False, "schema": {"type": "string"}},
    ]
    document = {"openapi": "3.1.0", "paths": paths, "components": {},
            "x-native-contract": {"scope": "native-only", "complete": False,
                                  "responses_declared": False, "success_responses_declared": False,
                                  "bounded_success_responses_declared": True,
                                  "common_source_sha256": {name: native.digest(source_tree / native.Path(native.CONTRACT).parent / name) for name in native.COMMON_SOURCES},
                                  "response_source_sha256": native.digest(source_tree / native.RESPONSES),
                                  "source_sha256": native.digest(source_tree / native.CONTRACT)}}
    # Synthetic wrapper fixtures only; owning-router/real-producer tests are separate.
    document["x-native-knowledge-forwarding"] = {
        "version": 1, "complete": False, "runtime_verified": False,
        "public_prefixes": list(knowledge.PREFIXES), "upstream_prefix": knowledge.UPSTREAM,
        "methods": sorted(m.upper() for m in knowledge.METHODS),
        "response_mode": "streamed-status-content-type-body",
        "source_sha256": {name: native.digest(source_tree / knowledge.NATIVE_ROOT / name)
                          for name in knowledge.NATIVE_SOURCES},
        "transforms": [
            {"method": "DELETE", "suffix": "datasets/{dataset_id}", "kind": "account-password-confirmation"},
            {"method": "GET", "suffix": "datasets/{dataset_id}/impact", "kind": "gateway-owned"},
        ],
    }
    for prefix in knowledge.PREFIXES:
        for method in knowledge.METHODS:
            op = paths[prefix + "/{path}"][method]
            op["x-native-knowledge-proxy"] = True
            op["x-native-response-schema"] = "streamed-owner-declared-or-opaque"
            op["security"] = [{}, {"NativeBearerAuth": []}, {"NativeKnowledgeApiKey": []}]
            op["responses"]["200"]["content"]["application/json"]["schema"] = {}
            op["responses"]["200"]["content"]["*/*"] = {"schema": {}}
            op["responses"]["422"] = {"content": {"application/json": {"schema": {}}, "*/*": {"schema": {}}}}
            op["responses"]["default"]["content"] = {"*/*": {"schema": {}}}
    path = knowledge.UPSTREAM + "/fixture"
    owner = {"document": {"paths": {path: {"get": {"responses": {
                 "200": {"content": {"application/json": {"schema": {}}}}}}}}},
             "route_order": [{"path": path, "method": "get", "handler": "fixture", "source": "scripts/harness/knowledge_proxy_contract.py"}],
             "source_sha256": {"scripts/harness/knowledge_proxy_contract.py": native.digest(source_tree / "scripts/harness/knowledge_proxy_contract.py")},
             "export_dependencies": {"package": "tiktoken", "version": "synthetic-fixture",
                                     "source_sha256": "0" * 64, "asset_sha256": "0" * 64, "asset_bytes": 1,
                                     "route_discovery": copy.deepcopy(knowledge.FASTAPI_ROUTE_DISCOVERY)}}
    return knowledge.compose(document, owner, source_tree)



def test_complete_owned_subset_still_reports_full_contract_gaps(source_tree, document):
    result = native.verify(document, source_tree)
    assert result["native_paths"] == 57
    assert result["native_operations"] == 80
    assert result["published_operations"] == 452
    assert len(result["missing_common_operations"]) == 372
    assert len(result["undeclared_response_operations"]) == 11
    assert len(result["forwarded_response_operations"]) == 10
    assert len(result["bounded_response_operations"]) == 69
    assert len(result["default_response_gaps"]) == 80
    assert result["dynamic_row_schemas_remain_open"] is True
    assert result["full_native_gate_satisfied"] is False
    assert result["native_runtime_verified"] is False


@pytest.mark.parametrize("mutation", ["path", "method", "id", "extra", "source", "response_source", "complete", "response", "success_marker", "default"])
def test_partial_or_mislabeled_producer_rejected(source_tree, document, mutation):
    path = next(iter(document["paths"]))
    method = next(iter(document["paths"][path]))
    if mutation == "path":
        del document["paths"][path]
    elif mutation == "method":
        del document["paths"][path][method]
    elif mutation == "id":
        document["paths"][path][method]["operationId"] = "unreviewed"
    elif mutation == "extra":
        document["paths"]["/unknown"] = {"get": {"operationId": "unknown"}}
    elif mutation == "source":
        document["x-native-contract"]["source_sha256"] = "0" * 64
    elif mutation == "response_source":
        document["x-native-contract"]["response_source_sha256"] = "0" * 64
    elif mutation == "success_marker":
        document["x-native-contract"]["success_responses_declared"] = True
    elif mutation == "default":
        del document["paths"][path][method]["responses"]["default"]
    elif mutation == "complete":
        document["x-native-contract"]["complete"] = True
    else:
        document["paths"][path][method]["responses"]["200"] = {}
    with pytest.raises(ValueError):
        native.verify(document, source_tree)


@pytest.mark.parametrize("mutation", ["constant", "dispatcher", "missing_proof", "outside", "empty"])
def test_shared_and_dispatcher_source_drift_fails_closed(source_tree, document, mutation):
    ownership_path = source_tree / "sdk/openapi-profiles.json"
    ownership = json.loads(ownership_path.read_text())
    if mutation == "constant":
        contract = source_tree / native.CONTRACT
        contract.write_text(contract.read_text().replace('"/api/v1/scheduled-tasks"', '"/removed"'))
        document["x-native-contract"]["source_sha256"] = native.digest(contract)
    elif mutation == "dispatcher":
        source = source_tree / ownership["native_only"][0]["source"]
        source.write_text(source.read_text().replace(ownership["native_only"][0]["source_evidence"][0], "REMOVED"))
    elif mutation == "missing_proof":
        del ownership["native_only"][0]["shared_source_evidence"]
    elif mutation == "outside":
        ownership["native_only"][0]["shared_source_evidence"] = {"../../outside": ["x"]}
    else:
        ownership["native_only"][0]["shared_source_evidence"] = {native.CONTRACT: []}
    ownership_path.write_text(json.dumps(ownership))
    with pytest.raises(ValueError):
        native.verify(document, source_tree)


@pytest.mark.parametrize("section", ["paths", "components", "openapi", "x-native-knowledge-forwarding", "x-python-knowledge-owner"])
def test_strict_previous_export_constraint_drift_is_not_ignored(document, section):
    previous = copy.deepcopy(document)
    document[section] = {"changed_constraint": {"minimum": 2}}
    assert native.declaration_drift(previous, document) == [section]
    assert native.declaration_drift(previous, previous) == []


def test_previous_mixed_publication_cannot_be_relabelled(document):
    with pytest.raises(ValueError, match="bounded"):
        native.declaration_drift({"paths": document["paths"]}, document)


def test_output_aliases_and_repository_paths_rejected(source_tree, tmp_path):
    published = source_tree / "sdk/openapi.json"
    before = published.read_bytes()
    hardlink = tmp_path / "hardlink.json"
    os.link(published, hardlink)
    symlink = tmp_path / "symlink.json"
    symlink.symlink_to(published)
    for target in [published, hardlink, symlink, source_tree / "new.json"]:
        with pytest.raises(ValueError):
            native.safe_output(target, source_tree)
    assert published.read_bytes() == before
    native.safe_output(tmp_path / "safe.json", source_tree)


def test_output_cannot_overwrite_producer(tmp_path, monkeypatch):
    producer = tmp_path / "producer"
    producer.write_text("protected")
    called = []
    monkeypatch.setattr(native.subprocess, "run", lambda *args, **_kwargs: called.append(args))
    assert native.main(["--producer", str(producer), "--output", str(producer),
                        "--coverage-output", str(tmp_path / "coverage")]) == 2
    assert producer.read_text() == "protected"
    assert called == []


def test_output_coverage_hardlink_rejected_before_execution(tmp_path, monkeypatch):
    output = tmp_path / "schema"
    coverage = tmp_path / "coverage"
    output.write_text("protected")
    os.link(output, coverage)
    called = []
    monkeypatch.setattr(native.subprocess, "run", lambda *args, **_kwargs: called.append(args))
    assert native.main(["--producer", str(tmp_path / "producer"), "--output", str(output),
                        "--coverage-output", str(coverage)]) == 2
    assert output.read_text() == "protected"
    assert called == []


@pytest.mark.parametrize("selected", ["schema", "coverage"])
def test_output_cannot_replace_verified_tokenizer_input(tmp_path, monkeypatch, capsys, selected):
    cache = tmp_path / "tokenizer"
    cache.mkdir()
    asset = cache / "asset"
    asset.write_text("protected")
    producer = tmp_path / "producer"
    producer.write_text("not executed")
    called = []
    monkeypatch.setattr(native.subprocess, "run", lambda *args, **_kwargs: called.append(args))
    schema = asset if selected == "schema" else tmp_path / "schema"
    coverage = asset if selected == "coverage" else tmp_path / "coverage"
    assert native.main(["--producer", str(producer), "--output", str(schema),
                        "--coverage-output", str(coverage), "--knowledge-tokenizer-cache", str(cache)]) == 2
    assert "output overlaps verified tokenizer cache" in capsys.readouterr().err
    assert asset.read_text() == "protected"
    assert called == []


@pytest.mark.parametrize("mutation", ["binding", "mount", "ownership", "duplicate", "id"])
def test_common_native_contract_evidence_cannot_hide_python_or_runtime_gaps(source_tree, document, mutation):
    path = source_tree / native.COMMON
    manifest = json.loads(path.read_text())
    if mutation == "binding":
        document["x-native-contract"]["common_source_sha256"]["roles_api.rs"] = "0" * 64
    elif mutation == "mount":
        source = source_tree / "rust/platform-native/crates/platform-gateway/src/migration_host/foundation.rs"
        source.write_text(source.read_text().replace("roles_api::router", "removed::router"))
    elif mutation == "ownership":
        manifest["operations"][0]["path"] = "/api/v1/scheduled-tasks"
    elif mutation == "duplicate":
        manifest["operations"].append(manifest["operations"][0])
    else:
        manifest["operations"][0]["operation_id"] = "changed"
    path.write_text(json.dumps(manifest))
    with pytest.raises(ValueError):
        native.verify(document, source_tree)


@pytest.mark.parametrize("mutation", ["missing_items", "empty_items", "empty_variants", "untyped_item", "empty_item", "required", "open_variant", "not_array", "invalid_property", "invalid_keyword", "invalid_nested_item"])
def test_service_list_array_requires_both_declared_item_shapes(source_tree, document, mutation):
    schema = document["paths"]["/api/v1/services"]["get"]["responses"]["200"]["content"]["application/json"]["schema"]
    if mutation == "missing_items":
        del schema["items"]
    elif mutation == "empty_items":
        schema["items"] = {}
    elif mutation == "empty_variants":
        schema["items"]["anyOf"] = []
    elif mutation == "untyped_item":
        del schema["items"]["anyOf"][0]["type"]
    elif mutation == "empty_item":
        schema["items"]["anyOf"][0]["properties"] = {}
    elif mutation == "required":
        schema["items"]["anyOf"][0]["required"] = []
    elif mutation == "open_variant":
        schema["items"]["anyOf"][0]["additionalProperties"] = True
    elif mutation == "invalid_property":
        schema["items"]["anyOf"][0]["properties"]["fixture"] = 7
    elif mutation == "invalid_keyword":
        schema["items"]["anyOf"][0]["properties"]["fixture"] = {"type": 7}
    elif mutation == "invalid_nested_item":
        schema["items"]["anyOf"][0]["properties"]["fixture"] = {"type": "array", "items": 7}
    else:
        schema["type"] = "object"
    with pytest.raises(ValueError, match="bounded success"):
        native.verify(document, source_tree)


def test_array_exception_cannot_reclassify_other_successes(source_tree, document):
    operation = document["paths"]["/api/v1/services"]["post"]
    operation["responses"]["200"]["content"]["application/json"]["schema"] = copy.deepcopy(
        document["paths"]["/api/v1/services"]["get"]["responses"]["200"]["content"]["application/json"]["schema"])
    with pytest.raises(ValueError, match="bounded success"):
        native.verify(document, source_tree)


@pytest.mark.parametrize("name", ["native_services_contract.rs", "services_api.rs", "protocol.rs", "request_validation.rs", "service_registry.rs", "service_config_api.rs"])
def test_all_service_production_sources_have_fresh_compiled_bindings(source_tree, document, name):
    document["x-native-contract"]["common_source_sha256"][name] = "0" * 64
    with pytest.raises(ValueError, match="compiled producer"):
        native.verify(document, source_tree)


def test_services_router_mount_drift_fails_closed(source_tree, document):
    source = source_tree / "rust/platform-native/crates/platform-gateway/src/migration_host/foundation.rs"
    source.write_text(source.read_text().replace("services_api::router", "removed::router"))
    document["x-native-contract"]["common_source_sha256"]["../../platform-gateway/src/migration_host/foundation.rs"] = native.digest(source)
    with pytest.raises(ValueError, match="dispatcher/schema evidence"):
        native.verify(document, source_tree)


@pytest.mark.parametrize("name", ["native_users_contract.rs", "users_api.rs", "users_store.rs", "users_store/guard.rs", "rate_config_api.rs", "usage_parser.rs"])
def test_all_user_production_sources_have_fresh_compiled_bindings(source_tree, document, name):
    document["x-native-contract"]["common_source_sha256"][name] = "0" * 64
    with pytest.raises(ValueError, match="compiled producer"):
        native.verify(document, source_tree)


@pytest.mark.parametrize("source_name, proof", [
    ("foundation.rs", "users_api::router"),
    ("bootstrap/foundation.rs", "profile_identity: auth.admission.clone(),"),
])
def test_users_router_and_profile_resolver_drift_fail_closed(source_tree, document, source_name, proof):
    source = source_tree / "rust/platform-native/crates/platform-gateway/src/migration_host" / source_name
    source.write_text(source.read_text().replace(proof, "removed_user_contract_proof"))
    name = "../../platform-gateway/src/migration_host/" + source_name
    if name in native.COMMON_SOURCES:
        document["x-native-contract"]["common_source_sha256"][name] = native.digest(source)
    with pytest.raises(ValueError, match="dispatcher/schema evidence"):
        native.verify(document, source_tree)


@pytest.mark.parametrize("name", [
    "../../platform-auth/src/service.rs",
    "session_storage.rs",
    "../../platform-auth/src/api_keys.rs",
    "native_sessions_contract.rs", "../../platform-gateway/src/lib.rs",
    "../../platform-gateway/src/session_history.rs",
    "../../platform-gateway/src/migration_host/bootstrap/graph.rs",
    "../../platform-gateway/src/migration_host/bootstrap/foundation_backends.rs",
    "../../platform-gateway/src/migration_agent/control.rs",
    "../../platform-gateway-integrations/src/migration_adapters/sdk_session_cleanup.rs",
    "sessions_api.rs",
])
def test_assistant_session_compiled_sources_must_match(source_tree, document, name):
    document["x-native-contract"]["common_source_sha256"][name] = "0" * 64
    with pytest.raises(ValueError, match="compiled producer differs"):
        native.verify(document, source_tree)


@pytest.mark.parametrize("relative, proof", [
    ("session_history.rs", ".route(contract::ITEM, get(detail).delete(delete))"),
    ("session_history.rs", "Json(contract::delete_response(session_id))"),
    ("migration_host/bootstrap/graph.rs", "options.sessions.cleanup = Some(self.native_foundation.runtime.clone());"),
    ("migration_host/bootstrap/foundation_backends.rs", "sdk_session_cleanup::delete_owned("),
    ("migration_agent/control.rs", "session_cleanup_result(&scope.session_id, &p)"),
])
def test_assistant_delete_owner_evidence_drift_fails_closed(source_tree, document, relative, proof):
    name = "../../platform-gateway/src/" + relative
    source = source_tree / native.Path(native.CONTRACT).parent / name
    source.write_text(source.read_text().replace(proof, "removed_assistant_delete_proof"))
    # Even a rebuilt producer cannot replace the independently required mount/owner proof.
    document["x-native-contract"]["common_source_sha256"][name] = native.digest(source)
    with pytest.raises(ValueError, match="dispatcher/schema evidence"):
        native.verify(document, source_tree)


def test_assistant_delete_missing_operation_is_not_coverage(source_tree, document):
    del document["paths"]["/api/v1/assistant/sessions/{session_id}"]["delete"]
    with pytest.raises(ValueError, match="native operation ownership differs"):
        native.verify(document, source_tree)


@pytest.mark.parametrize("relative, proof", [
    ("rust/platform-native/crates/platform-auth/src/http.rs", ".route(contract::LOGIN, post(login))"),
    ("rust/platform-native/crates/platform-auth/src/http.rs", "json!(CurrentUserResponse {"),
    ("rust/platform-native/crates/platform-auth/src/service.rs", "pub use crate::contract::{LoginResponse, UserProfile};"),
    ("rust/platform-native/crates/platform-gateway/src/migration_host/foundation.rs", ".merge(ai_platform_auth::http::router(auth.clone(), configured_roles).reset_fallback())"),
])
def test_auth_actual_handler_and_mount_evidence_cannot_be_dropped(source_tree, document, relative, proof):
    target = source_tree / relative
    target.write_text(target.read_text().replace(proof, "// removed auth binding", 1))
    name = os.path.relpath(target, (source_tree / native.CONTRACT).parent)
    document["x-native-contract"]["common_source_sha256"][name] = native.digest(target)
    with pytest.raises(ValueError, match="dispatcher/schema evidence"):
        native.verify(document, source_tree)


def test_auth_common_routes_keep_python_ownership(source_tree, document):
    published = json.loads((source_tree / "sdk/openapi.json").read_text())
    ownership = json.loads((source_tree / "sdk/openapi-profiles.json").read_text())
    common, _ = native.common_implementations(published, ownership, source_tree)
    for method, suffix in [("POST", "login"), ("POST", "logout"), ("GET", "me"),
                           ("POST", "validate-token"), ("POST", "change-password")]:
        path = "/api/v1/auth/" + suffix
        assert path in ownership["python_paths"]
        assert f"{method} {path}" in common
        assert document["paths"][path][method.lower()]["operationId"] == published["paths"][path][method.lower()]["operationId"]


@pytest.mark.parametrize("name", [
    "../../platform-auth/src/contract.rs", "../../platform-auth/src/http.rs",
    "../../platform-auth/src/lib.rs", "../../platform-auth/src/accounts.rs",
    "../../platform-auth/src/sessions.rs", "../../platform-gateway/src/migration_host/foundation.rs",
])
def test_auth_compiled_source_bindings_must_match(source_tree, document, name):
    document["x-native-contract"]["common_source_sha256"][name] = "0" * 64
    with pytest.raises(ValueError, match="compiled producer differs"):
        native.verify(document, source_tree)


@pytest.mark.parametrize("mutation", ["json", "marker", "chunk", "error", "opening", "default"])
def test_generic_invocation_sse_shape_cannot_be_replaced_by_json(source_tree, document, mutation):
    op = document["paths"]["/api/v1/stream"]["post"]
    if mutation == "json":
        op["responses"]["200"]["content"] = {"application/json": {"schema": {"type": "object", "properties": {"fake": {}}}}}
    elif mutation == "marker":
        op["x-native-response-schema"] = "bounded-source-owned"
    elif mutation == "chunk":
        op["x-native-sse"]["data_schema"] = {"type": "object"}
    elif mutation == "error":
        op["x-native-sse"]["error_data"] = "json"
    elif mutation == "opening":
        op["x-native-sse"]["opening"] = "before-response"
    else:
        op["responses"].pop("default")
    with pytest.raises(ValueError, match="SSE declaration"):
        native.verify(document, source_tree)


@pytest.mark.parametrize("items", [{}, {"type": "string"}, {"type": "object", "properties": {"x": {}}, "required": [], "additionalProperties": False}])
def test_generic_task_list_requires_explicit_item_projection(source_tree, document, items):
    document["paths"]["/api/v1/tasks"]["get"]["responses"]["200"]["content"]["application/json"]["schema"]["items"] = items
    with pytest.raises(ValueError, match="bounded success"):
        native.verify(document, source_tree)


@pytest.mark.parametrize("mutation", ["available_type", "available_field", "available_required", "mine_bounded", "mine_items", "callback_200", "callback_content", "callback_location", "callback_auth", "ordinary_auth", "code_required", "error_missing", "state_default", "sixth_operation", "drop_error", "narrow_error", "drop_plaintext"])
def test_connector_declarations_fail_closed(source_tree, document, mutation):
    available = document["paths"][native.CONNECTOR_AVAILABLE]["get"]
    mine = document["paths"][native.CONNECTOR_MINE]["get"]
    callback = document["paths"][native.CONNECTOR_CALLBACK]["get"]
    schema = available["responses"]["200"]["content"]["application/json"]["schema"]
    if mutation == "drop_error":
        callback["responses"].pop("422")
    elif mutation == "narrow_error":
        callback["responses"]["400"]["content"]["application/json"]["schema"] = {"type": "object"}
    elif mutation == "drop_plaintext":
        callback["responses"]["500"]["content"].pop("text/plain")
    elif mutation == "available_type":
        schema["type"] = "object"
    elif mutation == "available_field":
        schema["items"]["properties"]["status"] = {"type": "string"}
    elif mutation == "available_required":
        schema["items"]["required"] = []
    elif mutation == "mine_bounded":
        mine["x-native-response-schema"] = "bounded-source-owned"
    elif mutation == "mine_items":
        mine["responses"]["200"]["content"]["application/json"]["schema"]["items"] = {"type": "object"}
    elif mutation == "callback_200":
        callback["responses"]["200"] = {}
    elif mutation == "callback_content":
        callback["responses"]["302"]["content"] = {"application/json": {"schema": {}}}
    elif mutation == "callback_location":
        callback["responses"]["302"]["headers"] = {}
    elif mutation == "callback_auth":
        callback["security"] = [{"NativeBearerAuth": []}]
    elif mutation == "ordinary_auth":
        available["security"] = [{"NativeBearerAuth": []}]
    elif mutation == "code_required":
        callback["parameters"][1]["required"] = True
    elif mutation == "error_missing":
        callback["parameters"].pop()
    elif mutation == "state_default":
        callback["parameters"][2]["schema"]["default"] = "issued"
    elif mutation == "sixth_operation":
        document["paths"]["/api/v1/connectors/{provider}/activate"] = {"post": {}}
    with pytest.raises(ValueError):
        native.verify(document, source_tree)


@pytest.mark.parametrize("name", ["native_connectors_contract.rs", "connector_user_api.rs", "connector_user_service.rs", "connector_user_store.rs", "connector_user_types.rs", "../../platform-auth/src/admission.rs", "../../platform-gateway/src/migration_host/bootstrap/foundation.rs"])
def test_connector_owner_binding_is_current(source_tree, document, name):
    document["x-native-contract"]["common_source_sha256"][name] = "0" * 64
    with pytest.raises(ValueError, match="differs from common native source"):
        native.verify(document, source_tree)


@pytest.mark.parametrize("name", [
    "native_api_keys_contract.rs", "self_service_api_keys_api.rs", "self_service_api_keys.rs",
    "self_service_api_keys_store.rs", "http_content_type.rs", "service_access_resolver.rs",
])
def test_api_key_owner_source_drift_rejected(source_tree, document, name):
    path = source_tree / native.Path(native.CONTRACT).parent / name
    path.write_text(path.read_text() + "\n// drift\n")
    with pytest.raises(ValueError, match="compiled producer differs"):
        native.verify(document, source_tree)


@pytest.mark.parametrize("change", ["opaque", "extra_secret", "missing_field", "open"])
def test_api_key_list_projection_boundary(source_tree, document, change):
    items = document["paths"]["/api/v1/api-keys"]["get"]["responses"]["200"]["content"]["application/json"]["schema"]["items"]
    if change == "opaque":
        items.clear()
    elif change == "extra_secret":
        items["properties"]["api_key"] = {"type": "string"}
        items["required"].append("api_key")
    elif change == "missing_field":
        del items["properties"]["expires_at"]
    else:
        items["additionalProperties"] = True
    with pytest.raises(ValueError, match="missing bounded success"):
        native.verify(document, source_tree)
