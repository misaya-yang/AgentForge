//! Source-owned account lifecycle declarations, shared by the mounted UsersApi.
//! No publication input and no replacement for admission or transactional guards.
use serde_json::{Value, json};

pub const USERS_ROUTES: &[(&str, &str, &str)] = &[
    ("GET", "/users", "list_users"),
    ("POST", "/users", "create_user"),
    ("GET", "/users/{user_id}", "get_user"),
    ("PUT", "/users/{user_id}", "update_user"),
    ("DELETE", "/users/{user_id}", "delete_user"),
    (
        "POST",
        "/users/{user_id}/reset-password",
        "reset_user_password",
    ),
    ("POST", "/users/{user_id}/enable", "enable_user"),
    ("POST", "/users/{user_id}/disable", "disable_user"),
    ("PUT", "/users/{user_id}/profile", "update_user_profile"),
];
pub const CREATE_FIELDS: &[&str] = &["email", "display_name", "department", "roles"];
pub const PROFILE_FIELDS: &[&str] = &["display_name"];
pub const UPDATE_FIELDS: &[&str] = &[
    "display_name",
    "department",
    "roles",
    "extra_permissions",
    "service_access_mode",
    "allowed_services",
    "denied_services",
    "status",
    "tier",
];
pub const DISPLAY_NAME_MAX: usize = 255;
pub const DEPARTMENT_MAX: usize = 50;
pub const PAGE_DEFAULT: i64 = 1;
pub const PAGE_SIZE_DEFAULT: i64 = 20;
pub const PAGE_SIZE_MAX: i64 = 100;
pub const DEPARTMENTS: &[&str] = &["cs", "sales", "tech", "admin"];
pub const SERVICE_ACCESS_MODES: &[&str] = &["all", "allowlist"];

pub fn allowed_methods(path: &str) -> Vec<&'static str> {
    USERS_ROUTES
        .iter()
        .filter(|(_, route, _)| path.strip_prefix("/api/v1") == Some(*route))
        .map(|(method, _, _)| *method)
        .collect()
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

// Defaults are exactly the production projection's existing values, including
// the null user_id sentinel which the subsequent production validation rejects.
// Only successfully projected responses are described by user_schema().
macro_rules! user_projection {
    ($($field:ident: $default:expr => $schema:expr),* $(,)?) => {
        pub fn user_defaults() -> Value { json!({$(stringify!($field):$default),*}) }
        pub fn user_schema() -> Value { object(json!({$(stringify!($field):$schema),*})) }
    };
}
user_projection! {
    user_id: Value::Null => json!({"type":"string"}),
    email: Value::Null => nullable_string(),
    display_name: Value::Null => nullable_string(),
    username: Value::Null => nullable_string(),
    department: Value::Null => nullable_string(),
    roles: Vec::<String>::new() => strings(),
    extra_permissions: Vec::<String>::new() => strings(),
    service_access_mode: "all" => json!({"type":"string"}),
    allowed_services: Vec::<String>::new() => strings(),
    denied_services: Vec::<String>::new() => strings(),
    status: "active" => json!({"type":"string"}),
    tier: "normal" => json!({"type":"string"}),
    force_password_change: true => json!({"type":"boolean"}),
    last_login_at: Value::Null => nullable_string(),
    created_at: Value::Null => nullable_string(),
}
pub fn listed(users: Vec<Value>, total: i64, page: i64, page_size: i64) -> Value {
    json!({"users":users,"total":total,"page":page,"page_size":page_size})
}
pub fn deleted(user_id: &str) -> Value {
    json!({"status":"success","message":format!("User {user_id} deleted")})
}
pub fn password_reset(user_id: &str) -> Value {
    json!({"status":"success","message":format!("Password reset to default for user {user_id}")})
}
pub fn enabled(user_id: &str) -> Value {
    json!({"status":"success","message":format!("User {user_id} enabled")})
}
pub fn disabled(user_id: &str) -> Value {
    json!({"status":"success","message":format!("User {user_id} disabled")})
}
pub(crate) fn success_schema(id: &str) -> Value {
    match id {
        "list_users" => object(
            json!({"users":{"type":"array","items":user_schema()},"total":{"type":"integer"},"page":{"type":"integer","minimum":1},"page_size":{"type":"integer","minimum":1,"maximum":PAGE_SIZE_MAX}}),
        ),
        "create_user" | "get_user" | "update_user" | "update_user_profile" => user_schema(),
        "delete_user" | "reset_user_password" | "enable_user" | "disable_user" => {
            object(json!({"status":{"const":"success"},"message":{"type":"string"}}))
        }
        _ => panic!("unowned user operation: {id}"),
    }
}
pub fn request_schema(model: &str) -> Value {
    let fields = match model {
        "UserCreate" => CREATE_FIELDS,
        "UserUpdate" => UPDATE_FIELDS,
        "ProfileUpdate" => PROFILE_FIELDS,
        _ => panic!("unowned user model: {model}"),
    };
    let mut properties = serde_json::Map::new();
    for field in fields {
        let required = model == "UserCreate" && ["email", "display_name"].contains(field);
        let nullable = !required && !(model == "UserCreate" && *field == "roles");
        let mut schema = if [
            "roles",
            "extra_permissions",
            "allowed_services",
            "denied_services",
        ]
        .contains(field)
        {
            strings()
        } else {
            json!({"type":"string"})
        };
        if nullable {
            schema["type"] = json!([schema["type"].as_str().expect("field type"), "null"]);
        }
        match *field {
            "display_name" => {
                schema["minLength"] = json!(1);
                schema["maxLength"] = json!(DISPLAY_NAME_MAX);
            }
            "department" => {
                schema["maxLength"] = json!(DEPARTMENT_MAX);
                let mut values: Vec<Value> = DEPARTMENTS.iter().map(|v| json!(v)).collect();
                values.push(Value::Null);
                schema["enum"] = json!(values);
                if model == "UserCreate" {
                    schema["default"] = Value::Null;
                }
            }
            "roles" if model == "UserCreate" => {
                schema["default"] = json!(["user"]);
            }
            "email" => {
                schema["description"] = json!(
                    "Trimmed and lowercased; a local part is expanded using the configured allowed domain. The resulting address must use that domain. No credential is returned."
                );
            }
            "service_access_mode" => {
                schema["description"] = json!(
                    "Trimmed and lowercased; accepts all or allowlist after normalization. Null leaves policy unchanged."
                );
            }
            _ => {}
        }
        properties.insert((*field).into(), schema);
    }
    json!({"type":"object","additionalProperties":true,"properties":properties,
        "required":if model == "UserCreate" { vec!["email","display_name"] } else { vec![] },
        "x-native-validation":"Unknown fields ignored. String lengths count Unicode characters. Update/profile omission preserves stored values; explicit null display_name/department is forwarded. Null roles, extra_permissions and service-access fields are unchanged. Status/tier values and assignment catalog/authority checks remain storage-owned; explicit null status/tier fails the PostgreSQL guard. Profile accepts only display_name."})
}
fn response(description: &str, schema: Value) -> Value {
    json!({"description":description,"content":{"application/json":{"schema":schema}}})
}
pub fn extend_document(document: &mut Value) {
    for (name, model) in [
        ("NativeUserCreate", "UserCreate"),
        ("NativeUserUpdate", "UserUpdate"),
        ("NativeUserProfileUpdate", "ProfileUpdate"),
    ] {
        document["components"]["schemas"][name] = request_schema(model);
    }
    for (method, route, id) in USERS_ROUTES {
        let path = format!("/api/v1{route}");
        let mut parameters = vec![];
        if route.contains("{user_id}") {
            parameters.push(json!({"name":"user_id","in":"path","required":true,"schema":{"type":"string"},"description":"Decoded slash is rejected as not-found by the existing router."}));
        }
        if *id == "list_users" {
            for (name, schema) in [
                (
                    "page",
                    json!({"type":"integer","minimum":1,"default":PAGE_DEFAULT}),
                ),
                (
                    "page_size",
                    json!({"type":"integer","minimum":1,"maximum":PAGE_SIZE_MAX,"default":PAGE_SIZE_DEFAULT}),
                ),
                ("status", json!({"type":"string"})),
                ("search", json!({"type":"string"})),
            ] {
                parameters.push(json!({"name":name,"in":"query","required":false,"schema":schema,"description":"Last occurrence wins; unknown query keys are ignored. Empty status/search do not filter."}));
            }
        }
        let mut operation = json!({"operationId":super::roles::operation_id(method,&path,id),"parameters":parameters,
            "security":[{"NativeBearerAuth":[]},{"NativeBoundApiKey":[]}],"x-native-owner":"users_api::UsersApi",
            "x-native-response-schema":"bounded-source-owned",
            "x-native-authorization":"Admission resolves current account/tenant authority. List/get require user:list; create requires user:create; delete requires user:delete; other administration writes require user:edit. Writes also require exact admin/platform_admin. PostgreSQL rechecks actor/target under tenant locking, protects self/operator accounts and retains an active administrator. Profile uses its separate identity-resolver hook (the production hook delegates to the same AdmissionService), no endpoint RBAC, and only the caller's own display_name.",
            "x-native-side-effects":"Create is not an upsert. Password reset and disable advance credential revision; disable/delete revoke bound keys; enable does not restore keys. Errors/cancellation can follow committed writes or invalidation. Read back state before considering another mutation; do not automatically replay.",
            "responses":{"200":response("Success",success_schema(id))}});
        // These are observed bounded handler/guard/admission responses, not an
        // exhaustive claim over arbitrary injected collaborator or host errors.
        let detail = object(json!({"detail":{"type":"string"}}));
        for (status, description) in [
            ("400", "Account invariant rejected"),
            ("401", "Authentication failed"),
            ("403", "Insufficient authority"),
            ("404", "Account not found in caller tenant"),
            ("409", "Account conflict or last-administrator protection"),
            (
                "503",
                "Database, default password or authentication authority unavailable",
            ),
        ] {
            let schema = if matches!(status, "401" | "403" | "503") {
                json!({"oneOf":[detail.clone(),object(json!({"error":{"type":"string"}}))]})
            } else {
                detail.clone()
            };
            operation["responses"][status] = response(description, schema);
        }
        operation["responses"]["422"] = response(
            "Request or storage guard validation failed",
            json!({"oneOf":[detail,object(json!({"detail":{"type":"array","items":{"type":"object","additionalProperties":true}}}))]}),
        );
        operation["responses"]["500"] = response(
            "Redacted internal error",
            object(
                json!({"error":{"type":"object","required":["code","message"],"properties":{"code":{"const":2000},"message":{"const":"Internal server error"},"trace_id":{}},"additionalProperties":false}}),
            ),
        );
        operation["responses"]["413"] =
            json!({"description":"Host request-body limit exceeded; transport-defined body"});
        operation["responses"]["default"] = json!({"description":"Other host/admission/cancellation responses remain undeclared","x-native-undeclared":true});
        let request = match *id {
            "create_user" => Some("NativeUserCreate"),
            "update_user" => Some("NativeUserUpdate"),
            "update_user_profile" => Some("NativeUserProfileUpdate"),
            _ => None,
        };
        if let Some(name) = request {
            operation["requestBody"] = json!({"required":true,"content":{"application/json":{"schema":{"$ref":format!("#/components/schemas/{name}")}}}});
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
    fn route_table_drives_export_and_guard() {
        let document = super::super::document();
        assert_eq!(USERS_ROUTES.len(), 9);
        for (method, route, id) in USERS_ROUTES {
            let path = format!("/api/v1{route}");
            assert!(allowed_methods(&path).contains(method));
            assert_eq!(
                document["paths"][&path][method.to_ascii_lowercase()]["operationId"],
                super::super::roles::operation_id(method, &path, id)
            );
        }
        assert_eq!(allowed_methods("/api/v1/users"), vec!["GET", "POST"]);
        assert_eq!(
            allowed_methods("/api/v1/users/{user_id}"),
            vec!["GET", "PUT", "DELETE"]
        );
        assert_eq!(
            allowed_methods("/api/v1/users/{user_id}/profile"),
            vec!["PUT"]
        );
        assert!(allowed_methods("/users").is_empty());
    }
}
