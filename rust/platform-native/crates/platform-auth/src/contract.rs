//! Shared wire types and route declarations for the existing account HTTP API.
//! No authentication state, database access, publication input or runtime startup.
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const LOGIN: &str = "/api/v1/auth/login";
pub const LOGOUT: &str = "/api/v1/auth/logout";
pub const ME: &str = "/api/v1/auth/me";
pub const VALIDATE: &str = "/api/v1/auth/validate-token";
pub const CHANGE_PASSWORD: &str = "/api/v1/auth/change-password";
pub const BODY_LIMIT: usize = 16 * 1024;
pub const ROUTES: &[(&str, &str, &str)] = &[
    ("POST", LOGIN, "login_api_v1_auth_login_post"),
    ("POST", LOGOUT, "logout_api_v1_auth_logout_post"),
    ("GET", ME, "get_current_user_api_v1_auth_me_get"),
    (
        "POST",
        VALIDATE,
        "validate_token_api_v1_auth_validate_token_post",
    ),
    (
        "POST",
        CHANGE_PASSWORD,
        "change_password_api_v1_auth_change_password_post",
    ),
];

// These remain serde's ordinary unknown-field-tolerant request types. No new
// length, password, email or duplicate-field policy is introduced by the schema.
#[derive(Deserialize)]
pub struct LoginInput {
    pub email: String,
    pub password: String,
}
#[derive(Deserialize)]
pub struct PasswordInput {
    pub current_password: String,
    pub new_password: String,
    pub confirm_password: String,
}
#[derive(Serialize)]
pub struct UserProfile {
    pub user_id: String,
    pub email: Option<String>,
    pub display_name: Option<String>,
    pub department: Option<String>,
    pub roles: Vec<String>,
    pub permissions: Vec<String>,
    pub tier: String,
}
#[derive(Serialize)]
pub struct LoginResponse {
    pub access_token: String,
    pub token_type: &'static str,
    pub expires_in: u64,
    pub user: UserProfile,
    pub force_password_change: bool,
}
#[derive(Serialize)]
pub struct CurrentUserResponse {
    pub user_id: String,
    pub tenant_id: String,
    pub email: Option<String>,
    pub display_name: Option<String>,
    pub department: Option<String>,
    pub roles: Vec<String>,
    pub permissions: Vec<String>,
    pub effective_permissions: Vec<String>,
    pub tier: String,
    pub force_password_change: bool,
}
#[derive(Serialize)]
pub struct ValidationResponse {
    pub valid: bool,
    pub user_id: String,
    pub roles: Vec<String>,
    pub tier: String,
}
pub fn logout_response(authenticated: bool) -> Value {
    json!({"status":"success","message":if authenticated { "Logged out successfully" } else { "Not logged in" }})
}
pub fn password_changed_response() -> Value {
    json!({"status":"success","message":"Password changed successfully"})
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
fn nullable_string() -> Value {
    json!({"type":["string","null"]})
}
pub fn login_request_schema() -> Value {
    json!({"type":"object","properties":{"email":{"type":"string"},"password":{"type":"string","writeOnly":true}},"required":["email","password"],"additionalProperties":true})
}
pub fn password_request_schema() -> Value {
    json!({"type":"object","properties":{"current_password":{"type":"string","writeOnly":true},"new_password":{"type":"string","writeOnly":true},"confirm_password":{"type":"string","writeOnly":true}},"required":["current_password","new_password","confirm_password"],"additionalProperties":true})
}
pub fn success_schema(path: &str) -> Value {
    match path {
        LOGIN => object(
            json!({"access_token":{"type":"string"},"token_type":{"type":"string"},"expires_in":{"type":"integer","minimum":0},"user":object(json!({"user_id":{"type":"string"},"email":nullable_string(),"display_name":nullable_string(),"department":nullable_string(),"roles":strings(),"permissions":strings(),"tier":{"type":"string"}})),"force_password_change":{"type":"boolean"}}),
        ),
        ME => object(
            json!({"user_id":{"type":"string"},"tenant_id":{"type":"string"},"email":nullable_string(),"display_name":nullable_string(),"department":nullable_string(),"roles":strings(),"permissions":strings(),"effective_permissions":strings(),"tier":{"type":"string"},"force_password_change":{"type":"boolean"}}),
        ),
        VALIDATE => object(
            json!({"valid":{"type":"boolean"},"user_id":{"type":"string"},"roles":strings(),"tier":{"type":"string"}}),
        ),
        LOGOUT | CHANGE_PASSWORD => object(
            json!({"status":{"type":"string","const":"success"},"message":{"type":"string"}}),
        ),
        _ => panic!("unknown auth response"),
    }
}
fn response(description: &str, schema: Value) -> Value {
    json!({"description":description,"content":{"application/json":{"schema":schema}}})
}
pub fn extend_document(document: &mut Value) {
    document["components"]["schemas"]["NativeLoginInput"] = login_request_schema();
    document["components"]["schemas"]["NativePasswordInput"] = password_request_schema();
    for (method, path, id) in ROUTES {
        let security = match *path {
            LOGIN => json!([]),
            LOGOUT => json!([{}, {"NativeBearerAuth":[]}]),
            _ => json!([{"NativeBearerAuth":[]}]),
        };
        let authority = match *path {
            LOGIN => {
                "Public account login; no Bearer credential required. Email/password policy and account/tenant state checks remain service-owned."
            }
            LOGOUT => {
                "No Authorization header returns success. A supplied header must contain exactly one Bearer credential. Logout verifies JWT, revokes its Redis session and records an audit event; it does not require an existing allowlist record or reload current account/tenant authority. API keys are not accepted."
            }
            _ => {
                "Exactly one Bearer credential is required. JWT verification, Redis allowlist/credential revision and current PostgreSQL account/tenant authority are checked. API keys are not accepted; failures never fall back to another credential."
            }
        };
        let mut operation = json!({"operationId":id,"security":security,
            "x-native-owner":"platform-auth::http",
            "x-native-response-schema":"bounded-source-owned",
            "x-native-authorization":authority,
            "x-native-side-effects":"Password change invalidates sessions and returns no replacement token. Logout may revoke a session. Errors/cancellation do not prove storage rollback; do not automatically repeat mutations.",
            "responses":{"200":response("Success",success_schema(path)),"default":{"description":"Undeclared host or middleware outcome","x-native-undeclared":true}}});
        let service_statuses: &[&str] = if *path == LOGIN {
            &["401", "403", "423", "503"]
        } else {
            &["401", "503"]
        };
        for status in service_statuses {
            operation["responses"][status] = response(
                "Authentication service rejected the operation",
                object(json!({"detail":{"type":"string"}})),
            );
        }
        if matches!(*path, LOGIN | CHANGE_PASSWORD) {
            let name = if *path == LOGIN {
                "NativeLoginInput"
            } else {
                "NativePasswordInput"
            };
            operation["requestBody"] = json!({"required":true,"content":{"application/json":{"schema":{"$ref":format!("#/components/schemas/{name}")}}}});
            operation["x-native-body-limit-bytes"] = json!(BODY_LIMIT);
            operation["x-native-validation"] = json!(
                "JSON fields must be present strings; unknown fields ignored. Native normalization/password policy remains service-owned. Body parsing precedes handler authentication; extractor statuses and fixed Invalid request body detail are retained."
            );
            for status in ["400", "413", "415", "422"] {
                operation["responses"][status] = response(
                    "Request body or authentication input rejected",
                    object(json!({"detail":{"type":"string"}})),
                );
            }
        }
        document["paths"][path][method.to_ascii_lowercase()] = operation;
    }
}

#[cfg(test)]
#[path = "contract_tests.rs"]
mod tests;
