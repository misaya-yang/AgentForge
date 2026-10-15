//! Source-owned self-service API-key lifecycle. No legacy config routes or credential execution.
use serde_json::{Value, json};
pub const BASE: &str = "/api/v1/api-keys";
pub const KEY: &str = "/api/v1/api-keys/{key_id}";
pub const REVOKE: &str = "/api/v1/api-keys/{key_id}/revoke";
pub const NAME_MAX: usize = 100;
pub const DESCRIPTION_MAX: usize = 500;
pub const SELF_SERVICE_API_KEY_ROUTES: &[(&str, &str, &str)] = &[
    ("POST", "/api-keys", "create_api_key"),
    ("GET", "/api-keys", "list_api_keys"),
    ("GET", "/api-keys/{key_id}", "get_api_key_detail"),
    ("POST", "/api-keys/{key_id}/revoke", "revoke_api_key"),
    ("DELETE", "/api-keys/{key_id}", "delete_api_key"),
];
fn object(properties: Value) -> Value {
    let required: Vec<_> = properties
        .as_object()
        .expect("properties")
        .keys()
        .cloned()
        .collect();
    json!({"type":"object","properties":properties,"required":required,"additionalProperties":false})
}
pub fn request_schema() -> Value {
    json!({"type":"object","required":["name"],"additionalProperties":true,"properties":{
        "name":{"type":"string","minLength":1,"maxLength":NAME_MAX},
        "description":{"type":["string","null"],"maxLength":DESCRIPTION_MAX,"default":null},
        "scopes":{"type":["array","null"],"items":{"type":"string"},"default":["knowledge:read","knowledge:write"]},
        "tier":{"type":["string","null"],"default":"normal","description":"Accepted for source compatibility; issuance uses the authenticated account tier, not this field."},
        "expires_at":{"type":["string","number","null"],"description":"Production parser accepts ISO/date or finite numeric epoch seconds/milliseconds, with lexical/range validation not expressible by this broad shape."}
    },"x-native-validation":"Unknown fields are ignored; whitespace-only names are accepted. Omitted/null scopes use defaults, empty/duplicate scopes are retained. Scope strings other than knowledge:read/write fail authorization with403, including for admin; they are not request-schema enum failures. Identity and tier derive only from verified authority."})
}
pub fn key_schema(create: bool) -> Value {
    let mut properties = json!({
        "key_id":{"type":"string"},"key_prefix":{"type":["string","null"]},
        "name":{"type":"string"},"description":{"type":["string","null"]},
        "user_id":{"type":["string","null"]},"derived_user_id":{"type":["string","null"]},
        "scopes":{"type":["array","null"],"items":{"type":"string"}},
        "roles":{"type":["array","null"],"items":{"type":"string"}},
        "tier":{"type":["string","null"]},"rate_limit":{"description":"Unconstrained JSON value deliberately retained by project_response; not narrowed to a fabricated rate-limit model."},
        "is_active":{"type":"boolean"},"created_at":{"type":"string"},
        "last_used_at":{"type":["string","null"]},"expires_at":{"type":["string","null"]},
        "use_count":{"type":"integer"}
    });
    if create {
        properties["api_key"] = json!({"type":"string","description":"One-time plaintext credential returned only on create; do not log."});
        properties["warning"] = json!({"type":"string"});
    }
    object(properties)
}
pub fn action_schema(revoke: bool) -> Value {
    object(
        json!({"key_id":{"type":"string"},"message":{"type":"string","const":if revoke {"API Key revoked"} else {"API Key deleted"}}}),
    )
}
fn response(description: &str, schema: Value) -> Value {
    json!({"description":description,"content":{"application/json":{"schema":schema}}})
}
pub fn extend_document(document: &mut Value) {
    for &(method, suffix, name) in SELF_SERVICE_API_KEY_ROUTES {
        let path = format!("/api/v1{suffix}");
        let id = format!(
            "{name}_{}_{}",
            path.trim_start_matches('/')
                .replace(['/', '{', '}', '-'], "_"),
            method.to_lowercase()
        );
        let mut op = json!({"operationId":id,"tags":["API Keys"],
            "security":[{"NativeBearerAuth":[]},{"NativeBoundApiKey":[]}],
            "x-native-owner":"self_service_api_keys_api/self_service_api_keys/self_service_api_keys_store",
            "x-native-response-schema":"bounded-source-owned",
            "x-native-authorization":"Actual host uses AdmissionService: current JWT session/account or database-bound API key, bearer precedence and no fallback after failed bearer verification. No guest/disabled-auth bypass. Exact admin bypasses owner filtering only, never tenant scoping. Empty legacy tenant maps to default on reads; create retains verified raw tenant. API-key header is deployment-configurable.",
            "x-native-ordering":"Create JSON syntax precedes database presence and identity; other operations do not parse bodies. Model/query validation follows database presence and identity. Committed writes are not rolled back by a later invalidation/response failure; an error is not evidence that a write did not occur.",
            "responses":{"default":{"description":"Other host/extractor/cancellation/collaborator outcomes remain undeclared.","x-native-undeclared":true}}});
        for status in ["401", "500", "503"] {
            op["responses"][status] = response(
                "Observed authority/store/projection/cache failure. Sanitized JSON detail/error variants remain open.",
                json!({"type":"object","additionalProperties":true}),
            );
        }
        if matches!(name, "create_api_key" | "get_api_key_detail") {
            op["responses"]["403"] = response(
                "Scope denial uses object detail; foreign-owner detail access uses string detail.",
                json!({"type":"object","properties":{"detail":{}},"required":["detail"]}),
            );
        }
        if path != BASE {
            op["parameters"] = json!([{"name":"key_id","in":"path","required":true,"schema":{"type":"string"},"description":"Opaque key ID, not UUID. Encoded slash segments are rejected."}]);
            op["responses"]["404"] = response(
                "Missing or scoped-out key. Detail checks a found key's owner and returns403 for another owner; revoke/delete hide unauthorized rows as404.",
                json!({"type":"object","properties":{"detail":{"type":"string"}},"required":["detail"]}),
            );
        }
        let schema = match name {
            "create_api_key" => {
                op["requestBody"] = json!({"required":true,"content":{"application/json":{"schema":request_schema()}}});
                op["responses"]["422"] = response(
                    "Malformed JSON or request validation; detail validation array remains unconstrained here.",
                    json!({"type":"object","properties":{"detail":{}},"required":["detail"]}),
                );
                key_schema(true)
            }
            "list_api_keys" => {
                op["parameters"] = json!([{"name":"include_inactive","in":"query","required":false,"schema":{"type":"boolean","default":false},"description":"Existing boolean coercion accepts source spellings; last duplicate wins. No pagination is implemented."}]);
                op["responses"]["422"] = response(
                    "Invalid boolean query; source validation detail array.",
                    json!({"type":"object","properties":{"detail":{}},"required":["detail"]}),
                );
                json!({"type":"array","items":key_schema(false)})
            }
            "get_api_key_detail" => key_schema(false),
            "revoke_api_key" => action_schema(true),
            "delete_api_key" => action_schema(false),
            _ => unreachable!("self-service route inventory"),
        };
        op["responses"]["200"] = response(
            "Actual filtered response. Plaintext is create-only; tenant_id, key_hash, internal IDs and arbitrary storage fields are excluded. A success does not establish immediate revocation at every downstream cache.",
            schema,
        );
        document["paths"][path][method.to_lowercase()] = op;
    }
}
