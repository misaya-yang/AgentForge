//! Source-owned common service lifecycle contract. No published-schema input.
//! The route inventory and mutation/list/schema builders are shared with ServicesApi.
//! Register/update accept dictionaries; registry conversion is deliberately not a
//! made-up strict request DTO. Dynamic service values remain unconstrained.
use serde_json::{Value, json};

pub const SERVICES_ROUTES: &[(&str, &str, &str)] = &[
    ("POST", "/services", "register_service"),
    ("GET", "/services", "list_services"),
    ("GET", "/services/{service_id}", "get_service"),
    ("PUT", "/services/{service_id}", "update_service"),
    ("DELETE", "/services/{service_id}", "delete_service"),
    ("GET", "/services/{service_id}/schema", "get_service_schema"),
];
pub const SERVICE_MUTABLE_FIELDS: &[&str] = &[
    "name",
    "description",
    "version",
    "service_type",
    "supported_modes",
    "connector_type",
    "connector_config",
    "input_schema",
    "output_schema",
    "accepted_content_types",
    "output_content_types",
    "session_enabled",
    "session_adapter",
    "timeout",
    "max_retries",
    "retry_delay",
    "circuit_breaker_enabled",
    "failure_threshold",
    "recovery_timeout",
    "rate_limit",
    "concurrency_limit",
    "status",
    "tags",
    "metadata",
    "async_config",
    "service_config",
];
pub fn allowed_methods(path: &str) -> Vec<&'static str> {
    SERVICES_ROUTES
        .iter()
        .filter(|(_, route, _)| path.strip_prefix("/api/v1") == Some(*route))
        .map(|(method, _, _)| *method)
        .collect()
}
pub fn is_dictionary(body: &Value) -> bool {
    body.is_object()
}
pub fn dictionary_schema() -> Value {
    json!({"type":"object","additionalProperties":true,
        "x-native-validation":"Existing dynamic dictionary boundary. Registration uses registry coercion/defaults and upserts; update ignores keys outside the shared mutable-field list and rejects a changed service_id. Null values are supplied to existing conversion, not treated as a generic no-op. No strict ServiceDefinition request DTO is claimed."})
}
fn object(properties: Value) -> Value {
    let required: Vec<_> = properties
        .as_object()
        .expect("schema properties")
        .keys()
        .cloned()
        .collect();
    json!({"type":"object","properties":properties,"required":required,"additionalProperties":false})
}
fn strings() -> Value {
    json!({"type":"array","items":{"type":"string"}})
}
fn dynamic() -> Value {
    json!({"x-native-dynamic-value":true})
}
macro_rules! shape {
    ($value:ident, $schema:ident, { $($field:ident: $ty:ty => $field_schema:expr),* $(,)? }) => {
        pub fn $value($($field:$ty),*) -> Value { json!({$(stringify!($field):$field),*}) }
        pub fn $schema() -> Value { object(json!({$(stringify!($field):$field_schema),*})) }
    };
}
pub fn registered(service_id: &str) -> Value {
    json!({"service_id":service_id,"status":"registered"})
}
pub fn changed(service_id: &str) -> Value {
    json!({"status":"success","service_id":service_id})
}
shape!(schema_value, schema_schema, {
    input_schema: &Value => dynamic(), output_schema: &Value => dynamic(),
    accepted_content_types: Vec<&str> => strings(), output_content_types: Vec<&str> => strings()
});
shape!(listed_service, listed_service_schema, {
    service_id: &str => json!({"type":"string"}), name: &str => json!({"type":"string"}),
    description: &str => json!({"type":"string"}), version: &str => json!({"type":"string"}),
    service_type: &str => json!({"type":"string"}), supported_modes: Vec<&str> => strings(),
    accepted_content_types: Vec<&str> => strings(), output_content_types: Vec<&str> => strings(),
    status: &str => json!({"type":"string"}), tags: &[String] => strings(), metadata: &Value => dynamic(),
    availability_status: &Value => dynamic(), last_health_check_at: &Value => dynamic(), last_health_error: &Value => dynamic()
});
pub fn virtual_assistant(control_present: bool, image_worker_running: bool) -> Value {
    json!({"service_id":"assistant","name":"AI 助手",
        "description":"内置多功能 AI 助手，支持知识库检索、网页搜索、工具调用、图像生成",
        "version":"2.0.0","service_type":"assistant","supported_modes":["chat","stream"],
        "accepted_content_types":["application/json"],"output_content_types":["application/json","text/event-stream"],
        "status":if control_present && image_worker_running {"active"} else {"unavailable"},
        "tags":["builtin","assistant","rag","tools"],"metadata":{"is_virtual":true,"endpoint":"/api/v1/assistant",
        "health":{"status":if control_present {"healthy"} else {"unavailable"},
            "worker":if image_worker_running {"healthy"} else {"unavailable"}},
        "features":["chat","stream","rag","web_search","tools","image_generation"]}})
}
fn virtual_assistant_schema() -> Value {
    let mut schema = listed_service_schema();
    for key in [
        "availability_status",
        "last_health_check_at",
        "last_health_error",
    ] {
        schema["properties"].as_object_mut().unwrap().remove(key);
    }
    let required: Vec<_> = schema["properties"]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect();
    schema["required"] = json!(required);
    schema
}
pub fn list_schema() -> Value {
    json!({"type":"array","items":{"anyOf":[listed_service_schema(),virtual_assistant_schema()]},
        "x-native-variants":"Virtual assistant precedes registered rows and has no availability fields. Dynamic metadata and availability values remain open."})
}
/// ServiceDefinition serde plus service_to_dict owns this projection. Do not
/// narrow its Value members, optional service_config, or future domain fields.
/// The compiler source binding and production serializer fixtures cover both.
pub fn detail_schema() -> Value {
    json!({"type":"object","properties":{
        "service_id":{"type":"string"},"name":{"type":"string"},
        "connector_config":dynamic(),"input_schema":dynamic(),"output_schema":dynamic(),
        "metadata":dynamic(),"rate_limit":dynamic(),"async_config":dynamic(),"service_config":dynamic()},
        "required":["service_id","name","connector_config","input_schema","output_schema","metadata","rate_limit","async_config"],
        "additionalProperties":true,"x-native-dynamic-row":true,
        "x-native-projection":"ServiceDefinition serde followed by service_to_dict; optional service_config omitted when absent. This intentionally declares only bounded identity and dynamic fields, not the complete domain record. SDK credential keys are removed even for admin."})
}
pub(crate) fn success_schema(id: &str) -> Value {
    match id {
        "register_service" => {
            object(json!({"service_id":{"type":"string"},"status":{"const":"registered"}}))
        }
        "update_service" | "delete_service" => {
            object(json!({"status":{"const":"success"},"service_id":{"type":"string"}}))
        }
        "get_service_schema" => schema_schema(),
        "list_services" => list_schema(),
        "get_service" => detail_schema(),
        _ => panic!("unowned common service operation: {id}"),
    }
}
fn response(description: &str, schema: Value) -> Value {
    json!({"description":description,"content":{"application/json":{"schema":schema}}})
}
pub fn extend_document(document: &mut Value) {
    document["components"]["schemas"]["NativeServiceDictionary"] = dictionary_schema();
    for (method, route, id) in SERVICES_ROUTES {
        let path = format!("/api/v1{route}");
        let mut parameters = Vec::new();
        if route.contains("{service_id}") {
            parameters.push(
                json!({"name":"service_id","in":"path","required":true,"schema":{"type":"string"}}),
            );
        }
        if *id == "list_services" {
            parameters.push(json!({"name":"service_type","in":"query","required":false,
                "schema":{"type":["string","null"]},"description":"Last occurrence wins. Unknown values retain the existing internal-error behavior; assistant also includes registered rows."}));
            parameters.push(json!({"name":"tags","in":"query","required":false,"style":"form","explode":true,
                "schema":{"type":["array","null"],"items":{"type":"string"}},"description":"Repeated tags are retained, including empty values. Tags filter registered rows, not the virtual assistant. Other query keys are ignored."}));
        }
        let mut operation = json!({"operationId":super::roles::operation_id(method,&path,id),"parameters":parameters,
            "security":[{"NativeBearerAuth":[]},{"NativeBoundApiKey":[]}],"x-native-owner":"services_api::ServicesApi",
            "x-native-response-schema":"bounded-source-owned",
            "x-native-authorization":"AdmissionService resolves authority per request. Reads require ServiceListRead; mutations require ServiceConfigWrite and explicit platform-admin role/permission. List/detail intersect key and tenant visibility; only exact admin bypasses user policy. Schema lookup retains its existing lack of detail visibility checks.",
            "x-native-cancellation":"Dropping an awaited operation does not undo prior persistence. Cancellation and transport response bodies remain host-defined.",
            "responses":{"200":response("Success",success_schema(id))}});
        for (status, description) in [
            (
                "400",
                "Existing registry/model validation or mutation failure",
            ),
            ("401", "Authentication failed"),
            (
                "403",
                "Insufficient capability, platform authority or visibility",
            ),
            ("404", "Service not found"),
            ("422", "Dictionary/JSON validation failed"),
            ("503", "Authentication authority unavailable"),
        ] {
            operation["responses"][status] =
                response(description, object(json!({"detail":dynamic()})));
        }
        operation["responses"]["500"] = response(
            "Redacted internal error",
            object(json!({"error":{
            "type":"object","required":["code","message"],"properties":{"code":{"const":2000},"message":{"const":"Internal server error"},"trace_id":{}},"additionalProperties":false}})),
        );
        operation["responses"]["413"] =
            json!({"description":"Host request-body limit exceeded; transport-defined body"});
        operation["responses"]["default"] = json!({"description":"Other host/admission/cancellation responses remain undeclared","x-native-undeclared":true});
        if matches!(*id, "register_service" | "update_service") {
            operation["requestBody"] = json!({"required":true,"content":{"application/json":{"schema":{"$ref":"#/components/schemas/NativeServiceDictionary"}}}});
            operation["x-native-mutable-fields"] = json!(SERVICE_MUTABLE_FIELDS);
        }
        if *id == "register_service" {
            operation["description"] = json!(
                "Legacy registration upserts. A successful storage write may precede cache/proxy invalidation failure; read back the exact ID after an uncertain outcome rather than replaying."
            );
        }
        if document["paths"].get(&path).is_none() {
            document["paths"][&path] = json!({});
        }
        document["paths"][&path][method.to_ascii_lowercase()] = operation;
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn source_route_inventory_drives_six_common_operations() {
        let doc = super::super::document();
        let mut common = 0;
        for (method, route, id) in SERVICES_ROUTES {
            let path = format!("/api/v1{route}");
            assert!(allowed_methods(&path).contains(method));
            let operation = &doc["paths"][&path][method.to_ascii_lowercase()];
            common += 1;
            assert_eq!(
                operation["operationId"],
                super::super::roles::operation_id(method, &path, id)
            );
            assert_eq!(operation["x-native-owner"], "services_api::ServicesApi");
        }
        assert_eq!(common, 6);
        assert_eq!(allowed_methods("/api/v1/services"), vec!["POST", "GET"]);
        assert_eq!(
            allowed_methods("/api/v1/services/{service_id}"),
            vec!["GET", "PUT", "DELETE"]
        );
        assert!(allowed_methods("/api/v1/service-registrations").is_empty());
        assert!(allowed_methods("/services").is_empty());
    }
    #[test]
    fn dictionary_and_dynamic_members_remain_open() {
        assert!(is_dictionary(&json!({"unknown":null})));
        assert!(!is_dictionary(&Value::Null));
        assert_eq!(dictionary_schema()["additionalProperties"], true);
        assert!(dictionary_schema().get("required").is_none());
        assert_eq!(detail_schema()["additionalProperties"], true);
        assert!(
            detail_schema()["properties"]["metadata"]
                .get("type")
                .is_none()
        );
        assert!(
            listed_service_schema()["properties"]["availability_status"]
                .get("type")
                .is_none()
        );
        assert!(
            schema_schema()["properties"]["input_schema"]
                .get("type")
                .is_none()
        );
    }
}
