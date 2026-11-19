//! Pure OpenAPI projection; linked by both live Gateway and offline producer.
pub fn extend_document(document: &mut serde_json::Value) {
    use serde_json::json;
    document["components"]["schemas"]["ProblemDetails"] = json!({"type":"object","required":["type","title","status","detail"],"properties":{
        "type":{"type":"string","format":"uri-reference"},"title":{"type":"string"},"status":{"type":"integer","minimum":400,"maximum":599},
        "detail":{"type":"string"},"instance":{"type":"string","format":"uri-reference"},"trace_id":{"type":"string"}},"additionalProperties":true});
    document["x-agentforge-problem-types"] = json!([
        "urn:agentforge:problem:invalid-request",
        "urn:agentforge:problem:authentication-required",
        "urn:agentforge:problem:permission-denied",
        "urn:agentforge:problem:not-found",
        "urn:agentforge:problem:conflict",
        "urn:agentforge:problem:request-too-large",
        "urn:agentforge:problem:validation",
        "urn:agentforge:problem:rate-limit",
        "urn:agentforge:problem:upstream",
        "urn:agentforge:problem:unavailable",
        "urn:agentforge:problem:timeout"
    ]);
    if let Some(paths) = document["paths"].as_object_mut() {
        for (path, item) in paths {
            if path.starts_with("/mcp")
                || path.starts_with("/a2a/")
                || path.starts_with("/v1/")
                || path == "/api/v1/responses"
                || path.starts_with("/api/v1/langgraph/")
                || path.starts_with("/internal/")
                || path.starts_with("/api/v1/proxy/")
                || path == "/api/v1/invoke"
                || path == "/api/v1/stream"
            {
                continue;
            }
            if let Some(item) = item.as_object_mut() {
                for (method, operation) in item {
                    if !matches!(
                        method.as_str(),
                        "get" | "post" | "put" | "patch" | "delete" | "head" | "options"
                    ) {
                        continue;
                    }
                    if let Some(responses) = operation["responses"].as_object_mut() {
                        for (status, response) in responses {
                            if status.starts_with('4') || status.starts_with('5') {
                                if response
                                    .get("content")
                                    .and_then(|c| c.get("application/json"))
                                    .is_some()
                                {
                                    response["content"]["application/problem+json"] = json!({"schema":{"$ref":"#/components/schemas/ProblemDetails"}});
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
