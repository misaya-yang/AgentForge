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
    let space_write = json!({"type":"object","additionalProperties":false,"required":["name"],"properties":{"name":text(200),"icon":{"type":"string","maxLength":32},"kind":{"type":"string","enum":["personal","team","tenant"]},"visibility":{"type":"string","enum":["private","members","tenant"]}}});
    let space_patch = json!({"type":"object","additionalProperties":false,"properties":space_write["properties"].clone()});
    let item_write = json!({"type":"object","additionalProperties":false,"required":["title"],"properties":{"title":text(500),"kind":{"type":"string","enum":["page","folder","file","link","slide","site","image","external"]},"icon":{"type":"string","maxLength":32},"parent_id":{"type":"string","maxLength":255},"position":{"type":"string","minLength":1,"maxLength":64},"content_format":{"type":"string","enum":["blocknote_json","markdown","binary"]},"source":{"type":"string","maxLength":96}}});
    let item_patch = json!({"type":"object","additionalProperties":false,"properties":{"title":text(500),"icon":{"type":"string","maxLength":32},"parent_id":{"type":"string","maxLength":255},"clear_parent":{"type":"boolean"},"position":{"type":"string","minLength":1,"maxLength":64},"space_id":{"type":"string","maxLength":255}}});
    let principal = json!({"type":"object","additionalProperties":false,"required":["principal_type","principal_id","role"],"properties":{"principal_type":{"type":"string","enum":["user","group","role"]},"principal_id":text(255),"role":{"type":"string","enum":["owner","editor","commenter","viewer"]}}});
    let members = json!({"type":"object","additionalProperties":false,"required":["members"],"properties":{"members":{"type":"array","maxItems":200,"items":principal}}});
    let shares = json!({"type":"object","additionalProperties":false,"required":["shares"],"properties":{"shares":{"type":"array","maxItems":200,"items":principal}}});
    let content = json!({"type":"object","additionalProperties":false,"required":["content_format","blocks","markdown"],"properties":{"content_format":{"type":"string","enum":["blocknote_json","markdown","binary"]},"blocks":{},"markdown":{"type":"string","maxLength":1000000}}});
    let item_query = vec![
        json!({"name":"view","in":"query","required":false,"schema":{"type":"string","enum":["all","recent","starred","mine","shared"]}}),
        json!({"name":"kind","in":"query","required":false,"schema":{"type":"string","enum":["page","folder","file","link","slide","site","image","external"]}}),
        json!({"name":"q","in":"query","required":false,"schema":{"type":"string","maxLength":200}}),
        json!({"name":"space_id","in":"query","required":false,"schema":{"type":"string","maxLength":255}}),
        json!({"name":"cursor","in":"query","required":false,"schema":{"type":"string","maxLength":512}}),
        json!({"name":"limit","in":"query","required":false,"schema":{"type":"integer","minimum":1,"maximum":200}}),
    ];
    let trash_query = item_query[1..].to_vec();
    let if_match = vec![json!({"name":"If-Match","in":"header","required":false,"schema":{"type":"string","maxLength":32}})];
    let diff_query = vec![
        json!({"name":"from","in":"query","required":true,"schema":{"type":"integer","minimum":1}}),
        json!({"name":"to","in":"query","required":true,"schema":{"type":"integer","minimum":1}}),
    ];
    let export_query = vec![json!({"name":"format","in":"query","required":false,"schema":{"type":"string","enum":["markdown","html"]}})];
    let import_body = json!({"type":"object","additionalProperties":false,"required":["format","content_base64"],"properties":{"format":{"type":"string","enum":["markdown","md","html","zip"]},"filename":{"type":"string","maxLength":255},"content_base64":{"type":"string","maxLength":12000000},"parent_id":{"type":"string","maxLength":255}}});
    for (path, method, function, status_code, body, query) in [
        ("/api/v1/knowledge/spaces", "get", "list_spaces", 200, None, vec![]),
        ("/api/v1/knowledge/spaces", "post", "create_space", 201, Some(space_write), vec![]),
        ("/api/v1/knowledge/spaces/{space_id}", "get", "get_space", 200, None, vec![]),
        ("/api/v1/knowledge/spaces/{space_id}", "patch", "patch_space", 200, Some(space_patch), vec![]),
        ("/api/v1/knowledge/spaces/{space_id}", "delete", "archive_space", 204, None, vec![]),
        ("/api/v1/knowledge/spaces/{space_id}/members", "get", "list_space_members", 200, None, vec![]),
        ("/api/v1/knowledge/spaces/{space_id}/members", "put", "replace_space_members", 200, Some(members), vec![]),
        ("/api/v1/knowledge/spaces/{space_id}/items", "post", "create_space_item", 201, Some(item_write), vec![]),
        ("/api/v1/knowledge/spaces/{space_id}/tree", "get", "list_space_tree", 200, None, vec![]),
        ("/api/v1/knowledge/spaces/{space_id}/import", "post", "import_space_pages", 201, Some(import_body), vec![]),
        ("/api/v1/knowledge/space-items", "get", "list_space_items", 200, None, item_query),
        ("/api/v1/knowledge/space-items/{item_id}", "get", "get_space_item", 200, None, vec![]),
        ("/api/v1/knowledge/space-items/{item_id}", "patch", "patch_space_item", 200, Some(item_patch), vec![]),
        ("/api/v1/knowledge/space-items/{item_id}", "delete", "delete_space_item", 204, None, vec![]),
        ("/api/v1/knowledge/space-items/{item_id}/restore", "post", "restore_space_item", 200, None, vec![]),
        ("/api/v1/knowledge/space-items/{item_id}/star", "put", "star_space_item", 204, None, vec![]),
        ("/api/v1/knowledge/space-items/{item_id}/star", "delete", "unstar_space_item", 204, None, vec![]),
        ("/api/v1/knowledge/space-items/{item_id}/shares", "put", "replace_space_item_shares", 200, Some(shares), vec![]),
        ("/api/v1/knowledge/space-items/{item_id}/content", "get", "get_space_item_content", 200, None, vec![]),
        ("/api/v1/knowledge/space-items/{item_id}/content", "put", "put_space_item_content", 200, Some(content), if_match.clone()),
        ("/api/v1/knowledge/space-items/{item_id}/revisions", "get", "list_space_item_revisions", 200, None, vec![]),
        ("/api/v1/knowledge/space-items/{item_id}/revisions/{revision}/restore", "post", "restore_space_item_revision", 200, None, if_match),
        ("/api/v1/knowledge/space-items/{item_id}/diff", "get", "diff_space_item_revisions", 200, None, diff_query),
        ("/api/v1/knowledge/space-items/{item_id}/backlinks", "get", "list_space_item_backlinks", 200, None, vec![]),
        ("/api/v1/knowledge/space-items/{item_id}/export", "get", "export_space_item", 200, None, export_query),
        ("/api/v1/knowledge/space-trash", "get", "list_space_trash", 200, None, trash_query),
    ] {
        document["paths"][path][method] = operation(function, path, method, status_code, body, query);
    }
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
        ("spaces_http.rs", include_str!("spaces_http.rs")),
    ] {
        sources[name] = json!(format!("sha256:{:x}", Sha256::digest(source.as_bytes())));
    }
    document["x-native-knowledge-owner"] = json!({"declaration_version":1,"source_sha256":sources,"coverage":"owned Confluence routes, path-backed upload, and knowledge spaces; reused query/ingest routes require their owner declarations"});
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
        assert_eq!(count, 39);
        let op =
            &doc["paths"]["/api/v1/knowledge/{dataset_id}/sources/confluence/connections"]["post"];
        assert_eq!(
            op["requestBody"]["content"]["application/json"]["schema"]["properties"]["api_token"]["writeOnly"],
            true
        );
        assert!(op["responses"].get("201").is_some());
    }
}
