use ai_platform_openapi::{breaking, stream_schema, validation};
use serde_json::{Value, json};
fn document() -> Value {
    let mut value = json!({"openapi":"3.1.0","info":{"title":"Synthetic release contract fixture","version":"1.2.3"},
        "paths":{"/stream":{"get":{"operationId":"stream","x-native-sse":{"data_schema":{"type":"object","required":["delta"],"properties":{"delta":{"type":"string"}}}},
        "responses":{"200":{"description":"stream","content":{"text/event-stream":{"schema":{"type":"string"}}}}}}}},
        "components":{"schemas":{"Payload":{"type":"string"}},"securitySchemes":{"Bearer":{"type":"http","scheme":"bearer"}}}});
    stream_schema::upgrade(&mut value);
    value
}
#[test]
fn official_schema_and_stream_records_validate() {
    let value = document();
    validation::validate(&value).unwrap();
    for event in [
        json!({"data":"{\"delta\":\"hello\"}"}),
        json!({"event":"error","data":"provider unavailable"}),
        json!({"data":"{\"delta\":\"x\"}","retry":0,"id":"42"}),
    ] {
        validation::validate_sse_event(&value, "/stream", "get", &event).unwrap();
    }
    for event in [
        json!({"data":{"delta":"x"}}),
        json!({"data":"not JSON"}),
        json!({"data":"{\"delta\":1}"}),
        json!({"event":"error","data":"x","retry":-1}),
        json!({"data":"{}"}),
    ] {
        assert!(
            validation::validate_sse_event(&value, "/stream", "get", &event).is_err(),
            "{event}"
        );
    }
}
#[test]
fn official_schema_rejects_invalid_schema_assertions_and_missing_items() {
    let value = document();
    // A broken initializer must never make all negative cases appear to pass.
    validation::validate(&value)
        .expect("official validator must initialize and accept the positive control");
    for pointer in [
        "/info/title",
        "/paths/~1stream/get/responses/200/description",
    ] {
        let mut bad = value.clone();
        *bad.pointer_mut(pointer).unwrap() = json!(7);
        let error = validation::validate(&bad).unwrap_err();
        assert!(!error.contains("initialization"), "{error}");
    }
    let mut bad = value.clone();
    bad["components"]["schemas"]["Payload"]["type"] = json!("invalid-json-schema-type");
    let error = validation::validate(&bad).unwrap_err();
    assert!(!error.contains("initialization"), "{error}");
    let mut bad = value;
    bad["paths"]["/stream"]["get"]["responses"]["200"]["content"]["text/event-stream"]
        .as_object_mut()
        .unwrap()
        .remove("itemSchema");
    let error = validation::validate(&bad).unwrap_err();
    assert!(!error.contains("initialization"), "{error}");
}
#[test]
fn changed_stream_schema_security_parameters_and_unknown_assertions_block() {
    let old = document();
    let mut cases = vec![];
    let mut new = old.clone();
    new["paths"]["/stream"]["get"]["responses"]["200"]["content"]["text/event-stream"]["itemSchema"] =
        json!({"type":"string"});
    cases.push(new);
    let mut new = old.clone();
    new["security"] = json!([{"Bearer":[]}]);
    cases.push(new);
    let mut new = old.clone();
    new["paths"]["/stream"]["parameters"] =
        json!([{"name":"x","in":"query","required":true,"schema":{"type":"string"}}]);
    cases.push(new);
    let mut new = old.clone();
    new["paths"]["/stream"]["get"]["responses"]["200"]["content"]["text/event-stream"]["schema"]
        ["unevaluatedProperties"] = json!(false);
    cases.push(new);
    for new in cases {
        assert!(!breaking::compare(&old, &new).unwrap().is_empty());
    }
}
#[test]
fn referenced_component_change_and_unresolved_external_refs_block() {
    let mut old = document();
    old["paths"]["/stream"]["get"]["responses"]["200"]["content"]["application/json"] =
        json!({"schema":{"$ref":"#/components/schemas/Payload"}});
    let mut new = old.clone();
    new["components"]["schemas"]["Payload"]["type"] = json!("integer");
    assert!(!breaking::compare(&old, &new).unwrap().is_empty());
    for reference in [
        "https://unbundled.invalid/schema",
        "#/components/schemas/Absent",
    ] {
        let mut bad = old.clone();
        bad["paths"]["/stream"]["get"]["requestBody"] =
            json!({"content":{"application/json":{"schema":{"$ref":reference}}}});
        assert!(breaking::compare(&bad, &bad).is_err());
    }
}
#[test]
fn additive_operations_annotations_and_explicit_major_release() {
    let old = document();
    let mut new = old.clone();
    new["info"]["version"] = json!("1.2.4");
    new["paths"]["/stream"]["get"]["summary"] = json!("updated docs");
    new["paths"]["/new"] = new["paths"]["/stream"].clone();
    assert!(breaking::compare(&old, &new).unwrap().is_empty());
    new["paths"].as_object_mut().unwrap().remove("/stream");
    assert!(breaking::authorize(&old, &new, true).is_err());
    new["info"]["version"] = json!("2.0.0");
    assert!(!breaking::authorize(&old, &new, true).unwrap().is_empty());
    assert!(!breaking::authorize(&old, &new, false).unwrap().is_empty());
}
#[test]
fn jsonl_gets_record_schema_without_replacing_wire_schema() {
    let mut doc = document();
    doc["paths"]["/stream"]["get"]["responses"]["200"]["content"]["application/x-ndjson"] =
        json!({"schema":{"type":"string"}});
    stream_schema::upgrade(&mut doc);
    let media =
        &doc["paths"]["/stream"]["get"]["responses"]["200"]["content"]["application/x-ndjson"];
    assert_eq!(media["schema"]["type"], "string");
    assert!(media["itemSchema"].is_object());
    validation::validate(&doc).unwrap();
}
#[test]
fn annotation_names_inside_payload_maps_are_real_contract_fields() {
    let mut old = document();
    old["paths"]["/stream"]["get"]["responses"]["200"]["content"]["application/json"] = json!({"schema":{"type":"object","properties":{"description":{"type":"string"},"x-user-field":{"enum":[{"description":"a"}]}}}});
    for key in ["description", "x-user-field"] {
        let mut new = old.clone();
        new["paths"]["/stream"]["get"]["responses"]["200"]["content"]["application/json"]["schema"]
            ["properties"]
            .as_object_mut()
            .unwrap()
            .remove(key);
        assert!(!breaking::compare(&old, &new).unwrap().is_empty());
    }
}
#[test]
fn version_and_cli_inputs_fail_closed() {
    for version in ["1.2", "01.2.3", "one.2.3", "1.2.3.4"] {
        assert!(breaking::major(version).is_err());
    }
    for args in [
        vec!["validate", "--candidate"],
        vec!["breaking", "--baseline", "x", "--unknown"],
        vec![
            "produce",
            "--release-version",
            "1.0.0",
            "--release-version",
            "2.0.0",
        ],
    ] {
        let args: Vec<_> = args.into_iter().map(str::to_owned).collect();
        assert_eq!(
            ai_platform_openapi::release::run(&args, Some(document())),
            2
        );
    }
}
#[test]
fn actual_native_producer_passes_official_schema() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_ai-platform-openapi"))
        .arg("--native-only")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let document: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(document["openapi"], "3.2.0");
    validation::validate(&document).unwrap();
}
#[test]
fn referenced_path_items_and_additional_http_methods_are_not_skipped() {
    let mut old = document();
    old["components"]["pathItems"]["Shared"] = old["paths"]["/stream"].clone();
    old["paths"]["/stream"] = json!({"$ref":"#/components/pathItems/Shared"});
    let mut new = old.clone();
    new["components"]["pathItems"]["Shared"]["get"]["responses"]["200"]["description"] =
        json!("docs only");
    assert!(breaking::compare(&old, &new).unwrap().is_empty());
    new["components"]["pathItems"]["Shared"]["get"]["operationId"] = json!("new-id");
    assert!(!breaking::compare(&old, &new).unwrap().is_empty());
    let mut old = document();
    old["paths"]["/stream"]["additionalOperations"] =
        json!({"CUSTOM":{"responses":{"200":{"description":"ok"}}}});
    let mut new = old.clone();
    new["paths"]["/stream"]["additionalOperations"]["CUSTOM"]["responses"] =
        json!({"201":{"description":"changed"}});
    assert!(!breaking::compare(&old, &new).unwrap().is_empty());
}
#[test]
fn stream_upgrade_does_not_insert_absent_optional_response_fields() {
    let mut doc = document();
    doc["paths"]["/stream"]["get"]["responses"]["default"] =
        json!({"description":"unspecified failure"});
    stream_schema::upgrade(&mut doc);
    assert!(
        doc["paths"]["/stream"]["get"]["responses"]["default"]
            .get("content")
            .is_none()
    );
    validation::validate(&doc).unwrap();
}
