//! Declarations for existing generic invocation/task routes; no execution or DTO relocation.
//! Existing production parsers/projections are checked against these shapes by crate tests.
use serde_json::{Value, json};
pub const INVOKE: &str = "/api/v1/invoke";
pub const STREAM: &str = "/api/v1/stream";
pub const SUBMIT: &str = "/api/v1/submit";
pub const TASKS: &str = "/api/v1/tasks";
pub const TASK: &str = "/api/v1/tasks/{task_id}";
pub const RESULT: &str = "/api/v1/tasks/{task_id}/result";
pub const ROUTES: &[(&str, &str, &str)] = &[
    ("post", INVOKE, "invoke_api_v1_invoke_post"),
    ("post", STREAM, "stream_api_v1_stream_post"),
    ("post", SUBMIT, "submit_api_v1_submit_post"),
    ("get", TASKS, "list_tasks_api_v1_tasks_get"),
    ("get", TASK, "get_task_api_v1_tasks__task_id__get"),
    ("delete", TASK, "cancel_task_api_v1_tasks__task_id__delete"),
    (
        "get",
        RESULT,
        "get_task_result_api_v1_tasks__task_id__result_get",
    ),
];
fn object(properties: Value) -> Value {
    let required: Vec<_> = properties
        .as_object()
        .expect("properties")
        .keys()
        .cloned()
        .collect();
    json!({"type":"object","properties":properties,"required":required,"additionalProperties":false})
}
fn nullable_string() -> Value {
    json!({"type":["string","null"]})
}
fn nullable_map() -> Value {
    json!({"type":["object","null"],"additionalProperties":true})
}
fn content_schema(input: bool) -> Value {
    let mut schema = object(json!({
        "type":{"type":"string","enum":["text","image","audio","video","file","json","tool_call","tool_result"]},
        "data":if input {nullable_string()} else {json!({"description":"Native ContentData JSON value; binary data is projected to base64 text."})},
        "url":nullable_string(),"mime_type":nullable_string(),"metadata":nullable_map()
    }));
    if input {
        schema["required"] = json!(["type"]);
        schema["additionalProperties"] = json!(true);
    }
    schema
}
pub fn request_schema() -> Value {
    json!({"type":"object","required":["service_id","inputs"],"additionalProperties":true,
        "properties":{
            "request_id":nullable_string(),"service_id":{"type":"string"},
            "inputs":{"type":"array","items":content_schema(true)},"session_id":nullable_string(),
            "context":nullable_map(),"parameters":nullable_map(),"callback_url":nullable_string(),
            "priority":{"type":["integer","number","string","boolean"],"default":0,
                "description":"Actual from_value parser additionally requires a finite integral value within i64; accepted string/boolean coercions remain parser-owned."},
            "user_id":nullable_string(),"tenant_id":nullable_string(),
            "timestamp":{"type":["string","number","null"],"description":"Parser-owned ISO/date or finite epoch coercion; not every string/number is accepted."}
        },"x-native-validation":"UnifiedRequestSchema::from_value owns coercion, range and timestamp validation. JSON unknown fields are accepted and dropped by to_domain. Body user_id/tenant_id never grant authority; verified account values replace them. No route-local body-size bound is declared; host middleware owns deployment limits."})
}
pub fn response_schema() -> Value {
    object(
        json!({"request_id":{"type":"string"},"status":{"type":"string"},
        "outputs":{"type":"array","items":content_schema(false)},"session_id":nullable_string(),
        "task_id":nullable_string(),"usage":nullable_map(),"error":nullable_map(),"metadata":nullable_map(),
        "timestamp":{"type":"string","description":"Protocol timestamp; aware UTC renders Z, naive forms remain possible."}}),
    )
}
pub fn chunk_schema() -> Value {
    object(
        json!({"request_id":{"type":"string"},"chunk_index":{"type":"integer"},
        "content":content_schema(false),"is_final":{"type":"boolean"},"event_type":{"type":"string","description":"Existing protocol StreamEventType wire value."},
        "tool_call":{"anyOf":[{"type":"null"},object(json!({"tool_call_id":{"type":"string"},"name":{"type":"string"},"arguments":{"type":"string"},"status":{"type":"string"}}))]},
        "metadata":nullable_map()}),
    )
}
pub fn task_schema() -> Value {
    object(
        json!({"task_id":{"type":"string"},"request_id":{"type":"string"},"service_id":{"type":"string"},
        "status":{"type":"string","enum":["pending","processing","completed","failed","cancelled"]},
        "created_at":{"type":"string"},"updated_at":nullable_string(),"user_id":{"type":"string"},"tenant_id":{"type":"string"},
        "callback_url":nullable_string(),"result":{"description":"Stored arbitrary JSON or the explicit UnifiedResponse wire projection, including null."},
        "error":nullable_string(),"progress":{"type":"number"}}),
    )
}
pub fn cancelled_schema() -> Value {
    object(json!({"task_id":{"type":"string"},"status":{"type":"string","const":"cancelled"}}))
}
fn response(description: &str, schema: Value) -> Value {
    json!({"description":description,"content":{"application/json":{"schema":schema}}})
}
pub fn extend_document(document: &mut Value) {
    document["components"]["schemas"]["NativeInvocationRequest"] = request_schema();
    document["components"]["schemas"]["NativeInvocationResponse"] = response_schema();
    document["components"]["schemas"]["NativeInvocationStreamChunk"] = chunk_schema();
    document["components"]["schemas"]["NativeGenericTask"] = task_schema();
    for (method, path, id) in ROUTES {
        let mut operation = json!({"operationId":id,"security":[{"NativeBearerAuth":[]}],
            "x-native-owner":"invocation_http/task_http",
            "x-native-response-schema":"bounded-source-owned",
            "x-native-authorization":"Current JWT session plus PostgreSQL account/tenant authority is required. API keys, guest cookies and disabled-auth resolver modes are not admitted by these route factories.",
            "responses":{"default":{"description":"Other host, middleware, extractor and collaborator outcomes remain undeclared.","x-native-undeclared":true}}});
        if [INVOKE, STREAM, SUBMIT].contains(path) {
            operation["requestBody"] = json!({"required":true,"content":{"application/json":{"schema":{"$ref":"#/components/schemas/NativeInvocationRequest"}}},"description":"JSON and application/*+json accepted by existing content-type classifier; absent/non-JSON content does not become an invocation object."});
            let statuses: &[&str] = if *path == STREAM {
                &["400", "401", "403", "422", "500", "503"]
            } else {
                &["400", "401", "403", "404", "422", "429", "500", "503"]
            };
            for status in statuses {
                operation["responses"][*status] = response(
                    "Observed parser, authority or dispatcher failure; body variants are not narrowed here.",
                    json!({"type":"object","additionalProperties":true}),
                );
            }
            if *path == STREAM {
                operation["responses"]["200"] = json!({"description":"Pull-driven SSE. HTTP 200 is not execution success; opening/read failures after headers are event:error with text data, while ordinary data frames carry StreamChunk JSON. EOF alone is not a provider-completion guarantee.",
                    "content":{"text/event-stream":{"schema":{"type":"string"}}},
                    "headers":{"x-session-id":{"description":"Present only when a nonempty session ID was prepared.","schema":{"type":"string"}}}});
                operation["x-native-response-schema"] = json!("bounded-sse-source-owned");
                operation["x-native-sse"] = json!({"data_schema":chunk_schema(),"error_event":"error","error_data":"text","opening":"first-body-poll","completion":"producer-defined; EOF is not proof of provider completion","cancellation":"Dropping the body drops owned stream/admission state; no remote-effect cessation guarantee."});
            } else {
                let mut schema = response_schema();
                if *path == SUBMIT {
                    schema["properties"]["status"]["const"] = json!("pending");
                    schema["properties"]["task_id"] = json!({"type":"string"});
                    operation["x-native-task-admission"] = json!(
                        "200 pending acknowledges task admission, not execution. 503 may carry error=task_admission_unconfirmed and task_id; read that exact task before an explicit caller decision, never infer safe automatic replay. Durable execution currently requires JWT TaskActor revalidation."
                    );
                }
                operation["responses"]["200"] =
                    response("Invocation response or pending task receipt.", schema);
            }
        } else {
            for status in ["401", "500", "503"] {
                operation["responses"][status] = response(
                    "Authentication or task-storage failure; existing sanitized body variants.",
                    json!({"type":"object","additionalProperties":true}),
                );
            }
            if *path == TASKS {
                operation["parameters"] = json!([
                    {"name":"status","in":"query","required":false,"schema":{"type":"string","enum":["pending","processing","completed","failed","cancelled"]}},
                    {"name":"limit","in":"query","required":false,"schema":{"type":"integer","minimum":1,"maximum":100,"default":50},"description":"Existing query parser also accepts signed/whitespace/underscore and zero-only fractional spellings of integers."},
                    {"name":"offset","in":"query","required":false,"schema":{"type":"integer","minimum":0,"default":0},"description":"Large positive offsets saturate at usize::MAX; no additional cap."}]);
                operation["responses"]["422"] = response(
                    "Invalid query; detail can be a validation array or string.",
                    json!({"type":"object","additionalProperties":true}),
                );
                operation["responses"]["200"] = response(
                    "Owner-and-tenant scoped task list, including for administrators.",
                    json!({"type":"array","items":task_schema()}),
                );
            } else {
                operation["parameters"] = json!([{"name":"task_id","in":"path","required":true,"schema":{"type":"string"}}]);
                operation["responses"]["404"] = response(
                    "Absent or hidden task; result also returns404 when no UnifiedResponse result is available.",
                    json!({"type":"object","properties":{"detail":{"type":"string"}}}),
                );
                operation["x-native-task-visibility"] = json!(
                    "Verified owner or exact admin within the same nonempty tenant only. No generic platform_admin cross-tenant exception."
                );
                let schema = if *method == "delete" {
                    cancelled_schema()
                } else if *path == RESULT {
                    response_schema()
                } else {
                    task_schema()
                };
                operation["responses"]["200"] =
                    response("Owned task observation or cancellation transition.", schema);
                if *method == "delete" {
                    operation["responses"]["409"] = response(
                        "Completed/failed task cannot transition to cancelled; terminal result is retained.",
                        json!({"type":"object","properties":{"detail":{"type":"string"}}}),
                    );
                    operation["x-native-cancellation"] = json!(
                        "Pending/processing task transitions to cancelled; repeated cancellation is accepted. Worker abort is best effort: no dequeue, history deletion or remote-effect cessation guarantee."
                    );
                }
            }
        }
        document["paths"][*path][*method] = operation;
    }
}
