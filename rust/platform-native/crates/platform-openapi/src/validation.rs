//! Offline official OAS 3.2 schema-base validation (including the 2020-12 dialect).
use jsonschema::{Draft, Resource, Retrieve, Uri};
use serde_json::Value;
struct Offline;
impl Retrieve for Offline {
    fn retrieve(&self, _: &Uri<String>) -> Result<Value, Box<dyn std::error::Error + Send + Sync>> {
        Err(std::io::Error::other("unbundled schema retrieval is forbidden").into())
    }
}
const RESOURCES: &[&str] = &[
    include_str!("../../../../../sdk/openapi/3.2/schema-2026-08-30.json"),
    include_str!("../../../../../sdk/openapi/3.2/schema-base-2026-08-30.json"),
    include_str!("../../../../../sdk/openapi/3.2/meta-2026-02-26.json"),
    include_str!("../../../../../sdk/openapi/3.2/dialect-2026-02-26.json"),
];
// referencing 0.33 selects an overriding dynamic anchor's contents but retains the
// originating resolver URI when that subschema has no $id (anchors/mod.rs:93-105).
// The official schema-base anchor then resolves #/$defs/dialect against schema,
// not schema-base. Bind local references to their lexical resource URI before
// compilation: absolute and relative forms denote exactly the same target. Keep
// dynamicRef dynamic (including its anchor); do not inline or erase assertions.
fn bind_local_references(value: &mut Value, resource_uri: &str) {
    match value {
        Value::Object(map) => {
            let resource_uri = map
                .get("$id")
                .and_then(Value::as_str)
                .unwrap_or(resource_uri)
                .to_owned();
            for (key, child) in map {
                if matches!(key.as_str(), "$ref" | "$dynamicRef") {
                    if let Some(reference) = child
                        .as_str()
                        .filter(|reference| reference.starts_with('#'))
                    {
                        *child = Value::String(format!("{resource_uri}{reference}"));
                    }
                } else {
                    bind_local_references(child, &resource_uri);
                }
            }
        }
        Value::Array(values) => {
            for child in values {
                bind_local_references(child, resource_uri);
            }
        }
        _ => {}
    }
}
pub fn validate(document: &Value) -> Result<(), String> {
    let mut options = jsonschema::options()
        .with_draft(Draft::Draft202012)
        .with_retriever(Offline);
    for source in RESOURCES {
        let mut value: Value = serde_json::from_str(source).map_err(|e| e.to_string())?;
        let uri = value["$id"]
            .as_str()
            .ok_or("official resource lacks id")?
            .to_owned();
        bind_local_references(&mut value, &uri);
        options = options.with_resource(
            uri,
            Resource::from_contents(value).map_err(|e| e.to_string())?,
        );
    }
    let mut root: Value = serde_json::from_str(RESOURCES[1]).map_err(|e| e.to_string())?;
    let root_uri = root["$id"]
        .as_str()
        .ok_or("official root lacks id")?
        .to_owned();
    bind_local_references(&mut root, &root_uri);
    let validator = options
        .build(&root)
        .map_err(|e| format!("official schema initialization: {e}"))?;
    let errors: Vec<_> = validator
        .iter_errors(document)
        .take(32)
        .map(|e| format!("{}: {e}", e.instance_path))
        .collect();
    if !errors.is_empty() {
        return Err(errors.join("\n"));
    }
    if document["openapi"] != "3.2.0" {
        return Err("release must use OpenAPI 3.2.0".into());
    }
    streaming_items(document)?;
    Ok(())
}
fn streaming_items(document: &Value) -> Result<(), String> {
    if let Some(paths) = document["paths"].as_object() {
        for (path, item) in paths {
            if let Some(operations) = item.as_object() {
                for (method, operation) in operations {
                    if let Some(responses) = operation["responses"].as_object() {
                        for (status, response) in responses {
                            if let Some(content) = response["content"].as_object() {
                                for (media, description) in content {
                                    if [
                                        "text/event-stream",
                                        "application/x-ndjson",
                                        "application/jsonl",
                                        "application/json-seq",
                                    ]
                                    .contains(&media.as_str())
                                        && description.get("itemSchema").is_none()
                                    {
                                        return Err(format!(
                                            "stream itemSchema missing: {method} {path} {status} {media}"
                                        ));
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    Ok(())
}
/// Replay one parsed SSE event. JSON Schema contentSchema is an annotation, so JSON
/// decoding and content validation are explicit instead of pretending it asserts itself.
pub fn validate_sse_event(
    document: &Value,
    path: &str,
    method: &str,
    event: &Value,
) -> Result<(), String> {
    let operation = &document["paths"][path][method];
    let mut schema =
        operation["responses"]["200"]["content"]["text/event-stream"]["itemSchema"].clone();
    if !schema.is_object() {
        return Err("SSE item schema missing".into());
    }
    schema["components"] = document["components"].clone();
    let validator = jsonschema::options()
        .with_draft(Draft::Draft202012)
        .with_retriever(Offline)
        .build(&schema)
        .map_err(|e| e.to_string())?;
    validator.validate(event).map_err(|e| e.to_string())?;
    if event["event"] != "error" {
        if let Some(content) = operation.pointer("/x-native-sse/data_schema") {
            let data = event["data"].as_str().ok_or("event data must be string")?;
            let parsed: Value = serde_json::from_str(data).map_err(|e| e.to_string())?;
            let mut root = content.clone();
            root["components"] = document["components"].clone();
            let validator = jsonschema::options()
                .with_draft(Draft::Draft202012)
                .with_retriever(Offline)
                .build(&root)
                .map_err(|e| e.to_string())?;
            validator.validate(&parsed).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}
