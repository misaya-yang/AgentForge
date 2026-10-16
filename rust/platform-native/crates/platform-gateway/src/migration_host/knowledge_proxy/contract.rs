//! Shared routing declarations for the native Gateway -> Knowledge service boundary.
//! This declares a streaming proxy, not ownership of Knowledge business schemas.
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

pub const PUBLIC_PREFIXES: [&str; 3] = ["/api/v1/knowledge", "/v1/knowledge", "/api/v1/kb-tools"];
pub const UPSTREAM_PREFIX: &str = "/api/v1/knowledge";
pub const METHODS: [&str; 5] = ["GET", "POST", "PUT", "PATCH", "DELETE"];

pub fn public_upstream_path(path: &str) -> String {
    if path.is_empty() {
        UPSTREAM_PREFIX.to_owned()
    } else {
        format!("{UPSTREAM_PREFIX}/{path}")
    }
}

pub fn read_operation(method: &str, path: &str) -> bool {
    matches!(method, "GET" | "HEAD" | "OPTIONS")
        || method == "POST"
            && ["retrieve", "retrieve_batch"].contains(
                &path
                    .trim_end_matches('/')
                    .rsplit('/')
                    .next()
                    .unwrap_or("")
                    .to_lowercase()
                    .as_str(),
            )
}

pub fn dataset_impact(method: &str, path: &str) -> bool {
    let segments: Vec<_> = path.trim_matches('/').split('/').collect();
    method == "GET" && segments.len() == 3 && segments[0] == "datasets" && segments[2] == "impact"
}

pub fn dataset_delete(method: &str, path: &str) -> bool {
    let segments: Vec<_> = path.trim_matches('/').split('/').collect();
    method == "DELETE" && segments.len() == 2 && segments[0] == "datasets"
}

/// The same stable ID rule used by the retained Python Gateway multi-method route.
fn operation_id(prefix: &str) -> String {
    if prefix == "/api/v1/kb-tools" {
        return "proxy_kb_tools_api_v1_kb_tools__path__delete".into();
    }
    format!(
        "proxy_knowledge_{}_path__delete",
        prefix.trim_start_matches('/').replace('/', "_") + "_"
    )
}

pub fn extend_document(document: &mut Value) {
    document["x-native-contract"]["success_responses_declared"] = json!(false);
    document["x-native-contract"]["bounded_success_responses_declared"] = json!(true);
    for prefix in PUBLIC_PREFIXES {
        for method in METHODS {
            let path = format!("{prefix}/{{path}}");
            document["paths"][&path][method.to_lowercase()] = json!({
                "operationId": operation_id(prefix),
                "tags": [if prefix == "/api/v1/kb-tools" { "KB Tools" } else { "Knowledge Base" }],
                "summary": "Signed streaming proxy to the selected Knowledge owner",
                "parameters": [{"name":"path","in":"path","required":true,
                    "schema":{"type":"string","x-native-max-utf8-bytes":8192,
                        "x-native-rule":"Reject control characters, backslash, ?, #, %, and dot/dot-dot segments before normalization/signing."}}],
                "security": [{},{"NativeBearerAuth":[]},{"NativeKnowledgeApiKey":[]}],
                "x-native-response-schema":"streamed-owner-declared-or-opaque",
                "x-native-knowledge-proxy":true,
                "x-native-admission":"Guest/configured-static-key/current-account branches are deployment-dependent. Selected invalid credentials do not become an anonymous identity. Bound keys require knowledge:read for GET or POST retrieve/retrieve_batch, knowledge:write otherwise. Knowledge remains current actor/object ACL authority; these alternatives are not unconditional admission guarantees.",
                "responses": {
                    "200":{"description":"Upstream success when the selected owner route returns 200; body remains opaque at this wildcard.","content":{"application/json":{"schema":{}},"*/*":{"schema":{}}}},
                    "422":{"description":"Owner validation failure when applicable; not a universal FastAPI validation envelope.","content":{"application/json":{"schema":{}},"*/*":{"schema":{}}}},
                    "default":{"description":"Stream upstream status, content type and bytes without buffering; native admission/transport may reject before forwarding. No exhaustive response/body guarantee.","x-native-undeclared":true,"content":{"*/*":{"schema":{}}}}
                }
            });
        }
    }
    document["components"]["securitySchemes"]["NativeKnowledgeApiKey"] = json!({
        "type":"apiKey","in":"header","name":"X-API-Key",
        "description":"Deployment-configurable admission header. Current database-bound keys require operation-specific Knowledge scope. Explicit configured static-key fallback yields no tenant and is not a database-bound authority guarantee. All credentials are removed before signed forwarding."
    });
    document["x-native-knowledge-forwarding"] = json!({
        "version":1,"public_prefixes":PUBLIC_PREFIXES,"upstream_prefix":UPSTREAM_PREFIX,
        "methods":METHODS,"response_mode":"streamed-status-content-type-body",
        "request_authority":"fresh-body-bound-v2-signature; client authority and credential headers stripped",
        "transforms":[
            {"method":"DELETE","suffix":"datasets/{dataset_id}","kind":"account-password-confirmation","description":"Validate current account password, remove password, add dataset/user/tenant-bound 60-second confirmation before signing."},
            {"method":"GET","suffix":"datasets/{dataset_id}/impact","kind":"gateway-owned","description":"Signed fresh Knowledge dataset-owner check, then tenant/Agent-ACL-scoped Gateway impact query. Not a Python route passthrough."}
        ],
        "all_json_body_operations":"Reserved _gateway_delete_confirmation rejected; excessive JSON nesting rejected; other bytes preserved except dataset-delete transform.",
        "native_error_statuses":[400,401,403,413,429,502,503,504],
        "transport":"Bounded replayable request bytes; only connect failure retries once with a fresh signature. No timeout/post-send mutation replay. Streaming backpressure, route-scoped breaker, forced cache-control no-store; hop-by-hop headers stripped; SSE content-length removed.",
        "source_sha256": source_sha256(),
        "complete":false,"runtime_verified":false
    });
}

fn source_sha256() -> Value {
    let mut sources = serde_json::Map::new();
    for (name, source) in [
        (
            "migration_host/knowledge_proxy/contract.rs",
            include_bytes!("contract.rs").as_slice(),
        ),
        (
            "migration_host/knowledge_proxy/mod.rs",
            include_bytes!("mod.rs").as_slice(),
        ),
        (
            "migration_host/knowledge_proxy/forwarding.rs",
            include_bytes!("forwarding.rs").as_slice(),
        ),
        (
            "migration_host/knowledge_proxy/management.rs",
            include_bytes!("management.rs").as_slice(),
        ),
        (
            "migration_host/knowledge_proxy/impact.sql",
            include_bytes!("impact.sql").as_slice(),
        ),
        (
            "migration_host/application.rs",
            include_bytes!("../application.rs").as_slice(),
        ),
        (
            "migration_host/foundation.rs",
            include_bytes!("../foundation.rs").as_slice(),
        ),
        (
            "migration_content/access.rs",
            include_bytes!("../../migration_content/access.rs").as_slice(),
        ),
    ] {
        sources.insert(name.into(), json!(format!("{:x}", Sha256::digest(source))));
    }
    Value::Object(sources)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mapping_and_intercepts_preserve_existing_behavior() {
        assert_eq!(public_upstream_path(""), "/api/v1/knowledge");
        assert_eq!(
            public_upstream_path("dataset/sources"),
            "/api/v1/knowledge/dataset/sources"
        );
        for method in ["GET", "HEAD", "OPTIONS"] {
            assert!(read_operation(method, "anything"));
        }
        assert!(read_operation("POST", "dataset/RETRIEVE/"));
        assert!(read_operation("POST", "dataset/retrieve_batch"));
        assert!(!read_operation(
            "POST",
            "dataset/sources/confluence/binding/sync"
        ));
        assert!(dataset_delete("DELETE", "/datasets/dataset/"));
        assert!(!dataset_delete("DELETE", "dataset/documents/document"));
        assert!(dataset_impact("GET", "datasets/dataset/impact"));
        assert!(!dataset_impact("POST", "datasets/dataset/impact"));
    }
    #[test]
    fn wildcard_schemas_never_invent_owner_response_guarantees() {
        let mut document = json!({"paths":{},"components":{"securitySchemes":{}}});
        extend_document(&mut document);
        assert_eq!(document["paths"].as_object().unwrap().len(), 3);
        assert_eq!(
            document["paths"]["/api/v1/kb-tools/{path}"]["get"]["operationId"],
            "proxy_kb_tools_api_v1_kb_tools__path__delete"
        );
        assert_eq!(
            document["paths"]["/api/v1/knowledge/{path}"]["get"]["operationId"],
            "proxy_knowledge_api_v1_knowledge__path__delete"
        );
        assert_eq!(
            document["paths"]["/v1/knowledge/{path}"]["get"]["operationId"],
            "proxy_knowledge_v1_knowledge__path__delete"
        );
        for path in document["paths"].as_object().unwrap().values() {
            assert_eq!(path.as_object().unwrap().len(), 5);
            for operation in path.as_object().unwrap().values() {
                assert_eq!(
                    operation["responses"]["200"]["content"]["application/json"]["schema"],
                    json!({})
                );
                for status in ["200", "422", "default"] {
                    assert_eq!(
                        operation["responses"][status]["content"]["*/*"]["schema"],
                        json!({})
                    );
                }
                assert_eq!(
                    operation["responses"]["default"]["x-native-undeclared"],
                    true
                );
            }
        }
        assert_eq!(document["x-native-knowledge-forwarding"]["complete"], false);
    }
}
