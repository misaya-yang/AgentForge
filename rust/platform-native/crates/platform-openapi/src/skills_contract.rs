//! Declarations tied to the native immutable Skills HTTP handlers.
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
fn operation(
    doc: &mut Value,
    path: &str,
    method: &str,
    function: &str,
    status: &str,
    schema: Value,
    body: Option<Value>,
) {
    let name = path
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect::<String>();
    let mut op = json!({"operationId":format!("{function}{name}_{method}"),"tags":["skills"],"parameters":[],"responses":{status:{"description":"Successful Response","content":{"application/json":{"schema":schema}}},"default":{"description":"Native identity, capability, policy or artifact failure","x-native-undeclared":true}}});
    if path.contains("{name}") {
        op["parameters"] =
            json!([{"name":"name","in":"path","required":true,"schema":{"type":"string"}}]);
    }
    if let Some(body) = body {
        op["requestBody"] = json!({"required":true,"content":{"application/json":{"schema":body}}});
    }
    doc["paths"][path][method] = op;
}
pub fn extend_document(doc: &mut Value) {
    let opaque = || json!({"type":"object","additionalProperties":true,"x-native-response-schema":"immutable-manifest-projection"});
    operation(
        doc,
        "/api/v1/skills",
        "get",
        "list_skills",
        "200",
        json!({"type":"object","required":["total","skills"],"properties":{"total":{"type":"integer"},"skills":{"type":"array","items":opaque()}}}),
        None,
    );
    doc["paths"]["/api/v1/skills"]["get"]["parameters"] = json!([{"name":"enabled_only","in":"query","required":false,"schema":{"type":"boolean","default":true}}]);
    operation(
        doc,
        "/api/v1/skills/upload",
        "post",
        "upload_skill",
        "201",
        opaque(),
        None,
    );
    doc["paths"]["/api/v1/skills/upload"]["post"]["requestBody"] = json!({"required":true,"content":{"multipart/form-data":{"schema":{"type":"object","required":["file"],"properties":{"file":{"type":"string","format":"binary","description":"UTF-8 .md Skill artifact, at most 50000 bytes"}}}}}});
    operation(
        doc,
        "/api/v1/skills/{name}",
        "get",
        "get_skill",
        "200",
        opaque(),
        None,
    );
    let optional = |v: Value| json!({"anyOf":[v,{"type":"null"}],"default":null});
    operation(
        doc,
        "/api/v1/skills/{name}",
        "patch",
        "update_skill",
        "200",
        opaque(),
        Some(
            json!({"type":"object","properties":{"title":optional(json!({"type":"string"})),"description":optional(json!({"type":"string"})),"tags":optional(json!({"type":"array","items":{"type":"string"}})),"permissions":optional(json!({"type":"array","items":{"type":"string"}})),"enabled":optional(json!({"type":"boolean"}))}}),
        ),
    );
    operation(
        doc,
        "/api/v1/skills/{name}",
        "delete",
        "delete_skill",
        "200",
        json!({"type":"object","required":["deleted","name"],"properties":{"deleted":{"const":true},"name":{"type":"string"}}}),
        None,
    );
    operation(
        doc,
        "/api/v1/skills/{name}/test",
        "post",
        "test_skill",
        "200",
        json!({"oneOf":[{"type":"object","required":["success","type","result"],"properties":{"success":{"const":true},"type":{"const":"skill_instructions"},"result":{"type":"string"}}},{"type":"object","required":["success","error"],"properties":{"success":{"const":false},"error":{"type":"string"}}}]}),
        Some(
            json!({"type":"object","required":["input"],"properties":{"input":{"type":"string"}}}),
        ),
    );
    for (action, function) in [("enable", "enable_skill"), ("disable", "disable_skill")] {
        operation(
            doc,
            &format!("/api/v1/skills/{{name}}/{action}"),
            "post",
            function,
            "200",
            opaque(),
            None,
        );
    }
    for (name, bytes) in [
        (
            "../../platform-gateway/src/migration_content/skills.rs",
            include_bytes!("../../platform-gateway/src/migration_content/skills.rs").as_slice(),
        ),
        (
            "../../platform-gateway/src/migration_host/application.rs",
            include_bytes!("../../platform-gateway/src/migration_host/application.rs").as_slice(),
        ),
        (
            "../../platform-openapi/src/skills_contract.rs",
            include_bytes!("skills_contract.rs").as_slice(),
        ),
    ] {
        doc["x-native-contract"]["common_source_sha256"][name] =
            json!(format!("{:x}", Sha256::digest(bytes)));
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn eight_actual_operations_and_empty_test_input() {
        let mut d = json!({"paths":{},"x-native-contract":{"common_source_sha256":{}}});
        extend_document(&mut d);
        assert_eq!(
            d["paths"]
                .as_object()
                .unwrap()
                .values()
                .map(|v| v.as_object().unwrap().len())
                .sum::<usize>(),
            8
        );
        assert!(d["paths"]["/api/v1/skills/{name}/test"]["post"]["requestBody"]["content"]["application/json"]["schema"]["properties"]["input"].get("minLength").is_none());
        assert!(
            d["paths"]["/api/v1/skills/upload"]["post"]["responses"]
                .get("201")
                .is_some()
        );
    }
}
