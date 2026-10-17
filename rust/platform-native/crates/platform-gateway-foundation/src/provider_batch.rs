//! Provider Batch ports and bounded OpenAI implementation. No credential enters a task row.
use crate::{task_delivery::DeliveryIdentity, task_manager::TaskEnvelope};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{future::Future, pin::Pin, sync::Arc, time::Duration};
use uuid::Uuid;
pub type BatchFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, BatchError>> + Send + 'a>>;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BatchError {
    NotConfigured,
    InvalidPlan,
    Unavailable,
    UnknownSubmission,
    BindingMismatch,
    NotTerminal,
    AccountingRequired,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BatchPlan {
    pub provider_id: String,
    pub model_id: String,
    pub input_file_id: String,
    pub endpoint: String,
}
impl BatchPlan {
    pub fn from_envelope(envelope: &TaskEnvelope) -> Result<Option<Self>, BatchError> {
        let Some(value) = envelope
            .request
            .parameters
            .as_ref()
            .and_then(|p| p.get("provider_batch"))
        else {
            return Ok(None);
        };
        let plan: Self =
            serde_json::from_value(value.clone()).map_err(|_| BatchError::InvalidPlan)?;
        plan.validate()?;
        Ok(Some(plan))
    }
    pub fn validate(&self) -> Result<(), BatchError> {
        if [&self.provider_id, &self.model_id, &self.input_file_id]
            .iter()
            .any(|s| s.is_empty() || s.len() > 255 || s.chars().any(char::is_control))
            || !self.input_file_id.starts_with("file-")
            || !["/v1/responses", "/v1/chat/completions", "/v1/embeddings"]
                .contains(&self.endpoint.as_str())
        {
            return Err(BatchError::InvalidPlan);
        }
        Ok(())
    }
}
pub trait BatchProvider: Send + Sync {
    /// Called only for the winner of a newly committed submission intent.
    fn submit<'a>(&'a self, plan: &'a BatchPlan, nonce: Uuid) -> BatchFuture<'a, Value>;
    /// Read-only receipt discovery is safe after original execution authority expires.
    fn observe<'a>(&'a self, job_id: &'a str) -> BatchFuture<'a, Value>;
}
/// Host must provide explicit batch pricing/accounting; online model prices and
/// guessed discounts are forbidden. Implementations must settle idempotently by
/// task+claim+provider job and freeze their own batch price-book components.
pub trait BatchAccounting: Send + Sync {
    fn reserve<'a>(
        &'a self,
        identity: &'a DeliveryIdentity,
        claim: Uuid,
        plan: &'a BatchPlan,
    ) -> BatchFuture<'a, ()>;
    fn admit<'a>(
        &'a self,
        identity: &'a DeliveryIdentity,
        plan: &'a BatchPlan,
    ) -> BatchFuture<'a, ()>;
    fn settle<'a>(
        &'a self,
        identity: &'a DeliveryIdentity,
        claim: Uuid,
        plan: &'a BatchPlan,
        receipt: &'a Value,
    ) -> BatchFuture<'a, ()>;
}
#[derive(Default)]
pub struct BatchProviders {
    clients: std::collections::HashMap<(String, String), Arc<dyn BatchProvider>>,
}
impl BatchProviders {
    pub fn register(
        &mut self,
        tenant: &str,
        provider: &str,
        client: Arc<dyn BatchProvider>,
    ) -> Result<(), BatchError> {
        if tenant.is_empty()
            || provider.is_empty()
            || self.clients.contains_key(&(tenant.into(), provider.into()))
        {
            return Err(BatchError::InvalidPlan);
        }
        self.clients
            .insert((tenant.into(), provider.into()), client);
        Ok(())
    }
    pub fn get(&self, tenant: &str, provider: &str) -> Result<Arc<dyn BatchProvider>, BatchError> {
        self.clients
            .get(&(tenant.into(), provider.into()))
            .cloned()
            .ok_or(BatchError::NotConfigured)
    }
}
/// Only the official origin is supported by this bounded provider implementation.
/// Other compatible origins need their own operator-admitted implementation.
pub struct OpenAiBatchProvider {
    client: reqwest::Client,
    key: String,
}
impl OpenAiBatchProvider {
    pub fn new(key: String) -> Result<Self, BatchError> {
        if key.is_empty() || key.len() > 8192 || key.chars().any(char::is_control) {
            return Err(BatchError::NotConfigured);
        }
        let client = reqwest::Client::builder()
            .retry(reqwest::retry::never())
            .https_only(true)
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|_| BatchError::Unavailable)?;
        Ok(Self { client, key })
    }
    async fn request(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<Value, BatchError> {
        let mut request = self
            .client
            .request(method, format!("https://api.openai.com/v1/{path}"))
            .bearer_auth(&self.key);
        if let Some(body) = body {
            request = request.json(&body)
        }
        tokio::time::timeout(Duration::from_secs(30), async {
            let mut response = request.send().await.map_err(|_| BatchError::Unavailable)?;
            if !response.status().is_success() {
                return Err(BatchError::Unavailable);
            }
            let mut bytes = vec![];
            while let Some(chunk) = response
                .chunk()
                .await
                .map_err(|_| BatchError::Unavailable)?
            {
                if chunk.len() > 1_048_576usize.saturating_sub(bytes.len()) {
                    return Err(BatchError::Unavailable);
                }
                bytes.extend_from_slice(&chunk);
            }
            serde_json::from_slice(&bytes).map_err(|_| BatchError::Unavailable)
        })
        .await
        .map_err(|_| BatchError::Unavailable)?
    }
}
impl BatchProvider for OpenAiBatchProvider {
    fn submit<'a>(&'a self, plan: &'a BatchPlan, nonce: Uuid) -> BatchFuture<'a, Value> {
        Box::pin(async move {
            plan.validate()?;
            self.request(reqwest::Method::POST,"batches",Some(json!({"input_file_id":plan.input_file_id,"endpoint":plan.endpoint,"completion_window":"24h","metadata":{"agentforge_nonce":nonce.to_string()}}))).await.map_err(|_|BatchError::UnknownSubmission)
        })
    }
    fn observe<'a>(&'a self, job_id: &'a str) -> BatchFuture<'a, Value> {
        Box::pin(async move {
            if !valid_job(job_id) {
                return Err(BatchError::BindingMismatch);
            }
            self.request(reqwest::Method::GET, &format!("batches/{job_id}"), None)
                .await
        })
    }
}
pub(crate) fn valid_job(value: &str) -> bool {
    value.starts_with("batch_")
        && value.len() <= 255
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
}
pub(crate) fn verify_receipt(
    receipt: &Value,
    plan: &BatchPlan,
    nonce: Uuid,
    expected_job: Option<&str>,
) -> Result<String, BatchError> {
    let job = receipt["id"]
        .as_str()
        .filter(|s| valid_job(s))
        .ok_or(BatchError::BindingMismatch)?;
    if expected_job.is_some_and(|id| id != job)
        || receipt["input_file_id"] != plan.input_file_id
        || receipt["endpoint"] != plan.endpoint
        || receipt["completion_window"] != "24h"
        || receipt["metadata"]["agentforge_nonce"] != nonce.to_string()
    {
        return Err(BatchError::BindingMismatch);
    }
    Ok(job.into())
}
pub(crate) fn terminal_status(receipt: &Value) -> Result<&'static str, BatchError> {
    match receipt["status"].as_str() {
        Some("completed") => Ok("completed"),
        Some("failed" | "expired" | "cancelled") => Ok("failed"),
        Some("validating" | "in_progress" | "finalizing" | "cancelling") => {
            Err(BatchError::NotTerminal)
        }
        _ => Err(BatchError::BindingMismatch),
    }
}
#[cfg(test)]
#[path = "provider_batch_tests.rs"]
mod tests;
