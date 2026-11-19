//! Production DTO/parser/projection checks. No services, credentials or model.
use ai_platform_gateway_foundation::{
    gateway_dispatcher::GatewayDispatchError,
    invocation_http::{
        StreamChunkSchema, UnifiedRequestSchema, UnifiedResponseSchema, gateway_error_response,
    },
    native_contract::invocation_tasks as contract,
    protocol::{
        ContentData, ContentItem, ProtocolTimestamp, StreamChunk, StreamEventType, UnifiedRequest,
        UnifiedResponse,
    },
    task_http::TaskSchema,
    task_manager::{Task, TaskError, TaskResult, TaskStatus},
};
use serde_json::{Value, json};
fn valid(schema: Value, value: &Value) -> bool {
    jsonschema::validator_for(&schema).unwrap().is_valid(value)
}
fn document() -> Value {
    let mut d = json!({"paths":{},"components":{"schemas":{}}});
    contract::extend_document(&mut d);
    d
}
#[test]
fn actual_request_parser_acceptance_is_covered_without_moving_dtos() {
    for extra in [
        json!({}),
        json!({"priority":true}),
        json!({"priority":" +2.0 "}),
        json!({"timestamp":"2026-10-14T02:00:00Z"}),
        json!({"timestamp":0}),
        json!({"request_id":null,"session_id":null,"context":{},"parameters":null,"extra":"ignored"}),
    ] {
        let mut value =
            json!({"service_id":"fixture","inputs":[{"type":"text","data":"hi","unknown":1}]});
        value
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        assert!(
            UnifiedRequestSchema::from_value(value.clone()).is_ok(),
            "{value}"
        );
        assert!(valid(contract::request_schema(), &value), "{value}");
    }
    for value in [
        json!({}),
        json!([]),
        json!({"service_id":1,"inputs":[]}),
        json!({"service_id":"s","inputs":[{"type":"unknown"}]}),
        json!({"service_id":"s","inputs":[{"type":"json","data":{}}]}),
    ] {
        assert!(UnifiedRequestSchema::from_value(value.clone()).is_err());
        assert!(!valid(contract::request_schema(), &value));
    }
    // Documented coercion boundary: the shape does not promise every string is an integer.
    let value = json!({"service_id":"s","inputs":[],"priority":"not-an-integer"});
    assert!(valid(contract::request_schema(), &value));
    assert!(UnifiedRequestSchema::from_value(value).is_err());
}
#[test]
fn actual_domain_conversion_ignores_body_authority_and_extras() {
    let schema=UnifiedRequestSchema::from_value(json!({"service_id":"s","inputs":[{"type":"text","data":"x","secret":"drop"}],"user_id":"spoof","tenant_id":"other","extra":true})).unwrap();
    let domain = schema.to_domain("verified-user", "verified-tenant");
    assert_eq!(domain.user_id, "verified-user");
    assert_eq!(domain.tenant_id, "verified-tenant");
    let wire = serde_json::to_value(domain).unwrap();
    assert!(wire.get("extra").is_none());
    assert!(wire["inputs"][0].get("secret").is_none());
}
#[test]
fn actual_response_and_stream_projections_preserve_binary_nulls_and_wire_fields() {
    let mut content = ContentItem::text("x");
    content.data = Some(ContentData::Bytes(vec![255, 0, 1]));
    let mut response = UnifiedResponse::new("r", "completed", vec![content.clone()]);
    response.timestamp = ProtocolTimestamp::parse_isoformat("2026-10-14T02:00:00Z").unwrap();
    let value = serde_json::to_value(UnifiedResponseSchema::from_domain(&response)).unwrap();
    assert!(valid(contract::response_schema(), &value));
    assert_eq!(value["outputs"][0]["data"], "/wAB");
    assert_eq!(value["timestamp"], "2026-10-14T02:00:00Z");
    assert!(value["task_id"].is_null());
    let chunk = StreamChunk {
        request_id: "r".into(),
        chunk_index: 0,
        content,
        is_final: false,
        event_type: StreamEventType::TextDelta,
        tool_call: None,
        metadata: None,
    };
    let value = serde_json::to_value(StreamChunkSchema::from_domain(&chunk)).unwrap();
    assert!(valid(contract::chunk_schema(), &value));
    assert_eq!(value["content"]["data"], "/wAB");
}
#[test]
fn actual_task_projection_covers_all_states_and_arbitrary_stored_results() {
    let request = UnifiedRequest::new("r", "s", vec![]);
    let now = ProtocolTimestamp::parse_isoformat("2026-10-14T02:00:00Z").unwrap();
    for status in [
        TaskStatus::Pending,
        TaskStatus::Processing,
        TaskStatus::Completed,
        TaskStatus::Failed,
        TaskStatus::Cancelled,
    ] {
        for result in [
            None,
            Some(TaskResult::Json(json!([1, "x", null]))),
            Some(TaskResult::Response(Box::new(UnifiedResponse::new(
                "r",
                "completed",
                vec![],
            )))),
        ] {
            let mut task = Task::new("t".into(), &request, now.naive_utc());
            task.status = status;
            task.result = result;
            let value = serde_json::to_value(TaskSchema::from_domain(&task).unwrap()).unwrap();
            assert!(valid(contract::task_schema(), &value));
            assert!(!value.as_object().unwrap().contains_key("request_data"));
            assert!(!value.as_object().unwrap().contains_key("metadata"));
            assert!(!value.as_object().unwrap().contains_key("retries"));
        }
    }
}
#[tokio::test]
async fn uncertain_submit_error_projection_and_terminal_cancel_policy_are_explicit() {
    let response = gateway_error_response(
        &GatewayDispatchError::Task(TaskError::AdmissionUnconfirmed("t".into())),
        &axum::http::HeaderMap::new(),
    );
    assert_eq!(response.status(), 503);
    let body = axum::body::to_bytes(response.into_body(), 1024)
        .await
        .unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&body).unwrap(),
        json!({"error":"task_admission_unconfirmed","task_id":"t"})
    );
    assert!(TaskStatus::Pending.can_transition_to(TaskStatus::Cancelled));
    assert!(TaskStatus::Processing.can_transition_to(TaskStatus::Cancelled));
    assert!(!TaskStatus::Completed.can_transition_to(TaskStatus::Cancelled));
    assert!(!TaskStatus::Failed.can_transition_to(TaskStatus::Cancelled));
    let d = document();
    assert!(
        d["paths"][contract::SUBMIT]["post"]["responses"]
            .get("503")
            .is_some()
    );
    assert!(
        d["paths"][contract::TASK]["delete"]["responses"]
            .get("409")
            .is_some()
    );
}
#[test]
fn exact_seven_routes_keep_jwt_sse_and_task_list_contracts_distinct() {
    let d = document();
    assert_eq!(d["paths"].as_object().unwrap().len(), 6);
    for (method, path, id) in contract::ROUTES {
        let op = &d["paths"][path][method];
        assert_eq!(op["operationId"], *id);
        assert_eq!(op["security"], json!([{"NativeBearerAuth":[]}]));
        assert_eq!(op["responses"]["default"]["x-native-undeclared"], true);
    }
    let stream = &d["paths"][contract::STREAM]["post"];
    assert!(
        stream["responses"]["200"]["content"]
            .get("application/json")
            .is_none()
    );
    assert_eq!(stream["x-native-sse"]["error_data"], "text");
    assert_eq!(stream["x-native-sse"]["opening"], "first-body-poll");
    assert!(stream["responses"].get("429").is_none());
    let list = &d["paths"][contract::TASKS]["get"]["responses"]["200"]["content"]["application/json"]
        ["schema"];
    assert_eq!(list["type"], "array");
    assert_eq!(list["items"], contract::task_schema());
}
