"""Response declaration coverage and value-type diagnostics are separate checks."""

from __future__ import annotations

import copy

import pytest

from scripts.harness import openapi_contract_gate as gate

ARRAY = {"type": "array", "items": {"type": "object"}}
STRING = {"type": "string"}
LABEL = "GET /things status '422' media 'application/json' field 'detail'"


def object_schema(detail=ARRAY, *, tag=None, required=True):
    schema = {"type": "object", "properties": {"detail": copy.deepcopy(detail)}}
    if required:
        schema["required"] = ["detail"]
    if tag is not None:
        schema["properties"]["tag"] = {"const": tag}
        schema.setdefault("required", []).append("tag")
    return schema


def spec(schema, *, components=None):
    return {
        "openapi": "3.1.0",
        "components": {"schemas": components or {}},
        "paths": {"/things": {"get": {
            "operationId": "list_things",
            "responses": {"422": {"content": {"application/json": {"schema": schema}}}},
        }}},
    }


@pytest.mark.parametrize("keyword", ["oneOf", "anyOf"])
def test_every_branch_declares_detail_but_string_expansion_remains_red(keyword):
    baseline = spec(object_schema(required=False))  # Actual publication is optional.
    current = spec({keyword: [object_schema(ARRAY), object_schema(STRING)]})
    assert gate.compare_specs(baseline, current) == [
        f"response field type expanded: {LABEL} 'array' -> 'array|string'"
    ]


@pytest.mark.parametrize("keyword", ["oneOf", "anyOf"])
def test_same_value_type_preserved_in_every_alternative(keyword):
    current = spec({keyword: [object_schema(tag="a"), object_schema(tag="b")]})
    assert gate.compare_specs(spec(object_schema()), current) == []


@pytest.mark.parametrize("keyword", ["oneOf", "anyOf"])
def test_field_missing_from_one_branch_stays_red(keyword):
    current = spec({keyword: [object_schema(), {"type": "object", "properties": {"message": STRING}}]})
    assert gate.compare_specs(spec(object_schema()), current) == [f"response field lost: {LABEL}"]


@pytest.mark.parametrize("required", [True, False])
def test_direct_property_removal_preserves_existing_optional_field_rule(required):
    assert gate.compare_specs(spec(object_schema(required=required)), spec({"type": "object"})) == [
        f"response field lost: {LABEL}"
    ]


def test_optional_field_declarations_do_not_become_requiredness_checks():
    baseline = spec(object_schema())
    current = spec({"anyOf": [object_schema(required=False), object_schema()]})
    assert gate.compare_specs(baseline, current) == []


@pytest.mark.parametrize("keyword", ["oneOf", "anyOf"])
def test_local_refs_and_allof_preserve_fields_and_type_expansion(keyword):
    baseline = spec({"$ref": "#/components/schemas/Error"}, components={"Error": object_schema()})
    current = spec({keyword: [
        {"allOf": [{"$ref": "#/components/schemas/Error"}, {"properties": {"extra": STRING}}]},
        {"$ref": "#/components/schemas/TextError"},
    ]}, components={"Error": object_schema(), "TextError": object_schema(STRING)})
    assert gate.compare_specs(baseline, current) == [
        f"response field type expanded: {LABEL} 'array' -> 'array|string'"
    ]


def test_repeated_ref_in_sibling_alternatives_is_not_a_cycle():
    ref = {"$ref": "#/components/schemas/Error"}
    current = spec({"anyOf": [ref, ref]}, components={"Error": object_schema()})
    assert gate.compare_specs(spec(object_schema()), current) == []


def test_nested_alternatives_cannot_hide_missing_property():
    current = spec({"anyOf": [object_schema(), {"oneOf": [object_schema(), {"type": "string"}]}]})
    assert gate.compare_specs(spec(object_schema()), current) == [f"response field lost: {LABEL}"]


def test_nested_alternatives_preserve_real_type_expansion():
    current = spec({"anyOf": [object_schema(), {"oneOf": [object_schema(), object_schema(STRING)]}]})
    assert gate.compare_specs(spec(object_schema()), current) == [
        f"response field type expanded: {LABEL} 'array' -> 'array|string'"
    ]


def test_outer_allof_declaration_applies_to_all_alternatives():
    current = spec({"allOf": [object_schema(), {"anyOf": [{}, {"properties": {"message": STRING}}]}]})
    assert gate.compare_specs(spec(object_schema()), current) == []


@pytest.mark.parametrize("unknown", [
    {}, {"$ref": "#/components/schemas/Missing"}, {"$ref": "https://example.invalid/schema"},
    {"$ref": "#/components/schemas/Cycle"},
])
def test_unknown_alternative_never_proves_property_coverage(unknown):
    current = spec({"anyOf": [object_schema(), unknown]}, components={
        "Cycle": {"$ref": "#/components/schemas/Other"},
        "Other": {"$ref": "#/components/schemas/Cycle"},
    })
    assert gate.compare_specs(spec(object_schema()), current) == [f"response field lost: {LABEL}"]


def test_recursive_allof_does_not_erase_independently_declared_property():
    current = spec({"$ref": "#/components/schemas/Error"}, components={
        "Error": {"allOf": [object_schema(), {"$ref": "#/components/schemas/Error"}]},
    })
    assert gate.compare_specs(spec(object_schema()), current) == []


def test_in_memory_cycle_terminates_without_inventing_a_field():
    schema = {"anyOf": [object_schema()]}
    schema["anyOf"].append(schema)
    assert gate.compare_specs(spec(object_schema()), spec(schema)) == [f"response field lost: {LABEL}"]


@pytest.mark.parametrize("base_type,current_type,expands", [
    (ARRAY, STRING, True),
    (ARRAY, {"type": ["array", "null"]}, True),
    ({"anyOf": [ARRAY, STRING]}, ARRAY, False),
    ({"type": "number"}, {"type": "integer"}, False),
    ({"type": "integer"}, {"type": "number"}, True),
    (ARRAY, {"allOf": [{"type": ["array", "string"]}, ARRAY]}, False),
    (ARRAY, {"allOf": [{"anyOf": [ARRAY, STRING]}, STRING]}, True),
    (ARRAY, {"oneOf": [ARRAY, STRING]}, True),
])
def test_field_value_type_projection(base_type, current_type, expands):
    violations = gate.compare_specs(spec(object_schema(base_type)), spec(object_schema(current_type)))
    assert bool(violations) is expands
    assert all(v.startswith("response field type expanded:") for v in violations)


def test_property_type_references_are_resolved():
    baseline = spec(object_schema({"$ref": "#/components/schemas/Detail"}), components={"Detail": ARRAY})
    current = spec(object_schema({"$ref": "#/components/schemas/Detail"}), components={"Detail": {
        "anyOf": [ARRAY, STRING],
    }})
    assert gate.compare_specs(baseline, current) == [
        f"response field type expanded: {LABEL} 'array' -> 'array|string'"
    ]


@pytest.mark.parametrize("unknown", [
    {}, {"$ref": "#/components/schemas/Missing"}, {"$ref": "#/components/schemas/Cycle"},
    {"anyOf": [ARRAY, {}]}, {"not": STRING}, {"type": "string", "enum": []},
    {"oneOf": [ARRAY, {"anyOf": [ARRAY, STRING]}]},
    {"$ref": "#/components/schemas/Detail", "type": "array"},
])
def test_unsupported_value_shape_is_not_invented(unknown):
    current = spec(object_schema(unknown), components={
        "Cycle": {"$ref": "#/components/schemas/Cycle"}, "Detail": STRING,
    })
    assert gate._schema_types(current, unknown) is None
    # Explicit limitation: field declaration survives, but value compatibility is unverified.
    assert gate.compare_specs(spec(object_schema()), current) == []


@pytest.mark.parametrize("version,expanded", [("3.0.3", True), ("3.1.0", False)])
def test_nullable_only_changes_openapi30_type_projection(version, expanded):
    baseline = spec(object_schema())
    current = spec(object_schema({**ARRAY, "nullable": True}))
    baseline["openapi"] = current["openapi"] = version
    assert bool(gate.compare_specs(baseline, current)) is expanded


def test_other_contract_diagnostics_survive_alternative_fix():
    baseline = spec(object_schema())
    baseline["paths"]["/removed"] = {"get": {"responses": {"200": {}}}}
    baseline["paths"]["/things"]["get"]["responses"]["404"] = {}
    current = spec({"oneOf": [object_schema(ARRAY), object_schema(STRING)]})
    current["paths"]["/things"]["get"]["security"] = [{"Bearer": []}]
    assert gate.compare_specs(baseline, current) == [
        "security requirements changed: GET /things",
        "response status lost: GET /things status '404'",
        f"response field type expanded: {LABEL} 'array' -> 'array|string'",
        "path removed: /removed",
    ]


def test_native_style_union_really_requires_detail_and_admits_incompatible_string():
    from jsonschema import Draft202012Validator

    baseline = object_schema(required=False)
    current = {"oneOf": [object_schema(ARRAY), object_schema(STRING)]}
    assert not Draft202012Validator(current).is_valid({})
    assert Draft202012Validator(current).is_valid({"detail": "invalid role"})
    assert not Draft202012Validator(baseline).is_valid({"detail": "invalid role"})
    assert gate.compare_specs(spec(baseline), spec(current)) == [
        f"response field type expanded: {LABEL} 'array' -> 'array|string'"
    ]


@pytest.mark.parametrize("version,expanded", [("3.0.3", True), ("3.1.0", False)])
def test_response_ref_siblings_follow_schema_dialect(version, expanded):
    baseline = spec(object_schema())
    current = spec({"$ref": "#/components/schemas/Error", "properties": {"detail": ARRAY}}, components={
        "Error": object_schema({"anyOf": [ARRAY, STRING]}),
    })
    baseline["openapi"] = current["openapi"] = version
    assert bool(gate.compare_specs(baseline, current)) is expanded


def test_response_ref_sibling_property_is_retained_in_openapi31():
    current = spec({"$ref": "#/components/schemas/Error", "properties": {"detail": ARRAY}}, components={
        "Error": {"type": "object", "properties": {"message": STRING}},
    })
    assert gate.compare_specs(spec(object_schema()), current) == []


def test_response_oneof_exclusivity_is_not_rewritten_as_anyof():
    from jsonschema import Draft202012Validator

    current = {"oneOf": [object_schema({"anyOf": [ARRAY, STRING]}), object_schema(STRING)]}
    assert Draft202012Validator(current).is_valid({"detail": []})
    assert not Draft202012Validator(current).is_valid({"detail": "excluded overlap"})
    assert gate.compare_specs(spec(object_schema()), spec(current)) == []
    assert gate._schema_types(spec(current), gate._schema_properties(spec(current), current)["detail"]) is None


@pytest.mark.parametrize("constraint", [
    {"not": object_schema(STRING)},
    {"if": object_schema(STRING), "then": False},
    {"enum": [{"detail": []}]},
    {"const": {"detail": []}},
    {"patternProperties": {"^detail$": ARRAY}},
])
def test_enclosing_constraint_does_not_produce_false_type_expansion(constraint):
    from jsonschema import Draft202012Validator

    current = {**object_schema({"anyOf": [ARRAY, STRING]}), **constraint}
    assert Draft202012Validator(current).is_valid({"detail": []})
    assert not Draft202012Validator(current).is_valid({"detail": "excluded"})
    properties = gate._schema_properties(spec(current), current)
    assert "detail" in properties
    assert gate._response_types_supported(spec(current), current) is False
    assert gate.compare_specs(spec(object_schema()), spec(current)) == []


@pytest.mark.parametrize("use_ref", [False, True])
def test_enclosing_allof_constraint_without_properties_still_blocks_type_projection(use_ref):
    from jsonschema import Draft202012Validator

    constraint = {"not": object_schema(STRING)}
    current = spec({"allOf": [
        object_schema({"anyOf": [ARRAY, STRING]}),
        {"$ref": "#/components/schemas/Constraint"} if use_ref else constraint,
    ]}, components={"Constraint": constraint})
    schema = current["paths"]["/things"]["get"]["responses"]["422"]["content"]["application/json"]["schema"]
    assert not Draft202012Validator({**current, **schema}).is_valid({"detail": "excluded"})
    assert gate._response_types_supported(current, schema) is False
    assert gate.compare_specs(spec(object_schema()), current) == []


def test_nonobject_response_cannot_prove_a_property_value_expansion():
    current = object_schema({"anyOf": [ARRAY, STRING]})
    current["type"] = "string"
    assert gate._response_types_supported(spec(current), current) is False
    assert gate.compare_specs(spec(object_schema()), spec(current)) == []
