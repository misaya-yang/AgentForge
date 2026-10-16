//! Lightweight source-owned declarations for routes implemented in this crate.
//! Compilable by the OpenAPI producer without linking the service. Responses
//! remain explicitly opaque where handlers return dynamic database projections.
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
fn text(max: usize) -> Value {
    json!({"type":"string","minLength":1,"maxLength":max})
}
fn object(fields: Value) -> Value {
    json!({"type":"object","additionalProperties":false,"required":fields.as_object().unwrap().keys().collect::<Vec<_>>(),"properties":fields})
}
fn operation(
    function: &str,
    path: &str,
    method: &str,
    status: u16,
    body: Option<Value>,
    queries: Vec<Value>,
) -> Value {
    let id = format!(
        "{function}{}_{}",
        path.chars()
            .map(|c| if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            })
            .collect::<String>(),
        method
    );
    let mut parameters = vec![];
    for part in path.split('/') {
        if let Some(name) = part.strip_prefix('{').and_then(|s| s.strip_suffix('}')) {
            parameters
                .push(json!({"name":name,"in":"path","required":true,"schema":{"type":"string"}}));
        }
    }
    parameters.extend(queries);
    let mut out = json!({"operationId":id,"tags":["Knowledge"],"parameters":parameters,"responses":{status.to_string():{"description":"Native Knowledge handler response","content":{"application/json":{"schema":{"type":"object","additionalProperties":true}}}},"default":{"description":"Native identity, authorization, admission or dependency failure","x-native-undeclared":true}},"x-native-response-schema":"opaque-native-database-projection","x-native-authentication":"verified gateway HMAC identity and current actor ACL"});
    if let Some(body) = body {
        out["requestBody"] =
            json!({"required":true,"content":{"application/json":{"schema":body}}});
    }
    out
}
pub fn extend_document(document: &mut Value) {
    let prefix = "/api/v1/knowledge/{dataset_id}/sources/confluence";
    let offset = || {
        vec![
            json!({"name":"offset","in":"query","required":false,"schema":{"type":"integer","default":0,"minimum":0,"maximum":100000}}),
        ]
    };
    let status = json!({"type":"string","enum":["active","disabled"]});
    let email = json!({"type":"string","minLength":1,"maxLength":320,"pattern":"^[^:\\x00-\\x1f]*@[^:\\x00-\\x1f]*$","writeOnly":true});
    let token = json!({"type":"string","minLength":1,"maxLength":4096,"pattern":"^[^\\x00-\\x1f]+$","writeOnly":true});
    let create = object(
        json!({"request_id":{"type":"string","format":"uuid"},"name":text(255),"domain":{"type":"string","description":"Validated Atlassian Cloud hostname without URL path or credentials"},"email":email,"api_token":token}),
    );
    let replace = object(
        json!({"expected_revision":text(80),"expected_status":status,"email":email,"api_token":token}),
    );
    let control =
        object(json!({"expected_revision":text(80),"expected_status":status,"status":status}));
    let binding = object(
        json!({"request_id":{"type":"string","format":"uuid"},"connection_id":text(255),"space_key":{"type":"string","minLength":1,"maxLength":255,"pattern":"^[A-Za-z0-9_~-]+$"},"space_id":text(255),"root_page_id":text(255)}),
    );
    for (suffix, method, function, status_code, body, query) in [
        (
            "/connections",
            "get",
            "list_source_connections",
            200,
            None,
            offset(),
        ),
        (
            "/connections",
            "post",
            "create_source_connection",
            201,
            Some(create),
            vec![],
        ),
        (
            "/connections/manage",
            "get",
            "list_managed_connections",
            200,
            None,
            offset(),
        ),
        (
            "/connections/{connection_id}",
            "get",
            "get_source_connection",
            200,
            None,
            vec![],
        ),
        (
            "/connections/{connection_id}/space",
            "get",
            "get_source_space",
            200,
            None,
            vec![
                json!({"name":"space_key","in":"query","required":true,"schema":{"type":"string"}}),
            ],
        ),
        (
            "/connections/{connection_id}/credentials",
            "post",
            "replace_source_credentials",
            200,
            Some(replace),
            vec![],
        ),
        (
            "/connections/{connection_id}/control",
            "get",
            "get_connection_impact",
            200,
            None,
            offset(),
        ),
        (
            "/connections/{connection_id}/control",
            "patch",
            "control_source_connection",
            200,
            Some(control),
            vec![],
        ),
        (
            "/bindings",
            "post",
            "create_source_binding",
            201,
            Some(binding),
            vec![],
        ),
        (
            "/bindings/{binding_id}",
            "get",
            "get_source_binding",
            200,
            None,
            vec![],
        ),
        (
            "/{binding_id}/sync",
            "post",
            "start_manual_sync",
            202,
            Some(object(
                json!({"request_id":{"type":"string","format":"uuid"}}),
            )),
            vec![],
        ),
        (
            "/{binding_id}/sync/{request_id}",
            "get",
            "get_manual_sync",
            200,
            None,
            vec![],
        ),
    ] {
        let path = format!("{prefix}{suffix}");
        document["paths"][&path][method] =
            operation(function, &path, method, status_code, body, query);
    }
    let path = "/api/v1/knowledge/{dataset_id}/documents/upload";
    let mut upload = operation("upload_document", path, "post", 200, None, vec![]);
    upload["requestBody"] = json!({"required":true,"content":{"multipart/form-data":{"schema":{"type":"object","required":["file"],"properties":{"file":{"type":"string","format":"binary"},"processing_mode":{"type":"string","maxLength":64,"default":"auto","description":"scanned and multimodal currently rejected by the source bounded publication policy"}}}}}});
    document["paths"][path]["post"] = upload;
    let mut sources = json!({});
    for (name, source) in [
        ("confluence/http.rs", include_str!("confluence/http.rs")),
        (
            "confluence/connections.rs",
            include_str!("confluence/connections.rs"),
        ),
        (
            "confluence/bindings.rs",
            include_str!("confluence/bindings.rs"),
        ),
        ("upload.rs", include_str!("upload.rs")),
    ] {
        sources[name] = json!(format!("sha256:{:x}", Sha256::digest(source.as_bytes())));
    }
    document["x-native-knowledge-owner"] = json!({"declaration_version":1,"source_sha256":sources,"coverage":"owned Confluence routes and path-backed upload; reused query/ingest routes require their owner declarations"});
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn declarations_are_owned_and_secrets_write_only() {
        let mut doc = json!({"paths":{}});
        extend_document(&mut doc);
        let count: usize = doc["paths"]
            .as_object()
            .unwrap()
            .values()
            .map(|v| v.as_object().unwrap().len())
            .sum();
        assert_eq!(count, 13);
        let op =
            &doc["paths"]["/api/v1/knowledge/{dataset_id}/sources/confluence/connections"]["post"];
        assert_eq!(
            op["requestBody"]["content"]["application/json"]["schema"]["properties"]["api_token"]["writeOnly"],
            true
        );
        assert!(op["responses"].get("201").is_some());
    }
}
