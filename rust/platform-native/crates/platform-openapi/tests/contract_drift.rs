//! Port of openapi_contract_gate.py synthetic drift selftest.
use ai_platform_openapi::gate::{compare_specs, operation_id_drift, operation_ids};
use serde_json::{Value, json};
fn baseline() -> Value {
    json!({"openapi":"3.1.0","components":{"securitySchemes":{"BearerAuth":{"type":"http","scheme":"bearer"}},"schemas":{"CreateThing":{"type":"object","required":["name","kind"],"properties":{"name":{"type":"string"},"kind":{"type":"string"},"note":{"type":"string"}}},"ThingResponse":{"type":"object","required":["id"],"properties":{"id":{"type":"string"},"name":{"type":"string"}}}}},"paths":{"/things":{"post":{"operationId":"create_thing","requestBody":{"content":{"application/json":{"schema":{"$ref":"#/components/schemas/CreateThing"}}}},"responses":{"201":{"content":{"application/json":{"schema":{"$ref":"#/components/schemas/ThingResponse"}}}},"422":{}}},"get":{"operationId":"list_things","parameters":[{"name":"limit","in":"query","required":false,"schema":{"type":"integer","enum":[10,20]}}],"responses":{"200":{}}}},"/things/{id}":{"get":{"operationId":"get_thing","responses":{"200":{},"404":{}}}}}})
}
#[test]
fn source_synthetic_drift_matrix() {
    let base = baseline();
    assert!(compare_specs(&base, &base).is_empty());
    let changes = [
        ("/paths/~1things~1{id}", Value::Null),
        ("/paths/~1things/get", Value::Null),
        (
            "/components/schemas/CreateThing/required",
            json!(["name", "kind", "note"]),
        ),
        ("/paths/~1things/post/operationId", json!("make_thing")),
        ("/paths/~1things/post/responses/422", Value::Null),
        ("/paths/~1things/get/parameters", json!([])),
        ("/paths/~1things/get/parameters/0/required", json!(true)),
        ("/paths/~1things/get/parameters/0/schema/enum", json!([10])),
        (
            "/components/schemas/ThingResponse/properties/name",
            Value::Null,
        ),
        ("/components/securitySchemes", json!({})),
    ];
    for (pointer, value) in changes {
        let mut current = base.clone();
        if value.is_null() {
            let (parent, key) = pointer.rsplit_once('/').unwrap();
            let key = key.replace("~1", "/").replace("~0", "~");
            current
                .pointer_mut(parent)
                .unwrap()
                .as_object_mut()
                .unwrap()
                .remove(&key);
        } else {
            *current.pointer_mut(pointer).unwrap() = value;
        }
        assert!(
            !compare_specs(&base, &current).is_empty(),
            "drift not detected: {pointer}"
        );
    }
}
#[test]
fn compatible_growth_and_relaxed_requiredness_pass() {
    let base = baseline();
    let mut current = base.clone();
    current["components"]["schemas"]["CreateThing"]["required"] = json!(["name"]);
    current["paths"]["/things/new"] =
        json!({"get":{"operationId":"new_thing","responses":{"200":{}}}});
    current["paths"]["/things"]["post"]["responses"]["202"] = json!({});
    current["components"]["schemas"]["CreateThing"]["properties"]["extra"] =
        json!({"type":"string"});
    assert!(compare_specs(&base, &current).is_empty());
}
#[test]
fn operation_id_process_drift_is_detected() {
    let expected = operation_ids(&baseline());
    let mut observed = expected.clone();
    observed.insert("POST /things".into(), json!("hash_seed_dependent_id"));
    let drift = operation_id_drift(&expected, &observed);
    assert_eq!(drift.len(), 1);
    assert!(drift[0].contains("POST /things"));
}
