//! Source matrix: tests/contract/test_openapi_response_alternatives.py.
use ai_platform_openapi::gate::compare_specs;
use serde_json::{Value, json};
fn array() -> Value {
    json!({"type":"array","items":{"type":"object"}})
}
fn string() -> Value {
    json!({"type":"string"})
}
fn object(detail: Value) -> Value {
    json!({"type":"object","properties":{"detail":detail},"required":["detail"]})
}
fn spec(schema: Value) -> Value {
    json!({"openapi":"3.1.0","components":{"schemas":{}},"paths":{"/things":{"get":{"operationId":"list_things","responses":{"422":{"content":{"application/json":{"schema":schema}}}}}}}})
}
fn expanded() -> Vec<String> {
    vec!["response field type expanded: GET /things status '422' media 'application/json' field 'detail' 'array' -> 'array|string'".into()]
}
fn lost() -> Vec<String> {
    vec![
        "response field lost: GET /things status '422' media 'application/json' field 'detail'"
            .into(),
    ]
}
#[test]
fn alternatives_preserve_declaration_and_detect_type_expansion() {
    for keyword in ["anyOf", "oneOf"] {
        assert_eq!(
            compare_specs(
                &spec(object(array())),
                &spec(json!({keyword:[object(array()),object(string())]}))
            ),
            expanded()
        );
        assert!(
            compare_specs(
                &spec(object(array())),
                &spec(json!({keyword:[object(array()),object(array())]}))
            )
            .is_empty()
        );
        assert_eq!(
            compare_specs(
                &spec(object(array())),
                &spec(json!({keyword:[object(array()),{"properties":{"message":string()}}]}))
            ),
            lost()
        );
    }
}
#[test]
fn optional_declaration_still_must_survive() {
    for required in [json!([]), json!(["detail"])] {
        let mut baseline = object(array());
        baseline["required"] = required;
        assert_eq!(
            compare_specs(&spec(baseline), &spec(json!({"type":"object"}))),
            lost()
        );
    }
}
#[test]
fn unknown_alternatives_do_not_prove_fields() {
    for unknown in [
        json!({}),
        json!({"$ref":"#/components/schemas/Missing"}),
        json!({"$ref":"https://example.invalid/schema"}),
        json!({"$ref":"#/components/schemas/Cycle"}),
    ] {
        let mut current = spec(json!({"anyOf":[object(array()),unknown]}));
        current["components"]["schemas"] = json!({"Cycle":{"$ref":"#/components/schemas/Other"},"Other":{"$ref":"#/components/schemas/Cycle"}});
        assert_eq!(compare_specs(&spec(object(array())), &current), lost());
    }
}
#[test]
fn local_refs_sibling_reuse_and_recursive_allof() {
    let mut repeated = spec(
        json!({"anyOf":[{"$ref":"#/components/schemas/Error"},{"$ref":"#/components/schemas/Error"}]}),
    );
    repeated["components"]["schemas"]["Error"] = object(array());
    assert!(compare_specs(&spec(object(array())), &repeated).is_empty());
    let mut recursive = spec(json!({"$ref":"#/components/schemas/Error"}));
    recursive["components"]["schemas"]["Error"] =
        json!({"allOf":[object(array()),{"$ref":"#/components/schemas/Error"}]});
    assert!(compare_specs(&spec(object(array())), &recursive).is_empty());
}
#[test]
fn field_type_projection_source_matrix() {
    for (base, current, expands) in [
        (array(), string(), true),
        (array(), json!({"type":["array","null"]}), true),
        (json!({"anyOf":[array(),string()]}), array(), false),
        (json!({"type":"number"}), json!({"type":"integer"}), false),
        (json!({"type":"integer"}), json!({"type":"number"}), true),
        (
            array(),
            json!({"allOf":[{"type":["array","string"]},array()]}),
            false,
        ),
        (
            array(),
            json!({"allOf":[{"anyOf":[array(),string()]},string()]}),
            true,
        ),
        (array(), json!({"oneOf":[array(),string()]}), true),
    ] {
        assert_eq!(
            !compare_specs(&spec(object(base)), &spec(object(current))).is_empty(),
            expands
        );
    }
}
#[test]
fn unsupported_types_are_unverified_not_invented() {
    for unknown in [
        json!({}),
        json!({"$ref":"#/components/schemas/Missing"}),
        json!({"anyOf":[array(),{}]}),
        json!({"not":string()}),
        json!({"type":"string","enum":[]}),
        json!({"oneOf":[array(),{"anyOf":[array(),string()]}]}),
        json!({"$ref":"#/components/schemas/Detail","type":"array"}),
    ] {
        assert!(compare_specs(&spec(object(array())), &spec(object(unknown))).is_empty());
    }
}
#[test]
fn nullable_is_openapi30_only() {
    for (version, expands) in [("3.0.3", true), ("3.1.0", false)] {
        let mut base = spec(object(array()));
        let mut current = spec(object(json!({"type":"array","nullable":true})));
        base["openapi"] = json!(version);
        current["openapi"] = json!(version);
        assert_eq!(!compare_specs(&base, &current).is_empty(), expands);
    }
}
#[test]
fn ref_siblings_follow_dialect() {
    for (version, expands) in [("3.0.3", true), ("3.1.0", false)] {
        let mut base = spec(object(array()));
        let mut current =
            spec(json!({"$ref":"#/components/schemas/Error","properties":{"detail":array()}}));
        current["components"]["schemas"]["Error"] = object(json!({"anyOf":[array(),string()]}));
        base["openapi"] = json!(version);
        current["openapi"] = json!(version);
        assert_eq!(!compare_specs(&base, &current).is_empty(), expands);
    }
}
#[test]
fn enclosing_constraints_block_false_expansion() {
    for constraint in [
        json!({"not":object(string())}),
        json!({"if":object(string()),"then":false}),
        json!({"enum":[{"detail":[]}]}),
        json!({"const":{"detail":[]}}),
        json!({"patternProperties":{"^detail$":array()}}),
    ] {
        let mut current = object(json!({"anyOf":[array(),string()]}));
        current
            .as_object_mut()
            .unwrap()
            .extend(constraint.as_object().unwrap().clone());
        assert!(compare_specs(&spec(object(array())), &spec(current)).is_empty());
    }
}
#[test]
fn nested_alternatives_and_outer_allof() {
    assert_eq!(
        compare_specs(
            &spec(object(array())),
            &spec(json!({"anyOf":[object(array()),{"oneOf":[object(array()),{"type":"string"}]}]}))
        ),
        lost()
    );
    assert_eq!(
        compare_specs(
            &spec(object(array())),
            &spec(json!({"anyOf":[object(array()),{"oneOf":[object(array()),object(string())]}]}))
        ),
        expanded()
    );
    assert!(compare_specs(&spec(object(array())),&spec(json!({"allOf":[object(array()),{"anyOf":[{}, {"properties":{"message":string()}}]}]}))).is_empty());
}
