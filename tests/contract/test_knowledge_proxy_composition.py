"""Real mounted Python declarations + native mapping contract; no running services.

Native document dictionaries below are explicitly synthetic wrapper fixtures.
The Rust producer/runtime helper and signing tests separately verify native code.
"""
from __future__ import annotations

import copy
import hashlib
import json
import os
from pathlib import Path

import pytest

from scripts.harness import knowledge_proxy_contract as contract


@pytest.fixture(scope="module")
def owner():
    return contract.owner_document()


def native_fixture():
    marker = {
        "version": 1, "complete": False, "runtime_verified": False,
        "public_prefixes": list(contract.PREFIXES), "upstream_prefix": contract.UPSTREAM,
        "methods": sorted(m.upper() for m in contract.METHODS),
        "response_mode": "streamed-status-content-type-body",
        "source_sha256": {p: contract.digest(contract.ROOT / contract.NATIVE_ROOT / p)
                          for p in contract.NATIVE_SOURCES},
        "transforms": [
            {"method": "DELETE", "suffix": "datasets/{dataset_id}", "kind": "account-password-confirmation"},
            {"method": "GET", "suffix": "datasets/{dataset_id}/impact", "kind": "gateway-owned"},
        ],
    }
    operation = {"x-native-knowledge-proxy": True,
                 "x-native-response-schema": "streamed-owner-declared-or-opaque",
                 "security": [{}, {"NativeBearerAuth": []}, {"NativeKnowledgeApiKey": []}],
                 "responses": {"200": {"content": {"application/json": {"schema": {}}, "*/*": {"schema": {}}}},
                               "422": {"content": {"application/json": {"schema": {}}, "*/*": {"schema": {}}}},
                               "default": {"x-native-undeclared": True, "content": {"*/*": {"schema": {}}}}}}
    return {"paths": {prefix + "/{path}": {method: copy.deepcopy(operation)
                                          for method in contract.METHODS}
                      for prefix in contract.PREFIXES},
            "components": {"schemas": {}}, "x-native-knowledge-forwarding": marker}


def test_export_uses_actual_required_mounts_without_publication_or_application(monkeypatch):
    import sys

    from knowledge_service.api import route_registry

    original = Path.read_text

    def no_publication(path, *args, **kwargs):
        assert not str(path).endswith(("sdk/openapi.json", "sdk/openapi-profiles.json"))
        return original(path, *args, **kwargs)

    monkeypatch.setattr(Path, "read_text", no_publication)
    main_before = "knowledge_service.main" in sys.modules
    result = contract.owner_document()
    assert ("knowledge_service.main" in sys.modules) == main_before
    assert route_registry.API_MOUNTS == (
        ("capability_plane", ""), ("bm25_v2", "/api/v1"), ("knowledge", "/api/v1"),
        ("confluence_manual_sync", "/api/v1"), ("eval", "/api/v1"),
        ("embedding_migration", "/api/v1"),
    )
    assert all(path.startswith(contract.UPSTREAM + "/") for path in result["document"]["paths"])
    assert not any("/internal/" in path for path in result["document"]["paths"])
    assert any("confluence_manual_sync.py" in route["source"] for route in result["route_order"])
    assert any("embedding_migration.py" in route["source"] for route in result["route_order"])
    assert any("bm25_v2.py" in route["source"] for route in result["route_order"])
    from fastapi import APIRouter
    from fastapi.routing import _IncludedRouter

    mounted = APIRouter()
    route_registry.include_required_api_routes(mounted)
    assert len(mounted.routes) == len(route_registry.API_MOUNTS) == 6
    assert all(isinstance(route, _IncludedRouter) for route in mounted.routes)
    document, effective = contract.mounted_owner_declarations(mounted.routes)
    assert document == result["document"]
    assert [(row["method"], row["path"], row["handler"]) for row in result["route_order"]] == [
        (method.lower(), route.path_format, route.endpoint.__name__)
        for route in effective for method in sorted(route.methods)]
    assert result["export_dependencies"]["route_discovery"] == contract.FASTAPI_ROUTE_DISCOVERY


def test_lazy_nested_routes_keep_framework_context_and_prune_private_models():
    from fastapi import APIRouter, Depends, Header
    from fastapi.openapi.utils import get_openapi
    from fastapi.routing import _IncludedRouter
    from pydantic import create_model

    detail = create_model("DiscoveryDetail", value=(str, ...))
    public_model = create_model("DiscoveryPublic", detail=(detail, ...))
    error_model = create_model("DiscoveryError", reason=(str, ...))
    private_model = create_model("DiscoveryPrivate", secret_shape=(str, ...))

    def outer_dependency(value: str = Header(..., alias="X-Outer")):
        raise AssertionError("declaration discovery must not resolve dependencies")

    def inner_dependency(value: str = Header(..., alias="X-Inner")):
        raise AssertionError("declaration discovery must not resolve dependencies")

    target, middle, leaf, private, hidden = (APIRouter() for _ in range(5))

    @target.get(contract.UPSTREAM + "/direct")
    def direct():
        raise AssertionError("declaration discovery must not call handlers")

    @leaf.get("/items/{item_id}", response_model=public_model, tags=["leaf"])
    def item(item_id: int):
        raise AssertionError("declaration discovery must not call handlers")

    @private.get("/private", response_model=private_model)
    def private_item():
        raise AssertionError("declaration discovery must not call handlers")

    @hidden.get("/hidden", response_model=private_model)
    def hidden_item():
        raise AssertionError("declaration discovery must not call handlers")

    middle.include_router(leaf, prefix="/nested", tags=["inner"],
                          dependencies=[Depends(inner_dependency)],
                          responses={409: {"model": error_model}})
    middle.include_router(hidden, include_in_schema=False)
    target.include_router(middle, prefix=contract.UPSTREAM, tags=["outer"],
                          dependencies=[Depends(outer_dependency)],
                          responses={418: {"description": "outer response"}})
    target.include_router(private, prefix="/api/v1/internal")
    assert isinstance(target.routes[1], _IncludedRouter)
    full = get_openapi(title="fixture", version="source", routes=target.routes)
    document, routes = contract.mounted_owner_declarations(target.routes)
    path = contract.UPSTREAM + "/nested/items/{item_id}"
    assert [(route.path_format, route.endpoint) for route in routes] == [
        (contract.UPSTREAM + "/direct", direct), (path, item)]
    assert list(document["paths"]) == [contract.UPSTREAM + "/direct", path]
    operation = document["paths"][path]["get"]
    assert operation == full["paths"][path]["get"]
    assert operation["tags"] == ["outer", "inner", "leaf"]
    assert {parameter["name"] for parameter in operation["parameters"]} == {
        "item_id", "X-Outer", "X-Inner"}
    assert {"200", "409", "418", "422"} == set(operation["responses"])
    schemas = document["components"]["schemas"]
    assert {"DiscoveryPublic", "DiscoveryDetail", "DiscoveryError"} <= schemas.keys()
    assert "DiscoveryPrivate" in full["components"]["schemas"]
    assert "DiscoveryPrivate" not in schemas
    assert all(schema == full["components"]["schemas"][name] for name, schema in schemas.items())


@pytest.mark.parametrize("mode", ["duplicate", "unsupported"])
def test_lazy_route_discovery_rejects_duplicate_or_unsupported_operations(mode):
    from fastapi import APIRouter

    leaf, target = APIRouter(), APIRouter()

    @leaf.api_route("/items", methods=["GET" if mode == "duplicate" else "OPTIONS"])
    def item():
        raise AssertionError("declaration discovery must not call handlers")

    target.include_router(leaf, prefix=contract.UPSTREAM)
    if mode == "duplicate":
        target.include_router(leaf, prefix=contract.UPSTREAM)
    with pytest.raises(ValueError, match="duplicate or unsupported"):
        contract.mounted_owner_declarations(target.routes)


@pytest.mark.parametrize("mutation", ["missing", "external", "component", "mismatch", "discriminator"])
def test_schema_selection_fails_closed_on_unreviewed_or_missing_shapes(mutation):
    path = contract.UPSTREAM + "/fixture"
    document = {"paths": {path: {"get": {"responses": {
        "200": {"content": {"application/json": {"schema": {"$ref": "#/components/schemas/Public"}}}}}}}},
        "components": {"schemas": {"Public": {"type": "object"}}}}
    if mutation == "missing":
        document["components"]["schemas"] = {}
    elif mutation == "external":
        document["components"]["schemas"]["Public"] = {"$ref": "https://example.invalid/schema"}
    elif mutation == "component":
        document["components"]["securitySchemes"] = {}
    elif mutation == "mismatch":
        document["paths"][path]["post"] = {"responses": {}}
    else:
        document["components"]["schemas"]["Public"] = {
            "discriminator": {"mapping": {"other": "#/components/schemas/Other"}}}
    with pytest.raises(ValueError):
        contract._select_owner_document(document, {("get", path)})


@pytest.mark.parametrize("mutation", ["version", "source"])
def test_route_discovery_requires_reviewed_framework_identity(monkeypatch, mutation):
    assert contract.route_discovery_input() == contract.FASTAPI_ROUTE_DISCOVERY
    altered = copy.deepcopy(contract.FASTAPI_ROUTE_DISCOVERY)
    if mutation == "version":
        altered["version"] = "0.0.0"
    else:
        altered["source_sha256"]["fastapi/routing.py"] = "0" * 64
    monkeypatch.setattr(contract, "FASTAPI_ROUTE_DISCOVERY", altered)
    with pytest.raises(ValueError, match="reviewed FastAPI version/source"):
        contract.route_discovery_input()


def test_request_constraints_are_generated_from_the_owning_model(owner):
    from knowledge_service.api.routes.confluence_manual_sync import ManualSyncRequest

    path = contract.UPSTREAM + "/{dataset_id}/sources/confluence/{binding_id}/sync"
    operation = owner["document"]["paths"][path]["post"]
    assert set(operation["responses"]) == {"202", "422"}
    assert operation["requestBody"]["required"] is True
    name = operation["requestBody"]["content"]["application/json"]["schema"]["$ref"].rsplit("/", 1)[1]
    assert owner["document"]["components"]["schemas"][name] == ManualSyncRequest.model_json_schema()
    assert owner["document"]["components"]["schemas"][name]["additionalProperties"] is False


def test_composition_preserves_aliases_statuses_and_dynamic_gaps(owner):
    document = contract.compose(native_fixture(), owner)
    assert set(document["paths"]) == {prefix + "/{path}" for prefix in contract.PREFIXES}
    report = contract.validate_composition(document, contract.ROOT)
    assert report["knowledge_owner_operations"] == len(owner["route_order"])
    records = {item["handler"]: item for item in document["x-python-knowledge-owner"]["forwarded_operations"]}
    sync = records["start_manual_sync"]
    assert set(sync["operation"]["responses"]) == {"202", "422"}
    assert sync["public_paths"] == [
        prefix + "/{dataset_id}/sources/confluence/{binding_id}/sync" for prefix in contract.PREFIXES]
    assert records["delete_dataset"]["request_mapping"] == "account-password-confirmation"
    assert records["create_source_binding"]["operation"]["responses"].get("201")
    # Real sanitized handlers manually parse Request.json. Their schema is absent.
    connection = records["create_source_connection"]
    assert "requestBody" not in connection["operation"]
    assert f"POST {connection['path']}" in report["knowledge_owner_undeclared_request_body_operations"]
    assert f"POST {sync['path']}" in report["knowledge_owner_opaque_success_operations"]
    assert document["x-python-knowledge-owner"]["complete"] is False


def test_owner_source_or_declaration_mutation_fails_closed(owner):
    changed = copy.deepcopy(owner)
    key = next(iter(changed["source_sha256"]))
    changed["source_sha256"][key] = "0" * 64
    with pytest.raises(ValueError, match="source changed"):
        contract.compose(native_fixture(), changed)
    document = contract.compose(native_fixture(), owner)
    document["x-python-knowledge-owner"]["forwarded_operations"][0]["operation"]["responses"] = {}
    with pytest.raises(ValueError, match="modified"):
        contract.validate_composition(document, contract.ROOT)


@pytest.mark.parametrize("mutation", ["missing", "changed"])
def test_gate_requires_framework_binding_even_with_recomputed_digest(owner, mutation):
    document = contract.compose(native_fixture(), owner)
    dependencies = document["x-python-knowledge-owner"]["export_dependencies"]
    # compose keeps source evidence by reference; isolate the mutation from owner.
    dependencies = copy.deepcopy(dependencies)
    document["x-python-knowledge-owner"]["export_dependencies"] = dependencies
    if mutation == "missing":
        del dependencies["route_discovery"]
    else:
        dependencies["route_discovery"]["version"] = "changed"
    document["x-python-knowledge-owner"]["declarations_sha256"] = contract._composition_digest(document)
    with pytest.raises(ValueError, match="route discovery dependency evidence"):
        contract.validate_composition(document, contract.ROOT)


@pytest.mark.parametrize("mutation", ["prefix", "method", "transform", "hash", "response", "auth", "missing_media", "typed_media"])
def test_native_mapping_auth_transforms_or_response_cannot_be_hidden(owner, mutation):
    document = native_fixture()
    marker = document["x-native-knowledge-forwarding"]
    operation = document["paths"][contract.PREFIXES[0] + "/{path}"]["get"]
    if mutation == "prefix":
        marker["upstream_prefix"] = "/api/v1/other"
    elif mutation == "method":
        marker["methods"].remove("PATCH")
    elif mutation == "transform":
        marker["transforms"] = []
    elif mutation == "hash":
        marker["source_sha256"][next(iter(marker["source_sha256"]))] = "0" * 64
    elif mutation == "auth":
        operation["security"] = [{"NativeBearerAuth": []}]
    elif mutation == "missing_media":
        del operation["responses"]["200"]["content"]["*/*"]
    elif mutation == "typed_media":
        operation["responses"]["default"]["content"]["*/*"]["schema"] = {"type": "string"}
    else:
        operation["responses"]["200"]["content"]["application/json"]["schema"] = {"type": "object"}
    with pytest.raises(ValueError):
        contract.compose(document, owner)


def test_namespacing_keeps_all_owner_refs_resolvable(owner):
    document = contract.compose(native_fixture(), owner)

    def visit(value):
        if isinstance(value, dict):
            if "$ref" in value:
                reference = value["$ref"]
                assert reference.startswith("#/components/schemas/KnowledgeOwner__")
                assert reference.rsplit("/", 1)[1] in document["components"]["schemas"]
            for item in value.values():
                visit(item)
        elif isinstance(value, list):
            for item in value:
                visit(item)

    visit(document["x-python-knowledge-owner"])
    visit(document["components"]["schemas"])


@pytest.mark.parametrize("mutation", ["duplicate", "missing_operation", "internal", "impact", "reference"])
def test_owner_route_mismatch_internal_exposure_and_unknown_refs_fail(owner, mutation):
    changed = copy.deepcopy(owner)
    first = changed["route_order"][0]
    if mutation == "duplicate":
        changed["route_order"].append(first)
    elif mutation == "missing_operation":
        changed["route_order"].pop()
    elif mutation in {"internal", "impact"}:
        path = "/api/v1/internal/private" if mutation == "internal" else contract.UPSTREAM + "/datasets/{dataset_id}/impact"
        changed["route_order"].append({"path": path, "method": "get", "handler": "fixture", "source": first["source"]})
        changed["document"]["paths"][path] = {"get": {"responses": {}}}
    else:
        changed["document"]["components"]["schemas"]["Bad"] = {"$ref": "https://example.invalid/schema"}
    with pytest.raises(ValueError):
        contract.compose(native_fixture(), changed)


def test_existing_native_gate_consumes_composition_and_rejects_stale_mapping(owner, tmp_path):
    from scripts.harness import openapi_contract_gate as gate

    document = contract.compose(native_fixture(), owner)
    candidate, evidence = tmp_path / "candidate.json", tmp_path / "evidence.json"
    candidate.write_text(json.dumps(document))
    baseline = contract.ROOT / "sdk/openapi.json"
    # The deliberately partial fixture cannot pass the full native gate.
    assert gate.run_real_gate(baseline, evidence, "native", candidate) == 1
    report = json.loads(evidence.read_text())
    assert report["knowledge_forwarded_wildcard_operations"] == 10
    assert report["knowledge_owner_operations"] == len(owner["route_order"])
    assert report["knowledge_owner_declaration_only"] is True
    assert report["native_runtime_verified"] is False
    document["x-native-knowledge-forwarding"]["upstream_prefix"] = "/wrong-owner"
    candidate.write_text(json.dumps(document))
    assert gate.run_real_gate(baseline, evidence, "native", candidate) == 2
    assert "forwarding declaration" in json.loads(evidence.read_text())["error"]


def test_missing_or_corrupt_tokenizer_asset_fails_before_router_import(tmp_path, monkeypatch):
    verified = contract.tokenizer_input(Path(os.environ["TIKTOKEN_CACHE_DIR"]))
    monkeypatch.setenv("TIKTOKEN_CACHE_DIR", str(tmp_path))
    with pytest.raises(ValueError, match="missing or corrupt offline"):
        contract.owner_document()
    (tmp_path / hashlib.sha1(verified["url"].encode()).hexdigest()).write_bytes(b"corrupt")
    with pytest.raises(ValueError, match="missing or corrupt offline"):
        contract.owner_document()


def test_owner_route_loading_does_not_initialize_optional_client_packages():
    import sys

    before = set(sys.modules)
    owner = contract.owner_document()
    added = set(sys.modules) - before
    assert owner["route_order"]
    assert not any(name == "qdrant_client" or name.startswith("qdrant_client.")
                   or name == "langchain_core" or name.startswith("langchain_core.")
                   or name == "urllib3" or name.startswith("urllib3.")
                   for name in added)


def test_agent_exports_stay_discoverable_without_initializing_integrations():
    import sys

    before = set(sys.modules)
    from knowledge_service.services import knowledge

    names = {
        "DifyCompatibleKBAPI", "KBSearchResult", "KBToolConfig", "KnowledgeBaseTool",
        "KnowledgeRetriever", "MultiDatasetRetriever", "MultiKnowledgeBaseTool",
        "create_kb_tool", "create_langchain_kb_tool", "create_multi_kb_tool",
        "create_retrieval_tool",
    }
    assert names <= set(knowledge.__all__)
    assert names <= set(dir(knowledge))
    with pytest.raises(AttributeError, match="has no attribute"):
        _ = knowledge._unknown_contract_export
    assert knowledge.__name__ + ".langgraph_tools" not in set(sys.modules) - before


def test_transport_free_deadline_identity_is_used_by_retrieval(monkeypatch):
    from knowledge_service.services.knowledge import retrieval_service
    from knowledge_service.services.knowledge import vector_store_contracts as state

    assert retrieval_service.CollectionReadAuthorityError is state.CollectionReadAuthorityError
    assert issubclass(state.CollectionReadAuthorityError, state.VectorStoreError)
    assert retrieval_service.remaining_interactive_budget_seconds is state.remaining_interactive_budget_seconds
    monkeypatch.setattr(state.time, "monotonic", lambda: 100.0)
    token = state._INTERACTIVE_DEADLINE.set(101.25)
    try:
        assert state.remaining_interactive_budget_seconds() == 1.25
    finally:
        state._INTERACTIVE_DEADLINE.reset(token)
    assert state.remaining_interactive_budget_seconds() is None


def test_single_budget_owner_is_shared_by_worker_and_document_parser(monkeypatch):
    from knowledge_service.core.exceptions import ValidationFailedError
    from knowledge_service.services.knowledge import document_processor, worker
    from knowledge_service.services.knowledge import extracted_text_budget as budget

    assert worker._require_extracted_text_budget is budget._require_extracted_text_budget
    assert document_processor._require_extracted_text_budget is budget._require_extracted_text_budget
    assert worker._require_extracted_text_counts_budget is budget._require_extracted_text_counts_budget
    assert (budget.MAX_EXTRACTED_TEXT_CHARS, budget.MAX_EXTRACTED_TEXT_BYTES) == (16_000_000, 48 * 1024 * 1024)
    monkeypatch.setattr(budget, "MAX_EXTRACTED_TEXT_CHARS", 3)
    monkeypatch.setattr(budget, "MAX_EXTRACTED_TEXT_BYTES", 6)
    assert budget._require_extracted_text_budget("abc") == "abc"
    with pytest.raises(ValidationFailedError, match="character limit"):
        worker._require_extracted_text_budget("abcd")
    with pytest.raises(ValidationFailedError, match="byte limit"):
        document_processor._require_extracted_text_budget("界界界")


def test_refusal_classes_keep_public_qualification_and_shared_route_identity():
    from knowledge_service.api.routes import bm25_v2
    from knowledge_service.services.knowledge import (
        knowledge_service,
        lifecycle_errors,
        vector_store_contracts,
        worker,
    )

    error = lifecycle_errors.Bm25V2LifecycleError
    assert bm25_v2.Bm25V2LifecycleError is knowledge_service.Bm25V2LifecycleError is worker.Bm25V2LifecycleError is error
    assert error.__module__ == "knowledge_service.services.knowledge.bm25_v2_lifecycle"
    assert bm25_v2.VectorStoreError is vector_store_contracts.VectorStoreError
    assert vector_store_contracts.VectorStoreError.__module__ == "knowledge_service.services.knowledge.vector_store"
    refusal = error("blocked", code="fixture", http_status=409)
    assert (str(refusal), refusal.code, refusal.http_status) == ("blocked", "fixture", 409)
