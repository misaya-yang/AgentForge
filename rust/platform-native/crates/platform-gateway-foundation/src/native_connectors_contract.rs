//! Source-owned personal connector journey declarations; no published schema input.
use serde_json::{Value, json};

/// Canonical registration order, including principal_router appended at the end.
/// Templates omit only the application's /api/v1 prefix.
pub const CONNECTOR_USER_ROUTES: &[(&str, &str, &str)] = &[
    ("GET", "/connectors/available", "list_available_connectors"),
    ("GET", "/connectors/mine", "list_my_connectors"),
    ("GET", "/connectors/auth/{provider}", "initiate_oauth"),
    ("GET", "/connectors/callback/{provider}", "oauth_callback"),
    ("DELETE", "/connectors/{provider}", "disconnect_connector"),
    (
        "POST",
        "/connectors/{provider}/activate",
        "activate_connector_mcp",
    ),
    (
        "GET",
        "/connectors/{provider}/mcp-status",
        "connector_mcp_status",
    ),
    ("POST", "/connectors/{provider}/search", "search_connector"),
    (
        "POST",
        "/connectors/{provider}/principals",
        "create_connector_principal",
    ),
    (
        "GET",
        "/connectors/{provider}/principals",
        "list_connector_principals",
    ),
    (
        "DELETE",
        "/connectors/{provider}/principals/{grant_id}",
        "revoke_connector_principal",
    ),
];

/// First five mounted operations form the personal connection/return/disconnect journey.
/// Other mounted connector operations remain outside this bounded declaration.
pub const PERSONAL_ROUTES: &[(&str, &str, &str)] = &[
    CONNECTOR_USER_ROUTES[0],
    CONNECTOR_USER_ROUTES[1],
    CONNECTOR_USER_ROUTES[2],
    CONNECTOR_USER_ROUTES[3],
    CONNECTOR_USER_ROUTES[4],
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
pub fn available_schema() -> Value {
    json!({"type":"array","items":object(json!({
        "provider":{"type":"string"},"display_name":{"type":"string"},
        "description":{"type":["string","null"]},"icon_url":{"type":["string","null"]},
        "enabled":{"type":"boolean"},"connected":{"type":"boolean"},"status":{"type":["string","null"]}
    }))})
}
fn response(description: &str, schema: Value) -> Value {
    json!({"description":description,"content":{"application/json":{"schema":schema}}})
}
pub fn extend_document(document: &mut Value) {
    for &(method, suffix, name) in PERSONAL_ROUTES {
        let path = format!("/api/v1{suffix}");
        let id = format!(
            "{name}_{}_{}",
            path.trim_start_matches('/').replace(['/', '{', '}'], "_"),
            method.to_lowercase()
        );
        let callback = name == "oauth_callback";
        let mut op = json!({"operationId":id,"tags":["Connectors"],
            "security":[{"NativeBearerAuth":[]},{"NativeBoundApiKey":[]}],
            "x-native-owner":"connector_user_api/connector_user_service",
            "x-native-response-schema":"bounded-source-owned",
            "x-native-authorization":"Current account/session JWT or database-bound API key through AdmissionService. Bearer takes precedence; failed bearer verification never falls back to a key. No guest or disabled-auth bypass. Header name is deployment-configurable.",
            "responses":{"default":{"description":"Other host, transport, cancellation, middleware or collaborator outcomes remain undeclared.","x-native-undeclared":true}}});
        if suffix.contains("{provider}") {
            op["parameters"] =
                json!([{"name":"provider","in":"path","required":true,"schema":{"type":"string"}}]);
        }
        let statuses: &[&str] = if callback {
            &["400", "404", "422", "500", "503"]
        } else if name == "initiate_oauth" {
            &["400", "401", "404", "500", "503"]
        } else {
            &["401", "500", "503"]
        };
        for status in statuses {
            op["responses"][*status] = response(
                "Observed source failure; existing detail/transport variants are not narrowed.",
                json!({}),
            );
        }
        op["responses"]["500"]["content"]["text/plain"] = json!({"schema":{"type":"string"}});
        match name {
            "list_available_connectors" => {
                op["responses"]["200"] = response(
                    "Tenant effective enabled configurations and this user's connection status.",
                    available_schema(),
                )
            }
            "list_my_connectors" => {
                op["x-native-response-schema"] = json!("opaque-row-array-source-owned");
                op["responses"]["200"] = response(
                    "Rows selected by LIST_MINE_SQL, scoped to user/tenant and excluding revoked records. Runtime passes row Values through; field values and collaborator projections remain open. Tokens are not selected by the production SQL.",
                    json!({"type":"array","items":{}}),
                );
            }
            "initiate_oauth" => {
                op["responses"]["200"] = response(
                    "Issued state is persisted before returning the provider authorization URL; this is not connection success.",
                    object(json!({"auth_url":{"type":"string"},"state":{"type":"string"}})),
                )
            }
            "disconnect_connector" => {
                op["responses"]["200"] = response(
                    "Local credentials revoked; best-effort MCP stop is not proof of provider-side revocation.",
                    object(
                        json!({"status":{"type":"string","const":"disconnected"},"provider":{"type":"string"}}),
                    ),
                );
            }
            "oauth_callback" => {
                op["security"] = json!([]);
                op["x-native-response-schema"] = json!("redirect-source-owned");
                op["x-native-authorization"] = json!(
                    "Issued state authorizes the callback, without bearer resolution. Malformed state and provider mismatch are rejected before consumption. Otherwise issued state is atomically consumed, then its expiry, tenant, user, provider and nonce are checked. Denial consumes state without token exchange or connector mutation."
                );
                op["parameters"].as_array_mut().expect("provider").extend([
                    json!({"name":"code","in":"query","required":false,"schema":{"type":"string"}}),
                    json!({"name":"state","in":"query","required":false,"schema":{"type":"string","default":""}}),
                    json!({"name":"error","in":"query","required":false,"schema":{"type":"string"}}),
                ]);
                op["x-native-query-validation"] = json!(
                    "Missing both code and error returns422. Error plus code or an empty error returns400 before consuming state. Empty/malformed state returns400. Query parsing uses the last duplicate value. Provider error text is never echoed in redirect."
                );
                op["responses"]["302"] = json!({"description":"Validated success returns /connections?connected=<provider>&status=connected; validated denial returns /connections?status=authorization_failed. A valid configured console origin may prefix either location.","headers":{"Location":{"required":true,"schema":{"type":"string"}}}});
            }
            _ => unreachable!("bounded personal route inventory"),
        }
        document["paths"][path][method.to_lowercase()] = op;
    }
}
