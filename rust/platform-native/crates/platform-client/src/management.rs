//! Public Gateway task, policy, capability and local-node management contract.
//! No capability execution, scheduling or Agent loop is implemented here.
use crate::{ApiRequest, Client, Error};
use reqwest::Method;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Pending,
    Processing,
    Completed,
    Failed,
    Cancelled,
}
impl TaskStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Processing => "processing",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
    pub fn terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }
    pub fn parse(value: &str) -> Result<Self, Error> {
        match value {
            "pending" => Ok(Self::Pending),
            "processing" => Ok(Self::Processing),
            "completed" => Ok(Self::Completed),
            "failed" => Ok(Self::Failed),
            "cancelled" => Ok(Self::Cancelled),
            _ => Err(Error::Request),
        }
    }
}
#[derive(Clone, Copy)]
pub struct TaskQuery {
    pub status: Option<TaskStatus>,
    pub limit: u16,
    pub offset: u64,
}
impl Default for TaskQuery {
    fn default() -> Self {
        Self {
            status: None,
            limit: 50,
            offset: 0,
        }
    }
}
impl TaskQuery {
    pub fn request(self) -> Result<ApiRequest, Error> {
        if !(1..=100).contains(&self.limit) {
            return Err(Error::Request);
        }
        let mut request = ApiRequest::get(&["api", "v1", "tasks"])
            .query("limit", self.limit)
            .query("offset", self.offset);
        if let Some(status) = self.status {
            request = request.query("status", status.as_str());
        }
        Ok(request)
    }
}
pub fn task_request(id: &str) -> ApiRequest {
    ApiRequest::get(&["api", "v1", "tasks", id])
}
pub fn task_result_request(id: &str) -> ApiRequest {
    ApiRequest::get(&["api", "v1", "tasks", id, "result"])
}
pub fn cancel_task_request(id: &str) -> ApiRequest {
    ApiRequest::mutation(Method::DELETE, &["api", "v1", "tasks", id], None)
}
pub fn capabilities_request() -> ApiRequest {
    ApiRequest::get(&["api", "v1", "assistant", "tools"])
}
pub fn policies_request() -> ApiRequest {
    ApiRequest::get(&["api", "v1", "assistant", "policies"])
}
pub fn node_events_request(device: &str, after: u64, limit: u16) -> Result<ApiRequest, Error> {
    if !(1..=500).contains(&limit) {
        return Err(Error::Request);
    }
    Ok(
        ApiRequest::get(&["api", "v1", "local-nodes", device, "events"])
            .query("after_sequence", after)
            .query("limit", limit),
    )
}
/// Cursor is advanced only after validating the whole batch for this device.
/// Callers choose retention; the cursor survives dropping old rendered events.
#[derive(Default)]
pub struct NodeEventCursor {
    pub after_sequence: u64,
}
impl NodeEventCursor {
    pub fn accept(&mut self, device: &str, response: &Value) -> Result<Vec<Value>, Error> {
        if response["device_id"].as_str() != Some(device) {
            return Err(Error::Response);
        }
        let events = response["events"].as_array().ok_or(Error::Response)?;
        if events.len() > 500 {
            return Err(Error::Response);
        }
        let mut cursor = self.after_sequence;
        let mut ids = std::collections::HashSet::new();
        let mut accepted = Vec::new();
        for event in events {
            let sequence = event["sequence"].as_u64().ok_or(Error::Response)?;
            let id = event["event_id"]
                .as_str()
                .filter(|id| !id.is_empty())
                .ok_or(Error::Response)?;
            if sequence <= self.after_sequence {
                continue;
            }
            if sequence <= cursor || !ids.insert(id) {
                return Err(Error::Response);
            }
            cursor = sequence;
            accepted.push(event.clone());
        }
        self.after_sequence = cursor;
        Ok(accepted)
    }
}
impl Client {
    pub async fn tasks(&self, query: TaskQuery) -> Result<Value, Error> {
        self.execute(query.request()?).await
    }
    pub async fn task(&self, id: &str) -> Result<Value, Error> {
        self.execute(task_request(id)).await
    }
    pub async fn task_result(&self, id: &str) -> Result<Value, Error> {
        self.execute(task_result_request(id)).await
    }
    pub async fn cancel_task(&self, id: &str) -> Result<Value, Error> {
        self.execute(cancel_task_request(id)).await
    }
    pub async fn capabilities(&self) -> Result<Value, Error> {
        self.execute(capabilities_request()).await
    }
    pub async fn policies(&self) -> Result<Value, Error> {
        self.execute(policies_request()).await
    }
    pub async fn local_nodes(&self) -> Result<Value, Error> {
        self.execute(ApiRequest::get(&["api", "v1", "local-nodes"]))
            .await
    }
    pub async fn local_node(&self, id: &str, view: &str) -> Result<Value, Error> {
        if !["status", "capabilities", "doctor", "grants"].contains(&view) {
            return Err(Error::Request);
        }
        self.execute(ApiRequest::get(&["api", "v1", "local-nodes", id, view]))
            .await
    }
    pub async fn local_node_events(
        &self,
        id: &str,
        after: u64,
        limit: u16,
    ) -> Result<Value, Error> {
        self.execute(node_events_request(id, after, limit)?).await
    }
}
