//! Native role schema and response ownership. No published-schema input.
//! Routes are taken from the same table used by the mounted router and guard.
pub const ROLES_ROUTES: &[(&str, &str, &str)] = &[
    ("GET", "/roles", "list_roles"),
    ("POST", "/roles", "create_role"),
    ("GET", "/roles/permissions", "list_permissions"),
    ("GET", "/roles/{role_name}", "get_role"),
    ("PUT", "/roles/{role_name}", "update_role"),
    ("DELETE", "/roles/{role_name}", "delete_role"),
    ("GET", "/roles/{role_name}/users", "get_role_users"),
];

use serde_json::{Value, json};
pub const NAME_MAX: usize = 100;
pub const DESCRIPTION_MAX: usize = 255;

pub fn allowed_methods(path: &str) -> Vec<&'static str> {
    // Preserve the existing literal-path fallback: PUT/DELETE on permissions
    // address the ordinary role named "permissions". GET lists the catalog.
    if path == "/api/v1/roles/permissions" {
        return vec!["GET", "PUT", "DELETE"];
    }
    ROLES_ROUTES
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
pub fn create_schema() -> Value {
    json!({"type":"object","additionalProperties":true,"required":["role_name","description"],"properties":{
        "role_name":{"type":"string","minLength":1,"maxLength":NAME_MAX,"pattern":"^[a-z_]+$"},
        "description":{"type":"string","minLength":1,"maxLength":DESCRIPTION_MAX},
        "permissions":{"type":"array","items":{"type":"string"},"default":[]}},
        "x-native-validation":"Unknown fields ignored. Permission codes must exist in the live catalog, except admin:*. Shared-role mutations require current platform-administrator authority."})
}
pub fn update_schema() -> Value {
    json!({"type":"object","additionalProperties":true,"properties":{
        "description":{"type":["string","null"],"minLength":1,"maxLength":DESCRIPTION_MAX},
        "permissions":{"type":["array","null"],"items":{"type":"string"}}},
        "x-native-validation":"Unknown fields ignored; omitted or null fields leave stored values unchanged. System roles cannot be changed. Permission catalog validation and current platform-administrator authority are required."})
}

macro_rules! shape {
    ($value:ident, $schema:ident, { $($field:ident: $ty:ty => $field_schema:expr),* $(,)? }) => {
        pub fn $value($($field:$ty),*) -> Value { json!({$(stringify!($field):$field),*}) }
        pub fn $schema() -> Value { object(json!({$(stringify!($field):$field_schema),*})) }
    };
}
shape!(role_value, role_schema, {
    role_name: &str => json!({"type":"string"}), description: Option<&str> => json!({"type":["string","null"]}),
    permissions: &[Value] => strings(), is_system: bool => json!({"type":"boolean"}), user_count:i64 => json!({"type":"integer"}),
    created_at: Option<&str> => json!({"type":["string","null"]}), updated_at: Option<&str> => json!({"type":["string","null"]})
});
shape!(permission_value, permission_schema, {
    permission_code: &str => json!({"type":"string"}), name: &str => json!({"type":"string"}),
    category: &str => json!({"type":"string"}), resource: &str => json!({"type":"string"}), action: &str => json!({"type":"string"}),
    description: Option<&str> => json!({"type":["string","null"]}),is_system: bool => json!({"type":"boolean"})
});
pub fn role_list(roles: Vec<Value>) -> Value {
    json!({"total":roles.len(),"roles":roles})
}
pub fn permission_list(permissions: Vec<Value>) -> Value {
    json!({"total":permissions.len(),"permissions":permissions})
}
pub fn role_users(role_name: String, users: Vec<Value>) -> Value {
    json!({"role_name":role_name,"total":users.len(),"users":users})
}
pub fn deleted(role_name: &str) -> Value {
    json!({"status":"success","message":format!("Role {role_name} deleted")})
}
pub(crate) fn success_schema(id: &str) -> Value {
    match id {
        "list_roles" => object(
            json!({"total":{"type":"integer","minimum":0},"roles":{"type":"array","items":role_schema()}}),
        ),
        "list_permissions" => object(
            json!({"total":{"type":"integer","minimum":0},"permissions":{"type":"array","items":permission_schema()}}),
        ),
        "get_role_users" => object(
            json!({"role_name":{"type":"string"},"total":{"type":"integer","minimum":0},"users":{"type":"array","items":{"x-native-dynamic-row":true}}}),
        ),
        "delete_role" => object(json!({"status":{"const":"success"},"message":{"type":"string"}})),
        "get_role" | "create_role" | "update_role" => role_schema(),
        _ => panic!("unowned role operation: {id}"),
    }
}
fn response(description: &str, schema: Value) -> Value {
    json!({"description":description,"content":{"application/json":{"schema":schema}}})
}
pub fn operation_id(method: &str, path: &str, id: &str) -> String {
    let path: String = path
        .trim_start_matches('/')
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!("{id}_{path}_{}", method.to_ascii_lowercase())
}
pub fn extend_document(document: &mut Value) {
    document["components"]["schemas"]["NativeRoleCreate"] = create_schema();
    document["components"]["schemas"]["NativeRoleUpdate"] = update_schema();
    for (method, route, id) in ROLES_ROUTES {
        let path = format!("/api/v1{route}");
        let mut parameters = vec![];
        if route.contains("{role_name}") {
            parameters.push(
                json!({"name":"role_name","in":"path","required":true,"schema":{"type":"string"}}),
            );
        }
        if *id == "list_permissions" {
            parameters.push(json!({"name":"category","in":"query","required":false,"schema":{"type":"string"},"description":"Last category occurrence wins; other query keys are ignored."}));
        }
        let mut operation = json!({"operationId":operation_id(method,&path,id),"parameters":parameters,
            "security":[{"NativeBearerAuth":[]},{"NativeBoundApiKey":[]}],
            "x-native-owner":"roles_api::RolesApi",
            "x-native-route-precedence":"GET /roles/permissions lists the permission catalog; PUT/DELETE at the same literal path preserve the /roles/{role_name} mutation semantics for role_name=permissions.",
            "x-native-response-schema":"bounded-source-owned",
            "x-native-authorization":"AdmissionService resolves per-request authority; role:list for reads, role:create/edit/delete for writes. Active tenant account is required; mutations additionally recheck current platform-admin status in storage. Member reads are tenant scoped.",
            "responses":{"200":response("Success",success_schema(id))}});
        let detail = object(json!({"detail":{"type":"string"}}));
        for (status, description) in [
            (
                "400",
                "Role invariant or permission catalog validation failed",
            ),
            ("401", "Authentication failed"),
            ("403", "Insufficient authority"),
            ("404", "Role not found"),
            ("503", "Database or authentication authority unavailable"),
        ] {
            let schema = if matches!(status, "401" | "403" | "503") {
                json!({"oneOf":[detail.clone(),object(json!({"error":{"type":"string"}}))]})
            } else {
                detail.clone()
            };
            operation["responses"][status] = response(description, schema);
        }
        operation["responses"]["422"] = response(
            "Request validation failed",
            object(
                json!({"detail":{"type":"array","items":{"type":"object","additionalProperties":true}}}),
            ),
        );
        operation["responses"]["500"] = response(
            "Redacted internal error",
            object(
                json!({"error":{"type":"object","required":["code","message"],"properties":{"code":{"const":2000},"message":{"const":"Internal server error"},"trace_id":{}},"additionalProperties":false}}),
            ),
        );
        operation["responses"]["default"] = json!({"description":"Other host/admission/cancellation responses remain undeclared","x-native-undeclared":true});
        operation["responses"]["413"] =
            json!({"description":"Host request-body limit exceeded; transport-defined body"});
        operation["x-native-cancellation"] = json!(
            "Cancellation response is host-defined and is not claimed as covered by this bounded schema."
        );
        let request = match *id {
            "create_role" => Some("NativeRoleCreate"),
            "update_role" => Some("NativeRoleUpdate"),
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
    fn route_table_drives_export_and_method_guard() {
        let doc = super::super::document();
        for (method, path, id) in ROLES_ROUTES {
            let path = format!("/api/v1{path}");
            assert!(allowed_methods(&path).contains(method));
            assert_eq!(
                doc["paths"][&path][method.to_ascii_lowercase()]["operationId"],
                operation_id(method, &path, id)
            );
            assert!(doc["paths"][&path][method.to_ascii_lowercase()]["responses"]["200"]["content"]["application/json"]["schema"].is_object());
        }
        assert_eq!(
            allowed_methods("/api/v1/roles/permissions"),
            vec!["GET", "PUT", "DELETE"]
        );
        assert_eq!(
            allowed_methods("/api/v1/roles/{role_name}"),
            vec!["GET", "PUT", "DELETE"]
        );
        assert!(allowed_methods("/roles").is_empty());
    }
}
