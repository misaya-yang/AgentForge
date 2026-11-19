//! Owner-authenticated run and SDK approval management. No execution or retry loop.
use crate::{ApiRequest, Client, Error};
use reqwest::Method;
use serde_json::{Value, json};

pub fn validate_id(id: &str) -> Result<(), Error> {
    if id.len() != 36
        || !id.bytes().enumerate().all(|(i, b)| {
            if [8, 13, 18, 23].contains(&i) {
                b == b'-'
            } else {
                b.is_ascii_hexdigit() && !b.is_ascii_uppercase()
            }
        })
    {
        return Err(Error::Request);
    }
    Ok(())
}
pub fn run_request(id: &str) -> Result<ApiRequest, Error> {
    validate_id(id)?;
    Ok(ApiRequest::get(&["api", "v1", "assistant", "runs", id]))
}
pub fn cancel_request(id: &str) -> Result<ApiRequest, Error> {
    validate_id(id)?;
    Ok(ApiRequest::mutation(
        Method::POST,
        &["api", "v1", "assistant", "tasks", id, "cancel"],
        Some(json!({"reason":"client_interrupt"})),
    ))
}
pub fn approval_request(id: &str) -> Result<ApiRequest, Error> {
    validate_id(id)?;
    Ok(ApiRequest::get(&[
        "api",
        "v1",
        "assistant",
        "approvals",
        id,
    ]))
}
/// Values copied unchanged from a reviewed server approval. Never fabricate IDs/hashes.
pub struct ApprovalBinding {
    pub run_id: String,
    pub tool_call_id: String,
    pub arguments_hash: String,
}
impl ApprovalBinding {
    pub fn validate(&self) -> Result<(), Error> {
        validate_id(&self.run_id)?;
        if self.tool_call_id.is_empty()
            || self.tool_call_id.len() > 160
            || self.tool_call_id.chars().any(char::is_control)
            || self.arguments_hash.len() != 64
            || !self
                .arguments_hash
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(Error::Request);
        }
        Ok(())
    }
    fn verify(&self, value: &Value, approved: bool) -> Result<(), Error> {
        self.validate()?;
        let a = &value["approval"];
        if a["run_id"] != self.run_id
            || a["tool_call_id"] != self.tool_call_id
            || a["arguments_hash"] != self.arguments_hash
            || a["status"] != "pending"
        {
            return Err(Error::Contract(
                "Approval changed or is no longer pending; inspect it again",
            ));
        }
        if approved
            && (value["preview"]["can_approve"] != true
                || value["preview"]["arguments_hash"] != self.arguments_hash
                || a["active"] != true
                || a["expires_in_seconds"].as_f64().unwrap_or(0.0) <= 0.0)
        {
            return Err(Error::Contract(
                "Action cannot be verified for approval; inspect it again",
            ));
        }
        Ok(())
    }
}
// Explicit projections prevent new server fields (e.g. credentials/raw arguments)
// from silently becoming CLI output. The reviewed preview is server-curated.
fn project(value: &Value, fields: &[&str]) -> Value {
    let mut result = json!({});
    for key in fields {
        if let Some(v) = value.get(*key) {
            result[*key] = v.clone();
        }
    }
    result
}
/// Preserve observed SDK usage without exposing arbitrary future provider metadata.
fn usage_tokens(value: &Value) -> Value {
    let mut result = json!({});
    for key in ["input_tokens", "output_tokens", "total_tokens", "cached_tokens", "cache_write_tokens", "cache_read_input_tokens", "cache_creation_input_tokens"] {
        if let Some(v) = value.get(key).filter(|v| v.as_i64().is_some_and(|n| n >= 0)) { result[key] = v.clone(); }
    }
    if let Some(details) = value.get("input_tokens_details").filter(|v| v.is_object()) {
        let mut filtered = json!({});
        for key in ["cached_tokens", "cache_write_tokens"] {
            if let Some(v) = details.get(key).filter(|v| v.as_i64().is_some_and(|n| n >= 0)) {
                filtered[key] = v.clone();
            }
        }
        result["input_tokens_details"] = filtered;
    }
    result
}
fn run_view(id: &str, value: Value) -> Result<Value, Error> {
    let run = &value["run"];
    if run["run_id"].as_str() != Some(id)
        || !matches!(run["engine"].as_str(), Some("agent_runtime" | "openai_agents" | "claude_agent_sdk"))
        || run["status"].as_str().is_none_or(str::is_empty)
    {
        return Err(Error::Response);
    }
    let mut result = project(run, &["run_id","session_id","engine","status","terminal_reason","started_at","finished_at","updated_at"]);
    if let Some(usage) = run.get("usage").filter(|v| v.is_object()) {
        let mut filtered = usage_tokens(usage);
        if let Some(sdk) = usage.get("sdk_usage").filter(|v| v.is_object()) {
            let mut metadata = json!({});
            for (key, allowed) in [
                ("state", &["reported", "unavailable"][..]),
                ("cost_state", &["reported", "unavailable"][..]),
                ("provider_outcome", &["completed", "unknown"][..]),
                ("platform_pricing_state", &["two_rate_compatible", "reconciliation_required"][..]),
                ("audit_persistence", &["durable", "unresolved_admission", "unavailable", "identity_conflict"][..]),
            ] {
                if let Some(value) = sdk.get(key).and_then(Value::as_str).filter(|value| allowed.contains(value)) {
                    metadata[key] = json!(value);
                }
            }
            metadata["accounting_status"] = json!("not_asserted");
            if let Some(cost) = sdk.get("provider_cost_usd").filter(|v| v.as_f64().is_some_and(|n| n.is_finite() && n >= 0.0)) { metadata["provider_cost_usd"] = cost.clone(); }
            if let Some(raw) = sdk.get("provider_usage") {
                if raw.is_object() { metadata["provider_usage"] = usage_tokens(raw); }
                else if let Some(items) = raw.as_array().filter(|items| items.len() <= 32) { metadata["provider_usage"] = Value::Array(items.iter().map(usage_tokens).collect()); }
            }
            filtered["sdk_usage"] = metadata;
        }
        result["usage"] = filtered;
    }
    Ok(json!({"run":result}))
}
fn approval_view(id: &str, value: Value) -> Result<Value, Error> {
    let a = &value["approval"];
    if a["approval_id"].as_str() != Some(id)
        || !matches!(
            a["engine"].as_str(),
            Some("openai_agents" | "claude_agent_sdk")
        )
        || !a["status"].is_string()
    {
        return Err(Error::Response);
    }
    let binding = ApprovalBinding {
        run_id: a["run_id"].as_str().ok_or(Error::Response)?.into(),
        tool_call_id: a["tool_call_id"].as_str().ok_or(Error::Response)?.into(),
        arguments_hash: a["arguments_hash"].as_str().ok_or(Error::Response)?.into(),
    };
    binding.validate().map_err(|_| Error::Response)?;
    Ok(
        json!({"approval": project(a, &["approval_id","run_id","tool_call_id","arguments_hash","status","tool_name","capability_id","engine","active","expires_in_seconds"]),
        "preview": project(&value["preview"], &["can_approve","reason","tool_name","effect","target","parameters","authorization_scope","expires_in_seconds","arguments_hash"])}),
    )
}
impl Client {
    pub async fn assistant_run(&self, id: &str) -> Result<Value, Error> {
        let value = self.execute(run_request(id)?).await?;
        run_view(id, value)
    }
    /// Single attempt. An invalid response after submission is an unknown outcome.
    pub async fn cancel_assistant_run(&self, id: &str) -> Result<Value, Error> {
        let value = self.execute(cancel_request(id)?).await?;
        if value["task_id"].as_str() != Some(id) || !value["cancelled"].is_boolean() {
            return Err(Error::Response);
        }
        Ok(project(&value, &["task_id", "session_id", "cancelled"]))
    }
    pub async fn sdk_approval(&self, id: &str) -> Result<Value, Error> {
        approval_view(id, self.execute(approval_request(id)?).await?)
    }
    /// Revalidate the reviewed binding, then submit exactly once. Expected fields
    /// preserve the reviewed identity and durable hash at the Gateway decision.
    pub async fn decide_sdk_approval(
        &self,
        id: &str,
        binding: &ApprovalBinding,
        approved: bool,
    ) -> Result<Value, Error> {
        binding.validate()?;
        binding.verify(&self.sdk_approval(id).await?, approved)?;
        let value = self.execute(ApiRequest::mutation(Method::POST,
            &["api","v1","assistant","approvals",id], Some(json!({"approved":approved,"expected_run_id":binding.run_id,"expected_tool_call_id":binding.tool_call_id,"expected_arguments_hash":binding.arguments_hash})))).await?;
        let a = &value["approval"];
        if a["approval_id"].as_str() != Some(id)
            || a["run_id"] != binding.run_id
            || a["approved"] != approved
            || a["status"] != if approved { "approved" } else { "rejected" }
        {
            return Err(Error::Response);
        }
        Ok(json!({"approval":project(a, &["approval_id","run_id","status","approved"])}))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    const ID: &str = "00000000-0000-4000-8000-000000000001";
    #[test]
    fn run_usage_preserves_partial_observations_and_excludes_runtime_identity() {
        let value = json!({"run":{"run_id":ID,"status":"completed","engine":"openai_agents","harness_thread_id":"not-sdk-lineage","usage":{"output_tokens":2,"secret":"hidden","sdk_usage":{"state":"unavailable","cost_state":"reported","provider_cost_usd":0.25,"provider_usage":{"cache_read_input_tokens":3,"secret":"hidden"},"accounting_status":"settled"}}}});
        let projected = run_view(ID,value).unwrap();
        assert_eq!(projected["run"]["usage"]["output_tokens"],2);
        assert!(projected["run"]["usage"].get("input_tokens").is_none());
        assert_eq!(projected["run"]["usage"]["sdk_usage"]["provider_usage"]["cache_read_input_tokens"],3);
        assert_eq!(projected["run"]["usage"]["sdk_usage"]["accounting_status"],"not_asserted");
        assert!(!projected.to_string().contains("hidden"));
        assert!(projected["run"].get("harness_thread_id").is_none());
    }
    #[test]
    fn run_usage_rejects_nested_or_unknown_metadata_states() {
        let value=json!({"run":{"run_id":ID,"status":"completed","engine":"openai_agents","usage":{"sdk_usage":{"state":{"secret":"hidden"},"cost_state":"settled","provider_outcome":[],"platform_pricing_state":1,"audit_persistence":"secret"}}}});
        let projected=run_view(ID,value).unwrap();
        assert_eq!(projected["run"]["usage"]["sdk_usage"],json!({"accounting_status":"not_asserted"}));
    }
    #[test]
    fn shared_run_recovery_contract() {
        let fixture: Value = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"), "/../../../../sdk/fixtures/run_recovery_v1.json"
        ))).unwrap();
        for case in fixture["cases"].as_array().unwrap() {
            let result = run_view(ID, case["response"].clone());
            if case["expected"].is_null() {
                assert!(result.is_err(), "{}", case["name"]);
            } else {
                assert_eq!(result.unwrap(), case["expected"], "{}", case["name"]);
            }
        }
    }
    #[tokio::test]
    async fn status_refresh_never_submits_and_cancel_requires_exact_echo() {
        let run = json!({"run":{"run_id":ID,"engine":"openai_agents","status":"running"}});
        let (client, server) = server(vec![run.clone(), run.clone(),
            json!({"task_id":"different","cancelled":true}), run]);
        client.assistant_run(ID).await.unwrap();
        client.assistant_run(ID).await.unwrap();
        assert!(matches!(client.cancel_assistant_run(ID).await, Err(Error::Response)));
        client.assistant_run(ID).await.unwrap();
        let requests = server.join().unwrap();
        assert_eq!(requests.len(), 4);
        for index in [0, 1, 3] {
            assert!(requests[index].starts_with(&format!("GET /api/v1/assistant/runs/{ID} ")));
        }
        assert!(requests[2].starts_with(&format!("POST /api/v1/assistant/tasks/{ID}/cancel ")));
        for request in requests {
            assert!(request.to_ascii_lowercase().contains("authorization: bearer synthetic-test-token"));
        }
    }
    fn binding() -> ApprovalBinding {
        ApprovalBinding {
            run_id: ID.into(),
            tool_call_id: "call_1".into(),
            arguments_hash: "a".repeat(64),
        }
    }
    fn fixture() -> Value {
        json!({"approval":{"approval_id":ID,"run_id":ID,"tool_call_id":"call_1","arguments_hash":"a".repeat(64),"status":"pending","engine":"openai_agents","active":true,"expires_in_seconds":30,"arguments":{"password":"hidden"},"credential":"hidden"},"preview":{"can_approve":true,"arguments_hash":"a".repeat(64),"credential":"hidden"}})
    }
    #[test]
    fn actual_routes_and_exact_decision_binding() {
        assert_eq!(
            run_request(ID).unwrap().segments,
            ["api", "v1", "assistant", "runs", ID]
        );
        let r = cancel_request(ID).unwrap();
        assert_eq!(r.method, Method::POST);
        assert_eq!(
            r.segments,
            ["api", "v1", "assistant", "tasks", ID, "cancel"]
        );
        assert!(binding().verify(&fixture(), true).is_ok());
        for field in ["run_id", "tool_call_id", "arguments_hash", "status"] {
            let mut v = fixture();
            v["approval"][field] = json!("changed");
            assert!(binding().verify(&v, true).is_err());
        }
        for field in ["can_approve", "arguments_hash"] {
            let mut v = fixture();
            v["preview"][field] = Value::Null;
            assert!(binding().verify(&v, true).is_err());
            assert!(binding().verify(&v, false).is_ok());
        }
    }
    #[test]
    fn projection_drops_raw_arguments_and_extra_fields() {
        let value = approval_view(ID, fixture()).unwrap();
        assert!(!value.to_string().contains("hidden"));
        let mut wrong = fixture();
        wrong["approval"]["approval_id"] = json!("different");
        assert!(approval_view(ID, wrong).is_err());
    }
    #[test]
    fn ids_and_hashes_are_bounded() {
        for id in [
            "../secret",
            "",
            "00000000000040008000000000000001",
            "00000000-0000-4000-8000-00000000000A",
        ] {
            assert!(validate_id(id).is_err());
        }
        let mut b = binding();
        b.arguments_hash = "a".repeat(63);
        assert!(b.validate().is_err());
        b = binding();
        b.tool_call_id = "x\n".into();
        assert!(b.validate().is_err());
    }
    fn server(responses: Vec<Value>) -> (Client, std::thread::JoinHandle<Vec<String>>) {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let handle = std::thread::spawn(move || {
            let mut requests = Vec::new();
            for body in responses {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                    .unwrap();
                let mut bytes = Vec::new();
                let mut buffer = [0; 4096];
                loop {
                    let n = stream.read(&mut buffer).unwrap();
                    assert!(n > 0);
                    bytes.extend_from_slice(&buffer[..n]);
                    if let Some(end) = bytes.windows(4).position(|b| b == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&bytes[..end]);
                        let length = headers
                            .lines()
                            .find_map(|line| {
                                line.to_ascii_lowercase()
                                    .strip_prefix("content-length: ")
                                    .map(|v| v.parse::<usize>().unwrap())
                            })
                            .unwrap_or(0);
                        if bytes.len() >= end + 4 + length {
                            break;
                        }
                    }
                }
                requests.push(String::from_utf8(bytes).unwrap());
                let body = body.to_string();
                write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",body.len(),body).unwrap();
            }
            requests
        });
        (
            Client::new(&origin)
                .unwrap()
                .authenticated("synthetic-test-token".into()),
            handle,
        )
    }
    #[tokio::test]
    async fn decision_transmits_reviewed_binding_once() {
        let (client, server) = server(vec![
            fixture(),
            json!({"approval":{"approval_id":ID,"run_id":ID,"status":"approved","approved":true}}),
        ]);
        client
            .decide_sdk_approval(ID, &binding(), true)
            .await
            .unwrap();
        let requests = server.join().unwrap();
        assert_eq!(requests.len(), 2);
        assert!(requests[0].starts_with(&format!("GET /api/v1/assistant/approvals/{ID} ")));
        assert!(requests[1].starts_with(&format!("POST /api/v1/assistant/approvals/{ID} ")));
        let body: Value =
            serde_json::from_str(requests[1].split("\r\n\r\n").nth(1).unwrap()).unwrap();
        assert_eq!(
            body,
            json!({"approved":true,"expected_run_id":ID,"expected_tool_call_id":"call_1","expected_arguments_hash":"a".repeat(64)})
        );
    }
    #[tokio::test]
    async fn changed_binding_never_submits_and_bad_echo_fails() {
        let mut changed = fixture();
        changed["approval"]["arguments_hash"] = json!("b".repeat(64));
        let (client, server) = server(vec![changed]);
        assert!(matches!(
            client.decide_sdk_approval(ID, &binding(), true).await,
            Err(Error::Contract(_))
        ));
        assert_eq!(server.join().unwrap().len(), 1);
        let (client, server) = self::server(vec![
            fixture(),
            json!({"approval":{"approval_id":"wrong","run_id":ID,"status":"approved","approved":true}}),
        ]);
        assert!(matches!(
            client.decide_sdk_approval(ID, &binding(), true).await,
            Err(Error::Response)
        ));
        assert_eq!(server.join().unwrap().len(), 2);
    }
}
