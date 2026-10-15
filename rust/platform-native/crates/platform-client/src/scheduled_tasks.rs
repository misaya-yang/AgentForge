//! Gateway-owned scheduled task admission and lifecycle, not a client scheduler.
//! Contract: platform-gateway migration_agent/scheduled.rs.
mod history;
pub use history::*;
use crate::{ApiRequest, Client, Error};
use reqwest::Method;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateScheduledTask {
    pub title: String,
    pub message: String,
    pub model_id: String,
    /// Explicit RFC3339 time with offset; the server validates syntax and futurity.
    pub run_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interval_seconds: Option<u32>,
    #[serde(default)]
    pub max_retries: u8,
}
impl CreateScheduledTask {
    pub fn request(&self) -> Result<ApiRequest, Error> {
        if self.title.trim().is_empty()
            || self.title.chars().count() > 120
            || self.message.trim().is_empty()
            || self.message.chars().count() > 200_000
            || self.model_id.trim().is_empty()
            || self.model_id.len() > 255
            || self.run_at.trim().is_empty()
            || self.run_at.len() > 64
            || self
                .interval_seconds
                .is_some_and(|n| !(60..=31_536_000).contains(&n))
            || self.max_retries > 3
        {
            return Err(Error::Request);
        }
        Ok(ApiRequest::mutation(
            Method::POST,
            &["api", "v1", "scheduled-tasks"],
            Some(serde_json::to_value(self).map_err(|_| Error::Request)?),
        ))
    }
}
#[derive(Clone, Copy)]
pub enum ScheduleAction {
    Pause,
    Resume,
    Cancel,
}
impl ScheduleAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pause => "pause",
            Self::Resume => "resume",
            Self::Cancel => "cancel",
        }
    }
}
pub fn list_request() -> ApiRequest {
    ApiRequest::get(&["api", "v1", "scheduled-tasks"])
}
pub fn executions_request(id: &str) -> ApiRequest {
    ApiRequest::get(&["api", "v1", "scheduled-tasks", id, "executions"])
}
pub fn action_request(id: &str, action: ScheduleAction) -> ApiRequest {
    ApiRequest::mutation(
        Method::POST,
        &["api", "v1", "scheduled-tasks", id, action.as_str()],
        None,
    )
}
impl Client {
    pub async fn scheduled_tasks(&self) -> Result<Value, Error> {
        self.execute(list_request()).await
    }
    pub async fn create_scheduled_task(&self, input: &CreateScheduledTask) -> Result<Value, Error> {
        self.execute(input.request()?).await
    }
    pub async fn scheduled_task_executions(&self, id: &str) -> Result<Value, Error> {
        self.execute(executions_request(id)).await
    }
    /// Single attempt. A lost response never authorizes automatic re-admission.
    pub async fn scheduled_task_action(
        &self,
        id: &str,
        action: ScheduleAction,
    ) -> Result<Value, Error> {
        self.execute(action_request(id, action)).await
    }
}
