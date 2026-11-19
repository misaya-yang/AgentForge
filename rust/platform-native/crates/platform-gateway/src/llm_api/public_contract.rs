//! Pure additive producer shared by offline OpenAPI and the live Gateway.
//! Provider-native payloads stay open; Gateway errors have an exact envelope.
use serde_json::{Map, Value, json};

fn section<'a>(document: &'a mut Value, key: &str) -> Option<&'a mut Map<String, Value>> {
    document
        .as_object_mut()?
        .entry(key)
        .or_insert_with(|| json!({}))
        .as_object_mut()
}

pub fn extend_document(document: &mut Value) {
    let Some(paths) = section(document, "paths") else {
        return;
    };
    for (path, operation, required, streaming) in [
        (
            "/v1/chat/completions",
            "native_llm_chat_completions",
            vec!["model", "messages"],
            true,
        ),
        (
            "/v1/embeddings",
            "native_llm_embeddings",
            vec!["model", "input"],
            false,
        ),
        (
            "/v1/messages",
            "native_llm_anthropic_messages",
            vec!["model", "messages"],
            true,
        ),
    ] {
        let mut request = json!({
            "type":"object", "additionalProperties":true, "required":required,
            "description":"Provider-native JSON. Unknown fields, cache controls and reasoning extensions pass through unchanged when protocols match. Only registered model alias resolution, a missing enforced output cap and Chat stream_options.include_usage may change the body. Conflicting top-level duplicate keys are rejected.",
            "properties":{
                "model":{"type":"string","minLength":1,"maxLength":512,"description":"Registered provider/model identity, or an unqualified model ID only when unique within the current tenant."},
                "messages":{"type":"array","items":{}}, "input":{},
                "stream":{"type":"boolean","default":false},
                "max_tokens":{"type":"integer","minimum":1},
                "max_completion_tokens":{"type":"integer","minimum":1}
            }
        });
        if let Some(properties) = request["properties"].as_object_mut() {
            if !streaming {
                properties.remove("messages");
                properties.remove("max_tokens");
                properties.remove("max_completion_tokens");
                properties.insert("stream".into(), json!({"type":"boolean","enum":[false]}));
            } else {
                properties.remove("input");
                if path == "/v1/messages" {
                    properties.remove("max_completion_tokens");
                }
            }
        }
        let mut success = json!({
            "description":"Native provider response. Unknown response fields and original bytes are preserved. A successful completion requires observed terminal usage and canonical accounting.",
            "headers":{"x-route-decision":{"description":"Optional: present only when an authorized routing policy produced a persisted decision for this invocation.","schema":{"type":"string","pattern":"^[0-9a-f]{64}$"}}},
            "content":{"application/json":{"schema":{},"description":"Provider-native response JSON; no closed Gateway projection."}}
        });
        if streaming {
            success["content"]["text/event-stream"] = json!({
                "schema":{"type":"string"},
                "description":"Unmodified native SSE bytes. OpenAI uses final usage plus [DONE]; Anthropic uses cumulative message_delta usage plus message_stop. Interrupted or unknown streams have a body error, never a synthesized success terminator."
            });
        }
        let operation = json!({
            "operationId":operation,"tags":["Native LLM"],
            "security":[{"NativeLlmBearer":[]},{"NativeLlmApiKey":[]}],
            "x-native-authority":"Existing AuthService/AdmissionService current account or bound API key; tenant/model/service policy, routing, atomic admission and frozen-price accounting are shared with the Gateway.",
            "x-native-passthrough":"same-protocol-byte-preserving; mismatched or unsupported capability fails closed; no automatic provider retry",
            "requestBody":{"required":true,"content":{"application/json":{"schema":request}}},
            "responses":{
                "200":success,
                "default":{
                    "description":"Gateway rejections use the exact NativeLlmGatewayError envelope. Provider rejections preserve their native status/body and may have a different envelope. Non-2xx without observed usage is unknown spend, not a zero-cost success.",
                    "content":{"application/json":{"schema":{"anyOf":[{"$ref":"#/components/schemas/NativeLlmGatewayError"},{}]}},"*/*":{"schema":{}}}
                }
            }
        });
        if let Some(item) = paths
            .entry(path)
            .or_insert_with(|| json!({}))
            .as_object_mut()
        {
            item.insert("post".into(), operation);
        }
    }
    for (path, operation, collection) in [
        ("/v1/models", "native_llm_models", true),
        ("/v1/models/{model}", "native_llm_model", false),
    ] {
        let model = json!({"type":"object","additionalProperties":true,"required":["id","object","created","owned_by"],"properties":{"id":{"type":"string"},"object":{"type":"string","enum":["model"]},"created":{"type":"integer"},"owned_by":{"type":"string"}}});
        let response = if collection {
            json!({"type":"object","additionalProperties":true,"required":["object","data"],"properties":{"object":{"type":"string","enum":["list"]},"data":{"type":"array","items":model}}})
        } else {
            model
        };
        let mut operation = json!({
            "operationId":operation,"tags":["Native LLM"],
            "description":"Current authenticated tenant's visible enabled registered models, intersected with key, tenant and user service/model restrictions. No provider credential is exposed.",
            "security":[{"NativeLlmBearer":[]},{"NativeLlmApiKey":[]}],
            "responses":{"200":{"description":"Visible registered model catalog.","content":{"application/json":{"schema":response}}},"default":{"description":"Exact Gateway error envelope.","content":{"application/json":{"schema":{"$ref":"#/components/schemas/NativeLlmGatewayError"}}}}}
        });
        if !collection {
            operation["parameters"] = json!([{"name":"model","in":"path","required":true,"schema":{"type":"string"},"description":"Provider-qualified model ID; the native route captures embedded slashes."}]);
        }
        if let Some(item) = paths
            .entry(path)
            .or_insert_with(|| json!({}))
            .as_object_mut()
        {
            item.insert("get".into(), operation);
        }
    }
    let Some(components) = section(document, "components") else {
        return;
    };
    let schemes = components
        .entry("securitySchemes")
        .or_insert_with(|| json!({}));
    if let Some(schemes) = schemes.as_object_mut() {
        schemes.insert("NativeLlmBearer".into(), json!({"type":"http","scheme":"bearer","description":"Existing current-account access token or opaque bound database API key, as sent by OpenAI SDKs. JWT-shaped bearer values remain on JWT verification and never fall back to another credential."}));
        schemes.insert("NativeLlmApiKey".into(), json!({"type":"apiKey","in":"header","name":"x-api-key","description":"Anthropic SDK key header, normalized to the deployment's canonical API-key header and verified by the same AdmissionService/AuthService. Duplicate or conflicting credentials are rejected; no provider key is accepted as identity."}));
    }
    let schemas = components.entry("schemas").or_insert_with(|| json!({}));
    if let Some(schemas) = schemas.as_object_mut() {
        schemas.insert("NativeLlmGatewayError".into(), json!({
            "type":"object","additionalProperties":false,"required":["error"],
            "properties":{"error":{"type":"object","additionalProperties":false,"required":["type","code","message"],"properties":{"type":{"type":"string","enum":["gateway_error"]},"code":{"type":"string"},"message":{"type":"string","description":"The same sanitized stable code string as error.code."}}}}
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn additive_native_contract_keeps_responses_and_unknown_provider_fields_open() {
        let old = json!({"post":{"operationId":"existing_responses","x-existing":"preserve","responses":{"200":{"content":{"text/event-stream":{"schema":{"type":"string"}}}}}}});
        let mut document = json!({"openapi":"3.1.0","paths":{"/v1/responses":old},"components":{"schemas":{"Existing":{"type":"integer"}},"securitySchemes":{"NativeBearerAuth":{"type":"http","scheme":"bearer"}}}});
        extend_document(&mut document);
        assert_eq!(document["openapi"], "3.1.0");
        assert_eq!(document["paths"]["/v1/responses"], old);
        assert_eq!(
            document["components"]["schemas"]["Existing"],
            json!({"type":"integer"})
        );
        for path in ["/v1/chat/completions", "/v1/embeddings", "/v1/messages"] {
            let operation = &document["paths"][path]["post"];
            let request = &operation["requestBody"]["content"]["application/json"]["schema"];
            assert_eq!(
                request["additionalProperties"], true,
                "future provider/cache fields must remain expressible"
            );
            assert_eq!(
                operation["security"],
                json!([{"NativeLlmBearer":[]},{"NativeLlmApiKey":[]}])
            );
            assert!(
                operation["responses"]["200"]["headers"]["x-route-decision"]
                    .get("required")
                    .is_none()
            );
        }
        assert!(
            document["paths"]["/v1/chat/completions"]["post"]["responses"]["200"]["content"]
                .get("text/event-stream")
                .is_some()
        );
        assert!(
            document["paths"]["/v1/embeddings"]["post"]["responses"]["200"]["content"]
                .get("text/event-stream")
                .is_none()
        );
        assert_eq!(
            document["components"]["schemas"]["NativeLlmGatewayError"]["properties"]["error"]["properties"]
                ["type"]["enum"],
            json!(["gateway_error"])
        );
        assert!(document["paths"]["/v1/models"]["get"].is_object());
        assert!(document["paths"]["/v1/models/{model}"]["get"].is_object());
        assert!(!document.to_string().contains("itemSchema"));
        let once = document.clone();
        extend_document(&mut document);
        assert_eq!(document, once);
    }
}
