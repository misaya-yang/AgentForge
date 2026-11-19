//! Source-owned declaration adapter for the actually mounted provider/model
//! administration factories. Dynamic catalog projections remain opaque.
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
fn s(min: usize, max: Option<usize>) -> Value {
    let mut v = json!({"type":"string","minLength":min});
    if let Some(n) = max {
        v["maxLength"] = json!(n)
    }
    v
}
fn nullable(v: Value) -> Value {
    json!({"anyOf":[v,{"type":"null"}],"default":null})
}
fn object(p: Value, required: &[&str]) -> Value {
    json!({"type":"object","properties":p,"required":required})
}
fn array(v: Value) -> Value {
    json!({"type":"array","items":v})
}
fn provider(update: bool) -> Value {
    let mut p = json!({"provider_id":s(1,Some(50)),"display_name":s(1,Some(100)),"api_type":{"type":"string","default":"openai"},"base_url":nullable(s(0,Some(500))),"metadata":{"type":"object","additionalProperties":true,"default":{}},"is_enabled":{"type":"boolean","default":true},"api_key":{"anyOf":[{"type":"string"},{"type":"null"}],"default":null,"writeOnly":true}});
    if update {
        p.as_object_mut().unwrap().remove("provider_id");
        for v in p.as_object_mut().unwrap().values_mut() {
            let mut raw = v.clone();
            if let Some(o) = raw.as_object_mut() {
                o.remove("default");
            }
            *v = if raw.get("anyOf").is_some() {
                raw
            } else {
                nullable(raw)
            };
        }
        object(p, &[])
    } else {
        object(p, &["provider_id", "display_name"])
    }
}
fn provider_response() -> Value {
    let mut p = provider(false)["properties"].clone();
    let o = p.as_object_mut().unwrap();
    o.remove("api_key");
    o.insert("tenant_id".into(), s(0, None));
    o.insert("has_api_key".into(), json!({"type":"boolean"}));
    o.insert(
        "allow_environment_credentials".into(),
        json!({"type":"boolean","default":false}),
    );
    for k in ["created_at", "updated_at"] {
        o.insert(k.into(), json!({"type":"string","format":"date-time"}));
    }
    let required = o.keys().cloned().collect::<Vec<_>>();
    json!({"type":"object","properties":p,"required":required})
}
fn model(update: bool) -> Value {
    let mut p = json!({"model_id":s(1,Some(100)),"provider_id":s(1,Some(50)),"display_name":s(1,Some(100)),"context_window":{"type":"integer","minimum":1,"default":128000},"max_output_tokens":{"type":"integer","minimum":1,"default":4096},"supports_vision":{"type":"boolean","default":false},"supports_tools":{"type":"boolean","default":true},"input_price_per_1k":{"anyOf":[{"type":"number","minimum":0},{"type":"string"}],"default":"0"},"output_price_per_1k":{"anyOf":[{"type":"number","minimum":0},{"type":"string"}],"default":"0"},"access_level":{"type":"string","default":"public"},"is_enabled":{"type":"boolean","default":true},"sort_order":{"type":"integer","default":0},"capability_overrides":{"type":"object","additionalProperties":true,"default":{}}});
    if update {
        let o = p.as_object_mut().unwrap();
        o.remove("provider_id");
        o.insert(
            "expected_capability_revision".into(),
            json!({"type":"integer","minimum":1}),
        );
        for v in o.values_mut() {
            let mut raw = v.clone();
            raw.as_object_mut().unwrap().remove("default");
            *v = nullable(raw);
        }
        object(p, &[])
    } else {
        object(p, &["model_id", "provider_id", "display_name"])
    }
}
fn model_response() -> Value {
    let mut p = model(false)["properties"].clone();
    let o = p.as_object_mut().unwrap();
    for k in ["input_price_per_1k", "output_price_per_1k"] {
        o.insert(
            k.into(),
            json!({"type":"string","description":"Native Decimal serialization"}),
        );
    }
    o.insert("tenant_id".into(), s(0, None));
    for k in ["catalog_capabilities", "effective_capabilities"] {
        o.insert(
            k.into(),
            json!({"type":"object","additionalProperties":true,"default":{}}),
        );
    }
    o.insert(
        "capability_revision".into(),
        json!({"type":"integer","minimum":1,"default":1}),
    );
    for k in ["created_at", "updated_at"] {
        o.insert(k.into(), json!({"type":"string","format":"date-time"}));
    }
    let req = o.keys().cloned().collect::<Vec<_>>();
    json!({"type":"object","properties":p,"required":req})
}
fn all_fields(properties: Value) -> Value {
    let required = properties
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    json!({"type":"object","properties":properties,"required":required})
}
fn sync_response() -> Value {
    let row = all_fields(
        json!({"model_id":s(0,None),"provider_id":s(0,None),"display_name":s(0,None),"is_enabled":{"type":"boolean"}}),
    );
    all_fields(
        json!({"provider_id":s(0,None),"template_id":nullable(s(0,None)),"created_models":array(row.clone()),"updated_models":array(row),"skipped_models":array(all_fields(json!({"model_id":s(0,None),"reason":s(0,None)}))),"discovery_warnings":array(s(0,None))}),
    )
}
fn template_response() -> Value {
    let credential = all_fields(
        json!({"name":s(0,None),"label":s(0,None),"field_type":s(0,None),"required":{"type":"boolean"},"placeholder":nullable(s(0,None))}),
    );
    let catalog_model = all_fields(
        json!({"model_id":s(0,None),"display_name":s(0,None),"context_window":{"type":"integer"},"max_output_tokens":{"type":"integer"},"supports_vision":{"type":"boolean"},"supports_tools":{"type":"boolean"},"input_price_per_1k":{"type":"number"},"output_price_per_1k":{"type":"number"},"access_level":s(0,None),"sort_order":{"type":"integer"}}),
    );
    all_fields(
        json!({"template_id":s(0,None),"display_name":s(0,None),"description":s(0,None),"default_provider_id":s(0,None),"api_type":s(0,None),"default_base_url":s(0,None),"credential_fields":array(credential),"discovery_strategy":s(0,None),"default_models":array(catalog_model),"default_metadata":{"type":"object","additionalProperties":true},"advanced":{"type":"boolean"}}),
    )
}
fn adapter_response() -> Value {
    all_fields(
        json!({"id":s(0,None),"kind":s(0,None),"label":s(0,None),"settings_schema":{"type":"object","additionalProperties":true}}),
    )
}
fn query(name: &str, schema: Value, required: bool) -> Value {
    json!({"in":"query","name":name,"required":required,"schema":schema})
}
fn add(
    doc: &mut Value,
    path: &str,
    method: &str,
    name: &str,
    body: Option<Value>,
    response: Value,
    mut params: Vec<Value>,
) {
    for segment in path.split('/') {
        if let Some(id) = segment.strip_prefix('{').and_then(|s| s.strip_suffix('}')) {
            params.push(json!({"in":"path","name":id,"required":true,"schema":{"type":"string"}}));
        }
    }
    let normalized = path
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect::<String>();
    let mut op = json!({"operationId":format!("{name}{normalized}_{method}"),"tags":[if path.contains("model")&&!path.contains("provider"){"models"}else{"providers"}],"parameters":params,"responses":{"200":{"description":"Successful Response","content":{"application/json":{"schema":response}}},"422":{"description":"Semantic request validation failure","content":{"application/json":{"schema":{"type":"object","required":["detail"],"properties":{"detail":{"type":"array","items":{"type":"object","additionalProperties":true}}}}}}},"default":{"description":"Authentication, capability or service failure","x-native-undeclared":true}}});
    if let Some(b) = body {
        op["requestBody"] = json!({"required":true,"content":{"application/json":{"schema":b}}});
    }
    doc["paths"][path][method] = op;
}
pub fn extend_document(doc: &mut Value) {
    let flag = || {
        query(
            "include_disabled",
            json!({"type":"boolean","default":false}),
            false,
        )
    };
    let provider_id = || query("provider_id", nullable(s(0, None)), false);
    let opaque = || json!({"type":"object","additionalProperties":true,"x-native-response-schema":"opaque-service-projection"});
    add(
        doc,
        "/api/v1/providers",
        "get",
        "list_providers",
        None,
        array(provider_response()),
        vec![flag()],
    );
    add(
        doc,
        "/api/v1/providers",
        "post",
        "create_provider",
        Some(provider(false)),
        provider_response(),
        vec![],
    );
    let mut template = provider(true);
    template["properties"]
        .as_object_mut()
        .unwrap()
        .remove("api_type");
    template["properties"]["template_id"] = s(1, None);
    template["properties"]["provider_id"] = nullable(s(1, Some(50)));
    template["properties"]["is_enabled"] = json!({"type":"boolean","default":true});
    template["required"] = json!(["template_id"]);
    add(
        doc,
        "/api/v1/providers/from-template",
        "post",
        "create_provider_from_template",
        Some(template),
        provider_response(),
        vec![],
    );
    add(
        doc,
        "/api/v1/provider-templates",
        "get",
        "list_templates",
        None,
        array(template_response()),
        vec![],
    );
    for (method, name, body, response) in [
        ("get", "get_provider", None, provider_response()),
        (
            "put",
            "update_provider",
            Some(provider(true)),
            provider_response(),
        ),
        (
            "delete",
            "delete_provider",
            None,
            object(
                json!({"provider_id":{"type":"string"},"status":{"const":"deleted"}}),
                &["provider_id", "status"],
            ),
        ),
    ] {
        add(
            doc,
            "/api/v1/providers/{provider_id}",
            method,
            name,
            body,
            response,
            vec![],
        );
    }
    add(
        doc,
        "/api/v1/providers/{provider_id}/test",
        "post",
        "test_provider_connection",
        None,
        opaque(),
        vec![],
    );
    add(
        doc,
        "/api/v1/providers/{provider_id}/models/sync",
        "post",
        "sync_provider_models",
        None,
        sync_response(),
        vec![],
    );
    add(
        doc,
        "/api/v1/models",
        "get",
        "list_models",
        None,
        array(model_response()),
        vec![provider_id(), flag()],
    );
    add(
        doc,
        "/api/v1/models",
        "post",
        "create_model",
        Some(model(false)),
        model_response(),
        vec![],
    );
    for (method, name, body, response) in [
        ("get", "get_model", None, model_response()),
        ("put", "update_model", Some(model(true)), model_response()),
        (
            "delete",
            "delete_model",
            None,
            object(
                json!({"model_id":{"type":"string"},"status":{"const":"deleted"}}),
                &["model_id", "status"],
            ),
        ),
    ] {
        add(
            doc,
            "/api/v1/models/{model_id}",
            method,
            name,
            body,
            response,
            vec![provider_id()],
        );
    }
    add(
        doc,
        "/api/v1/models/{model_id}/capabilities/reset",
        "post",
        "reset_model_capabilities",
        None,
        model_response(),
        vec![
            provider_id(),
            query(
                "expected_capability_revision",
                nullable(json!({"type":"integer","minimum":1})),
                false,
            ),
        ],
    );
    add(
        doc,
        "/api/v1/models/{model_id}/toggle",
        "patch",
        "toggle_model",
        None,
        opaque(),
        vec![
            provider_id(),
            query("is_enabled", json!({"type":"boolean"}), true),
        ],
    );
    add(
        doc,
        "/api/v1/model-capability-adapters",
        "get",
        "list_capability_adapters",
        None,
        array(adapter_response()),
        vec![],
    );
    add(
        doc,
        "/api/v1/model-capability-adapters/{adapter_id}/schema",
        "get",
        "get_capability_adapter_schema",
        None,
        adapter_response(),
        vec![],
    );
    for (name, bytes) in [
        (
            "provider_templates.rs",
            include_bytes!("../../platform-gateway-foundation/src/provider_templates.rs")
                .as_slice(),
        ),
        (
            "../data/capability-adapters.json",
            include_bytes!("../../platform-gateway-foundation/data/capability-adapters.json")
                .as_slice(),
        ),
        (
            "provider_admin_api.rs",
            include_bytes!("../../platform-gateway-foundation/src/provider_admin_api.rs")
                .as_slice(),
        ),
        (
            "model_admin_api.rs",
            include_bytes!("../../platform-gateway-foundation/src/model_admin_api.rs").as_slice(),
        ),
        (
            "../../platform-openapi/src/provider_contract.rs",
            include_bytes!("provider_contract.rs").as_slice(),
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
    fn eighteen_mounted_operations_preserve_request_bounds_and_secret_projection() {
        let mut d = json!({"paths":{},"x-native-contract":{"common_source_sha256":{}}});
        extend_document(&mut d);
        assert_eq!(
            d["paths"]
                .as_object()
                .unwrap()
                .values()
                .map(|v| v.as_object().unwrap().len())
                .sum::<usize>(),
            18
        );
        assert_eq!(
            provider(false)["properties"]["provider_id"]["maxLength"],
            50
        );
        assert_eq!(provider(false)["properties"]["api_key"]["writeOnly"], true);
        assert!(provider_response()["properties"].get("api_key").is_none());
        assert!(model(true)["properties"].get("provider_id").is_none());
        assert_eq!(
            model_response()["properties"]["input_price_per_1k"]["type"],
            "string"
        );
    }
}
