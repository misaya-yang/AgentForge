//! Source-owned assistant session contracts; no published-schema input.
//! DTOs, routes and limits are shared with the real mounted Gateway handlers.
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
pub const COLLECTION: &str = "/api/v1/assistant/sessions";
pub const ITEM: &str = "/api/v1/assistant/sessions/{session_id}";
pub const HISTORY: &str = "/api/v1/assistant/sessions/{session_id}/history";
pub const LIST_MAX: i64 = 200;
pub const HISTORY_MAX: i64 = 500;
pub const ROUTES: &[(&str, &str, &str)] = &[
    ("GET", COLLECTION, "list_sessions"),
    ("POST", COLLECTION, "create_session"),
    ("GET", ITEM, "get_session"),
    ("DELETE", ITEM, "delete_session"),
    ("GET", HISTORY, "get_session_history"),
];
pub fn default_list_limit() -> i64 {
    50
}
pub fn default_history_limit() -> i64 {
    100
}
#[derive(Deserialize)]
pub struct ListLimit {
    #[serde(default = "default_list_limit")]
    pub limit: i64,
}
#[derive(Deserialize)]
pub struct HistoryLimit {
    #[serde(default = "default_history_limit")]
    pub limit: i64,
}
#[derive(Deserialize, Default)]
pub struct CreateSession {
    pub metadata: Option<Map<String, Value>>,
}
pub fn create_metadata(body: &[u8]) -> Result<Value, ()> {
    let request = if body.is_empty() {
        None
    } else {
        serde_json::from_slice::<Option<CreateSession>>(body).map_err(|_| ())?
    };
    Ok(Value::Object(
        request.and_then(|r| r.metadata).unwrap_or_default(),
    ))
}
fn object(properties: Value) -> Value {
    let required: Vec<_> = properties
        .as_object()
        .expect("properties")
        .keys()
        .cloned()
        .collect();
    json!({"type":"object","properties":properties,"required":required,"additionalProperties":false})
}
macro_rules! response_dto {
    ($name:ident, $schema:ident, {$($field:ident: $ty:ty => $shape:expr),* $(,)?}) => {
        #[derive(Serialize)]
        pub struct $name { $(pub $field: $ty),* }
        pub fn $schema() -> Value { object(json!({$(stringify!($field):$shape),*})) }
    };
}
response_dto!(SessionResponse, session_schema, {
    session_id: String => json!({"type":"string"}),
    user_id: String => json!({"type":"string"}),
    tenant_id: String => json!({"type":"string"}),
    service_id: Option<String> => json!({"type":["string","null"]}),
    created_at: Option<String> => json!({"type":["string","null"]}),
    updated_at: Option<String> => json!({"type":["string","null"]}),
    metadata: Value => json!({}),
    message_count: usize => json!({"type":"integer","minimum":0}),
});
// The private identity is deliberately absent on the wire; it is only an
// internal projection/deduplication key and never a source-access grant.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HistoryMessage {
    pub role: String,
    pub content: String,
    pub timestamp: Option<String>,
    pub metadata: Value,
    #[serde(skip)]
    pub identity: Value,
}
pub fn history_message_schema() -> Value {
    object(json!({"role":{"type":"string"},"content":{"type":"string"},
        "timestamp":{"type":["string","null"]},"metadata":{}}))
}
#[derive(Serialize)]
pub struct SessionList<T: Serialize> {
    pub sessions: Vec<T>,
    pub total: usize,
}
pub fn history_response(session_id: &str, messages: Vec<HistoryMessage>, total: usize) -> Value {
    json!({"session_id":session_id,"messages":messages,"total":total})
}
pub const DELETED_STATUS: &str = "deleted";
pub fn delete_response(session_id: &str) -> Value {
    json!({"session_id":session_id,"status":DELETED_STATUS})
}
fn success_schema(id: &str) -> Value {
    match id {
        "list_sessions" => object(
            json!({"sessions":{"type":"array","items":session_schema()},"total":{"type":"integer","minimum":0}}),
        ),
        "get_session_history" => object(
            json!({"session_id":{"type":"string"},"messages":{"type":"array","items":history_message_schema()},"total":{"type":"integer","minimum":0}}),
        ),
        "create_session" | "get_session" => session_schema(),
        "delete_session" => object(
            json!({"session_id":{"type":"string"},"status":{"type":"string","const":DELETED_STATUS}}),
        ),
        _ => panic!("unowned assistant session operation"),
    }
}
fn response(description: &str, schema: Value) -> Value {
    json!({"description":description,"content":{"application/json":{"schema":schema}}})
}
pub fn extend_document(document: &mut Value) {
    for (method, path, id) in ROUTES {
        let mut parameters = vec![];
        if path.contains("{session_id}") {
            parameters.push(
                json!({"name":"session_id","in":"path","required":true,"schema":{"type":"string"}}),
            );
        }
        let limit = match *id {
            "list_sessions" => Some((default_list_limit(), LIST_MAX)),
            "get_session_history" => Some((default_history_limit(), HISTORY_MAX)),
            _ => None,
        };
        if let Some((default, maximum)) = limit {
            parameters.push(json!({"name":"limit","in":"query","required":false,"schema":{"type":"integer","default":default,"minimum":1,"maximum":maximum},"description":"Serde query parsing; duplicate limit and non-integer values fail. Unknown keys ignored."}));
        }
        let detail = object(json!({"detail":{"type":"string"}}));
        let mut operation = json!({"operationId":super::roles::operation_id(method,path,id),
            "parameters":parameters,"security":[{"NativeBearerAuth":[]}],
            "x-native-owner":"platform_gateway::session_history and platform_gateway::sessions",
            "x-native-response-schema":"bounded-source-owned",
            "x-native-authorization":"Account JWT only; API keys rejected. Current authenticated tenant/user scope all storage access. Path identifiers are strings, not UUID-validated. History source visibility is rechecked fail-closed.",
            "responses":{"200":response("Success",success_schema(id)),
                "401":response("Account authentication required or failed",detail.clone()),
                "403":response("Authentication authority rejected account",detail.clone()),
                "500":response("Storage or projection failed",detail.clone()),
                "503":response("Authentication authority unavailable",detail.clone()),
                "default":{"description":"Other host/extractor/admission responses remain undeclared","x-native-undeclared":true}}});
        if path.contains("{session_id}") {
            operation["responses"]["404"] =
                response("Missing, deleted or other-owner session", detail.clone());
        }
        if limit.is_some() || *id == "create_session" {
            operation["responses"]["422"] =
                response("Invalid limit or request body", detail.clone());
        }
        if *id == "delete_session" {
            // Concrete AuthService::authenticate emits credentials/unavailable only.
            operation["responses"]
                .as_object_mut()
                .unwrap()
                .remove("403");
            operation["x-native-owner"] = json!(
                "platform_gateway::session_history::delete with NativeServicesRuntime durable cleanup"
            );
            operation["x-native-authorization"] = json!(
                "Account JWT only; API keys rejected. Exact authenticated tenant/user/session scope is checked by the durable cleanup owner before mutation and under its lifecycle lock. Session IDs are strings, not UUID-validated."
            );
            operation["responses"]["200"]["description"] = json!(
                "Logical deletion committed and permanent deletion barrier confirmed; asynchronous SDK cleanup may remain pending"
            );
            operation["responses"]["500"] = response(
                "Cleanup or barrier readback failed; detail is Failed to delete session",
                detail.clone(),
            );
            operation["responses"]["503"] = response(
                "Authentication authority unavailable, cleanup collaborator absent, or deletion/barrier not confirmed; deletion detail is Session deletion was not completed",
                detail.clone(),
            );
            operation["description"] = json!(
                "No request body or query fields consumed. Deletes only the authenticated owner session through the shared durable cleanup owner. Required Runtime cleanup must acknowledge deleted or not_found for this session (or HTTP 404); malformed or unknown acknowledgement blocks deletion. Success confirms logical deletion and committed SDK cleanup intent, not immediate physical erasure. Repeated or concurrently completed deletion returns 404. No fallback manager delete. A failed or ambiguous acknowledgement may follow a committed deletion; inspect state rather than assume rollback."
            );
        } else if *id == "get_session_history" {
            operation["responses"]["503"] = response(
                "Authentication or source visibility authority unavailable",
                json!({"anyOf":[detail.clone(),object(json!({"detail":{"type":"object","required":["code"],"properties":{"code":{"const":"ASSISTANT_SOURCE_CHECK_UNAVAILABLE"}},"additionalProperties":false}}))]}),
            );
            operation["description"] = json!(
                "Legacy plus Runtime/image history projection; latest limit messages. Dynamic metadata is open. Total preserves existing projection semantics, not a guaranteed count of currently visible messages. May renew expired session TTL; does not execute a turn."
            );
        } else if *id == "list_sessions" {
            operation["description"] = json!(
                "Active assistant and legacy null/empty-service sessions, ordered by updated_at descending. Total is returned row count. Metadata projects title, langgraph_thread_id, folder and pinned; message_count is zero."
            );
        } else if *id == "create_session" {
            operation["requestBody"] = json!({"required":false,"content":{"application/json":{"schema":{"anyOf":[{"type":"object","properties":{"metadata":{"type":["object","null"],"additionalProperties":true}},"additionalProperties":true},{"type":"null"},{"type":"array","minItems":1,"maxItems":1,"items":{"type":["object","null"],"additionalProperties":true}}]}}}});
            operation["responses"]["413"] =
                json!({"description":"Host request-body limit exceeded; transport-defined body"});
            operation["description"] = json!(
                "Empty, null or omitted body creates empty metadata; unknown keys ignored. Serde also accepts a one-element array containing metadata object or null. JSON is parsed from bytes without Content-Type enforcement. Session and single-kernel assignment commit atomically. Create timestamps omit UTC offset; subsequent reads include it. Ambiguous acknowledgement requires readback, not blind retry."
            );
        }
        if document["paths"].get(*path).is_none() {
            document["paths"][*path] = json!({});
        }
        document["paths"][*path][method.to_ascii_lowercase()] = operation;
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_session_contract_routes_and_auth_are_exact() {
        let doc = super::super::document();
        for (method, path, id) in ROUTES {
            let op = &doc["paths"][*path][method.to_ascii_lowercase()];
            assert_eq!(
                op["operationId"],
                super::super::roles::operation_id(method, path, id)
            );
            assert_eq!(op["security"], json!([{"NativeBearerAuth":[]}]));
            assert_eq!(op["responses"]["default"]["x-native-undeclared"], true);
        }
        assert_eq!(ROUTES.len(), 5);
        let request = &doc["paths"][COLLECTION]["post"]["requestBody"];
        assert_eq!(request["required"], false);
        let variants = request["content"]["application/json"]["schema"]["anyOf"]
            .as_array()
            .unwrap();
        assert_eq!(variants.len(), 3);
        let sequence = variants
            .iter()
            .find(|schema| schema["type"] == "array")
            .unwrap();
        assert_eq!(sequence["minItems"], 1);
        assert_eq!(sequence["maxItems"], 1);
        assert_eq!(sequence["items"]["type"], json!(["object", "null"]));
        assert_eq!(sequence["items"]["additionalProperties"], true);
        assert_eq!(
            doc["paths"][COLLECTION]["get"]["parameters"][0]["schema"]["maximum"],
            LIST_MAX
        );
        assert_eq!(
            doc["paths"][HISTORY]["get"]["parameters"][1]["schema"]["default"],
            default_history_limit()
        );
    }
    #[test]
    fn native_session_delete_contract_matches_handler_envelope() {
        let doc = super::super::document();
        let op = &doc["paths"][ITEM]["delete"];
        assert_eq!(op["security"], json!([{"NativeBearerAuth":[]}]));
        assert_eq!(
            op["parameters"],
            json!([{"name":"session_id","in":"path","required":true,"schema":{"type":"string"}}])
        );
        assert!(op.get("requestBody").is_none());
        let responses = op["responses"].as_object().unwrap();
        let mut statuses = responses.keys().map(String::as_str).collect::<Vec<_>>();
        statuses.sort_unstable();
        assert_eq!(statuses, vec!["200", "401", "404", "500", "503", "default"]);
        let schema = &responses["200"]["content"]["application/json"]["schema"];
        let actual = delete_response("legacy-not-a-uuid");
        assert_eq!(
            actual,
            json!({"session_id":"legacy-not-a-uuid","status":"deleted"})
        );
        assert_eq!(
            schema["properties"],
            json!({"session_id":{"type":"string"},"status":{"type":"string","const":"deleted"}})
        );
        assert_eq!(schema["required"], json!(["session_id", "status"]));
        assert_eq!(schema["additionalProperties"], false);
        for status in ["401", "404", "500", "503"] {
            assert_eq!(
                responses[status]["content"]["application/json"]["schema"],
                object(json!({"detail":{"type":"string"}}))
            );
        }
        assert_eq!(responses["default"]["x-native-undeclared"], true);
    }
    #[test]
    fn native_session_contract_preserves_create_parser_and_limits() {
        for input in [
            b"".as_slice(),
            b"null",
            b"{}",
            b"{\"metadata\":null}",
            b"{\"unknown\":true}",
            b"[null]",
        ] {
            assert_eq!(create_metadata(input), Ok(json!({})));
        }
        assert_eq!(
            create_metadata(br#"{"metadata":{"nested":[null,1,"text"]}}"#),
            Ok(json!({"nested":[null,1,"text"]}))
        );
        assert_eq!(
            create_metadata(br#"[{"title":"x"}]"#),
            Ok(json!({"title":"x"}))
        );
        for input in [
            b"{".as_slice(),
            b"[]",
            b"{\"metadata\":2}",
            b"[null,null]",
            b"[2]",
            b"[[]]",
        ] {
            assert!(create_metadata(input).is_err());
        }
        assert_eq!(
            serde_json::from_value::<ListLimit>(json!({}))
                .unwrap()
                .limit,
            50
        );
        assert_eq!(
            serde_json::from_value::<HistoryLimit>(json!({}))
                .unwrap()
                .limit,
            100
        );
    }
    #[test]
    fn native_session_contract_serialization_matches_envelopes() {
        let session = SessionResponse {
            session_id: "s".into(),
            user_id: "u".into(),
            tenant_id: "t".into(),
            service_id: None,
            created_at: None,
            updated_at: None,
            metadata: json!(["legacy"]),
            message_count: 0,
        };
        let actual = serde_json::to_value(&session).unwrap();
        let schema = session_schema();
        assert_eq!(
            actual.as_object().unwrap().keys().collect::<Vec<_>>(),
            schema["properties"]
                .as_object()
                .unwrap()
                .keys()
                .collect::<Vec<_>>()
        );
        let list = serde_json::to_value(SessionList {
            sessions: vec![session],
            total: 1,
        })
        .unwrap();
        assert_eq!(list["sessions"][0], actual);
        let message = HistoryMessage {
            role: "assistant".into(),
            content: "text".into(),
            timestamp: None,
            metadata: json!({"dynamic":true}),
            identity: json!({"secret":"internal"}),
        };
        let message = serde_json::to_value(&message).unwrap();
        assert_eq!(
            message.as_object().unwrap().keys().collect::<Vec<_>>(),
            history_message_schema()["properties"]
                .as_object()
                .unwrap()
                .keys()
                .collect::<Vec<_>>()
        );
        assert!(message.get("identity").is_none());
        assert_eq!(
            history_response("s", vec![], 0),
            json!({"session_id":"s","messages":[],"total":0})
        );
    }
}
