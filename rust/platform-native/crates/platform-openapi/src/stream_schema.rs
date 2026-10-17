//! OAS 3.2 streaming describes parsed records, never the entire wire body.
use serde_json::{Value, json};
pub const DIALECT: &str = "https://spec.openapis.org/oas/3.2/dialect/2026-02-26";
pub fn upgrade(document: &mut Value) {
    document["openapi"] = json!("3.2.0");
    document["jsonSchemaDialect"] = json!(DIALECT);
    if let Some(paths) = document["paths"].as_object_mut() {
        for item in paths.values_mut() {
            if let Some(operations) = item.as_object_mut() {
                for (method, operation) in operations.iter_mut().filter(|(method, v)| {
                    [
                        "get", "put", "post", "delete", "options", "head", "patch", "trace",
                        "query",
                    ]
                    .contains(&method.as_str())
                        && v.is_object()
                }) {
                    let _ = method;
                    let data_schema = operation.pointer("/x-native-sse/data_schema").cloned();
                    if let Some(responses) = operation
                        .get_mut("responses")
                        .and_then(Value::as_object_mut)
                    {
                        for response in responses.values_mut() {
                            if let Some(content) =
                                response.get_mut("content").and_then(Value::as_object_mut)
                            {
                                for (media, description) in content {
                                    if media == "text/event-stream" {
                                        let mut event = json!({"type":"object","properties":{
                                            "data":{"type":"string"},"event":{"type":"string"},
                                            "id":{"type":"string"},"retry":{"type":"integer","minimum":0}
                                        }});
                                        if let Some(schema) = &data_schema {
                                            // Invocation failures are explicitly text, unlike normal JSON chunks.
                                            event = json!({"oneOf":[
                                                {"type":"object","required":["data"],"properties":{
                                                    "data":{"type":"string","contentMediaType":"application/json","contentSchema":schema},
                                                    "event":{"type":"string","not":{"const":"error"}},
                                                    "id":{"type":"string"},"retry":{"type":"integer","minimum":0}}},
                                                {"type":"object","required":["event","data"],"properties":{
                                                    "event":{"const":"error","type":"string"},"data":{"type":"string"},
                                                    "id":{"type":"string"},"retry":{"type":"integer","minimum":0}}}
                                            ]});
                                        }
                                        // Opaque provider passthrough includes JSON, [DONE], and text errors.
                                        // Do not invent a closed provider data schema or synthetic terminator.
                                        description["itemSchema"] = event;
                                    } else if [
                                        "application/x-ndjson",
                                        "application/jsonl",
                                        "application/json-seq",
                                    ]
                                    .contains(&media.as_str())
                                    {
                                        if description.get("itemSchema").is_none() {
                                            description["itemSchema"] = json!({});
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
