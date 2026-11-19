//! Source-owned declarations for mounted public diagnostics and transparent proxy.
//! No publication or legacy schema is loaded; opaque proxy payloads stay opaque.
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
pub const SESSION_TOOLS: &str = "/api/v1/assistant/sessions/{session_id}/tools";
pub const SESSION_METRICS: &str = "/api/v1/assistant/sessions/{session_id}/metrics";
pub const TENANT_METRICS: &str = "/api/v1/assistant/metrics/tenant";
pub const PROXY_LIST: &str = "/api/v1/proxy";
pub const PROXY_ROOT: &str = "/api/v1/proxy/{service_name}";
pub const PROXY_SLASH: &str = "/api/v1/proxy/{service_name}/";
pub const PROXY_WILDCARD: &str = "/api/v1/proxy/{service_name}/{*path}";
fn object(properties: Value) -> Value {
    json!({"type":"object","required":properties.as_object().unwrap().keys().collect::<Vec<_>>(),"properties":properties})
}
fn response(schema: Value) -> Value {
    json!({"description":"Native handler response","content":{"application/json":{"schema":schema}}})
}
fn path(name: &str) -> Value {
    json!({"name":name,"in":"path","required":true,"schema":{"type":"string"}})
}
fn query(name: &str, default: i64, max: i64) -> Value {
    json!({"name":name,"in":"query","required":false,"schema":{"type":"integer","minimum":1,"maximum":max,"default":default}})
}
fn get(id: &str, parameters: Vec<Value>, schema: Value) -> Value {
    json!({"operationId":id,"parameters":parameters,"responses":{"200":response(schema),"default":{"description":"Native identity, authorization, validation or dependency error","x-native-undeclared":true}},"x-native-response-schema":"source-owned-diagnostic"})
}
pub fn extend_document(document: &mut Value) {
    extend_health_models(document);

    document["paths"]["/api/v1/setup/state"]["get"] = get(
        "setup_state_api_v1_setup_state_get",
        vec![],
        object(
            json!({"configured":{"type":"boolean"},"missing":{"type":"array","items":{"type":"string"}},"mode":{"type":"string"},"default_model":{"type":"string"}}),
        ),
    );
    let tool = object(
        json!({"name":{"type":"string"},"effect":{},"approval":{},"required_inputs":{"type":"array","items":{}},"device_required":{"type":"boolean"}}),
    );
    let platform = object(
        json!({"name":{"type":"string"},"description":{},"tenant_visible":{"type":"boolean"},"required_inputs":{"type":"array","items":{}},"device_required":{"type":"boolean"}}),
    );
    document["paths"][SESSION_TOOLS]["get"] = get(
        "get_session_tools_api_v1_assistant_sessions__session_id__tools_get",
        vec![path("session_id")],
        object(
            json!({"platform_catalog":{"type":"array","items":platform},"next_turn_estimate":object(json!({"status":{"type":"string","enum":["unavailable","available_now"]},"tools":{"type":"array","items":tool}})),"last_run_pinned":{"anyOf":[{"type":"null"},object(json!({"run_id":{"type":"string"},"status":{"type":"string"},"tools":{"type":"array","items":tool}}))]},"note":{"type":"string"}}),
        ),
    );
    document["paths"][SESSION_METRICS]["get"] = get(
        "get_session_metrics_api_v1_assistant_sessions__session_id__metrics_get",
        vec![path("session_id"), query("limit", 50, 200)],
        object(
            json!({"session_id":{"type":"string"},"request_count":{"type":"integer"},"avg_tokens":{"type":"integer"},"avg_utilization":{"type":"number"},"avg_compression_ratio":{"type":["number","null"]},"avg_cache_hit_rate":{"type":["number","null"]},"total_tokens_used":{"type":["integer","null"]},"calculation":{"type":["object","null"],"required":["source","token_kind","context_window","non_text_tokens_unknown"],"properties":{"source":{"type":"string","const":"prepared_provider_request"},"token_kind":{"type":"string","const":"codepoint_estimate"},"context_window":{"type":"integer","minimum":1},"non_text_tokens_unknown":{"type":"boolean"}},"additionalProperties":false}}),
        ),
    );
    document["paths"][TENANT_METRICS]["get"] = get(
        "get_tenant_metrics_api_v1_assistant_metrics_tenant_get",
        vec![query("hours", 24, 168)],
        object(
            json!({"tenant_id":{"type":"string"},"hours":{"type":"integer"},"request_count":{"type":"integer"},"unique_sessions":{"type":"integer"},"total_tokens":{"type":"integer"},"avg_tokens_per_request":{"type":["integer","null"]},"avg_utilization":{"type":["number","null"]}}),
        ),
    );
    document["paths"][PROXY_LIST]["get"] = get(
        "list_proxy_services_api_v1_proxy_get",
        vec![],
        object(
            json!({"count":{"type":"integer"},"services":{"type":"array","items":{"type":"object","additionalProperties":true}}}),
        ),
    );
    for (route, id, has_path) in [
        (
            "/api/v1/proxy/{service_name}/{path}",
            "transparent_proxy_handler_api_v1_proxy__service_name___path__delete",
            true,
        ),
        (
            PROXY_ROOT,
            "transparent_proxy_root_handler_api_v1_proxy__service_name__delete",
            false,
        ),
    ] {
        for method in ["get", "post", "put", "patch", "delete", "head", "options"] {
            let mut parameters = vec![path("service_name")];
            if has_path {
                parameters.push(path("path"));
            }
            let mut operation = json!({"operationId":id,"parameters":parameters,"x-native-response-schema":"streamed-upstream-or-diagnostic","responses":{"200":{"description":"Upstream response, potentially streamed","content":{"*/*":{"schema":{}}}},"default":{"description":"Upstream status/content or native rejection","x-native-undeclared":true,"content":{"*/*":{"schema":{}}}}}});
            if ["post", "put", "patch"].contains(&method) {
                operation["requestBody"] =
                    json!({"required":false,"content":{"*/*":{"schema":{}}}});
            }
            document["paths"][route][method] = operation;
        }
    }
    document["paths"]["/api/v1/proxy/{service_name}/_health"]["get"] = get(
        "proxy_service_health_api_v1_proxy__service_name___health_get",
        vec![path("service_name")],
        object(
            json!({"status":{"type":"string","enum":["healthy","unhealthy"]},"service":{"type":"string"},"availability_status":{},"last_health_check_at":{},"last_health_error":{}}),
        ),
    );
    document["paths"]["/api/v1/proxy/{service_name}/_selftest"]["get"] = get(
        "proxy_service_selftest_api_v1_proxy__service_name___selftest_get",
        vec![path("service_name")],
        json!({"type":"object","additionalProperties":true}),
    );
    let mut proofs = serde_json::Map::new();
    for (name, bytes) in [
        ("models.rs", include_bytes!("../models.rs").as_slice()),
        ("lib.rs", include_bytes!("../lib.rs").as_slice()),
        (
            "../../platform-gateway-foundation/src/setup_api.rs",
            include_bytes!("../../../platform-gateway-foundation/src/setup_api.rs").as_slice(),
        ),
        (
            "../../platform-gateway-foundation/src/health_api.rs",
            include_bytes!("../../../platform-gateway-foundation/src/health_api.rs").as_slice(),
        ),
        (
            "../../platform-gateway-foundation/src/architecture_status_api.rs",
            include_bytes!("../../../platform-gateway-foundation/src/architecture_status_api.rs")
                .as_slice(),
        ),
        (
            "migration_host/native_public_contract.rs",
            include_bytes!("native_public_contract.rs").as_slice(),
        ),
        (
            "migration_host/context_metrics.rs",
            include_bytes!("context_metrics.rs").as_slice(),
        ),
        (
            "migration_host/catalog/session_tools.rs",
            include_bytes!("catalog/session_tools.rs").as_slice(),
        ),
        (
            "migration_compat/proxy_http.rs",
            include_bytes!("../migration_compat/proxy_http.rs").as_slice(),
        ),
        (
            "migration_host/context_metrics/producer.rs",
            include_bytes!("context_metrics/producer.rs").as_slice(),
        ),
        (
            "migration_host/bootstrap/graph.rs",
            include_bytes!("bootstrap/graph.rs").as_slice(),
        ),
    ] {
        proofs.insert(name.into(), json!(format!("{:x}", Sha256::digest(bytes))));
    }
    document["x-native-public-diagnostics"] =
        json!({"source_sha256":proofs,"runtime_verified":false,"complete":false});
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn declarations_cover_actual_mounted_diagnostics_and_proxy() {
        let mut d = json!({"paths":{}});
        extend_document(&mut d);
        assert_eq!(d["paths"].as_object().unwrap().len(), 15);
        assert_eq!(d["paths"][PROXY_ROOT].as_object().unwrap().len(), 7);
        assert_eq!(
            d["paths"][SESSION_METRICS]["get"]["parameters"][1]["schema"]["maximum"],
            200
        );
        assert_eq!(
            d["paths"][TENANT_METRICS]["get"]["parameters"][0]["schema"]["maximum"],
            168
        );
        let metrics = &d["paths"][SESSION_METRICS]["get"]["responses"]["200"]["content"]["application/json"]
            ["schema"];
        assert_eq!(
            metrics["properties"]["avg_cache_hit_rate"]["type"],
            json!(["number", "null"])
        );
        assert_eq!(
            metrics["properties"]["avg_compression_ratio"]["type"],
            json!(["number", "null"])
        );
        assert_eq!(
            metrics["properties"]["calculation"]["properties"]["token_kind"]["const"],
            "codepoint_estimate"
        );
        assert!(d["x-native-public-diagnostics"]["source_sha256"]["migration_host/context_metrics/producer.rs"].as_str().is_some_and(|s|s.len()==64));
        assert_eq!(d["x-native-public-diagnostics"]["complete"], false);
    }
}

fn extend_health_models(document: &mut Value) {
    let string = json!({"type":"string"});
    let strings = json!({"type":"array","items":{"type":"string"}});
    let dependency =
        object(json!({"service_id":string,"required":{"type":"boolean"},"status":string}));
    let service = object(
        json!({"service_id":string,"display_name":string,"bounded_context":string,"responsibility":string,"lifecycle":string,"exposure":string,"status":string,"version":string,"state_owner":string,"scale_support":string,"replicas":{"type":"integer","minimum":0},"active_in_mode":{"type":"boolean"},"dependencies":{"type":"array","items":dependency},"degraded_reasons":strings,"last_check":{"type":["string","null"]}}),
    );
    let group = object(
        json!({"group_id":string,"display_name":string,"services":{"type":"array","items":service}}),
    );
    document["paths"]["/api/v1/admin/architecture-status"]["get"] = get(
        "architecture_status_api_v1_admin_architecture_status_get",
        vec![],
        object(
            json!({"schema_version":string,"topology_revision":string,"mode":string,"mode_configuration_valid":{"type":"boolean"},"last_check":string,"groups":{"type":"array","items":group}}),
        ),
    );
    let model = object(
        json!({"id":string,"name":string,"provider":string,"context_window":{"type":"integer"},"max_output_tokens":{"type":"integer"},"supports_vision":{"type":"boolean"},"supports_tools":{"type":"boolean"},"access_level":string,"input_price_per_1k":{"type":"number"},"output_price_per_1k":{"type":"number"},"effective_capabilities":{},"capability_revision":{"type":"integer"}}),
    );
    document["paths"]["/api/v1/assistant/models"]["get"] = get(
        "list_models_api_v1_assistant_models_get",
        vec![],
        object(json!({"models":{"type":"array","items":model}})),
    );
    document["paths"]["/api/v1/health"]["get"] = get(
        "gateway_health_api_v1_health_get",
        vec![],
        object(json!({"status":{"type":"string","const":"ok"}})),
    );
    for (route, id, parameters) in [
        (
            "/api/v1/health/services",
            "all_services_health_api_v1_health_services_get",
            vec![],
        ),
        (
            "/api/v1/health/services/{service_id}",
            "service_health_api_v1_health_services__service_id__get",
            vec![path("service_id")],
        ),
        (
            "/api/v1/health/providers",
            "all_providers_health_api_v1_health_providers_get",
            vec![],
        ),
    ] {
        document["paths"][route]["get"] = get(
            id,
            parameters,
            json!({"type":"object","additionalProperties":true}),
        );
    }
}
