//! OpenAPI declarations for the connector catalog and connection routes.
use serde_json::{Value, json};

fn operation(function: &str, path: &str, method: &str, status: u16, body: bool) -> Value {
    let id = format!(
        "{function}{}_{}",
        path.chars()
            .map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '_' })
            .collect::<String>(),
        method
    );
    let mut parameters = Vec::new();
    for part in path.split('/') {
        if let Some(name) = part.strip_prefix('{').and_then(|s| s.strip_suffix('}')) {
            parameters.push(json!({"name":name,"in":"path","required":true,"schema":{"type":"string"}}));
        }
    }
    if path == "/api/v1/catalog" && method == "get" {
        for name in ["q", "category", "kind"] {
            parameters.push(json!({"name":name,"in":"query","required":false,"schema":{"type":"string"}}));
        }
    }
    let mut out = json!({
        "operationId": id,
        "tags": ["Catalog"],
        "parameters": parameters,
        "responses": {
            status.to_string(): {"description": "Catalog handler response", "content": {"application/json": {"schema": {"type":"object","additionalProperties":true}}}},
            "default": {"description": "Identity, authorization or dependency failure", "x-native-undeclared": true}
        },
        "x-native-response-schema": "opaque-native-database-projection",
        "x-native-authentication": "verified platform session or API key"
    });
    if body {
        out["requestBody"] = json!({"required":true,"content":{"application/json":{"schema":{"type":"object","additionalProperties":true}}}});
    }
    out
}

pub fn extend_document(document: &mut Value) {
    for (path, method, function, status, body) in [
        ("/api/v1/catalog", "get", "list_catalog", 200, false),
        ("/api/v1/catalog", "post", "create_catalog_entry", 201, true),
        ("/api/v1/catalog/{entry_id}", "get", "get_catalog_entry", 200, false),
        ("/api/v1/connections", "get", "list_connections", 200, false),
        ("/api/v1/connections", "post", "create_connection", 201, true),
        ("/api/v1/connections/{entry_id}/oauth/start", "post", "start_connection_oauth", 200, true),
        ("/api/v1/connections/oauth/callback", "get", "connection_oauth_callback", 303, false),
        ("/api/v1/connections/{connection_id}/revoke", "post", "revoke_connection", 200, false),
        ("/api/v1/connections/{connection_id}/policy", "post", "policy_connection", 200, true),
        ("/api/v1/connections/oauth/client-metadata", "get", "connection_client_metadata", 200, false),
    ] {
        document["paths"][path][method] = operation(function, path, method, status, body);
    }
}
